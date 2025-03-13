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
use glob::Pattern;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

mod cli;
mod git;
mod diff;
mod repo;
mod utils;

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

    let env = env_logger::Env::default().filter_or("RUST_LOG", "info");
    env_logger::Builder::from_env(env)
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
            process_review_command(org, &action, repos)?;
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

    // Gather diff outputs from each repo.
    let outputs: Vec<String> = filtered_repos
        .par_iter()
        .map(|repo| repo.create(&root, buffer, commit.as_deref()))
        .collect::<eyre::Result<Vec<String>>>()?;

    println!("{}", change_id);
    for output in outputs {
        println!("{}\n", utils::indent(&output, 2));
    }
    Ok(())
}

fn filter_repos(all_reposlugs: Vec<String>, reposlug_ptns: Vec<String>) -> Vec<String> {
    if reposlug_ptns.is_empty() || reposlug_ptns.iter().all(|s| s.trim().is_empty()) {
        return all_reposlugs;
    }
    all_reposlugs
        .into_iter()
        .filter(|repo| {
            reposlug_ptns.iter().any(|ptn| {
                if let Ok(pattern) = Pattern::new(ptn) {
                    pattern.matches(repo)
                } else {
                    false
                }
            })
        })
        .collect()
}

fn process_review_command(
    org: String,
    action: &cli::Action,
    reposlug_ptns: Vec<String>,
) -> eyre::Result<()> {
    // 1. Get all repos in the organization.
    let all_reposlugs = git::find_repos_in_org(&org)?;
    info!("Found {} repos in '{}'", all_reposlugs.len(), org);
    debug!("All repos:\n{}", all_reposlugs.iter().join("\n"));

    // 2. Filter repository slugs using glob-style matching.
    let filtered_reposlugs = filter_repos(all_reposlugs, reposlug_ptns);
    info!(
        "After user input filter, {} repos remain",
        filtered_reposlugs.len()
    );
    debug!(
        "Filtered repos:\n{}",
        filtered_reposlugs.iter().join("\n")
    );

    // 3. Get the PR map (with the updated structure including author).
    let pr_map = git::get_prs_for_repos(filtered_reposlugs)?;
    debug!(
        "PR map:\n{}",
        pr_map
            .iter()
            .map(|(pr_name, vec)| {
                let details = vec
                    .iter()
                    .map(|(repo, pr, author)| format!("{}:{} ({})", repo, pr, author))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} -> [{}]", pr_name, details)
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    // 4. Define a filtering function based on the CLI action.
    let change_id_filter: Box<dyn Fn(&String) -> bool> = match action {
        cli::Action::Ls { change_id_ptns, .. } => {
            Box::new(move |key: &String| {
                change_id_ptns.iter().any(|ptn| {
                    Pattern::new(ptn).map_or(false, |glob_pat| glob_pat.matches(key))
                })
            })
        }
        cli::Action::Approve { change_id, .. } | cli::Action::Delete { change_id, .. } => {
            Box::new(move |key: &String| key == change_id)
        }
    };

    // 5. Filter the PR map based on the function.
    let filtered_pr_map: std::collections::HashMap<String, Vec<(String, u64, String)>> = pr_map
        .into_iter()
        .filter(|(key, _)| change_id_filter(key))
        .collect();

    debug!("Filtered PR map has {} keys", filtered_pr_map.len());
    debug!(
        "Filtered PR map:\n{}",
        filtered_pr_map
            .iter()
            .map(|(pr_name, vec)| {
                let details = vec
                    .iter()
                    .map(|(repo, pr, author)| format!("{}:{} ({})", repo, pr, author))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("{} -> [{}]", pr_name, details)
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    if filtered_pr_map.is_empty() {
        warn!("No open PRs found matching change_id(s).");
        return Ok(());
    }

    let summary = matches!(action, cli::Action::Ls { .. }) && filtered_pr_map.len() > 1;

    // 6. Convert the map into a sorted vector of (change_id, Vec<(reposlug, pr_number, author)>).
    let mut groups: Vec<(String, Vec<(String, u64, String)>)> = filtered_pr_map.into_iter().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    // 7. For each change_id group, process the repos concurrently using Rayon.
    let output_groups: Vec<(String, String, Vec<String>)> = groups
        .into_iter()
        .map(|(change_id, repo_infos)| {
            // Extract the author in its own block so that the borrow ends here.
            let author = {
                if let Some((_, _, author)) = repo_infos.first() {
                    author.clone()
                } else {
                    "unknown".to_string()
                }
            };
            let repo_outputs: Vec<String> = repo_infos
                .into_par_iter()
                .map(|(reposlug, pr_number, _)| {
                    let repo = Repo::create_repo_from_remote_with_pr(&reposlug, &change_id, pr_number);
                    repo.review(action, summary)
                })
                .collect::<eyre::Result<Vec<String>>>()?;
            Ok((change_id, author, repo_outputs))
        })
        .collect::<eyre::Result<Vec<(String, String, Vec<String>)>>>()?;

    // 8. Print the final hierarchical output.
    for (change_id, author, repo_outputs) in output_groups {
        println!("{} ({})", change_id, author);
        let joined = repo_outputs
            .into_iter()
            .map(|output| utils::indent(&output, 2))
            .collect::<Vec<_>>()
            .join("\n");
        println!("{}\n", joined);
    }
    Ok(())
}
