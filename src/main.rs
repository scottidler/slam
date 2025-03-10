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
        cli::SlamCommand::Review {
            change_id,
            org,
            approve,
            merge,
            admin_override,
            buffer,
            repos,
        } => {
            process_review_command(org, change_id, approve, merge, admin_override, buffer, repos)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
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
        .collect::<Result<Vec<String>, _>>()?;

    for output in outputs {
        println!("{}", output);
    }
    Ok(())
}

fn process_review_command(
    org: String,
    change_id: String,
    approve: bool,
    merge: bool,
    admin_override: bool,
    buffer: usize,
    user_repo_specs: Vec<String>,
) -> eyre::Result<()> {
    let repo_names = git::find_repos_in_org(&org)?;
    log::info!("Found {} repos in '{}'", repo_names.len(), org);

    let filtered_names: Vec<_> = repo_names
        .into_iter()
        .filter(|full_name| {
            user_repo_specs.is_empty() || user_repo_specs.iter().any(|pat| full_name.contains(pat))
        })
        .collect();
    log::info!("After user input filter, {} remain", filtered_names.len());

    let mut filtered_repos: Vec<Repo> = filtered_names
        .par_iter()
        .filter_map(|repo_name| {
            match git::get_pr_number_for_repo(repo_name, &change_id) {
                Ok(pr_number) if pr_number > 0 => {
                    info!("Found PR #{} for repo '{}'", pr_number, repo_name);
                    Some(Repo::create_repo_from_remote_with_pr(repo_name, &change_id, pr_number))
                }
                Ok(_) => {
                    debug!("No open PR found for '{}'", repo_name);
                    None
                }
                Err(err) => {
                    warn!("Error fetching PR for '{}': {}", repo_name, err);
                    None
                }
            }
        })
        .collect();

    filtered_repos.sort_by(|a, b| a.reponame.cmp(&b.reponame));

    if filtered_repos.is_empty() {
        warn!("No repositories found with an open PR for '{}'", change_id);
        return Ok(());
    }

    log::info!(
        "{} repositories have an open PR for '{}'",
        filtered_repos.len(),
        change_id
    );

    let outputs: Vec<String> = filtered_repos
        .par_iter()
        .map(|repo| repo.review(buffer, approve, merge, admin_override))
        .collect::<Result<Vec<String>, _>>()?;

    for output in outputs {
        println!("{}", output);
    }
    Ok(())
}
