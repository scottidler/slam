use eyre::Result;
use log::{info, debug, warn};
use std::fs::{read_to_string, write};
use std::path::{Path, PathBuf};

use crate::cli;
use crate::git;
use crate::diff;
use crate::utils;

#[derive(Debug, Clone)]
pub enum Change {
    Delete,
    Sub(String, String),
    Regex(String, String),
}

impl Change {
    pub fn from_args(delete: bool, sub: &Option<Vec<String>>, regex: &Option<Vec<String>>) -> Option<Self> {
        if delete {
            Some(Self::Delete)
        } else if let Some(sub_args) = sub {
            Some(Self::Sub(sub_args[0].clone(), sub_args[1].clone()))
        } else if let Some(regex_args) = regex {
            Some(Self::Regex(regex_args[0].clone(), regex_args[1].clone()))
        } else {
            None
        }
    }
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
        files_pattern: &Option<String>,
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

        if let Some(pattern) = files_pattern {
            match find_files_in_repo(repo, pattern) {
                Ok(matched_files) => {
                    files.extend(matched_files.into_iter().map(|f| f.display().to_string()));
                    files.sort();
                }
                Err(e) => {
                    warn!("Failed to find files in '{}': {}", repo.display(), e);
                    return None;
                }
            }

            if files_pattern.is_some() && files.is_empty() {
                debug!(
                    "Skipping '{}' as no files matched '{}'",
                    repo.display(),
                    files_pattern.as_deref().unwrap_or("None")
                );
                return None;
            }
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

    pub fn show_create_diff(&self, root: &Path, buffer: usize, commit: bool) -> String {
        let mut output = String::new();
        output.push_str(&format!("{}\n", self.reponame));

        let repo_path = root.join(&self.reponame);
        if let Some(change) = self.change.as_ref() {
            match change {
                Change::Delete => {
                    for file in &self.files {
                        output.push_str(&format!("{}\n", utils::indent(&format!("D {}", file), 2)));
                        let full_path = repo_path.join(file);
                        match std::fs::read_to_string(&full_path) {
                            Ok(content) => {
                                let diff = diff::generate_diff(&content, "", buffer);
                                for line in diff.lines() {
                                    output.push_str(&format!("{}\n", utils::indent(line, 4)));
                                }
                            }
                            Err(err) => {
                                output.push_str(&format!(
                                    "{}\n",
                                    utils::indent(&format!("(Could not read file for diff: {})", err), 2)
                                ));
                            }
                        }
                    }
                    if commit {
                        for file in &self.files {
                            let full_path = repo_path.join(file);
                            let _ = std::fs::remove_file(&full_path);
                        }
                    }
                }
                _ => {
                    for file in &self.files {
                        let full_path = repo_path.join(file);
                        if let Some(diff) = process_file(&full_path, change, buffer, commit) {
                            output.push_str(&format!("{}\n", utils::indent(&format!("M {}", file), 2)));
                            for line in diff.lines() {
                                output.push_str(&format!("{}\n", utils::indent(line, 4)));
                            }
                        }
                    }
                }
            }
        } else {
            for file in &self.files {
                output.push_str(&format!("{}\n", utils::indent(&format!("Matched file: {}", file), 2)));
            }
        }
        output
    }

    pub fn create(&self, root: &Path, buffer: usize, commit_msg: Option<&str>) -> Result<String> {
        let diff_output = self.show_create_diff(root, buffer, commit_msg.is_some());
        let commit_msg = match commit_msg {
            Some(msg) => msg,
            None => return Ok(diff_output),
        };

        let repo_path = root.join(&self.reponame);
        let pr_number = git::get_pr_number_for_repo(&self.reponame, &self.change_id)?;
        if pr_number != 0 {
            warn!(
                "Existing PR #{} found for repo: {}. Closing it and deleting branch before starting over.",
                pr_number, self.reponame
            );
            git::close_pr(&self.reponame, pr_number)?;
        }
        git::delete_local_branch(&repo_path, &self.change_id)?;
        git::delete_remote_branch(&repo_path, &self.change_id)?;
        git::checkout_branch(&repo_path, &self.change_id)?;
        git::stage_files(&repo_path)?;
        if !git::is_working_tree_clean(&repo_path) {
            git::commit_changes(&repo_path, commit_msg)?;
            git::push_branch(&repo_path, &self.change_id)?;
        } else {
            info!("No changes to commit in '{}'", self.reponame);
        }
        git::create_pr(&repo_path, &self.change_id, commit_msg);
        Ok(diff_output)
    }

    pub fn review(&self, action: &cli::Action, summary: bool) -> Result<String> {
        match action {
            cli::Action::Ls { buffer, .. } => {
                if summary {
                    Ok(format!("{} (# {})", self.reponame, self.pr_number))
                } else {
                    Ok(self.get_review_diff(*buffer))
                }
            }
            cli::Action::Approve { admin_override: _, .. } => {
                git::approve_pr(&self.reponame, &self.change_id)?;
                info!("PR for '{}' approved.", self.reponame);
                git::merge_pr(&self.reponame, &self.change_id, false)?;
                info!("Successfully merged '{}'", self.reponame);
                Ok(format!("Repo: {} -> Approved PR: {} (# {})", self.reponame, self.change_id, self.pr_number))
            }
            cli::Action::Delete { change_id: _ } => {
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
        output.push_str(&format!("{}\n", self.reponame));
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
