use eyre::{eyre, Result};
use log::{info, debug, warn, error};
use std::fs::{read_to_string, write};
use std::path::{Path, PathBuf};

use crate::cli;
use crate::git;
use crate::diff;
use crate::utils;
use crate::transaction;

#[derive(Debug, Clone)]
pub enum Change {
    Delete,
    Sub(String, String),
    Regex(String, String),
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub reponame: String,
    pub change_id: String,
    pub change: Option<Change>,
    pub files: Vec<String>,
    pub pr_number: u64,
}

impl Repo {
    pub fn create_repo_from_local(
        repo: &Path,
        root: &Path,
        change: &Option<Change>,
        file_ptns: &[String],
        change_id: &str,
    ) -> Option<Self> {
        debug!("Creating repo entry for '{}'", repo.display());

        let relative_repo = match repo.strip_prefix(root) {
            Ok(path) => path.display().to_string(),
            Err(e) => {
                warn!("Failed to strip prefix for '{}': {}", repo.display(), e);
                return None;
            }
        };

        let mut files = Vec::new();

        // If one or more file patterns were provided, find matches for each.
        if !file_ptns.is_empty() {
            for pattern in file_ptns {
                match find_files_in_repo(repo, pattern) {
                    Ok(matched_files) => {
                        files.append(&mut matched_files.into_iter().map(|f| f.display().to_string()).collect());
                    }
                    Err(e) => {
                        warn!("Failed to find files in '{}': {}", repo.display(), e);
                        return None;
                    }
                }
            }
            files.sort();
            files.dedup();
        }

        Some(Self {
            reponame: relative_repo,
            change_id: change_id.to_string(),
            change: change.clone(),
            files,
            pr_number: 0,
        })
    }

    pub fn create_repo_from_remote_with_pr(repo_name: &str, change_id: &str, pr_number: u64) -> Self {
        Self {
            reponame: repo_name.to_owned(),
            change_id: change_id.to_owned(),
            change: None,
            files: Vec::new(),
            pr_number,
        }
    }

    pub fn create_diff(&self, root: &Path, buffer: usize, commit: bool, simplified: bool) -> String {
        let repo_path = root.join(&self.reponame);
        let mut file_diffs = String::new();

        if let Some(change) = self.change.as_ref() {
            match change {
                // For Delete, we always generate a detailed diff.
                Change::Delete => {
                    for file in &self.files {
                        let mut file_diff = String::new();
                        file_diff.push_str(&format!("{}\n", utils::indent(&format!("D {}", file), 2)));
                        let full_path = repo_path.join(file);
                        match std::fs::read_to_string(&full_path) {
                            Ok(content) => {
                                let diff = diff::generate_diff(&content, "", buffer);
                                for line in diff.lines() {
                                    file_diff.push_str(&format!("{}\n", utils::indent(line, 4)));
                                }
                            }
                            Err(err) => {
                                file_diff.push_str(&format!(
                                    "{}\n",
                                    utils::indent(&format!("(Could not read file for diff: {})", err), 2)
                                ));
                            }
                        }
                        if !file_diff.trim().is_empty() {
                            file_diffs.push_str(&file_diff);
                        }
                    }
                }
                // For Sub and Regex, we want to run the file processing
                // and then decide how to output based on simplified.
                _ => {
                    for file in &self.files {
                        let full_path = repo_path.join(file);
                        if let Some(diff) = process_file(&full_path, change, buffer, commit) {
                            let mut file_diff = String::new();
                            if simplified {
                                file_diff.push_str(&format!(
                                    "{}\n",
                                    utils::indent(&format!(">< {}", file), 2)
                                ));
                            } else {
                                file_diff.push_str(&format!(
                                    "{}\n",
                                    utils::indent(&format!("M {}", file), 2)
                                ));
                                for line in diff.lines() {
                                    file_diff.push_str(&format!("{}\n", utils::indent(line, 4)));
                                }
                            }
                            file_diffs.push_str(&file_diff);
                        }
                    }
                }
            }
        } else {
            // Fallback when no change is specified.
            for file in &self.files {
                file_diffs.push_str(&format!(
                    "{}\n",
                    utils::indent(&format!(">< {}", file), 2)
                ));
            }
        }

        if file_diffs.trim().is_empty() {
            "".to_string()
        } else {
            format!("{}\n{}", self.reponame, file_diffs)
        }
    }

    /// The transactional create function performs all necessary Git operations
    /// (branch deletion, checkout, staging, commit, push, etc.) in a reversible way.
    ///
    /// If any step fails, the previously completed steps are rolled back.
    ///
    /// Note that the diff output is generated before making changes. When no commit
    /// message is provided, the diff output is returned as a dry run.
    pub fn create(
        &self,
        root: &Path,
        buffer: usize,
        commit_msg: Option<&str>,
        simplified: bool,
    ) -> Result<Option<String>> {
        let repo_path = root.join(&self.reponame);
        let mut transaction = transaction::Transaction::new();

        // Generate a dry-run diff (without committing) to detect if any change is present.
        let diff_output = self.create_diff(root, buffer, false, simplified);
        if diff_output.trim().is_empty() {
            info!("No changes detected in '{}'; skipping.", self.reponame);
            return Ok(None);
        }

        // Proceed with transactional updates.
        if git::has_untracked_files(&repo_path)? {
            return Err(eyre!("Untracked files exist in '{}'. Aborting.", repo_path.display()));
        }
        if git::has_modified_files(&repo_path)? {
            info!("Modified/staged files detected in '{}'; stashing changes.", repo_path.display());
            let stash_ref = git::stash_save(&repo_path)?;
            transaction.add_rollback({
                let repo_path = repo_path.clone();
                let stash_ref = stash_ref.clone();
                move || {
                    info!("Restoring stashed changes in '{}'", repo_path.display());
                    git::stash_pop(&repo_path, stash_ref.clone())
                }
            });
        }
        let head_branch = git::get_head_branch(&repo_path)?;
        let original_branch = git::current_branch(&repo_path)?;
        if original_branch != head_branch {
            info!("Switching from branch '{}' to HEAD branch '{}' in '{}'", original_branch, head_branch, repo_path.display());
            git::checkout(&repo_path, &head_branch)?;
            transaction.add_rollback({
                let repo_path = repo_path.clone();
                let original_branch = original_branch.clone();
                move || {
                    info!("Rolling back branch change: switching back to '{}'", original_branch);
                    git::checkout(&repo_path, &original_branch)
                }
            });
        }
        info!("Pulling latest changes in '{}'", repo_path.display());
        git::pull(&repo_path)?;
        if git::branch_exists(&repo_path, &self.change_id)? {
            info!("Local branch '{}' exists in '{}'; deleting it.", self.change_id, repo_path.display());
            git::delete_local_branch(&repo_path, &self.change_id)?;
        }
        if git::remote_branch_exists(&repo_path, &self.change_id)? {
            info!("Remote branch '{}' exists in '{}'; deleting it.", self.change_id, repo_path.display());
            git::delete_remote_branch(&repo_path, &self.change_id)?;
        }
        let branch_origin = git::current_branch(&repo_path)?;
        info!("Checking out new branch '{}' in '{}'", self.change_id, repo_path.display());
        git::checkout_branch(&repo_path, &self.change_id)?;
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            let branch_origin = branch_origin.clone();
            move || {
                info!("Rolling back branch checkout: switching back to '{}'", branch_origin);
                git::checkout(&repo_path, &branch_origin)
            }
        });
        info!("Applying file modifications for change '{}' in '{}'", self.change_id, self.reponame);
        let applied_diff = self.create_diff(root, buffer, true, simplified);
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            move || {
                info!("Rolling back file modifications in '{}'", repo_path.display());
                git::reset_hard(&repo_path)
            }
        });
        // If no commit message is provided, we treat it as a dry run.
        if commit_msg.is_none() {
            info!("Dry run detected for '{}'; rolling back all changes and returning diff.", self.reponame);
            transaction.rollback();
            return Ok(Some(applied_diff));
        }
        info!("Committing all changes in '{}' with message '{}'", self.reponame, commit_msg.unwrap());
        git::commit_all(&repo_path, commit_msg.unwrap())?;
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            move || {
                info!("Rolling back commit in '{}'", repo_path.display());
                git::reset_commit(&repo_path)
            }
        });
        info!("Pushing branch '{}' for '{}' to remote", self.change_id, self.reponame);
        git::push_branch(&repo_path, &self.change_id)?;
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            let change_id = self.change_id.clone();
            move || {
                info!("Rolling back push: deleting remote branch '{}' in '{}'", change_id, repo_path.display());
                git::delete_remote_branch(&repo_path, &change_id)
            }
        });
        let existing_pr = git::get_pr_number_for_repo(&self.reponame, &self.change_id)?;
        if existing_pr != 0 {
            info!("Existing PR #{} found for '{}'; closing it.", existing_pr, self.reponame);
            git::close_pr(&self.reponame, existing_pr)?;
        }
        info!("Creating a new PR for branch '{}' in '{}'", self.change_id, self.reponame);
        let pr_url = git::create_pr(&repo_path, &self.change_id, commit_msg.unwrap());
        if pr_url.is_none() {
            return Err(eyre!("Failed to create PR for repo '{}'", self.reponame));
        }
        transaction.commit();
        info!("Repository '{}' processed successfully.", self.reponame);
        Ok(Some(applied_diff))
    }

    pub fn review(&self, action: &cli::ReviewAction, summary: bool) -> Result<String> {
        match action {
            cli::ReviewAction::Ls { buffer, .. } => {
                if summary {
                    Ok(format!("{} (# {})", self.reponame, self.pr_number))
                } else {
                    Ok(self.get_review_diff(*buffer))
                }
            }
            cli::ReviewAction::Clone { .. } => Ok(String::new()),
            cli::ReviewAction::Approve { .. } => {
                // Retrieve the PR status using our simplified PrStatus struct.
                let status = git::get_pr_status(&self.reponame, self.pr_number)?;

                // Check that the PR is not a draft.
                if status.draft {
                    return Err(eyre!("PR {} in repo '{}' is a draft and cannot be approved.", self.pr_number, self.reponame));
                }

                // Ensure that the PR is mergeable (i.e. properly rebased on HEAD).
                if !status.mergeable {
                    return Err(eyre!("PR {} in repo '{}' is not mergeable; a rebase is required.", self.pr_number, self.reponame));
                }

                // Check that all status checks have passed.
                if !status.checked {
                    return Err(eyre!("PR {} in repo '{}' has not passed all status checks.", self.pr_number, self.reponame));
                }

                // Approve the PR if it hasn't already been reviewed.
                if status.reviewed {
                    warn!("PR {} is already reviewed; skipping re-approval.", self.pr_number);
                } else {
                    git::approve_pr(&self.reponame, self.pr_number)?;
                    info!("PR {} approved for repo '{}'.", self.pr_number, self.reponame);
                }

                // Merge the PR.
                match git::merge_pr(&self.reponame, self.pr_number, true) {
                    Ok(()) => {
                        info!("Successfully merged PR {} for repo '{}'.", self.pr_number, self.reponame);
                    }
                    Err(merge_err) => {
                        if merge_err.to_string().contains("Merge conflict") {
                            warn!("Merge conflict detected for repo {}. A rebase is required.", self.reponame);
                            return Err(merge_err);
                        } else {
                            error!("Merge failed for repo {}: {}", self.reponame, merge_err);
                            return Err(merge_err);
                        }
                    }
                }

                Ok(format!("Repo: {} -> Approved and merged PR: {} (# {})", self.reponame, self.change_id, self.pr_number))
            }
            cli::ReviewAction::Delete { change_id: _ } => {
                let mut messages = Vec::new();
                if self.pr_number != 0 {
                    git::close_pr(&self.reponame, self.pr_number)?;
                    messages.push(format!("Closed PR #{} for repo '{}'", self.pr_number, self.reponame));
                } else {
                    messages.push(format!("No open PR found for repo '{}'", self.reponame));
                }
                git::delete_remote_branch_gh(&self.reponame, &self.change_id)?;
                messages.push(format!("Deleted remote branch '{}' for repo '{}'", self.change_id, self.reponame));
                Ok(messages.join("\n"))
            }
        }
    }

    pub fn get_review_diff(&self, buffer: usize) -> String {
        let mut output = String::new();
        output.push_str(&format!("{} (# {})\n", self.reponame, self.pr_number));
        match git::get_pr_diff(&self.reponame, self.pr_number) {
            Ok(diff_text) => {
                let file_patches = diff::reconstruct_files_from_unified_diff(&diff_text);
                for (filename, orig_text, upd_text) in &file_patches {
                    let indicator = if upd_text.trim().is_empty() { "D" } else { "M" };
                    output.push_str(&format!("{}\n", utils::indent(&format!("{} {}", indicator, filename), 2)));
                    let colored_diff = if upd_text.trim().is_empty() {
                        diff::generate_diff(&orig_text, "", buffer)
                    } else {
                        diff::generate_diff(&orig_text, &upd_text, buffer)
                    };
                    for line in colored_diff.lines() {
                        output.push_str(&format!("{}\n", utils::indent(line, 4)));
                    }
                }
                if !file_patches.is_empty() {
                    output.push_str("\n");
                }
            }
            Err(e) => {
                output.push_str(&format!("  (Could not fetch PR diff: {})\n", e));
            }
        }
        output
    }
}

fn find_files_in_repo(repo: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let search_pattern = repo.join(pattern).to_string_lossy().to_string();
    let mut matches = Vec::new();

    for entry in glob::glob(&search_pattern)? {
        if let Ok(path) = entry {
            let relative_path = path.strip_prefix(repo)?.to_path_buf();
            matches.push(relative_path);
        }
    }
    Ok(matches)
}

fn process_file(full_path: &Path, change: &Change, buffer: usize, commit: bool) -> Option<String> {
    match change {
        Change::Delete => {
            if commit {
                let _ = std::fs::remove_file(full_path);
            }
            None
        },
        Change::Sub(pattern, replacement) => {
            let content = read_to_string(full_path).ok()?;
            if !content.contains(pattern) {
                return None;
            }
            let updated_content = content.replace(pattern, replacement);
            if updated_content == content {
                return None;
            }
            let diff = diff::generate_diff(&content, &updated_content, buffer);
            if commit {
                let _ = write(full_path, &updated_content);
            }
            Some(diff)
        },
        Change::Regex(pattern, replacement) => {
            let content = read_to_string(full_path).ok()?;
            let regex = regex::Regex::new(pattern).ok()?;
            if !regex.is_match(&content) {
                return None;
            }
            let updated_content = regex.replace_all(&content, replacement).to_string();
            if updated_content == content {
                return None;
            }
            let diff = diff::generate_diff(&content, &updated_content, buffer);
            if commit {
                let _ = write(full_path, &updated_content);
            }
            Some(diff)
        },
    }
}
