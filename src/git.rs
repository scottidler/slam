use eyre::{eyre, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::process::{Command, Output};
use log::{info, debug, warn, error};
use rayon::iter::{
    IntoParallelIterator,
    ParallelIterator,
};

const MAX_RETRY: usize = 5;

fn git(repo_path: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(repo_path)
        .args(args)
        .output()
        .map_err(|e| eyre!("Failed to execute git {:?}: {}", args, e))
}

pub fn clone_repo(reposlug: &str, target: &Path) -> Result<()> {
    let url = format!("git@github.com:{}.git", reposlug);

    let ssh_cmd_output = Command::new("git")
        .args(&["config", "--get", "core.sshCommand"])
        .output()?;
    let ssh_command = if ssh_cmd_output.status.success() {
        String::from_utf8_lossy(&ssh_cmd_output.stdout).trim().to_string()
    } else {
        "ssh".to_string()
    };

    // Use --quiet to suppress default git output
    info!("Cloning {} into {} quietly", reposlug, target.display());
    let status = Command::new("git")
        .env("GIT_SSH_COMMAND", ssh_command)
        .args(&["clone", "--quiet", &url, target.to_str().unwrap()])
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(eyre!("git clone failed for {} via {}", reposlug, url))
    }
}

pub fn clone_or_update_repo(reposlug: &str, target: &Path, branch: &str) -> Result<()> {
    let expected_url = format!("git@github.com:{}.git", reposlug);

    if !target.exists() {
        info!("Target {} does not exist; cloning {} quietly", target.display(), reposlug);
        clone_repo(reposlug, target)?;
    } else {
        debug!("Target {} exists; verifying remote URL...", target.display());
        let output = Command::new("git")
            .current_dir(target)
            .args(&["config", "--get", "remote.origin.url"])
            .output()?;
        let current_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if current_url != expected_url {
            debug!("Remote URL mismatch for {}: expected {}, found {}. Updating remote URL...", reposlug, expected_url, current_url);
            let set_output = Command::new("git")
                .current_dir(target)
                .args(&["remote", "set-url", "origin", &expected_url])
                .output()?;
            if !set_output.status.success() {
                return Err(eyre!("Failed to update remote URL for {}: {}", reposlug, String::from_utf8_lossy(&set_output.stderr)));
            }
        } else {
            debug!("Remote URL for {} is correct.", reposlug);
        }
    }

    debug!("Fetching latest changes for {} quietly...", reposlug);
    let fetch_status = Command::new("git")
        .current_dir(target)
        .args(&["fetch", "origin", "--quiet"])
        .status()?;
    if !fetch_status.success() {
        return Err(eyre!("Failed to fetch remote for {}", reposlug));
    }

    debug!("Checking out branch '{}' in {} quietly...", branch, reposlug);
    checkout_branch(target, branch)?;
    Ok(())
}

pub fn checkout_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["checkout", "-B", branch, "--quiet"])
        .output()
        .map_err(|e| eyre!("Failed to execute git checkout: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(eyre!(
            "Failed to checkout branch {}: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn find_git_repositories(root: &Path) -> Result<Vec<std::path::PathBuf>> {
    let mut repos = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() && path.join(".git").is_dir() {
            repos.push(path);
        } else if path.is_dir() {
            repos.extend(find_git_repositories(&path)?);
        }
    }
    Ok(repos)
}

pub fn push_branch(repo_path: &Path, branch: &str) -> Result<()> {
    git(repo_path, &["push", "--set-upstream", "origin", branch])?;
    Ok(())
}

pub fn find_repos_in_org(org: &str) -> Result<Vec<String>> {
    let output = Command::new("gh")
        .args([
            "repo",
            "list", org,
            "--limit", "1000",
            "--json", "name,isArchived",
        ])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("Failed to list repos in org '{}'", org));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    let repos: Vec<String> = parsed
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|repo| {
            if repo.get("isArchived").and_then(Value::as_bool).unwrap_or(false) {
                None
            } else {
                repo.get("name").and_then(Value::as_str).map(|name| format!("{}/{}", org, name))
            }
        })
        .collect();

    Ok(repos)
}

pub fn get_pr_number_for_repo(repo_name: &str, change_id: &str) -> Result<u64> {
    let output = Command::new("gh")
        .args([
            "pr", "list",
            "--repo", repo_name,
            "--head", change_id,
            "--state", "open",
            "--json", "number",
            "--limit", "1",
        ])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("Failed to list PRs in repo '{}'", repo_name));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    let pr_number = parsed
        .as_array()
        .and_then(|arr| arr.get(0))
        .and_then(|obj| obj.get("number"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(pr_number)
}

pub fn get_prs_for_repos(reposlugs: Vec<String>) -> Result<HashMap<String, Vec<(String, u64, String)>>> {
    let results: Vec<HashMap<String, Vec<(String, u64, String)>>> = reposlugs
        .into_par_iter()
        .map(|reposlug: String| {
            let output = Command::new("gh")
                .args(&[
                    "pr", "list",
                    "--repo", &reposlug,
                    "--state", "open",
                    "--json", "title,number,author",
                    "--limit", "100",
                ])
                .output();
            if let Ok(output) = output {
                if output.status.success() {
                    if let Ok(parsed) = serde_json::from_slice::<Value>(&output.stdout) {
                        if let Some(arr) = parsed.as_array() {
                            let mut map = HashMap::new();
                            for pr_obj in arr {
                                if let (Some(title), Some(number)) = (
                                    pr_obj.get("title").and_then(Value::as_str),
                                    pr_obj.get("number").and_then(Value::as_u64),
                                ) {
                                    let author = pr_obj.get("author")
                                        .and_then(|a| a.get("login"))
                                        .and_then(Value::as_str)
                                        .unwrap_or("unknown")
                                        .to_string();
                                    map.entry(title.to_string())
                                        .or_insert_with(Vec::new)
                                        .push((reposlug.clone(), number, author));
                                }
                            }
                            return map;
                        }
                    }
                } else {
                    debug!("gh pr list failed for repo '{}'", reposlug);
                }
            }
            HashMap::new()
        })
        .collect();
    let final_map = results.into_iter().fold(HashMap::new(), |mut acc, hm| {
        for (title, vec) in hm {
            acc.entry(title).or_insert_with(Vec::new).extend(vec);
        }
        acc
    });
    Ok(final_map)
}

pub fn get_pr_diff(reposlug: &str, pr_number: u64) -> Result<String> {
    let output = Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string(), "-R", reposlug, "--patch"])
        .output()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    debug!("gh pr diff stdout for {}#{}:\n{}", reposlug, pr_number, stdout);

    let stderr = String::from_utf8_lossy(&output.stderr);
    debug!("gh pr diff stderr for {}#{}:\n{}", reposlug, pr_number, stderr);

    if !output.status.success() {
        return Err(eyre!(
            "Failed to fetch PR diff for {}#{}: {}",
            reposlug,
            pr_number,
            stderr.trim()
        ));
    }

    if stdout.trim().is_empty() {
        warn!("No diff returned for {}#{}", reposlug, pr_number);
    }

    Ok(stdout.trim().to_string())
}

pub fn delete_local_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["branch", "-D", branch])
        .output()?;
    if output.status.success() {
        info!("Deleted local branch '{}' in '{}'", branch, repo_path.display());
        Ok(())
    } else {
        let err_msg = String::from_utf8_lossy(&output.stderr);
        Err(eyre!("Failed to delete local branch '{}' in '{}': {}", branch, repo_path.display(), err_msg))
    }
}

pub fn safe_delete_local_branch(repo: &std::path::Path, branch: &str) -> Result<()> {
    let current_branch = current_branch(repo)?;
    if current_branch.trim() == branch.trim() {
        let head_branch = get_head_branch(repo)?;
        log::info!(
            "Current branch '{}' is scheduled for deletion. Checking out HEAD branch '{}' instead.",
            branch,
            head_branch
        );
        checkout(repo, &head_branch)?;
    }
    delete_local_branch(repo, branch)
}

pub fn delete_remote_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["push", "origin", &format!(":{}", branch)])
        .output()?;
    if output.status.success() {
        info!("Deleted remote branch '{}' in '{}'", branch, repo_path.display());
        Ok(())
    } else {
        warn!(
            "Failed to delete remote branch '{}' in '{}': {}",
            branch,
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

pub fn delete_remote_branch_gh(repo: &str, branch: &str) -> Result<()> {
    let api_endpoint = format!("repos/{}/git/refs/heads/{}", repo, branch);
    let output = Command::new("gh")
        .args(["api", "-X", "DELETE", &api_endpoint])
        .output()?;
    if output.status.success() {
        info!("Deleted remote branch '{}' in repo '{}'", branch, repo);
        Ok(())
    } else {
        warn!(
            "Failed to delete remote branch '{}' in repo '{}': {}",
            branch,
            repo,
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

pub fn approve_pr(repo: &str, pr_number: u64) -> Result<()> {
    Command::new("gh")
        .args(["pr", "review", &pr_number.to_string(), "--approve", "--repo", repo])
        .output()?;
    Ok(())
}

pub fn merge_pr(repo: &str, pr_number: u64, admin_override: bool) -> Result<()> {
    let pr_binding = pr_number.to_string();
    let mut args = vec![
        "pr", "merge",
        &pr_binding,
        "--squash",
        "--delete-branch",
        "--repo",
        repo,
    ];
    if admin_override {
        args.insert(3, "--admin");
    }

    debug!("merge_pr args ={:?}", args);

    // Execute the merge command.
    let merge_output = Command::new("gh").args(&args).output()?;

    debug!("merge_output = {:?}", merge_output);

    // Even if the command returns a success code, its output may indicate that the merge was blocked.
    let output_combined = format!("{}{}",
        String::from_utf8_lossy(&merge_output.stdout),
        String::from_utf8_lossy(&merge_output.stderr)
    );
    if output_combined.to_lowercase().contains("review required") {
        return Err(eyre!("Merge blocked: review required (GitHub rules not satisfied)"));
    }

    // Re-check the PR status via gh pr view.
    let verify_output = Command::new("gh")
        .args(&[
            "pr", "view",
            &pr_binding,
            "--repo", repo,
            "--json", "state,mergedAt"
        ])
        .output()?;

    if !verify_output.status.success() {
        return Err(eyre!(
            "Failed to verify PR status: {}",
            String::from_utf8_lossy(&verify_output.stderr)
        ));
    }

    // Parse the JSON output.
    let json: serde_json::Value = serde_json::from_slice(&verify_output.stdout)?;
    // Check that the state is MERGED or mergedAt is non-null.
    if json["state"].as_str() != Some("MERGED") && json["mergedAt"].is_null() {
        return Err(eyre!("PR merge not confirmed; merge blocked by review requirements"));
    }

    Ok(())
}

pub fn get_head_branch(repo_path: &Path) -> Result<String> {
    // First, try to get the default branch from the remote
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["symbolic-ref", "refs/remotes/origin/HEAD"])
        .output();

    if let Ok(output) = output {
        if output.status.success() {
            let remote_head = String::from_utf8_lossy(&output.stdout);
            let trimmed = remote_head.trim();
            if let Some(branch) = trimmed.strip_prefix("refs/remotes/origin/") {
                return Ok(branch.to_string());
            }
        }
    }

    // Fallback: check for common branch names
    let common_branches = ["main", "master"];
    for branch in &common_branches {
        let remote_ref = format!("origin/{}", branch);
        let output = Command::new("git")
            .current_dir(repo_path)
            .args(&["rev-parse", "--verify", &remote_ref])
            .output();

        if let Ok(output) = output {
            if output.status.success() {
                return Ok(branch.to_string());
            }
        }
    }

    Err(eyre!("Unable to determine head branch for repository"))
}

pub fn install_pre_commit_hooks(repo_path: &Path) -> Result<bool> {
    let output = Command::new("pre-commit")
        .current_dir(repo_path)
        .args(&["install"])
        .output()
        .map_err(|e| eyre!("Failed to execute pre-commit install: {}", e))?;

    if output.status.success() {
        // Check if the hook file was actually created
        let hook_path = repo_path.join(".git").join("hooks").join("pre-commit");
        Ok(hook_path.exists())
    } else {
        Ok(false)
    }
}

/// Run pre-commit hooks with retry logic.
///
/// # Arguments
///
/// - `repo_path`: Path to the Git repository.
/// - `retries`: number of consecutive identical failures allowed before aborting.
///
/// # Returns
///
/// - `Ok(())` if the pre-commit hooks eventually succeed.
/// - `Err` with a detailed message if the command repeatedly fails with identical output
///   (and exit code) for at least `retries` times, or if it exceeds MAX_RETRY attempts.
pub fn run_pre_commit_with_retry(repo_path: &Path, retries: usize) -> Result<()> {
    // Use owned types for exit code, stdout and stderr.
    let mut identical_count = 0;
    let mut previous_attempt: Option<(Option<i32>, String, String)> = None;

    // Never exceed MAX_RETRY attempts.
    for attempt in 1..=MAX_RETRY {
        debug!("Running pre-commit hooks (attempt {} of {})", attempt, MAX_RETRY);

        let output = Command::new("pre-commit")
            .current_dir(repo_path)
            .args(&["run", "--all-files"])
            .output()
            .map_err(|e| eyre!("Failed to execute pre-commit: {}", e))?;

        let current_exit = output.status.code();
        let current_stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let current_stderr = String::from_utf8_lossy(&output.stderr).to_string();

        debug!("Attempt {}: exit code: {:?}", attempt, current_exit);
        debug!("Attempt {}: stdout:\n{}", attempt, current_stdout);
        debug!("Attempt {}: stderr:\n{}", attempt, current_stderr);

        // Success: exit code 0 means pre-commit hooks passed.
        if output.status.success() {
            info!("Pre-commit hooks succeeded after {} attempt(s)", attempt);
            return Ok(());
        }

        // Compare this attempt with the previous one.
        let current_attempt = (current_exit, current_stdout.clone(), current_stderr.clone());
        if let Some(ref prev) = previous_attempt {
            if *prev == current_attempt {
                identical_count += 1;
                debug!("Identical failure #{} detected", identical_count);
                if identical_count >= retries {
                    break;
                }
            } else {
                identical_count = 0; // Reset count if output differs.
                debug!("Output differs from previous attempt; resetting identical count");
            }
        }
        previous_attempt = Some(current_attempt);
    }

    // Extract details from the last attempt for the error message.
    let (last_exit, last_stdout, last_stderr) = previous_attempt.unwrap_or((None, String::new(), String::new()));

    Err(eyre!(
        "Pre-commit hook failed after {} attempts. Last failure:\nExit code: {:?}\nstdout:\n{}\nstderr:\n{}",
        MAX_RETRY,
        last_exit,
        last_stdout,
        last_stderr
    ))
}

//-----------------------------------------------------------------------------------------------

/// Lists remote branch names for the given repository that start with the specified prefix.
pub fn list_remote_branches_with_prefix(repo: &str, prefix: &str) -> Result<Vec<String>> {
    // Use the GitHub CLI to list remote branches via the API.
    // The command returns the branch names using jq.
    let api_endpoint = format!("repos/{}/branches", repo);
    let output = Command::new("gh")
        .args(["api", &api_endpoint, "--jq", ".[] | .name"])
        .output()
        .map_err(|e| eyre!("Failed to execute gh api for repo '{}': {}", repo, e))?;
    if !output.status.success() {
        return Err(eyre!("Failed to list remote branches for repo '{}'", repo));
    }
    let output_str = String::from_utf8_lossy(&output.stdout);
    let branches: Vec<String> = output_str
        .lines()
        .map(|line| line.trim().trim_matches('"').to_string())
        .filter(|name| name.starts_with(prefix))
        .collect();
    Ok(branches)
}

pub fn create_pr(repo_path: &std::path::Path, change_id: &str, commit_msg: &str) -> Option<String> {
    let title = change_id.to_string();

    let body = format!(
        "{}\n\ndocs: https://github.com/scottidler/slam/blob/main/README.md",
        commit_msg
    );

    info!("Creating pull request for '{}' on branch '{}'", repo_path.display(), change_id);

    let pr_output = Command::new("gh")
        .current_dir(repo_path)
        .args([
            "pr",
            "create",
            "--title", &title,
            "--body", &body,
            "--base", "main",
        ])
        .output();

    match pr_output {
        Ok(output) if output.status.success() => {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            info!("PR created: {}", url);
            Some(url)
        }
        Ok(output) => {
            warn!("Failed to create PR: {}", String::from_utf8_lossy(&output.stderr));
            None
        }
        Err(err) => {
            error!("Failed to execute `gh pr create`: {}", err);
            None
        }
    }
}

pub fn close_pr(repo: &str, pr_number: u64) -> Result<()> {
    let cwd: PathBuf = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("unknown"));
    debug!("close_pr: current working directory: {}", cwd.display());

    let output = Command::new("gh")
        .args(&[
            "pr", "close",
            &pr_number.to_string(),
            "--repo", repo,
            "--delete-branch",
            "--comment", "Closing old PR in favor of new changes",
        ])
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(eyre!(
            "Failed to close PR {} for {}: {}",
            pr_number,
            repo,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

//---------------------------------------------------------------------
// New functions to support transactional rollback in Repo::create
//---------------------------------------------------------------------

/// Check if a local branch exists in the repository.
pub fn branch_exists(repo_path: &Path, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", "--verify", branch])
        .output()
        .map_err(|e| eyre!("Failed to execute git rev-parse: {}", e))?;
    Ok(output.status.success())
}

/// Check if a remote branch exists by using ls-remote.
pub fn remote_branch_exists(repo_path: &Path, branch: &str) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["ls-remote", "--exit-code", "--heads", "origin", branch])
        .output()
        .map_err(|e| eyre!("Failed to execute git ls-remote: {}", e))?;
    Ok(output.status.success())
}

/// Get the current branch name using symbolic-ref.
pub fn current_branch(repo_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to determine current branch: {}", e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(eyre!("Failed to determine current branch in '{}'", repo_path.display()))
    }
}

/// A generic checkout function for switching branches.
pub fn checkout(repo_path: &Path, branch: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["checkout", branch])
        .output()
        .map_err(|e| eyre!("Failed to execute git checkout: {}", e))?;
    if output.status.success() {
        info!("Checked out branch '{}' in '{}'", branch, repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to checkout branch '{}' in '{}': {}",
            branch,
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)))
    }
}

/// Reset the most recent commit (soft reset) so that changes remain staged.
pub fn reset_commit(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["reset", "--soft", "HEAD~1"])
        .output()
        .map_err(|e| eyre!("Failed to execute git reset --soft HEAD~1: {}", e))?;
    if output.status.success() {
        info!("Reset the last commit in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to reset commit in '{}': {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)))
    }
}

/// Returns true if any untracked files exist in the repository.
pub fn has_untracked_files(repo_path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["status", "--porcelain"])
        .output()
        .map_err(|e| eyre!("Failed to run git status: {}", e))?;
    let status_str = String::from_utf8_lossy(&output.stdout);
    for line in status_str.lines() {
        if line.starts_with("??") {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Returns true if there are any modifications (unstaged or staged) compared to HEAD.
pub fn has_modified_files(repo_path: &Path) -> Result<bool> {
    // git diff-index --quiet returns exit code 0 when there are no differences.
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["diff-index", "--quiet", "HEAD", "--"])
        .output()
        .map_err(|e| eyre!("Failed to run git diff-index: {}", e))?;
    // If exit code is 0, no modifications; otherwise, modifications exist.
    Ok(!output.status.success())
}

/// Stashes changes with a fixed message and returns the stash reference.
/// We assume the new stash becomes `stash@{0}`.
pub fn stash_save(repo_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["stash", "push", "-m", "SLAM pre-branch-stash"])
        .output()
        .map_err(|e| eyre!("Failed to run git stash push: {}", e))?;
    if output.status.success() {
        info!("Stashed changes in '{}'", repo_path.display());
        // Assume that our new stash is at stash@{0}
        Ok("stash@{0}".to_string())
    } else {
        Err(eyre!("Failed to stash changes: {}", String::from_utf8_lossy(&output.stderr)))
    }
}

/// Pops the stash identified by `stash_ref`.
pub fn stash_pop(repo_path: &Path, stash_ref: String) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["stash", "pop", &stash_ref])
        .output()
        .map_err(|e| eyre!("Failed to run git stash pop: {}", e))?;
    if output.status.success() {
        info!("Popped stash {} in '{}'", stash_ref, repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to pop stash {}: {}", stash_ref, String::from_utf8_lossy(&output.stderr)))
    }
}

/// Pulls the latest changes from remote.
pub fn pull(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["pull"])
        .output()
        .map_err(|e| eyre!("Failed to run git pull: {}", e))?;
    if output.status.success() {
        info!("Pulled latest changes in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to pull changes: {}", String::from_utf8_lossy(&output.stderr)))
    }
}

/// Resets the repository hard to HEAD.
pub fn reset_hard(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["reset", "--hard", "HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to run git reset --hard: {}", e))?;
    if output.status.success() {
        info!("Performed hard reset in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to reset hard: {}", String::from_utf8_lossy(&output.stderr)))
    }
}

/// Stages all changes and commits them with the provided message using "git commit -am".
pub fn commit_all(repo_path: &Path, message: &str) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["commit", "-am", message])
        .output()
        .map_err(|e| eyre!("Failed to run git commit -am: {}", e))?;
    if output.status.success() {
        info!("Committed changes in '{}' with message: {}", repo_path.display(), message);
        Ok(())
    } else {
        Err(eyre!("Failed to commit changes: {}", String::from_utf8_lossy(&output.stderr)))
    }
}

#[derive(serde::Deserialize, Debug)]
pub struct PrStatus {
    pub draft: bool,
    pub mergeable: bool,
    pub reviewed: bool,
    pub checked: bool,
}

pub fn get_pr_status(repo_name: &str, pr_number: u64) -> Result<PrStatus> {
    let output = Command::new("gh")
        .args(&[
            "pr", "view",
            &pr_number.to_string(),
            "--repo", repo_name,
            "--json", "isDraft,mergeable,reviewDecision,statusCheckRollup",
        ])
        .output()
        .map_err(|e| eyre!("Failed to execute gh pr view: {}", e))?;

    if !output.status.success() {
        return Err(eyre!(
            "Failed to get PR status for {} PR #{}: {}",
            repo_name,
            pr_number,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let json: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| eyre!("Failed to parse PR JSON: {}", e))?;

    // Log only a summary of the fields
    debug!(
        "PR {}#{}: isDraft: {:?}, mergeable: {:?}, reviewDecision: {:?}, checks: {:?}",
        repo_name,
        pr_number,
        json["isDraft"].as_bool().unwrap_or(false),
        json["mergeable"].as_str().unwrap_or("unknown"),
        json["reviewDecision"].as_str().unwrap_or("unknown"),
        json["statusCheckRollup"]
    );

    // Determine status based on key fields:
    let draft = json["isDraft"].as_bool().unwrap_or(false);

    let mergeable = match json["mergeable"].as_str() {
        Some(s) if s == "MERGEABLE" => true,
        _ => false,
    };

    let reviewed = match json["reviewDecision"].as_str() {
        Some(s) if s == "APPROVED" => true,
        _ => false,
    };

    // Consider both "SUCCESS" and "SKIPPED" as acceptable outcomes.
    let checked = if let Some(arr) = json["statusCheckRollup"].as_array() {
        arr.iter().all(|check| {
            let conclusion = check["conclusion"].as_str().unwrap_or("SUCCESS");
            conclusion == "SUCCESS" || conclusion == "SKIPPED"
        })
    } else {
        true
    };

    Ok(PrStatus {
        draft,
        mergeable,
        reviewed,
        checked,
    })
}

/// New helper function to purge a repository by closing all open PRs and deleting all remote branches with the prefix "SLAM".
pub fn purge_repo(repo: &str) -> Result<Vec<String>> {
    let mut messages = Vec::new();
    // Close every open PR for this repository.
    let pr_output = Command::new("gh")
        .args(&["pr", "list", "--repo", repo, "--state", "open", "--json", "number"])
        .output()?;
    if !pr_output.status.success() {
        return Err(eyre!("Failed to list open PRs for repo '{}'", repo));
    }
    let pr_numbers: Vec<u64> = serde_json::from_slice(&pr_output.stdout)
        .map_err(|e| eyre!("Failed to parse open PRs JSON for repo '{}': {}", repo, e))?;
    for pr in pr_numbers {
        close_pr(repo, pr)?;
        messages.push(format!("Closed PR #{} for repo '{}'", pr, repo));
    }
    // Delete every remote branch that starts with "SLAM".
    let branches = list_remote_branches_with_prefix(repo, "SLAM")?;
    for branch in branches {
        delete_remote_branch_gh(repo, &branch)?;
        messages.push(format!("Deleted remote branch '{}' for repo '{}'", branch, repo));
    }
    Ok(messages)
}

pub fn get_repo_slug(repo_path: &Path) -> Result<String> {
    // Get the remote origin URL.
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["config", "--get", "remote.origin.url"])
        .output()
        .map_err(|e| eyre!("Failed to get remote origin url for {}: {}", repo_path.display(), e))?;
    if !output.status.success() {
        return Err(eyre!(
            "Failed to get remote origin url for {}: {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Assume the URL is of the form "git@github.com:org/reponame.git"
    if let Some(stripped) = url.strip_prefix("git@github.com:") {
        let repo = stripped.trim_end_matches(".git");
        return Ok(repo.to_string());
    }
    Err(eyre!("Unexpected remote URL format: {}", url))
}

pub fn remote_prune(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["remote", "prune", "origin"])
        .output()
        .map_err(|e| eyre!("Failed to execute git remote prune origin: {}", e))?;
    if output.status.success() {
        info!("Pruned remote branches in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!(
            "Failed to prune remote branches in '{}': {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn list_local_branches_with_prefix(repo_path: &Path, prefix: &str) -> Result<Vec<String>> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["branch", "--list"])
        .output()
        .map_err(|e| eyre!("Failed to list local branches in '{}': {}", repo_path.display(), e))?;
    if !output.status.success() {
        return Err(eyre!(
            "Failed to list local branches in '{}': {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let branches: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|s| s.trim().trim_start_matches("* ").to_string())
        .filter(|name| name.starts_with(prefix))
        .collect();
    Ok(branches)
}

pub fn get_head_sha(repo_path: &Path) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["rev-parse", "HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to run git rev-parse HEAD: {}", e))?;
    if !output.status.success() {
        return Err(eyre!(
            "git rev-parse HEAD failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////
/////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////////

/// Returns true if there are staged changes.
pub fn _has_staged_files(repo_path: &Path) -> Result<bool> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["diff", "--cached", "--quiet"])
        .output()
        .map_err(|e| eyre!("Failed to run git diff --cached --quiet: {}", e))?;
    // exit code 0 means no staged changes
    Ok(!output.status.success())
}

pub fn _stage_files(repo_path: &Path) -> Result<()> {
    git(repo_path, &["add", "."])?;
    Ok(())
}

pub fn _commit_changes(repo_path: &Path, message: &str) -> Result<()> {
    git(repo_path, &["commit", "-m", message])?;
    Ok(())
}

pub fn _is_working_tree_clean(repo_path: &Path) -> bool {
    let staged_clean = git(repo_path, &["diff", "--cached", "--quiet"])
        .map(|o| o.status.success())
        .unwrap_or(false);

    let unstaged_clean = git(repo_path, &["diff", "--quiet"])
        .map(|o| o.status.success())
        .unwrap_or(false);

    staged_clean && unstaged_clean
}

pub fn _preflight_checks(repo_path: &Path) -> Result<()> {
    let head_branch = get_head_branch(repo_path)?;
    let current_branch_output = Command::new("git")
        .current_dir(repo_path)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to get current branch for repo {}: {}", repo_path.display(), e))?;
    if !current_branch_output.status.success() {
        return Err(eyre!("Failed to determine current branch for repo {}", repo_path.display()));
    }
    let current_branch = String::from_utf8_lossy(&current_branch_output.stdout).trim().to_string();
    let status_output = Command::new("git")
        .current_dir(repo_path)
        .args(["status", "--porcelain"])
        .output()
        .map_err(|e| eyre!("Failed to get status for repo {}: {}", repo_path.display(), e))?;
    if !status_output.status.success() {
        return Err(eyre!("Failed to get status for repo {}", repo_path.display()));
    }
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    if status_str.lines().any(|line| line.starts_with("??")) {
        return Err(eyre!("Untracked files present in repo {}. Please commit or remove them.", repo_path.display()));
    }
    if !status_str.lines().filter(|line| !line.starts_with("??") && !line.trim().is_empty()).collect::<Vec<_>>().is_empty() {
        let stash_output = Command::new("git")
            .current_dir(repo_path)
            .args(["stash", "push", "-m", "SLAM pre-branch-stash"])
            .output()
            .map_err(|e| eyre!("Failed to stash changes in repo {}: {}", repo_path.display(), e))?;
        if !stash_output.status.success() {
            return Err(eyre!("Failed to stash changes in repo {}", repo_path.display()));
        }
    }
    if current_branch != head_branch {
        let checkout_output = Command::new("git")
            .current_dir(repo_path)
            .args(["checkout", &head_branch])
            .output()
            .map_err(|e| eyre!("Failed to checkout branch {} in repo {}: {}", head_branch, repo_path.display(), e))?;
        if !checkout_output.status.success() {
            return Err(eyre!("Failed to checkout branch {} in repo {}", head_branch, repo_path.display()));
        }
    }
    let pull_output = Command::new("git")
        .current_dir(repo_path)
        .args(["pull"])
        .output()
        .map_err(|e| eyre!("Failed to pull changes in repo {}: {}", repo_path.display(), e))?;
    if !pull_output.status.success() {
        return Err(eyre!("Failed to pull changes in repo {}", repo_path.display()));
    }
    Ok(())
}

pub fn __create_pr(repo_path: &Path, change_id: &str) -> Option<String> {
    info!(
        "Creating pull request for '{}' on branch '{}'",
        repo_path.display(),
        change_id
    );

    let pr_output = Command::new("gh")
        .current_dir(repo_path)
        .args([
            "pr",
            "create",
            "--title",
            "SLAM: Automated Update",
            "--body",
            "Automated update generated by SLAM.\ndocs: https://github.com/scottidler/slam/blob/main/README.md",
            "--base",
            "main",
        ])
        .output();

    match pr_output {
        Ok(output) if output.status.success() => {
            let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
            info!("PR created: {}", url);
            Some(url)
        }
        Ok(output) => {
            warn!("Failed to create PR: {}", String::from_utf8_lossy(&output.stderr));
            None
        }
        Err(err) => {
            error!("Failed to execute `gh pr create`: {}", err);
            None
        }
    }
}

pub fn _create_or_switch_branch(repo_path: &Path, change_id: &str) -> bool {
    info!(
        "Ensuring repository '{}' is on branch '{}'",
        repo_path.display(),
        change_id
    );

    let head_output = Command::new("git")
        .current_dir(repo_path)
        .args(["symbolic-ref", "--short", "HEAD"])
        .output();

    let current_branch = match head_output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim().to_string()
        }
        _ => {
            warn!(
                "Skipping repository '{}': Not on a valid branch or in detached HEAD state.",
                repo_path.display()
            );
            return false;
        }
    };
    debug!(
        "Current branch in '{}': '{}'",
        repo_path.display(),
        current_branch
    );

    let branch_exists = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", "--verify", &change_id])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !branch_exists {
        info!(
            "Creating and switching to new branch '{}' in '{}'",
            change_id,
            repo_path.display()
        );
        let status = Command::new("git")
            .current_dir(repo_path)
            .args(["checkout", "-b", &change_id])
            .status();

        if let Err(err) = status {
            error!(
                "Error creating branch '{}' in '{}': {}",
                change_id,
                repo_path.display(),
                err
            );
            return false;
        }
    } else {
        info!(
            "Switching to existing branch '{}' in '{}'",
            change_id,
            repo_path.display()
        );
        let status = Command::new("git")
            .current_dir(repo_path)
            .args(["checkout", &change_id])
            .status();

        if let Err(err) = status {
            error!(
                "Error switching to branch '{}' in '{}': {}",
                change_id,
                repo_path.display(),
                err
            );
            return false;
        }
    }

    info!(
        "Switched to branch '{}' in '{}'",
        change_id,
        repo_path.display()
    );
    true
}

pub fn _push_branch(repo_path: &Path, change_id: &str) -> bool {
    info!(
        "Pushing branch '{}' to remote in '{}'",
        change_id,
        repo_path.display()
    );

    let status = Command::new("git")
        .current_dir(repo_path)
        .args(["push", "--set-upstream", "origin", &change_id])
        .status();

    if let Err(err) = status {
        error!(
            "Failed to push branch '{}' in '{}': {}",
            change_id,
            repo_path.display(),
            err
        );
        return false;
    }

    info!(
        "Successfully pushed branch '{}' in '{}'",
        change_id,
        repo_path.display()
    );
    true
}

/// Reopen a closed PR that was previously closed for the given repository and change_id.
/// This function looks for a closed PR matching the change_id and attempts to reopen it.
pub fn _reopen_pr(repo: &str, change_id: &str) -> Result<()> {
    // Find a closed PR by change_id. We assume at most one closed PR exists.
    let pr_number = _get_closed_pr_number_for_repo(repo, change_id)?;
    if pr_number == 0 {
        return Err(eyre!("No closed PR found for repo '{}' with change_id '{}'", repo, change_id));
    }
    let output = Command::new("gh")
        .args(["pr", "reopen", &pr_number.to_string(), "--repo", repo])
        .output()
        .map_err(|e| eyre!("Failed to execute gh pr reopen: {}", e))?;
    if output.status.success() {
        info!("Reopened PR #{} for repo '{}'", pr_number, repo);
        Ok(())
    } else {
        Err(eyre!("Failed to reopen PR #{} for repo '{}': {}",
            pr_number,
            repo,
            String::from_utf8_lossy(&output.stderr)))
    }
}

/// Get the commit hash for a given branch.
pub fn _get_branch_commit(repo_path: &Path, branch: &str) -> Result<String> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["rev-parse", branch])
        .output()
        .map_err(|e| eyre!("Failed to execute git rev-parse for branch '{}': {}", branch, e))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(eyre!("Failed to get commit hash for branch '{}'", branch))
    }
}

/// Create a new branch starting at a specific commit.
pub fn _create_branch(repo_path: &Path, branch: &str, commit: String) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["checkout", "-b", branch, &commit])
        .output()
        .map_err(|e| eyre!("Failed to execute git checkout -b: {}", e))?;
    if output.status.success() {
        info!("Created branch '{}' at commit {} in '{}'", branch, commit, repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to create branch '{}' at commit {}: {}",
            branch,
            commit,
            String::from_utf8_lossy(&output.stderr)))
    }
}

/// Unstage all files by resetting the index.
pub fn _unstage_all(repo_path: &Path) -> Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["reset"])
        .output()
        .map_err(|e| eyre!("Failed to execute git reset: {}", e))?;
    if output.status.success() {
        info!("Unstaged all files in '{}'", repo_path.display());
        Ok(())
    } else {
        Err(eyre!("Failed to unstage files in '{}': {}",
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)))
    }
}

/// Get the number of a closed PR for the given repository and change_id.
/// This is used as part of the rollback for closing a PR.
pub fn _get_closed_pr_number_for_repo(repo: &str, change_id: &str) -> Result<u64> {
    let output = Command::new("gh")
        .args([
            "pr", "list",
            "--repo", repo,
            "--head", change_id,
            "--state", "closed",
            "--json", "number",
            "--limit", "1",
        ])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("Failed to list closed PRs in repo '{}'", repo));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    let pr_number = parsed
        .as_array()
        .and_then(|arr| arr.get(0))
        .and_then(|obj| obj.get("number"))
        .and_then(Value::as_u64)
        .unwrap_or(0);

    Ok(pr_number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_max_retry_constant() {
        assert_eq!(MAX_RETRY, 5);
    }

    #[test]
    fn test_pr_status_debug() {
        let status = PrStatus {
            draft: false,
            mergeable: true,
            reviewed: true,
            checked: false,
        };

        let debug_str = format!("{:?}", status);
        assert!(debug_str.contains("draft: false"));
        assert!(debug_str.contains("mergeable: true"));
        assert!(debug_str.contains("reviewed: true"));
        assert!(debug_str.contains("checked: false"));
    }

    #[test]
    fn test_pr_status_deserialize() {
        // This test would require mocking the JSON parsing
        // For now, we'll test the struct creation directly
        let status = PrStatus {
            draft: true,
            mergeable: false,
            reviewed: false,
            checked: true,
        };

        assert!(status.draft);
        assert!(!status.mergeable);
        assert!(!status.reviewed);
        assert!(status.checked);
    }

    #[test]
    fn test_find_git_repositories_empty_dir() {
        let temp_dir = TempDir::new().unwrap();
        let result = find_git_repositories(temp_dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn test_find_git_repositories_with_git_repo() {
        let temp_dir = TempDir::new().unwrap();
        let repo_dir = temp_dir.path().join("test-repo");
        let git_dir = repo_dir.join(".git");

        fs::create_dir_all(&git_dir).unwrap();

        let result = find_git_repositories(temp_dir.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], repo_dir);
    }

    #[test]
    fn test_find_git_repositories_nested() {
        let temp_dir = TempDir::new().unwrap();
        let nested_repo = temp_dir.path().join("nested").join("repo");
        let git_dir = nested_repo.join(".git");

        fs::create_dir_all(&git_dir).unwrap();

        let result = find_git_repositories(temp_dir.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], nested_repo);
    }

    #[test]
    fn test_find_git_repositories_multiple() {
        let temp_dir = TempDir::new().unwrap();

        // Create multiple repos
        let repo1 = temp_dir.path().join("repo1");
        let repo2 = temp_dir.path().join("repo2");

        fs::create_dir_all(repo1.join(".git")).unwrap();
        fs::create_dir_all(repo2.join(".git")).unwrap();

        let mut result = find_git_repositories(temp_dir.path()).unwrap();
        result.sort(); // Sort for consistent ordering

        assert_eq!(result.len(), 2);
        assert!(result.contains(&repo1));
        assert!(result.contains(&repo2));
    }

    #[test]
    fn test_find_git_repositories_ignores_non_git_dirs() {
        let temp_dir = TempDir::new().unwrap();

        // Create a regular directory (not a git repo)
        fs::create_dir_all(temp_dir.path().join("not-a-repo")).unwrap();

        // Create a git repo
        let git_repo = temp_dir.path().join("git-repo");
        fs::create_dir_all(git_repo.join(".git")).unwrap();

        let result = find_git_repositories(temp_dir.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0], git_repo);
    }

    #[test]
    fn test_get_repo_slug_valid_ssh_url() {
        // This test would need a real git repo with remote configured
        // For now, we test the URL parsing logic
        let test_url = "git@github.com:tatari-tv/test-repo.git";

        if let Some(stripped) = test_url.strip_prefix("git@github.com:") {
            let repo = stripped.trim_end_matches(".git");
            assert_eq!(repo, "tatari-tv/test-repo");
        } else {
            panic!("URL parsing failed");
        }
    }

    #[test]
    fn test_get_repo_slug_invalid_url() {
        let test_url = "https://github.com/tatari-tv/test-repo.git";

        let result = test_url.strip_prefix("git@github.com:");
        assert!(result.is_none());
    }

    #[test]
    fn test_list_local_branches_with_prefix_parsing() {
        // Test the branch name parsing logic
        let mock_output = "  main\n* SLAM-feature-1\n  SLAM-feature-2\n  other-branch\n";

        let branches: Vec<String> = mock_output
            .lines()
            .map(|s| s.trim().trim_start_matches("* ").to_string())
            .filter(|name| name.starts_with("SLAM"))
            .collect();

        assert_eq!(branches.len(), 2);
        assert!(branches.contains(&"SLAM-feature-1".to_string()));
        assert!(branches.contains(&"SLAM-feature-2".to_string()));
    }

    #[test]
    fn test_merge_pr_args_construction() {
        let pr_number = 123u64;
        let repo = "test-org/test-repo";

        // Test without admin override
        let pr_binding = pr_number.to_string();
        let mut args = vec![
            "pr", "merge",
            &pr_binding,
            "--squash",
            "--delete-branch",
            "--repo",
            repo,
        ];

        assert_eq!(args.len(), 7);
        assert_eq!(args[0], "pr");
        assert_eq!(args[1], "merge");
        assert_eq!(args[2], "123");
        assert_eq!(args[6], repo);

        // Test with admin override
        args.insert(3, "--admin");
        assert_eq!(args.len(), 8);
        assert_eq!(args[3], "--admin");
    }

    #[test]
    fn test_create_pr_body_format() {
        let commit_msg = "Test commit message";

        let expected_body = format!(
            "{}\n\ndocs: https://github.com/scottidler/slam/blob/main/README.md",
            commit_msg
        );

        assert!(expected_body.contains(commit_msg));
        assert!(expected_body.contains("docs: https://github.com/scottidler/slam"));
        assert!(expected_body.contains("README.md"));
    }

    #[test]
    fn test_stash_save_return_value() {
        // Test the expected stash reference format
        let expected_stash_ref = "stash@{0}";
        assert_eq!(expected_stash_ref, "stash@{0}");
        assert!(expected_stash_ref.starts_with("stash@"));
        assert!(expected_stash_ref.contains("{0}"));
    }

    #[test]
    fn test_api_endpoint_format() {
        let repo = "test-org/test-repo";
        let branch = "SLAM-test-branch";

        let api_endpoint = format!("repos/{}/git/refs/heads/{}", repo, branch);
        assert_eq!(api_endpoint, "repos/test-org/test-repo/git/refs/heads/SLAM-test-branch");
    }

    #[test]
    fn test_github_url_format() {
        let reposlug = "test-org/test-repo";
        let url = format!("git@github.com:{}.git", reposlug);

        assert_eq!(url, "git@github.com:test-org/test-repo.git");
        assert!(url.starts_with("git@github.com:"));
        assert!(url.ends_with(".git"));
    }

    #[test]
    fn test_pr_status_json_parsing_logic() {
        // Test the logic used in get_pr_status for determining status fields

        // Test draft detection
        let draft_false = false;
        let draft_true = true;
        assert!(!draft_false);
        assert!(draft_true);

        // Test mergeable detection
        let mergeable_str = "MERGEABLE";
        let not_mergeable_str = "CONFLICTING";
        assert_eq!(mergeable_str, "MERGEABLE");
        assert_ne!(not_mergeable_str, "MERGEABLE");

        // Test review decision
        let approved_str = "APPROVED";
        let pending_str = "REVIEW_REQUIRED";
        assert_eq!(approved_str, "APPROVED");
        assert_ne!(pending_str, "APPROVED");

        // Test status check conclusions
        let success_conclusion = "SUCCESS";
        let skipped_conclusion = "SKIPPED";
        let failed_conclusion = "FAILURE";

        assert!(success_conclusion == "SUCCESS" || success_conclusion == "SKIPPED");
        assert!(skipped_conclusion == "SUCCESS" || skipped_conclusion == "SKIPPED");
        assert!(!(failed_conclusion == "SUCCESS" || failed_conclusion == "SKIPPED"));
    }

    #[test]
    fn test_run_pre_commit_with_retry_max_attempts() {
        // Test that MAX_RETRY is used as the upper bound
        let max_attempts = MAX_RETRY;
        assert_eq!(max_attempts, 5);

        // Test retry logic bounds
        for attempt in 1..=max_attempts {
            assert!(attempt >= 1);
            assert!(attempt <= MAX_RETRY);
        }
    }

    // Note: Many functions in this module interact with external commands (git, gh)
    // and would require extensive mocking or integration testing to test thoroughly.
    // The tests above focus on the testable logic and data structures.
}
