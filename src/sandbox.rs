// src/sandbox.rs

use rayon::prelude::*;
use std::env;
use std::io::{self, Write};
use std::path::Path;

use colored::Colorize;
use eyre::Result;
use log::{debug, error, info, warn};

use crate::git;

/// Refreshes a single repository by pruning remote branches, cleaning local stale branches,
/// resetting, checking out the head branch, pulling the latest changes, and installing pre-commit hooks.
/// Returns a status string.
pub fn refresh_repo(repo: &Path) -> Result<String> {
    let success_emoji = "📥";
    let error_emoji = "❗";
    let missing_emoji = "❓";

    // Prune remote branches.
    debug!("Starting remote prune for repo '{}'", repo.display());
    git::remote_prune(repo)?;
    debug!("Finished remote prune for repo '{}'", repo.display());

    // Remove any local branches starting with "SLAM" that don't have a corresponding remote branch.
    match git::list_local_branches_with_prefix(repo, "SLAM") {
        Ok(local_branches) => {
            debug!("Found {} local SLAM branches in '{}'", local_branches.len(), repo.display());
            for branch in local_branches {
                match git::remote_branch_exists(repo, &branch) {
                    Ok(true) => {
                        debug!("Remote branch '{}' exists in '{}'", branch, repo.display());
                    }
                    Ok(false) => {
                        debug!("Remote branch '{}' does not exist in '{}'; deleting local branch", branch, repo.display());
                        git::safe_delete_local_branch(repo, &branch)?;
                        info!("Deleted local branch '{}' in '{}'", branch, repo.display());
                    }
                    Err(e) => {
                        warn!("Error checking remote branch '{}' in {}: {}", branch, repo.display(), e);
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to list local branches in {}: {}", repo.display(), e);
        }
    }

    // Ensure we have the latest changes on the HEAD branch.
    let branch = git::get_head_branch(repo)?;
    debug!("Determined HEAD branch '{}' for repo '{}'", branch, repo.display());
    let branch_display = branch.magenta();

    git::reset_hard(repo)?;
    debug!("Completed hard reset for repo '{}'", repo.display());

    git::checkout(repo, &branch)?;
    debug!("Checked out branch '{}' in repo '{}'", branch, repo.display());

    git::pull(repo)?;
    debug!("Pulled latest changes for repo '{}'", repo.display());

    // Install pre-commit hooks if a configuration exists.
    let hook_status = if repo.join(".pre-commit-config.yaml").exists() {
        debug!("Found pre-commit config in repo '{}'", repo.display());
        match git::install_pre_commit_hooks(repo) {
            Ok(true) => {
                debug!("Pre-commit hooks installed successfully in repo '{}'", repo.display());
                success_emoji
            }
            Ok(false) | Err(_) => {
                debug!("Pre-commit hooks installation failed or hook file missing in repo '{}'", repo.display());
                error_emoji
            }
        }
    } else {
        debug!("No pre-commit config found in repo '{}'", repo.display());
        missing_emoji
    };

    let reposlug = git::get_repo_slug(repo)?;
    debug!("Returning status for repo '{}'", reposlug);
    Ok(format!("{:>6} {} {}", branch_display, hook_status, reposlug))
}

/// Refreshes all repositories found in the current working directory.
/// Each repository is processed in parallel; status output is printed for each.
pub fn sandbox_refresh() -> Result<()> {
    let cwd = env::current_dir()?;
    debug!("Current working directory: '{}'", cwd.display());
    let repos = git::find_git_repositories(&cwd)?;
    debug!("Found {} repositories in '{}'", repos.len(), cwd.display());

    repos.par_iter().for_each(|repo| {
        debug!("Processing repo '{}'", repo.display());
        match refresh_repo(repo) {
            Ok(line) => {
                println!("{}", line);
                io::stdout().flush().expect("Failed to flush stdout");
            }
            Err(e) => {
                warn!("Error processing repo {}: {}", repo.to_string_lossy().trim_end(), e);
            }
        }
    });
    Ok(())
}

/// Sets up a sandbox environment by retrieving the list of repositories for a given organization,
/// filtering them based on provided patterns, and then cloning or updating each repository.
/// Pre-commit hooks are installed if available.
pub fn sandbox_setup(repo_ptns: Vec<String>) -> Result<()> {
    let org = "tatari-tv";
    debug!("Retrieving repository list for organization '{}'", org);
    let repos = git::find_repos_in_org(org)?;
    info!("Found {} repos in '{}'", repos.len(), org);

    let filtered_repos: Vec<String> = if repo_ptns.is_empty() {
        debug!("No repository patterns provided; using all repos");
        repos.clone()
    } else {
        debug!("Filtering repositories with patterns: {:?}", repo_ptns);
        repos.into_iter().filter(|r| {
            repo_ptns.iter().any(|ptn| r.contains(ptn))
        }).collect()
    };
    info!("After filtering, {} repos remain", filtered_repos.len());

    let cwd = env::current_dir()?;
    debug!("Sandbox setup working directory: '{}'", cwd.display());
    filtered_repos.par_iter().for_each(|reposlug| {
        let target = cwd.join(reposlug);
        if target.exists() {
            info!("Repository {} already exists in {}; updating...", reposlug, target.display());
            if let Err(e) = git::pull(&target) {
                warn!("Failed to pull repository {}: {}", reposlug, e);
            } else {
                debug!("Pulled repository {} successfully", reposlug);
            }
        } else {
            info!("Cloning repository {} into {}", reposlug, target.display());
            if let Err(e) = git::clone_repo(reposlug, &target) {
                warn!("Failed to clone repository {}: {}", reposlug, e);
            } else {
                debug!("Cloned repository {} successfully", reposlug);
            }
        }
        if target.join(".pre-commit-config.yaml").exists() {
            debug!("Found pre-commit config in repository {}", reposlug);
            match git::install_pre_commit_hooks(&target) {
                Ok(true) => info!("Pre-commit hooks installed in repository {}", reposlug),
                Ok(false) => warn!("Pre-commit hooks were not properly installed in repository {}", reposlug),
                Err(e) => error!("Error installing pre-commit hooks in repository {}: {}", reposlug, e),
            }
        } else {
            debug!("No pre-commit config found in repository {}", reposlug);
        }
    });
    Ok(())
}
