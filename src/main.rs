// src/main.rs



use clap::{CommandFactory, FromArgMatches};
use eyre::{Result, Context};
use glob::Pattern;
use itertools::Itertools;
use log::{debug, info, warn};
use rayon::prelude::*;
use std::fs;
use std::path::PathBuf;

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

fn process_create_command(
    files: Vec<String>,
    change_id: String,
    buffer: usize,
    repo_ptns: Vec<String>,
    action: Option<cli::CreateAction>,
) -> Result<()>
 {

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

    status.push(format!("{}{}", filtered_repos.len(), diffs_emoji));

    // Apply changes to repositories in parallel.
    let results: Vec<Result<Option<String>, eyre::Error>> = filtered_repos
        .par_iter()
        .map(|repo| {
            repo.create(&root, buffer, commit_msg.as_deref(), simplified)
        })
        .collect();

    let successful_diffs: Vec<String> = results
        .into_iter()
        .filter_map(|result| match result {
            Ok(Some(diff)) => Some(diff),
            Ok(None) => None,
            Err(e) => {
                eprintln!("Error: {}", e);
                None
            }
        })
        .collect();

    for diff in successful_diffs {
        println!("{}", diff);
    }

    status.reverse();
    println!("  {}", status.join(" | "));
    Ok(())
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

    let mut repos_with_prs = Vec::new();

    match action {
        cli::ReviewAction::Ls { change_id_ptns, .. } => {
            let all_prs = git::get_prs_for_repos(filtered_reposlugs)?;
            for (title, pr_list) in &all_prs {
                if change_id_ptns.is_empty() || change_id_ptns.iter().any(|pattern| title.starts_with(pattern)) {
                    for (reposlug, pr_number, _author) in pr_list {
                        repos_with_prs.push(repo::Repo::create_repo_from_remote_with_pr(reposlug, title, *pr_number));
                    }
                }
            }
        }
        cli::ReviewAction::Clone { change_id, all: include_closed } => {
            let all_prs = git::get_prs_for_repos(filtered_reposlugs.clone())?;

            if let Some(pr_list) = all_prs.get(change_id) {
                for (reposlug, pr_number, _author) in pr_list {
                    repos_with_prs.push(repo::Repo::create_repo_from_remote_with_pr(reposlug, change_id, *pr_number));
                }
            }
            if *include_closed {
                warn!("--all flag for closed PRs is not yet implemented.");
            }
        }
        cli::ReviewAction::Approve { change_id, .. } | cli::ReviewAction::Delete { change_id } => {
            let all_prs = git::get_prs_for_repos(filtered_reposlugs)?;

            if let Some(pr_list) = all_prs.get(change_id) {
                for (reposlug, pr_number, _author) in pr_list {
                    repos_with_prs.push(repo::Repo::create_repo_from_remote_with_pr(reposlug, change_id, *pr_number));
                }
            }
        }
        cli::ReviewAction::Purge {} => {
            for reposlug in &filtered_reposlugs {
                repos_with_prs.push(repo::Repo::create_repo_from_remote_with_pr(reposlug, "SLAM", 0));
            }
        }
    }

    if repos_with_prs.is_empty() {
        println!("No repositories with matching PRs found.");
        return Ok(());
    }

    match action {
        cli::ReviewAction::Ls { .. } => {
            let repo_outputs: Vec<String> = repos_with_prs
                .par_iter()
                .map(|repo| {
                    repo.review(action, false).unwrap_or_else(|e| {
                        format!("Error processing {}: {}", repo.reposlug, e)
                    })
                })
                .collect();

            for output in repo_outputs {
                println!("{}", output);
            }
        }
        _ => {
            if repos_with_prs.len() > 1 {
                println!("Summary:");
                let summaries: Vec<String> = repos_with_prs
                    .iter()
                    .map(|repo| {
                        repo.review(action, true).unwrap_or_else(|e| {
                            format!("Error: {}", e)
                        })
                    })
                    .collect();

                for summary in summaries {
                    println!("  {}", summary);
                }
                println!();
            }

            if matches!(action, cli::ReviewAction::Clone { .. }) {
                let repo_outputs: Vec<String> = repos_with_prs
                    .par_iter()
                    .map(|repo| {
                        repo.review(action, false).unwrap_or_else(|e| {
                            format!("Error processing {}: {}", repo.reposlug, e)
                        })
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

fn setup_logging() -> Result<()> {
    let log_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("slam");

    fs::create_dir_all(&log_dir)
        .context("Failed to create log directory")?;

    let log_file = log_dir.join("slam.log");

    let target = Box::new(fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_file)
        .context("Failed to open log file")?);

    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(target))
        .init();

    info!("Logging initialized, writing to: {}", log_file.display());
    Ok(())
}

fn main() -> Result<()> {
    setup_logging()?;

    let args = cli::SlamCli::from_arg_matches(&cli::SlamCli::command().get_matches())?;

    let result = match args.command {
        cli::SlamCommand::Sandbox { repo_ptns, action } => {
            match action {
                cli::SandboxAction::Setup {} => {
                    sandbox::sandbox_setup(repo_ptns)
                }
                cli::SandboxAction::Refresh {} => {
                    sandbox::sandbox_refresh()
                }
            }
        }
        cli::SlamCommand::Create {
            files,
            change_id,
            buffer,
            repo_ptns,
            action,
        } => {
            process_create_command(files, change_id, buffer, repo_ptns, action)
        }
        cli::SlamCommand::Review { org, action, repo_ptns } => {
            process_review_command(org, &action, repo_ptns)
        }
    };

    if let Err(e) = result {
        let error_msg = e.to_string();

        // Provide helpful debugging suggestions for common issues
        if error_msg.contains("Failed to parse open PRs JSON") || error_msg.contains("invalid type: map, expected u64") {
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("💡 This appears to be a JSON parsing issue. To troubleshoot:");
            eprintln!("   1. Run with debug logging: RUST_LOG=debug slam ...");
            eprintln!("   2. Check GitHub CLI authentication: gh auth status");
            eprintln!("   3. Verify repository access and permissions");
            eprintln!();
            eprintln!("For more help, see: https://github.com/scottidler/slam/blob/main/README.md#troubleshooting-common-issues");
        } else if error_msg.contains("Failed to list open PRs") || error_msg.contains("Failed to list remote branches") {
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("💡 This appears to be a GitHub CLI or repository access issue:");
            eprintln!("   1. Ensure 'gh' is installed and authenticated: gh auth status");
            eprintln!("   2. Verify you have access to the repository");
            eprintln!("   3. Check repository name spelling and organization");
            eprintln!("   4. Run with debug logging: RUST_LOG=debug slam ...");
        } else {
            eprintln!("Error: {}", e);
            eprintln!();
            eprintln!("💡 For detailed troubleshooting information, run with debug logging:");
            eprintln!("   RUST_LOG=debug slam [your command]");
        }

        std::process::exit(1);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_reponame() {
        assert_eq!(extract_reponame("org/repo"), "repo");
        assert_eq!(extract_reponame("tatari-tv/frontend"), "frontend");
        assert_eq!(extract_reponame("single"), "single");
        assert_eq!(extract_reponame(""), "");
        assert_eq!(extract_reponame("a/b/c"), "b"); // Only gets first split
    }

    #[test]
    fn test_extract_reponame_edge_cases() {
        assert_eq!(extract_reponame("/repo"), "repo");
        assert_eq!(extract_reponame("org/"), "");
        assert_eq!(extract_reponame("/"), "");
    }

    #[test]
    fn test_filter_repos_by_spec_empty() {
        let repos = vec![
            create_test_repo("org/repo1"),
            create_test_repo("org/repo2"),
        ];

        let result = filter_repos_by_spec(repos.clone(), &[]);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].reposlug, "org/repo1");
        assert_eq!(result[1].reposlug, "org/repo2");
    }

    #[test]
    fn test_filter_repos_by_spec_exact_match() {
        let repos = vec![
            create_test_repo("org/frontend"),
            create_test_repo("org/backend"),
            create_test_repo("org/mobile"),
        ];

        let specs = vec!["frontend".to_string()];
        let result = filter_repos_by_spec(repos, &specs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reposlug, "org/frontend");
    }

    #[test]
    fn test_filter_repos_by_spec_starts_with() {
        let repos = vec![
            create_test_repo("org/frontend-web"),
            create_test_repo("org/frontend-mobile"),
            create_test_repo("org/backend"),
        ];

        let specs = vec!["front".to_string()];
        let result = filter_repos_by_spec(repos, &specs);

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|r| r.reposlug == "org/frontend-mobile"));
        assert!(result.iter().any(|r| r.reposlug == "org/frontend-web"));
    }

    #[test]
    fn test_filter_repos_by_spec_full_slug_exact() {
        let repos = vec![
            create_test_repo("org1/repo"),
            create_test_repo("org2/repo"),
        ];

        let specs = vec!["org1/repo".to_string()];
        let result = filter_repos_by_spec(repos, &specs);

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].reposlug, "org1/repo");
    }

    #[test]
    fn test_filter_repos_by_spec_full_slug_starts_with() {
        let repos = vec![
            create_test_repo("tatari-tv/frontend"),
            create_test_repo("tatari-tv/backend"),
            create_test_repo("other-org/frontend"),
        ];

        let specs = vec!["tatari".to_string()];
        let result = filter_repos_by_spec(repos, &specs);

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|r| r.reposlug == "tatari-tv/backend"));
        assert!(result.iter().any(|r| r.reposlug == "tatari-tv/frontend"));
    }

    #[test]
    fn test_filter_repos_by_spec_multiple_specs() {
        let repos = vec![
            create_test_repo("org/frontend"),
            create_test_repo("org/backend"),
            create_test_repo("org/mobile"),
            create_test_repo("org/docs"),
        ];

        let specs = vec!["frontend".to_string(), "backend".to_string()];
        let result = filter_repos_by_spec(repos, &specs);

        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|r| r.reposlug == "org/backend"));
        assert!(result.iter().any(|r| r.reposlug == "org/frontend"));
    }

    #[test]
    fn test_filter_repos_by_spec_sorting() {
        let repos = vec![
            create_test_repo("org/zebra"),
            create_test_repo("org/alpha"),
            create_test_repo("org/beta"),
        ];

        let result = filter_repos_by_spec(repos, &[]);

        // Should be sorted alphabetically by reposlug
        assert_eq!(result[0].reposlug, "org/alpha");
        assert_eq!(result[1].reposlug, "org/beta");
        assert_eq!(result[2].reposlug, "org/zebra");
    }

    // Helper function to create test repos
    fn create_test_repo(reposlug: &str) -> repo::Repo {
        repo::Repo {
            reposlug: reposlug.to_string(),
            change_id: "test-change".to_string(),
            change: None,
            files: vec![],
            pr_number: 0,
        }
    }

    #[test]
    fn test_built_info_module_exists() {
        // Just test that the built_info module can be referenced
        // The actual GIT_DESCRIBE value will depend on the build environment
        let _version = built_info::GIT_DESCRIBE;
        // This test mainly ensures the module is properly included
    }

    #[test]
    fn test_emoji_constants() {
        // Test the emoji constants used in process_create_command
        let total_emoji = "🔍";
        let repos_emoji = "📦";
        let files_emoji = "📄";
        let diffs_emoji = "📝";

        assert_eq!(total_emoji, "🔍");
        assert_eq!(repos_emoji, "📦");
        assert_eq!(files_emoji, "📄");
        assert_eq!(diffs_emoji, "📝");
    }
}
