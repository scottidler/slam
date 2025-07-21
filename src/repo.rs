use std::fs;
use eyre::{eyre, Result};
use log::{info, debug, warn, error};
use std::path::{Path, PathBuf};

use crate::cli;
use crate::git;
use crate::diff;
use crate::utils;
use crate::transaction;

#[derive(Debug, Clone)]
pub enum Change {
    Delete,
    Add(String, String),
    Sub(String, String),
    Regex(String, String),
}

#[derive(Debug, Clone)]
pub struct Repo {
    pub reposlug: String,
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

        let relative_reposlug = match repo.strip_prefix(root) {
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
            reposlug: relative_reposlug,
            change_id: change_id.to_string(),
            change: change.clone(),
            files,
            pr_number: 0,
        })
    }

    pub fn create_repo_from_remote_with_pr(reposlug: &str, change_id: &str, pr_number: u64) -> Self {
        Self {
            reposlug: reposlug.to_owned(),
            change_id: change_id.to_owned(),
            change: None,
            files: Vec::new(),
            pr_number,
        }
    }

    /// Generate a diff for this repo+change.  If `commit` is true, any
    /// filesystem mutations should already have been applied by process_file.
    /// Generate a diff for this repo+change. If `commit` is true, file edits have been applied.
    pub fn create_diff(
        &self,
        root: &Path,
        buffer: usize,
        commit: bool,
        simplified: bool,
    ) -> String {
        let repo_path = root.join(&self.reposlug);
        let mut file_diffs = String::new();

        if let Some(change) = self.change.as_ref() {
            match change {
                Change::Delete => {
                    // existing delete logic…
                    for file in &self.files {
                        let full_path = repo_path.join(file);
                        let mut file_diff = format!("{}\n", utils::indent(&format!("D {}", file), 2));
                        match fs::read_to_string(&full_path) {
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

                Change::Add(path, contents) => {
                    // new Add logic: diff from empty → contents
                    let mut file_diff = format!("{}\n", utils::indent(&format!("A {}", path), 2));
                    let diff = diff::generate_diff("", contents, buffer);
                    for line in diff.lines() {
                        file_diff.push_str(&format!("{}\n", utils::indent(line, 4)));
                    }
                    if !file_diff.trim().is_empty() {
                        file_diffs.push_str(&file_diff);
                    }
                }

                Change::Sub(_, _) | Change::Regex(_, _) => {
                    // existing substitution logic…
                    for file in &self.files {
                        let full_path = repo_path.join(file);
                        if let Some(d) = process_file(&full_path, change, buffer, commit) {
                            let prefix = if simplified { "><" } else { "M" };
                            let mut file_diff = format!("{}\n", utils::indent(&format!("{} {}", prefix, file), 2));
                            for line in d.lines() {
                                file_diff.push_str(&format!("{}\n", utils::indent(line, 4)));
                            }
                            file_diffs.push_str(&file_diff);
                        }
                    }
                }
            }
        } else {
            // no-change dry-run: list matched files
            for file in &self.files {
                file_diffs.push_str(&format!("{}\n", utils::indent(&format!(">< {}", file), 2)));
            }
        }

        if file_diffs.trim().is_empty() {
            String::new()
        } else {
            format!("{}\n{}", self.reposlug, file_diffs)
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
        let repo_path = root.join(&self.reposlug);
        let mut transaction = transaction::Transaction::new();

        // Normalize change_id so that it always starts with "SLAM"
        let normalized_change_id = if self.change_id.starts_with("SLAM") {
            self.change_id.clone()
        } else {
            format!("SLAM-{}", self.change_id)
        };

        // Generate a dry-run diff (without committing) to detect if any change is present.
        let diff_output = self.create_diff(root, buffer, false, simplified);
        if diff_output.trim().is_empty() {
            info!("No changes detected in '{}'; skipping.", self.reposlug);
            return Ok(None);
        }

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
            info!(
                "Switching from branch '{}' to HEAD branch '{}' in '{}'",
                original_branch, head_branch, repo_path.display()
            );
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

        if git::branch_exists(&repo_path, &normalized_change_id)? {
            info!(
                "Local branch '{}' exists in '{}'; deleting it.",
                normalized_change_id,
                repo_path.display()
            );
            git::delete_local_branch(&repo_path, &normalized_change_id)?;
        }
        if git::remote_branch_exists(&repo_path, &normalized_change_id)? {
            info!(
                "Remote branch '{}' exists in '{}'; deleting it.",
                normalized_change_id,
                repo_path.display()
            );
            git::delete_remote_branch(&repo_path, &normalized_change_id)?;
        }

        let branch_origin = git::current_branch(&repo_path)?;
        info!(
            "Checking out new branch '{}' in '{}'",
            normalized_change_id,
            repo_path.display()
        );
        git::checkout_branch(&repo_path, &normalized_change_id)?;
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            let branch_origin = branch_origin.clone();
            move || {
                info!("Rolling back branch checkout: switching back to '{}'", branch_origin);
                git::checkout(&repo_path, &branch_origin)
            }
        });

        info!(
            "Applying file modifications for change '{}' in '{}'",
            normalized_change_id, self.reposlug
        );
        let applied_diff = self.create_diff(root, buffer, true, simplified);
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            move || {
                info!("Rolling back file modifications in '{}'", repo_path.display());
                git::reset_hard(&repo_path)
            }
        });

        // Run pre-commit hooks.
        git::run_pre_commit_with_retry(&repo_path, 2)?;


        // Dry run: if no commit message is provided, roll back changes and return diff.
        if commit_msg.is_none() {
            info!(
                "Dry run detected for '{}'; rolling back all changes and returning diff.",
                self.reposlug
            );
            transaction.rollback();
            return Ok(Some(applied_diff));
        }

        info!(
            "Committing all changes in '{}' with message '{}'",
            repo_path.display(),
            commit_msg.unwrap()
        );
        git::commit_all(&repo_path, commit_msg.unwrap())?;
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            move || {
                info!("Rolling back commit in '{}'", repo_path.display());
                git::reset_commit(&repo_path)
            }
        });

        info!(
            "Pushing branch '{}' for '{}' to remote",
            normalized_change_id, self.reposlug
        );
        git::push_branch(&repo_path, &normalized_change_id)?;
        transaction.add_rollback({
            let repo_path = repo_path.clone();
            let normalized_change_id = normalized_change_id.clone();
            move || {
                info!(
                    "Rolling back push: deleting remote branch '{}' in '{}'",
                    normalized_change_id,
                    repo_path.display()
                );
                git::delete_remote_branch(&repo_path, &normalized_change_id)
            }
        });

        let existing_pr = git::get_pr_number_for_repo(&self.reposlug, &normalized_change_id)?;
        if existing_pr != 0 {
            info!(
                "Existing PR #{} found for '{}'; closing it.",
                existing_pr, self.reposlug
            );
            git::close_pr(&self.reposlug, existing_pr)?;
        }

        info!(
            "Creating a new PR for branch '{}' in '{}'",
            normalized_change_id, self.reposlug
        );
        let pr_url = git::create_pr(&repo_path, &normalized_change_id, commit_msg.unwrap());
        if pr_url.is_none() {
            return Err(eyre!("Failed to create PR for repo '{}'", self.reposlug));
        }

        transaction.commit();
        info!("Repository '{}' processed successfully.", self.reposlug);
        Ok(Some(applied_diff))
    }

    pub fn review(&self, action: &cli::ReviewAction, summary: bool) -> Result<String> {
        match action {
            cli::ReviewAction::Ls { buffer, .. } => {
                if summary {
                    Ok(format!("{} (# {})", self.reposlug, self.pr_number))
                } else {
                    Ok(self.get_review_diff(*buffer))
                }
            }
            cli::ReviewAction::Clone { .. } => {
                let cwd = std::env::current_dir()?;
                let target = cwd.join(&self.reposlug);
                git::clone_or_update_repo(&self.reposlug, &target, &self.change_id)?;
                let rel_path = target.strip_prefix(&cwd).unwrap_or(&target);
                Ok(format!(
                    "ensure clone {} -> {} and checkout to {}",
                    self.reposlug,
                    rel_path.display(),
                    self.change_id
                ))
            }
            cli::ReviewAction::Approve { .. } => {
                let status = git::get_pr_status(&self.reposlug, self.pr_number)?;
                if status.draft {
                    return Err(eyre!("PR {} in repo '{}' is a draft and cannot be approved.", self.pr_number, self.reposlug));
                }
                if !status.mergeable {
                    return Err(eyre!("PR {} in repo '{}' is not mergeable; a rebase is required.", self.pr_number, self.reposlug));
                }
                if !status.checked {
                    return Err(eyre!("PR {} in repo '{}' has not passed all status checks.", self.pr_number, self.reposlug));
                }
                if status.reviewed {
                    warn!("PR {} is already reviewed; skipping re-approval.", self.pr_number);
                } else {
                    git::approve_pr(&self.reposlug, self.pr_number)?;
                    info!("PR {} approved for repo '{}'.", self.pr_number, self.reposlug);
                }
                match git::merge_pr(&self.reposlug, self.pr_number, true) {
                    Ok(()) => {
                        info!("Successfully merged PR {} for repo '{}'.", self.pr_number, self.reposlug);
                    }
                    Err(merge_err) => {
                        if merge_err.to_string().contains("Merge conflict") {
                            warn!("Merge conflict detected for repo {}. A rebase is required.", self.reposlug);
                            return Err(merge_err);
                        } else {
                            error!("Merge failed for repo {}: {}", self.reposlug, merge_err);
                            return Err(merge_err);
                        }
                    }
                }
                Ok(format!("Repo: {} -> Approved and merged PR: {} (# {})", self.reposlug, self.change_id, self.pr_number))
            }
            cli::ReviewAction::Delete { .. } => {
                let mut messages = Vec::new();
                if self.pr_number != 0 {
                    git::close_pr(&self.reposlug, self.pr_number)?;
                    messages.push(format!("Closed PR #{} for repo '{}'", self.pr_number, self.reposlug));
                } else {
                    messages.push(format!("No open PR found for repo '{}'", self.reposlug));
                }
                git::delete_remote_branch_gh(&self.reposlug, &self.change_id)?;
                messages.push(format!("Deleted remote branch '{}' for repo '{}'", self.change_id, self.reposlug));
                Ok(messages.join("\n"))
            }
            cli::ReviewAction::Purge {} => {
                let messages = git::purge_repo(&self.reposlug)?;
                Ok(messages.join("\n"))
            }
        }
    }

    pub fn get_review_diff(&self, buffer: usize) -> String {
        let mut output = String::new();
        output.push_str(&format!("{} (# {})\n", self.reposlug, self.pr_number));
        match git::get_pr_diff(&self.reposlug, self.pr_number) {
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
                let _ = fs::remove_file(full_path);
            }
            None
        }

        Change::Add(_path, contents) => {
            // ensure there's exactly one trailing newline
            let mut file_contents = contents.clone();
            if !file_contents.ends_with('\n') {
                file_contents.push('\n');
            }

            // diff from empty → contents with trailing newline
            let diff = diff::generate_diff("", &file_contents, buffer);

            if commit {
                // ensure parent dirs exist
                if let Some(parent) = full_path.parent() {
                    if let Err(e) = fs::create_dir_all(parent) {
                        eprintln!("failed to create directories for {}: {}", full_path.display(), e);
                    }
                }
                // write the new file
                if let Err(e) = fs::write(full_path, file_contents) {
                    eprintln!("failed to write {}: {}", full_path.display(), e);
                }
            }

            Some(diff)
        }

        Change::Sub(pattern, replacement) => {
            let content = fs::read_to_string(full_path).ok()?;
            if !content.contains(pattern) {
                return None;
            }
            let updated = content.replace(pattern, replacement);
            if updated == content {
                return None;
            }
            let diff = diff::generate_diff(&content, &updated, buffer);
            if commit {
                let _ = fs::write(full_path, &updated);
            }
            Some(diff)
        }

        Change::Regex(pattern, replacement) => {
            let content = fs::read_to_string(full_path).ok()?;
            let regex = regex::Regex::new(pattern).ok()?;
            if !regex.is_match(&content) {
                return None;
            }
            let updated = regex.replace_all(&content, replacement).to_string();
            if updated == content {
                return None;
            }
            let diff = diff::generate_diff(&content, &updated, buffer);
            if commit {
                let _ = fs::write(full_path, &updated);
            }
            Some(diff)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_change_debug() {
        let delete = Change::Delete;
        let add = Change::Add("test.txt".to_string(), "content".to_string());
        let sub = Change::Sub("old".to_string(), "new".to_string());
        let regex = Change::Regex(r"\d+".to_string(), "X".to_string());

        // Ensure Debug is implemented
        assert!(!format!("{:?}", delete).is_empty());
        assert!(!format!("{:?}", add).is_empty());
        assert!(!format!("{:?}", sub).is_empty());
        assert!(!format!("{:?}", regex).is_empty());
    }

    #[test]
    fn test_change_clone() {
        let original = Change::Add("test.txt".to_string(), "content".to_string());
        let cloned = original.clone();

        match (&original, &cloned) {
            (Change::Add(path1, content1), Change::Add(path2, content2)) => {
                assert_eq!(path1, path2);
                assert_eq!(content1, content2);
            }
            _ => panic!("Clone failed"),
        }
    }

    #[test]
    fn test_repo_create_repo_from_remote_with_pr() {
        let repo = Repo::create_repo_from_remote_with_pr(
            "test-org/test-repo",
            "SLAM-test",
            123
        );

        assert_eq!(repo.reposlug, "test-org/test-repo");
        assert_eq!(repo.change_id, "SLAM-test");
        assert_eq!(repo.pr_number, 123);
        assert!(repo.change.is_none());
        assert!(repo.files.is_empty());
    }

    #[test]
    fn test_repo_create_repo_from_local_basic() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let repo_path = root.join("test-repo");
        fs::create_dir_all(&repo_path).unwrap();

        let change = Some(Change::Delete);
        let file_ptns: Vec<String> = vec![];
        let change_id = "test-change";

        let result = Repo::create_repo_from_local(
            &repo_path,
            root,
            &change,
            &file_ptns,
            change_id
        );

        assert!(result.is_some());
        let repo = result.unwrap();
        assert_eq!(repo.reposlug, "test-repo");
        assert_eq!(repo.change_id, "test-change");
        assert!(matches!(repo.change, Some(Change::Delete)));
        assert!(repo.files.is_empty());
        assert_eq!(repo.pr_number, 0);
    }

    #[test]
    fn test_repo_create_repo_from_local_with_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let repo_path = root.join("test-repo");
        fs::create_dir_all(&repo_path).unwrap();

        // Create some test files
        fs::write(repo_path.join("test1.txt"), "content1").unwrap();
        fs::write(repo_path.join("test2.txt"), "content2").unwrap();
        fs::write(repo_path.join("other.md"), "markdown").unwrap();

        let change = None;
        let file_ptns = vec!["*.txt".to_string()];
        let change_id = "test-change";

        let result = Repo::create_repo_from_local(
            &repo_path,
            root,
            &change,
            &file_ptns,
            change_id
        );

        assert!(result.is_some());
        let repo = result.unwrap();
        assert_eq!(repo.files.len(), 2);
        assert!(repo.files.contains(&"test1.txt".to_string()));
        assert!(repo.files.contains(&"test2.txt".to_string()));
        assert!(!repo.files.contains(&"other.md".to_string()));
    }

    #[test]
    fn test_repo_create_repo_from_local_invalid_prefix() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let repo_path = PathBuf::from("/completely/different/path");

        let change = None;
        let file_ptns: Vec<String> = vec![];
        let change_id = "test-change";

        let result = Repo::create_repo_from_local(
            &repo_path,
            root,
            &change,
            &file_ptns,
            change_id
        );

        assert!(result.is_none());
    }

    #[test]
    fn test_find_files_in_repo() {
        let temp_dir = TempDir::new().unwrap();
        let repo_path = temp_dir.path();

        // Create test files
        fs::write(repo_path.join("file1.txt"), "content1").unwrap();
        fs::write(repo_path.join("file2.txt"), "content2").unwrap();
        fs::write(repo_path.join("file3.md"), "markdown").unwrap();

        let result = find_files_in_repo(repo_path, "*.txt");
        assert!(result.is_ok());

        let files = result.unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.to_string_lossy() == "file1.txt"));
        assert!(files.iter().any(|f| f.to_string_lossy() == "file2.txt"));
    }

    #[test]
    fn test_process_file_delete_no_commit() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "test content").unwrap();

        let change = Change::Delete;
        let result = process_file(&file_path, &change, 1, false);

        assert!(result.is_none());
        assert!(file_path.exists()); // File should still exist
    }

    #[test]
    fn test_process_file_delete_with_commit() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "test content").unwrap();

        let change = Change::Delete;
        let result = process_file(&file_path, &change, 1, true);

        assert!(result.is_none());
        assert!(!file_path.exists()); // File should be deleted
    }

    #[test]
    fn test_process_file_add_no_commit() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("new.txt");

        let change = Change::Add("new.txt".to_string(), "new content".to_string());
        let result = process_file(&file_path, &change, 1, false);

        assert!(result.is_some());
        let diff = result.unwrap();
        assert!(diff.contains("new content"));
        assert!(!file_path.exists()); // File should not be created
    }

    #[test]
    fn test_process_file_add_with_commit() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("new.txt");

        let change = Change::Add("new.txt".to_string(), "new content".to_string());
        let result = process_file(&file_path, &change, 1, true);

        assert!(result.is_some());
        assert!(file_path.exists()); // File should be created
        let content = fs::read_to_string(&file_path).unwrap();
        assert_eq!(content, "new content\n"); // Should have trailing newline
    }

    #[test]
    fn test_process_file_sub_no_match() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "original content").unwrap();

        let change = Change::Sub("nonexistent".to_string(), "replacement".to_string());
        let result = process_file(&file_path, &change, 1, false);

        assert!(result.is_none());
    }

    #[test]
    fn test_process_file_sub_with_match() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "original content").unwrap();

        let change = Change::Sub("original".to_string(), "modified".to_string());
        let result = process_file(&file_path, &change, 1, false);

        assert!(result.is_some());
        let diff = result.unwrap();
        assert!(diff.contains("original"));
        assert!(diff.contains("modified"));
    }

    #[test]
    fn test_process_file_regex_valid() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "version 123").unwrap();

        let change = Change::Regex(r"\d+".to_string(), "456".to_string());
        let result = process_file(&file_path, &change, 1, false);

        assert!(result.is_some());
        let diff = result.unwrap();
        assert!(diff.contains("123"));
        assert!(diff.contains("456"));
    }

    #[test]
    fn test_process_file_regex_invalid() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "test content").unwrap();

        let change = Change::Regex("[invalid".to_string(), "replacement".to_string());
        let result = process_file(&file_path, &change, 1, false);

        assert!(result.is_none()); // Invalid regex should return None
    }

    #[test]
    fn test_repo_create_diff_no_change() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let repo = Repo {
            reposlug: "test-repo".to_string(),
            change_id: "test-change".to_string(),
            change: None,
            files: vec!["file1.txt".to_string(), "file2.txt".to_string()],
            pr_number: 0,
        };

        let diff = repo.create_diff(root, 1, false, false);

        assert!(diff.contains("test-repo"));
        assert!(diff.contains(">< file1.txt"));
        assert!(diff.contains(">< file2.txt"));
    }

    #[test]
    fn test_repo_create_diff_add_change() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let repo = Repo {
            reposlug: "test-repo".to_string(),
            change_id: "test-change".to_string(),
            change: Some(Change::Add("new.txt".to_string(), "content".to_string())),
            files: vec![],
            pr_number: 0,
        };

        let diff = repo.create_diff(root, 1, false, false);

        assert!(diff.contains("test-repo"));
        assert!(diff.contains("A new.txt"));
        assert!(diff.contains("content"));
    }

    #[test]
    fn test_repo_get_review_diff_basic_format() {
        let repo = Repo {
            reposlug: "test-org/test-repo".to_string(),
            change_id: "SLAM-test".to_string(),
            change: None,
            files: vec![],
            pr_number: 123,
        };

        // This test checks the basic format without mocking git::get_pr_diff
        // The actual diff fetching would be tested in integration tests
        let diff = repo.get_review_diff(1);
        assert!(diff.contains("test-org/test-repo (# 123)"));
    }

    #[test]
    fn test_repo_debug() {
        let repo = Repo {
            reposlug: "test-repo".to_string(),
            change_id: "test-change".to_string(),
            change: Some(Change::Delete),
            files: vec!["test.txt".to_string()],
            pr_number: 42,
        };

        let debug_str = format!("{:?}", repo);
        assert!(debug_str.contains("test-repo"));
        assert!(debug_str.contains("test-change"));
        assert!(debug_str.contains("Delete"));
        assert!(debug_str.contains("42"));
    }
}
