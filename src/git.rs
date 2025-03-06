use eyre::{eyre, Result};
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Output};

/// Runs a Git command and returns the output
fn run_git_command(repo_path: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(repo_path)
        .args(args)
        .output()
        .map_err(|e| eyre!("Failed to execute git {:?}: {}", args, e))
}

/// Checks if the working tree of the given repository is clean.
pub fn is_working_tree_clean(repo_path: &Path) -> bool {
    let staged_clean = run_git_command(repo_path, &["diff", "--cached", "--quiet"])
        .map(|o| o.status.success())
        .unwrap_or(false);

    let unstaged_clean = run_git_command(repo_path, &["diff", "--quiet"])
        .map(|o| o.status.success())
        .unwrap_or(false);

    staged_clean && unstaged_clean
}

/// Finds all Git repositories recursively under the given root directory.
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

/// Creates or switches to the given branch in the repository.
pub fn create_or_switch_branch(repo_path: &Path, branch: &str) -> Result<()> {
    let branch_exists = run_git_command(repo_path, &["rev-parse", "--verify", branch])
        .map(|o| o.status.success())
        .unwrap_or(false);

    if branch_exists {
        run_git_command(repo_path, &["checkout", branch])?;
    } else {
        run_git_command(repo_path, &["checkout", "-b", branch])?;
    }
    Ok(())
}

/// Stages all modified files in the given repository.
pub fn stage_files(repo_path: &Path) -> Result<()> {
    run_git_command(repo_path, &["add", "."])?;
    Ok(())
}

/// Commits changes in the given repository with a message.
pub fn commit_changes(repo_path: &Path, message: &str) -> Result<()> {
    run_git_command(repo_path, &["commit", "-m", message])?;
    Ok(())
}

/// Pushes the current branch to the remote repository.
pub fn push_branch(repo_path: &Path, branch: &str) -> Result<()> {
    run_git_command(repo_path, &["push", "--set-upstream", "origin", branch])?;
    Ok(())
}

/// Finds GitHub repositories within an organization.
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

/// Retrieves the PR number for a given repository and branch.
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

/// Retrieves the diff of a PR from GitHub.
pub fn get_pr_diff(repo: &str, pr_number: u64) -> Result<String> {
    let output = Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string(), "-R", repo, "--patch"])
        .output()?;

    if !output.status.success() {
        return Err(eyre!("Failed to fetch PR diff for {}#{}", repo, pr_number));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Approves a GitHub PR remotely.
pub fn approve_pr(repo: &str, branch: &str) -> Result<()> {
    Command::new("gh")
        .args(["pr", "review", "--approve", "--repo", repo, "--branch", branch])
        .output()?;
    Ok(())
}

/// Merges a GitHub PR remotely.
pub fn merge_pr(repo: &str, branch: &str, admin_override: bool) -> Result<()> {
    let mut args = vec!["pr", "merge", "--squash", "--delete-branch", "--repo", repo, "--branch", branch];
    if admin_override {
        args.insert(3, "--admin");
    }
    Command::new("gh").args(&args).output()?;
    Ok(())
}
