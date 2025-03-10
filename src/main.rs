use std::{
    env,
    fs,
    io,
    io::Write,
    path::Path,
};

use clap::{CommandFactory, FromArgMatches};
use eyre::Result;
use log::{info, debug, warn};
use rayon::prelude::*;
use itertools::Itertools;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

mod cli;
mod git;
mod diff;
mod repo;

use cli::{SlamCli, get_cli_tool_status};
use repo::{Change, Repo};

fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    let log_dir = Path::new("/var/log/messages/slam");
    let log_file_path = log_dir.join("slam.log");

    let log_writer: Box<dyn Write + Send> = if log_dir.exists() && log_dir.is_dir() {
        match fs::OpenOptions::new().create(true).append(true).open(&log_file_path) {
            Ok(file) => Box::new(file),
            Err(err) => {
                eprintln!(
                    "Log directory exists but is not writable: {}. Falling back to stdout.",
                    err
                );
                Box::new(io::stdout())
            }
        }
    } else {
        eprintln!(
            "Log directory {} not found. Falling back to stdout.",
            log_dir.display()
        );
        Box::new(io::stdout())
    };

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(log_writer))
        .format(|buf, record| {
            writeln!(
                buf,
                "[{level}] {timestamp} - {msg}",
                level = record.level(),
                timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                msg = record.args()
            )
        })
        .init();

    let mut cmd = SlamCli::command();
    cmd = cmd.after_help(get_cli_tool_status());
    let cli = SlamCli::from_arg_matches(&cmd.get_matches())?;
    info!("Starting SLAM");

    match cli.command {
        cli::SlamCommand::Create {
            files,
            delete,
            sub,
            regex,
            change_id,
            buffer,
            commit,
            repos,
        } => {
            let change = Change::from_args(delete, &sub, &regex);
            process_create_command(files, change, change_id, buffer, commit, repos)?;
        }
        cli::SlamCommand::Review { org, repos, action } => {
            process_review_command(org, &action, buffer_from_action(&action), repos)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

/// Extract the buffer from the CLI Action variant.
fn buffer_from_action(action: &cli::Action) -> usize {
    match action {
        cli::Action::Ls { buffer, .. } => *buffer,
        _ => 1,
    }
}

fn process_create_command(
    files: Option<String>,
    change: Option<Change>,
    change_id: String,
    buffer: usize,
    commit: Option<String>,
    user_repo_specs: Vec<String>,
) -> eyre::Result<()> {
    let root = std::env::current_dir()?;
    let discovered_paths = git::find_git_repositories(&root)?;

    let mut discovered_repos = Vec::new();
    for path in discovered_paths {
        if let Some(repo) = Repo::create_repo_from_local(&path, &root, &change, &files, &change_id) {
            discovered_repos.push(repo);
        }
    }

    let filtered_repos: Vec<_> = discovered_repos
        .into_iter()
        .filter(|repo| {
            user_repo_specs.is_empty()
                || user_repo_specs.iter().any(|spec| repo.reponame.contains(spec))
        })
        .sorted_by(|a, b| a.reponame.cmp(&b.reponame))
        .collect();
    let outputs: Vec<String> = filtered_repos
        .par_iter()
        .map(|repo| repo.create(&root, buffer, commit.as_deref()))
        .collect::<eyre::Result<Vec<String>>>()?;

    for output in outputs {
        println!("{}", output);
    }
    Ok(())
}

fn process_review_command(
    org: String,
    action: &cli::Action,
    default_buffer: usize,
    user_repo_specs: Vec<String>,
) -> eyre::Result<()> {
    // Extract change IDs to filter on from the CLI Action.
    let filter_change_ids: Vec<String> = match action {
        cli::Action::Ls { change_ids, .. } => change_ids.clone(),
        cli::Action::Approve { change_id, .. } => vec![change_id.clone()],
        cli::Action::Delete { change_id } => vec![change_id.clone()],
    };

    let repo_names = git::find_repos_in_org(&org)?;
    info!("Found {} repos in '{}'", repo_names.len(), org);

    let filtered_names: Vec<_> = repo_names
        .into_iter()
        .filter(|full_name| {
            user_repo_specs.is_empty() || user_repo_specs.iter().any(|pat| full_name.contains(pat))
        })
        .collect();
    info!("After user input filter, {} remain", filtered_names.len());

    // Use a parallel iterator to filter repositories that have an open PR matching one of the change IDs.
    let mut filtered_repos: Vec<Repo> = filtered_names
        .par_iter()
        .filter_map(|repo_name| {
            for cid in &filter_change_ids {
                match git::get_pr_number_for_repo(repo_name, cid) {
                    Ok(pr_number) if pr_number > 0 => {
                        info!(
                            "Found PR #{} for repo '{}' (change id: {})",
                            pr_number, repo_name, cid
                        );
                        return Some(Repo::create_repo_from_remote_with_pr(repo_name, cid, pr_number));
                    }
                    Ok(_) => {
                        debug!("No open PR found for '{}' with change id '{}'", repo_name, cid);
                    }
                    Err(err) => {
                        warn!("Error fetching PR for '{}': {}", repo_name, err);
                    }
                }
            }
            None
        })
        .collect();

    filtered_repos.sort_by(|a, b| a.reponame.cmp(&b.reponame));

    if filtered_repos.is_empty() {
        warn!(
            "No repositories found with an open PR matching change id(s): {:?}",
            filter_change_ids
        );
        return Ok(());
    }

    info!(
        "{} repositories have an open PR matching change id(s): {:?}",
        filtered_repos.len(),
        filter_change_ids
    );

    let outputs: Vec<String> = filtered_repos
        .par_iter()
        .map(|repo| repo.review(action, default_buffer))
        .collect::<eyre::Result<Vec<String>>>()?;

    for output in outputs {
        println!("{}", output);
    }
    Ok(())
}
