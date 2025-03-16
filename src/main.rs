use std::{
    env,
    fs,
    io,
    io::Write,
    collections::HashMap,
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
mod transaction;

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
    info!("Starting SLAM");

    match cli.command {
        cli::SlamCommand::Create { files, change_id, buffer, repo_ptns, action } => {
            process_create_command(files, action, change_id, buffer, repo_ptns)?;
        }
        cli::SlamCommand::Review { org, repo_ptns, action } => {
            process_review_command(org, &action, repo_ptns)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

fn process_create_command(
    files: Vec<String>,
    action: Option<cli::CreateAction>,
    change_id: String,
    buffer: usize,
    repo_ptns: Vec<String>,
) -> Result<()> {
    env::remove_var("GITHUB_TOKEN");

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

    let mut filtered_repos: Vec<_> = discovered_repos
        .into_iter()
        .filter(|repo| {
            repo_ptns.is_empty() || repo_ptns.iter().any(|spec| repo.reponame.contains(spec))
        })
        .sorted_by(|a, b| a.reponame.cmp(&b.reponame))
        .collect();

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
                println!("  {}", repo.reponame);
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
        .collect::<eyre::Result<Vec<Option<String>>>>()?;
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

fn load_service_account_pat() -> std::io::Result<String> {
    let home = env::var("HOME").expect("HOME environment variable not set");
    let token_path = format!("{}/.config/github/tokens/service_account_pat", home);
    fs::read_to_string(token_path).map(|s| s.trim().to_string())
}

pub fn process_review_command(
    org: String,
    action: &cli::ReviewAction,
    reposlug_ptns: Vec<String>,
) -> Result<()> {

    let service_account_pat = load_service_account_pat()?;
    std::env::set_var("GITHUB_TOKEN", service_account_pat);

    // 1. Get all repos in the organization.
    let all_reposlugs = git::find_repos_in_org(&org)?;
    info!("Found {} repos in '{}'", all_reposlugs.len(), org);
    debug!("All repos:\n{}", all_reposlugs.iter().join("\n"));

    // 2. Filter repository slugs using glob-style matching.
    let filtered_reposlugs = if reposlug_ptns.is_empty() || reposlug_ptns.iter().all(|s| s.trim().is_empty()) {
        all_reposlugs
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
            .collect::<Vec<_>>()
    };
    info!(
        "After user input filter, {} repos remain",
        filtered_reposlugs.len()
    );
    debug!("Filtered repos:\n{}", filtered_reposlugs.iter().join("\n"));

    // 3. Get the PR map (titles -> Vec of (reposlug, pr_number, author)).
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

    // 4. Build a filter to ensure we only handle PRs starting with "SLAM" and matching user input.
    let change_id_filter: Box<dyn Fn(&String) -> bool> = match action {
        cli::ReviewAction::Ls { change_id_ptns, .. } => {
            Box::new(move |key: &String| {
                if !key.starts_with("SLAM") {
                    return false;
                }
                if change_id_ptns.is_empty() {
                    true
                } else {
                    change_id_ptns.iter().any(|ptn| {
                        if let Ok(glob_pat) = Pattern::new(ptn) {
                            glob_pat.matches(key)
                        } else {
                            false
                        }
                    })
                }
            })
        }
        cli::ReviewAction::Approve { change_id, .. }
        | cli::ReviewAction::Delete { change_id, .. } => {
            let change_id_owned = change_id.to_owned();
            Box::new(move |key: &String| key.starts_with("SLAM") && key == &change_id_owned)
        },
    };

    // 5. Filter the PR map to only keep entries that match our filter.
    let filtered_pr_map: HashMap<String, Vec<(String, u64, String)>> =
        pr_map
            .into_iter()
            .filter(|(key, _)| change_id_filter(&key.to_string()))
            .map(|(key, vec)| {
                let key_owned = key.to_string();
                let vec_owned = vec
                    .into_iter()
                    .map(|(repo, pr, author)| (repo.to_string(), pr, author.to_string()))
                    .collect();
                (key_owned, vec_owned)
            })
            .collect();

    if filtered_pr_map.is_empty() {
        warn!("No open PRs found matching change_id(s) starting with 'SLAM'.");
        return Ok(());
    }

    // 6. Determine if we are in "summary mode" (for listing only).
    let summary = match action {
        cli::ReviewAction::Ls { change_id_ptns, .. } => change_id_ptns.is_empty(),
        _ => false,
    };

    // 7. Sort the groups by change_id for a stable output.
    let mut groups: Vec<(String, Vec<(String, u64, String)>)> = filtered_pr_map.into_iter().collect();
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    // 8. Process each change_id group.
    for (change_id, repo_infos) in groups {
        let author = repo_infos
            .first()
            .map(|(_, _, author)| author.clone())
            .unwrap_or_else(|| "unknown".to_string());
        println!("{} ({})", change_id, author);

        // Process each repo in parallel; catch and log errors individually.
        let repo_outputs: Vec<String> = repo_infos
            .into_par_iter()
            .map(|(reposlug, pr_number, _)| -> String {
                let repo = repo::Repo::create_repo_from_remote_with_pr(&reposlug, &change_id, pr_number);
                match repo.review(action, summary) {
                    Ok(msg) => utils::indent(&msg, 2),
                    Err(e) => {
                        error!("Review failed for repo {}: {}", reposlug, e);
                        utils::indent(&format!("Repo {} failed: {}", reposlug, e), 2)
                    }
                }
            })
            .collect();

        for output in repo_outputs {
            println!("{}", output);
        }
        println!();
    }

    Ok(())
}
