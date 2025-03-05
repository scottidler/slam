// src/repo.rs
use std::fs::{read_to_string, write};
use std::path::{Path, PathBuf};
use std::process::Command;

use colored::*;
use eyre::Result;
use log::{debug, error, info, warn};
use regex::Regex;
use similar::{ChangeTag, TextDiff};

/// Represents the type of string replacement the user wants to perform
#[derive(Debug, Clone)]
pub enum Change {
    Sub(String, String),
    Regex(String, String),
}

/// Tracks the repository name, the branch/PR change ID, the type of change to be applied,
/// and any files that matched the user-specified glob.
#[derive(Debug, Clone)]
pub struct Repo {
    pub reponame: String,
    pub change_id: String,
    pub change: Option<Change>,
    pub files: Vec<String>,
}

impl Repo {
    /// Create a Repo from a local directory on disk.
    /// This replaces the old `create_repo` function.
    ///
    /// - `repo`: the local repository path (where `.git` resides)
    /// - `root`: a parent directory containing many repos (so we can compute a relative name)
    /// - `change`: optional [Change] to apply
    /// - `files_pattern`: optional file glob pattern (e.g. `**/*.md`)
    /// - `change_id`: branch/PR name, stored on the `Repo`
    pub fn create_repo_from_local(
        repo: &Path,
        root: &Path,
        change: &Option<Change>,
        files_pattern: &Option<String>,
        change_id: &str,
    ) -> Option<Repo> {
        debug!("Creating repo entry for '{}'", repo.display());

        // Compute a relative path like "some_subdir/my_repo"
        let relative_repo = match repo.strip_prefix(root) {
            Ok(path) => path.display().to_string(),
            Err(e) => {
                warn!("Failed to strip prefix for '{}': {}", repo.display(), e);
                return None;
            }
        };

        let mut files = Vec::new();

        // If a file pattern is requested, find all matching files in `repo`.
        if let Some(pattern) = files_pattern {
            debug!(
                "Searching for files matching '{}' in repository '{}'",
                pattern,
                repo.display()
            );
            match Self::find_files_in_repo(repo, pattern) {
                Ok(matched_files) => {
                    files.extend(
                        matched_files
                            .into_iter()
                            .map(|f| f.display().to_string())
                            .collect::<Vec<_>>(),
                    );
                    files.sort();
                    info!(
                        "Found {} matching files in repository '{}'",
                        files.len(),
                        repo.display()
                    );
                }
                Err(e) => {
                    warn!(
                        "Failed to find files in repository '{}': {}",
                        repo.display(),
                        e
                    );
                    return None;
                }
            }

            // If user asked for a pattern but nothing matched, we skip this repo entirely.
            if files_pattern.is_some() && files.is_empty() {
                debug!(
                    "Skipping repository '{}' as no files matched the pattern '{}'",
                    repo.display(),
                    files_pattern.as_deref().unwrap_or("None")
                );
                return None;
            }
        }

        info!(
            "Repository '{}' added with {} matching files.",
            relative_repo,
            files.len()
        );

        Some(Repo {
            reponame: relative_repo,
            change_id: change_id.to_string(),
            change: change.clone(),
            files,
        })
    }

    /// Create a Repo from a remote reference (e.g. "org_name/repo_name").
    /// Typically you'd have discovered these remote repos by running a `gh` command.
    /// This is a lightweight constructor that doesn't populate local `files`.
    ///
    /// - `remote_reponame`: e.g. "my-org/my-repo"
    /// - `change_id`: the branch/PR to focus on
    pub fn create_repo_from_remote(remote_reponame: &str, change_id: &str) -> Repo {
        Repo {
            reponame: remote_reponame.to_owned(),
            change_id: change_id.to_owned(),
            change: None,
            files: Vec::new(),
        }
    }

    /// Performs the entire "apply changes, optionally commit, push, create PR" sequence for this Repo.
    /// - `root`: path containing the local repository
    /// - `commit_msg`: optional commit message
    /// - `buffer`: number of context lines in the diff
    ///
    /// Returns `true` if changes were made to any file, otherwise `false`.
    pub fn output(&self, root: &Path, commit_msg: Option<&str>, buffer: usize) -> bool {
        let repo_path = root.join(&self.reponame);
        info!(
            "Processing repository '{}' at '{}'",
            self.reponame,
            repo_path.display()
        );

        // Create or check out the branch specified by `self.change_id`
        if !self.create_or_switch_branch(&repo_path) {
            warn!(
                "Skipping '{}' due to branch switching failure.",
                repo_path.display()
            );
            return false;
        }

        let mut changed_files = Vec::new();

        for file in &self.files {
            if let Some(change) = &self.change {
                let full_path = repo_path.join(file);
                debug!("Processing file '{}'", full_path.display());

                if let Some(diff) = self.process_file(&full_path, change, buffer, commit_msg.is_some()) {
                    info!("Changes detected in '{}'", full_path.display());
                    changed_files.push((file.clone(), diff));
                }
            }
        }

        if changed_files.is_empty() {
            info!("No changes detected in repository '{}'", self.reponame);
            return false;
        }

        info!("Changes found in repository '{}':", self.reponame);
        for (file, diff) in &changed_files {
            info!("  Modified file: '{}'", file);
            for line in diff.lines() {
                debug!("    {}", line);
            }
        }

        // If user wants to commit/push/PR, do it.
        if let Some(msg) = commit_msg {
            info!(
                "Committing changes in '{}' with message: '{}'",
                repo_path.display(),
                msg
            );
            self.commit_changes(&repo_path, msg);

            if !self.push_branch(&repo_path) {
                warn!("Skipping PR creation due to push failure.");
                return false;
            }

            if let Some(pr_url) = self.create_pr(&repo_path) {
                info!("PR created successfully: {}", pr_url);
            } else {
                warn!("Failed to create PR for repository '{}'", self.reponame);
            }
        }

        true
    }

    /// Approve a remote PR using GitHub CLI. This is relevant if you have no local checkout.
    /// Uses `--repo <self.reponame>` and `--branch <self.change_id>`.
    pub fn approve_pr_remote(&self) -> bool {
        info!(
            "Approving pull request for repo '{}', branch '{}'",
            self.reponame, self.change_id
        );

        let approve_status = Command::new("gh")
            .args(["pr", "review", "--approve", "--repo", &self.reponame, "--branch", &self.change_id])
            .status();

        match approve_status {
            Ok(s) if s.success() => {
                info!(
                    "Pull request approved for repo '{}', branch '{}'",
                    self.reponame, self.change_id
                );
                true
            }
            Ok(_) => {
                warn!(
                    "Failed to approve PR for repo '{}', branch '{}'",
                    self.reponame, self.change_id
                );
                false
            }
            Err(err) => {
                error!(
                    "Error running 'gh pr review --approve': repo '{}', branch '{}', error: {}",
                    self.reponame, self.change_id, err
                );
                false
            }
        }
    }

    /// Merge a remote PR (with admin privileges, squash, delete-branch) using GitHub CLI.
    /// This is relevant if you have no local checkout. Must be preceded by a successful `approve_pr_remote`.
    pub fn merge_pr_remote(&self) -> bool {
        info!(
            "Merging pull request for repo '{}', branch '{}'",
            self.reponame, self.change_id
        );

        // In some workflows you must call `approve_pr_remote` first. Shown here as example.
        let merge_status = Command::new("gh")
            .args([
                "pr",
                "merge",
                "--admin",
                "--squash",
                "--delete-branch",
                "--repo",
                &self.reponame,
                "--branch",
                &self.change_id,
            ])
            .status();

        match merge_status {
            Ok(s) if s.success() => {
                info!(
                    "Pull request merged for repo '{}', branch '{}'",
                    self.reponame, self.change_id
                );
                true
            }
            Ok(_) => {
                warn!(
                    "Failed to merge PR for repo '{}', branch '{}'",
                    self.reponame, self.change_id
                );
                false
            }
            Err(err) => {
                error!(
                    "Error running 'gh pr merge': repo '{}', branch '{}', error: {}",
                    self.reponame, self.change_id, err
                );
                false
            }
        }
    }

    /// Returns true if the repo's working tree is clean (no staged or unstaged changes).
    fn is_working_tree_clean(&self, repo_path: &Path) -> bool {
        let staged_clean = Command::new("git")
            .current_dir(repo_path)
            .args(["diff", "--cached", "--quiet"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        let unstaged_clean = Command::new("git")
            .current_dir(repo_path)
            .args(["diff", "--quiet"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if staged_clean && unstaged_clean {
            debug!("Working tree is clean in '{}'", repo_path.display());
            true
        } else {
            warn!(
                "Uncommitted changes found in '{}'. Staged clean: {}, Unstaged clean: {}",
                repo_path.display(),
                staged_clean,
                unstaged_clean
            );
            false
        }
    }

    /// Perform the user's requested changes (substitution or regex) on a single file.
    /// Returns a string of the diff if any changes were made, otherwise None.
    fn process_file(
        &self,
        full_path: &Path,
        change: &Change,
        buffer: usize,
        commit: bool,
    ) -> Option<String> {
        info!("Processing file '{}'", full_path.display());

        let content = match read_to_string(full_path) {
            Ok(content) => content,
            Err(err) => {
                error!("Failed to read file '{}': {}", full_path.display(), err);
                return None;
            }
        };

        let updated_content = match change {
            Change::Sub(pattern, replacement) => {
                if !content.contains(pattern) {
                    debug!(
                        "Substring '{}' not found in file '{}'; skipping.",
                        pattern,
                        full_path.display()
                    );
                    return None;
                }
                info!(
                    "Applying substring replacement '{}' -> '{}' in '{}'",
                    pattern,
                    replacement,
                    full_path.display()
                );
                content.replace(pattern, replacement)
            }
            Change::Regex(pattern, replacement) => {
                let regex = match Regex::new(pattern) {
                    Ok(re) => re,
                    Err(err) => {
                        error!(
                            "Failed to compile regex '{}' for file '{}': {}",
                            pattern,
                            full_path.display(),
                            err
                        );
                        return None;
                    }
                };
                if !regex.is_match(&content) {
                    debug!(
                        "Regex '{}' did not match in file '{}'; skipping.",
                        pattern,
                        full_path.display()
                    );
                    return None;
                }
                info!(
                    "Applying regex replacement '{}' -> '{}' in '{}'",
                    pattern,
                    replacement,
                    full_path.display()
                );
                regex.replace_all(&content, replacement).to_string()
            }
        };

        if updated_content == content {
            debug!(
                "Replacement resulted in no changes for file '{}'. Skipping.",
                full_path.display()
            );
            return None;
        }

        let diff = self.generate_diff(&content, &updated_content, buffer);
        info!("Generated diff for '{}'", full_path.display());

        if commit {
            if let Err(err) = write(full_path, &updated_content) {
                error!(
                    "Failed to write updated content to '{}': {}",
                    full_path.display(),
                    err
                );
                return None;
            }
            info!("Updated file '{}' successfully.", full_path.display());
        }

        Some(diff)
    }

    /// Build a unified-diff-like string using the `TextDiff` library.
    fn generate_diff(&self, original: &str, updated: &str, buffer: usize) -> String {
        info!("Generating diff with buffer size {}", buffer);

        let diff = TextDiff::from_lines(original, updated);
        let mut result = String::new();

        for (index, group) in diff.grouped_ops(buffer).iter().enumerate() {
            if index > 0 {
                result.push_str("\n...\n");
            }
            for op in group {
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Delete => {
                            result.push_str(&format!(
                                "{} | {}\n",
                                format!("-{:4}", change.old_index().unwrap() + 1).red(),
                                change.to_string().trim_end().red()
                            ));
                            debug!(
                                "Deleted line {}: {}",
                                change.old_index().unwrap() + 1,
                                change.to_string().trim_end()
                            );
                        }
                        ChangeTag::Insert => {
                            result.push_str(&format!(
                                "{} | {}\n",
                                format!("+{:4}", change.new_index().unwrap() + 1).green(),
                                change.to_string().trim_end().green()
                            ));
                            debug!(
                                "Inserted line {}: {}",
                                change.new_index().unwrap() + 1,
                                change.to_string().trim_end()
                            );
                        }
                        ChangeTag::Equal => {
                            result.push_str(&format!(
                                " {:4} | {}\n",
                                change.old_index().unwrap() + 1,
                                change.to_string().trim_end()
                            ));
                        }
                    }
                }
            }
        }

        info!("Diff generation complete.");
        result
    }

    /// Wrapper that stages and commits changes in this repo if needed.
    /// - `repo_path`: local path to the repo
    /// - `user_message`: commit message
    fn commit_changes(&self, repo_path: &Path, user_message: &str) {
        info!(
            "Attempting to commit changes in '{}' on branch '{}'",
            repo_path.display(),
            self.change_id
        );

        if !self.create_or_switch_branch(repo_path) {
            warn!(
                "Skipping commit in '{}' due to branch switching failure.",
                repo_path.display()
            );
            return;
        }

        if !self.stage_files(repo_path) {
            warn!("Skipping commit in '{}' due to staging failure.", repo_path.display());
            return;
        }

        if self.is_working_tree_clean(repo_path) {
            warn!("Skipping commit in '{}' because there are no changes.", repo_path.display());
            return;
        }

        info!(
            "Committing changes in '{}' with message: '{}'",
            repo_path.display(),
            user_message
        );

        self.commit(repo_path, user_message);
    }

    /// Ensures the local repo is on the correct branch, creating it if needed.
    fn create_or_switch_branch(&self, repo_path: &Path) -> bool {
        info!(
            "Ensuring repository '{}' is on branch '{}'",
            repo_path.display(),
            self.change_id
        );

        // Check current branch
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

        // Check if our target branch already exists
        let branch_exists = Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "--verify", &self.change_id])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !branch_exists {
            info!(
                "Creating and switching to new branch '{}' in '{}'",
                self.change_id,
                repo_path.display()
            );
            let status = Command::new("git")
                .current_dir(repo_path)
                .args(["checkout", "-b", &self.change_id])
                .status();

            if let Err(err) = status {
                error!(
                    "Error creating branch '{}' in '{}': {}",
                    self.change_id,
                    repo_path.display(),
                    err
                );
                return false;
            }
        } else {
            info!(
                "Switching to existing branch '{}' in '{}'",
                self.change_id,
                repo_path.display()
            );
            let status = Command::new("git")
                .current_dir(repo_path)
                .args(["checkout", &self.change_id])
                .status();

            if let Err(err) = status {
                error!(
                    "Error switching to branch '{}' in '{}': {}",
                    self.change_id,
                    repo_path.display(),
                    err
                );
                return false;
            }
        }

        info!(
            "Switched to branch '{}' in '{}'",
            self.change_id,
            repo_path.display()
        );
        true
    }

    /// Stages all modified files in the repository (i.e. `git add .`).
    /// Returns true if there is at least something staged.
    fn stage_files(&self, repo_path: &Path) -> bool {
        info!("Staging all changes in repository '{}'", repo_path.display());

        let output = Command::new("git")
            .current_dir(repo_path)
            .args(["add", "."])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let staged_files = Command::new("git")
                    .current_dir(repo_path)
                    .args(["diff", "--cached", "--name-only"])
                    .output()
                    .expect("Failed to check staged files");

                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let staged_output = String::from_utf8_lossy(&staged_files.stdout);

                info!("Successfully staged changes in '{}'", repo_path.display());
                debug!("git add stdout: {}", stdout);
                debug!("git add stderr: {}", stderr);
                debug!("Staged files: {}", staged_output);

                !staged_output.trim().is_empty()
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!("Git add failed in '{}': {}", repo_path.display(), stderr);
                false
            }
            Err(e) => {
                error!("Git add command failed in '{}': {}", repo_path.display(), e);
                false
            }
        }
    }

    /// Issues the actual `git commit -m <message>`.
    fn commit(&self, repo_path: &Path, user_message: &str) {
        let title = if user_message.is_empty() {
            "SLAM: Changes applied by slam".to_string()
        } else {
            format!("SLAM: {}", user_message)
        };

        let commit_message = format!(
            "{}\ndocs: https://github.com/scottidler/slam/blob/main/README.md",
            title
        );

        let commit_output = Command::new("git")
            .current_dir(repo_path)
            .args(["commit", "-m", &commit_message])
            .output();

        match commit_output {
            Ok(output) if output.status.success() => {
                info!(
                    "✅ Successfully committed changes in '{}':\n{}",
                    repo_path.display(),
                    String::from_utf8_lossy(&output.stdout)
                );
            }
            Ok(output) => {
                warn!(
                    "❌ Failed to commit changes in '{}':\n{}",
                    repo_path.display(),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            Err(e) => {
                error!(
                    "❌ Failed to execute git commit in '{}': {}",
                    repo_path.display(),
                    e
                );
            }
        }
    }

    /// Pushes the current branch to `origin`.
    fn push_branch(&self, repo_path: &Path) -> bool {
        info!(
            "Pushing branch '{}' to remote in '{}'",
            self.change_id,
            repo_path.display()
        );

        let status = Command::new("git")
            .current_dir(repo_path)
            .args(["push", "--set-upstream", "origin", &self.change_id])
            .status();

        if let Err(err) = status {
            error!(
                "Failed to push branch '{}' in '{}': {}",
                self.change_id,
                repo_path.display(),
                err
            );
            return false;
        }

        info!(
            "Successfully pushed branch '{}' in '{}'",
            self.change_id,
            repo_path.display()
        );
        true
    }

    /// Creates a pull request using `gh pr create` from the current branch (`self.change_id`) into `main`.
    /// Returns the URL of the new PR if successful, otherwise `None`.
    fn create_pr(&self, repo_path: &Path) -> Option<String> {
        info!(
            "Creating pull request for '{}' on branch '{}'",
            repo_path.display(),
            self.change_id
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

    /// Helper to find files in a local repository that match a glob pattern (like `**/*.rs`).
    fn find_files_in_repo(repo: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
        info!(
            "Searching for files matching '{}' in repository '{}'",
            pattern,
            repo.display()
        );

        let mut matches = Vec::new();
        let search_pattern = repo.join(pattern).to_string_lossy().to_string();
        debug!("Using search pattern: '{}'", search_pattern);

        for entry in glob::glob(&search_pattern)? {
            match entry {
                Ok(path) => {
                    let relative_path = path.strip_prefix(repo)?.to_path_buf();
                    debug!("Matched file: '{}'", relative_path.display());
                    matches.push(relative_path);
                }
                Err(e) => {
                    warn!(
                        "Failed to match file with pattern '{}': {}",
                        search_pattern, e
                    );
                }
            }
        }

        info!(
            "Found {} matching files in repository '{}'",
            matches.len(),
            repo.display()
        );

        Ok(matches)
    }
}

