use eyre::{eyre, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use log::{info, debug, warn, error};

fn git(repo_path: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(repo_path)
        .args(args)
        .output()
        .map_err(|e| eyre!("Failed to execute git {:?}: {}", args, e))
}

pub fn is_working_tree_clean(repo_path: &Path) -> bool {
    let staged_clean = git(repo_path, &["diff", "--cached", "--quiet"])
        .map(|o| o.status.success())
        .unwrap_or(false);

    let unstaged_clean = git(repo_path, &["diff", "--quiet"])
        .map(|o| o.status.success())
        .unwrap_or(false);

    staged_clean && unstaged_clean
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

pub fn checkout_branch(repo_path: &Path, branch: &str) -> eyre::Result<()> {
    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["checkout", "-B", branch])
        .output()
        .map_err(|e| eyre::eyre!("Failed to execute git checkout: {}", e))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(eyre::eyre!(
            "Failed to checkout branch {}: {}",
            branch,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub fn stage_files(repo_path: &Path) -> Result<()> {
    git(repo_path, &["add", "."])?;
    Ok(())
}

pub fn commit_changes(repo_path: &Path, message: &str) -> Result<()> {
    git(repo_path, &["commit", "-m", message])?;
    Ok(())
}

pub fn push_branch(repo_path: &Path, branch: &str) -> Result<()> {
    git(repo_path, &["push", "--set-upstream", "origin", branch])?;
    Ok(())
}

pub fn find_repos_in_org(org: &str) -> Result<Vec<String>> {
    let output = Command::new("gh")
        .args(["repo", "list", org, "--limit", "1000", "--json", "name"])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("Failed to list repos in org '{}'", org));
    }

    let parsed: Value = serde_json::from_slice(&output.stdout)?;
    let repos = parsed
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|repo| repo.get("name").and_then(Value::as_str))
        .map(|name| format!("{}/{}", org, name))
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
        warn!(
            "Failed to delete local branch '{}' in '{}': {}",
            branch,
            repo_path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
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

pub fn approve_pr(repo: &str, branch: &str) -> Result<()> {
    Command::new("gh")
        .args(["pr", "review", "--approve", "--repo", repo, "--branch", branch])
        .output()?;
    Ok(())
}

pub fn merge_pr(repo: &str, branch: &str, admin_override: bool) -> Result<()> {
    let mut args = vec!["pr", "merge", "--squash", "--delete-branch", "--repo", repo, "--branch", branch];
    if admin_override {
        args.insert(3, "--admin");
    }
    Command::new("gh").args(&args).output()?;
    Ok(())
}

//-----------------------------------------------------------------------------------------------

pub fn create_pr(repo_path: &std::path::Path, change_id: &str, commit_msg: &str) -> Option<String> {
    let title = change_id.to_string();

    let body = format!(
        "{}\n\ndocs: https://github.com/scottidler/slam/blob/main/README.md",
        commit_msg
    );

    log::info!("Creating pull request for '{}' on branch '{}'", repo_path.display(), change_id);

    let pr_output = std::process::Command::new("gh")
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
            log::info!("PR created: {}", url);
            Some(url)
        }
        Ok(output) => {
            log::warn!("Failed to create PR: {}", String::from_utf8_lossy(&output.stderr));
            None
        }
        Err(err) => {
            log::warn!("Failed to execute `gh pr create`: {}", err);
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
        Err(eyre::eyre!(
            "Failed to close PR {} for {}: {}",
            pr_number,
            repo,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
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


