use eyre::{eyre, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::process::{Command, Output};
use log::{info, debug, warn, error};
use regex::Regex;
use rayon::iter::{
    IntoParallelIterator,
    ParallelIterator,
};

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
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(|e| eyre!("Failed to get HEAD branch for repo {}: {}", repo_path.display(), e))?;
    if !output.status.success() {
        return Err(eyre!("Failed to get HEAD branch for repo {}.", repo_path.display()));
    }
    let full_ref = String::from_utf8_lossy(&output.stdout).trim().to_string();
    full_ref
        .strip_prefix("origin/")
        .map(String::from)
        .ok_or_else(|| eyre!("Unexpected format for HEAD branch: {}", full_ref))
}

/// Attempts to install pre-commit hooks for the repository at `repo_path`.
/// All output from the pre-commit command is captured and logged at debug level,
/// so nothing escapes to stdout. Returns:
/// - Ok(true) if hooks were installed successfully (and .git/hooks/pre-commit exists)
/// - Ok(false) if installation was attempted but failed (or the hook file is missing)
/// - Ok(false) if no .pre-commit-config.yaml exists.
pub fn install_pre_commit_hooks(repo_path: &Path) -> Result<bool> {
    let pre_commit_config = repo_path.join(".pre-commit-config.yaml");
    if pre_commit_config.exists() {
        // Capture the pre-commit version.
        let version_output = Command::new("pre-commit")
            .arg("--version")
            .output()
            .map_err(|e| eyre!("Failed to run pre-commit --version: {}", e))?;
        debug!("pre-commit version: {}", String::from_utf8_lossy(&version_output.stdout));

        // Run the install command.
        let install_output = Command::new("pre-commit")
            .current_dir(repo_path)
            .args(&["install"])
            .output()
            .map_err(|e| eyre!("Failed to run pre-commit install: {}", e))?;
        debug!("pre-commit install stdout: {}", String::from_utf8_lossy(&install_output.stdout));
        debug!("pre-commit install stderr: {}", String::from_utf8_lossy(&install_output.stderr));

        if install_output.status.success() {
            let hook_path = repo_path.join(".git/hooks/pre-commit");
            if hook_path.exists() {
                debug!("Pre-commit hooks installed in {}", repo_path.display());
                return Ok(true);
            } else {
                debug!("pre-commit install succeeded but {} not found", hook_path.display());
                return Ok(false);
            }
        } else {
            debug!("Failed to install pre-commit hooks in {}", repo_path.display());
            return Ok(false);
        }
    }
    // No configuration file exists.
    Ok(false)
}

/// Runs pre-commit hooks for all files in the repository located at `repo_path`.
/// If the command fails, it attempts to extract the value of the `INSTALL_PYTHON`
/// field from the .git/hooks/pre-commit file.
pub fn run_pre_commit(repo_path: &Path) -> Result<()> {
    info!("Running pre-commit hooks in '{}'", repo_path.display());
    let output = Command::new("pre-commit")
        .current_dir(repo_path)
        .args(&["run", "--all-files"])
        .output()
        .map_err(|e| eyre!("Failed to run pre-commit: {}", e))?;
    if !output.status.success() {
        let hook_path = repo_path.join(".git/hooks/pre-commit");
        let hook_content = std::fs::read_to_string(&hook_path).unwrap_or_default();
        let re = Regex::new(r"INSTALL_PYTHON=(\S+)").unwrap();
        let install_python = re.captures(&hook_content)
            .and_then(|caps| caps.get(1).map(|m| m.as_str()))
            .unwrap_or("Not found");
        return Err(eyre!("pre-commit run --all-files failed. INSTALL_PYTHON: {}", install_python));
    }
    Ok(())
}

//-----------------------------------------------------------------------------------------------

/// Lists remote branch names for the given repository that start with the specified prefix.
pub fn list_remote_branches_with_prefix(repo: &str, prefix: &str) -> eyre::Result<Vec<String>> {
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
        .output()
        .map_err(|e| eyre!("Failed to execute gh pr list: {}", e))?;
    if !output.status.success() {
        return Err(eyre!("Failed to list closed PRs in repo '{}'", repo));
    }
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| eyre!("Failed to parse JSON from gh pr list: {}", e))?;
    let pr_number = parsed.as_array()
        .and_then(|arr| arr.get(0))
        .and_then(|obj| obj.get("number"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(pr_number)
}
