// src/main.rs

use std::{
    env,
    fs,
    io,
    io::Write,
};

use clap::{CommandFactory, FromArgMatches};
use eyre::Result;
use glob::Pattern;
use itertools::Itertools;
use log::{debug, error, info, warn};
use rayon::prelude::*;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

mod cli;
mod diff;
mod git;
mod repo;
mod utils;
mod sandbox;
mod transaction;

/// Extracts the repository name (the part after '/') from a reposlug.
/// If the reposlug is not in the expected format, returns the full string.
fn extract_reponame(reposlug: &str) -> &str {
    reposlug.split('/').nth(1).unwrap_or(reposlug)
}

/// Filters the given vector of repositories according to a list of filtering specifications.
/// The filter criteria are applied in the following order:
/// 1. Exact match on the repository name (the part after '/')
/// 2. Starts-with match on the repository name
/// 3. Exact match on the full reposlug ("org/reponame")
/// 4. Starts-with match on the full reposlug
///
/// At the first level where one or more repositories match, those matches are used.
/// Finally, the resulting list is sorted by reposlug using itertools.
fn filter_repos_by_spec(repos: Vec<repo::Repo>, specs: &[String]) -> Vec<repo::Repo> {
    let filtered: Vec<repo::Repo> = if specs.is_empty() {
        repos
    } else {
        // Level 1: Exact match on repository name.
        let level1: Vec<repo::Repo> = repos
            .iter()
            .filter(|r| specs.iter().any(|spec| extract_reponame(&r.reposlug) == spec))
            .cloned()
            .collect();
        if !level1.is_empty() {
            level1
        } else {
            // Level 2: Starts-with match on repository name.
            let level2: Vec<repo::Repo> = repos
                .iter()
                .filter(|r| specs.iter().any(|spec| extract_reponame(&r.reposlug).starts_with(spec)))
                .cloned()
                .collect();
            if !level2.is_empty() {
                level2
            } else {
                // Level 3: Exact match on full reposlug.
                let level3: Vec<repo::Repo> = repos
                    .iter()
                    .filter(|r| specs.iter().any(|spec| r.reposlug == *spec))
                    .cloned()
                    .collect();
                if !level3.is_empty() {
                    level3
                } else {
                    // Level 4: Starts-with match on full reposlug.
                    repos
                        .iter()
                        .filter(|r| specs.iter().any(|spec| r.reposlug.starts_with(spec)))
                        .cloned()
                        .collect()
                }
            }
        }
    };

    filtered
        .into_iter()
        .sorted_by(|a, b| a.reposlug.cmp(&b.reposlug))
        .collect()
}

fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    let log_dir = utils::get_or_create_log_dir();
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

    let env_logger_env = env_logger::Env::default().filter_or("RUST_LOG", "info");
    env_logger::Builder::from_env(env_logger_env)
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

    let mut cmd = cli::SlamCli::command();
    cmd = cmd.after_help(cli::get_cli_tool_status());
    let cli = cli::SlamCli::from_arg_matches(&cmd.get_matches())?;
    info!("{}", "-".repeat(100));
    info!("Starting SLAM");

    match cli.command {
        cli::SlamCommand::Create { files, change_id, buffer, repo_ptns, action } => {
            process_create_command(files, action, change_id, buffer, repo_ptns)?;
        }
        cli::SlamCommand::Review { org, repo_ptns, action } => {
            process_review_command(org, &action, repo_ptns)?;
        }
        cli::SlamCommand::Sandbox { repo_ptns, action } => {
            process_sandbox_command(repo_ptns, action)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

fn process_sandbox_command(repo_ptns: Vec<String>, action: cli::SandboxAction) -> Result<()> {
    match action {
        cli::SandboxAction::Setup {} => sandbox::sandbox_setup(repo_ptns),
        cli::SandboxAction::Refresh {} => sandbox::sandbox_refresh(),
    }
}

fn process_create_command(
    files: Vec<String>,
    action: Option<cli::CreateAction>,
    change_id: String,
    buffer: usize,
    repo_ptns: Vec<String>,
) -> Result<()> {

    let total_emoji = "🔍";
    let repos_emoji = "📦";
    let files_emoji = "📄";
    let diffs_emoji = "📝";

    let (change, commit_msg, simplified) = match action {
        Some(action) => {
            let (change, commit_msg, simplified) = action.decompose();
            (Some(change), commit_msg, simplified)
        }
        None => (None, None, false),
    };

    let root = std::env::current_dir()?;
    let discovered_paths = git::find_git_repositories(&root)?;
    let mut discovered_repos = Vec::new();

    for path in discovered_paths {
        if let Some(repo) = repo::Repo::create_repo_from_local(&path, &root, &change, &files, &change_id) {
            discovered_repos.push(repo);
        }
    }

    let mut status = Vec::new();
    status.push(format!("{}{}", discovered_repos.len(), total_emoji));

    // Use the new filtering function instead of the inline lambda.
    let mut filtered_repos = filter_repos_by_spec(discovered_repos, &repo_ptns);

    if !repo_ptns.is_empty() {
        status.push(format!("{}{}", filtered_repos.len(), repos_emoji));
    }
    if !files.is_empty() {
        filtered_repos.retain(|repo| !repo.files.is_empty());
        status.push(format!("{}{}", filtered_repos.len(), files_emoji));
    }
    // Dry-run: if no change is specified, list matched repositories and exit.
    if change.is_none() {
        if filtered_repos.is_empty() {
            println!("No repositories matched your criteria.");
        } else {
            println!("Matched repositories:");
            for repo in &filtered_repos {
                println!("  {}", repo.reposlug);
                if !files.is_empty() {
                    for file in &repo.files {
                        println!("    {}", file);
                    }
                }
            }
            status.reverse();
            println!("\n  {}", status.join(" | "));
        }
        return Ok(());
    }

    let outputs: Vec<Option<String>> = filtered_repos
        .into_par_iter()
        .map(|repo| repo.create(&root, buffer, commit_msg.as_deref(), simplified))
        .collect::<Result<Vec<Option<String>>>>()?;
    let matches: Vec<String> = outputs.into_iter().filter_map(|s| s).collect();

    if !matches.is_empty() {
        println!("{}", change_id);
        for output in &matches {
            println!("{}\n", utils::indent(&output, 2));
        }
        status.push(format!("{}{}", matches.len(), diffs_emoji));
    }
    status.reverse();
    println!("  {}", status.join(" | "));

    Ok(())
}

fn _load_service_account_pat() -> std::io::Result<String> {
    let home = env::var("HOME").expect("HOME environment variable not set");
    let token_path = format!("{}/.config/github/tokens/service_account_pat", home);
    fs::read_to_string(token_path).map(|s| s.trim().to_string())
}

fn process_review_command(
    org: String,
    action: &cli::ReviewAction,
    reposlug_ptns: Vec<String>,
) -> Result<()> {
    let all_reposlugs = git::find_repos_in_org(&org)?;
    info!("Found {} repos in '{}'", all_reposlugs.len(), org);

    let filtered_reposlugs: Vec<String> = if reposlug_ptns.iter().all(|s| s.trim().is_empty()) {
        all_reposlugs.clone()
    } else {
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
    };
    info!("After filtering, {} repos remain", filtered_reposlugs.len());
    debug!("Filtered repository slugs: {:?}", filtered_reposlugs);

    match action {
        cli::ReviewAction::Purge {} => {
            // For the purge action, process each filtered repo individually.
            for reposlug in filtered_reposlugs {
                let repo = crate::repo::Repo::create_repo_from_remote_with_pr(&reposlug, "", 0);
                match repo.review(action, false) {
                    Ok(msg) => println!("{}: {}", reposlug, msg),
                    Err(e) => eprintln!("Error purging repo {}: {}", reposlug, e),
                }
            }
            return Ok(());
        }
        _ => {
            // Existing behavior: get a map of change IDs to PR details.
            let pr_map = git::get_prs_for_repos(filtered_reposlugs)?;
            let mut filtered_pr_map: Vec<(String, Vec<(String, u64, String)>)> = pr_map
                .into_iter()
                .filter(|(key, _)| {
                    // Filter by change ID only if action is not Purge.
                    match action {
                        cli::ReviewAction::Ls { change_id_ptns, .. } => {
                            key.starts_with("SLAM")
                                && (change_id_ptns.is_empty()
                                    || change_id_ptns.iter().any(|ptn| {
                                        if let Ok(pattern) = glob::Pattern::new(ptn) {
                                            pattern.matches(key)
                                        } else {
                                            false
                                        }
                                    }))
                        }
                        cli::ReviewAction::Approve { change_id, .. }
                        | cli::ReviewAction::Delete { change_id }
                        | cli::ReviewAction::Clone { change_id, .. } => key.starts_with("SLAM") && key == change_id,
                        _ => false,
                    }
                })
                .collect();
            if filtered_pr_map.is_empty() {
                warn!("No open PRs found matching Change ID");
                return Ok(());
            }
            filtered_pr_map.sort_by(|a, b| a.0.cmp(&b.0));
            debug!("Filtered PR groups: {:?}", filtered_pr_map);

            for (change_id, repo_infos) in filtered_pr_map {
                let author = repo_infos
                    .first()
                    .map(|(_, _, a)| a.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                println!("{} ({})", change_id, author);

                let repo_outputs: Vec<String> = repo_infos
                    .into_iter()
                    .map(|(reposlug, pr_number, _)| {
                        let repo = crate::repo::Repo::create_repo_from_remote_with_pr(&reposlug, &change_id, pr_number);
                        match repo.review(action, matches!(action, cli::ReviewAction::Ls { change_id_ptns, .. } if change_id_ptns.is_empty())) {
                            Ok(msg) => msg,
                            Err(e) => {
                                error!("Review failed for {}: {}", reposlug, e);
                                format!("Repo {} failed: {}", reposlug, e)
                            }
                        }
                    })
                    .collect();

                for output in repo_outputs {
                    println!("{}", output);
                }
                println!();
            }
        }
    }
    Ok(())
}
