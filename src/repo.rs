use crate::git;
use eyre::Result;
use log::{debug, info, warn};
use regex::Regex;
use similar::{ChangeTag, TextDiff};
use std::fs::{read_to_string, write};
use std::path::{Path, PathBuf};
use colored::*;

/// Represents the type of string replacement the user wants to perform.
#[derive(Debug, Clone)]
pub enum Change {
    Sub(String, String),
    Regex(String, String),
}

impl Change {
    pub fn from_args(sub: &Option<Vec<String>>, regex: &Option<Vec<String>>) -> Option<Self> {
        if let Some(sub_args) = sub {
            Some(Self::Sub(sub_args[0].clone(), sub_args[1].clone()))
        } else if let Some(regex_args) = regex {
            Some(Self::Regex(regex_args[0].clone(), regex_args[1].clone()))
        } else {
            None
        }
    }
}

/// Represents a repository and its associated change operations.
#[derive(Debug, Clone)]
pub struct Repo {
    pub reponame: String,
    pub change_id: String,
    pub change: Option<Change>,
    pub files: Vec<String>,
    pub pr_number: u64,
}

impl Repo {
    /// Creates a Repo object from a local directory on disk.
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
            match Self::find_files_in_repo(repo, pattern) {
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

    /// Creates a Repo from a remote reference (e.g., "org_name/repo_name") with a known PR number.
    pub fn create_repo_from_remote_with_pr(repo_name: &str, change_id: &str, pr_number: u64) -> Self {
        Self {
            reponame: repo_name.to_owned(),
            change_id: change_id.to_owned(),
            change: None,
            files: Vec::new(),
            pr_number,
        }
    }

    /// Applies changes to the repository, optionally committing and pushing them.
    pub fn output(&self, root: &Path, commit_msg: Option<&str>, buffer: usize) -> bool {
        let repo_path = root.join(&self.reponame);
        info!("Processing repository '{}'", self.reponame);

        if let Err(e) = git::create_or_switch_branch(&repo_path, &self.change_id) {
            warn!("Skipping '{}': {}", repo_path.display(), e);
            return false;
        }

        let mut changed_files = Vec::new();
        for file in &self.files {
            if let Some(change) = &self.change {
                let full_path = repo_path.join(file);
                if let Some(diff) = self.process_file(&full_path, change, buffer, commit_msg.is_some()) {
                    changed_files.push((file.clone(), diff));
                }
            }
        }

        if changed_files.is_empty() {
            info!("No changes detected in '{}'", self.reponame);
            return false;
        }

        println!("Repo: {}", self.reponame);
        for (file, diff) in &changed_files {
            println!("  Modified file: {}", file);
            for line in diff.lines() {
                println!("    {}", line);
            }
        }

        if let Some(msg) = commit_msg {
            let _ = git::commit_changes(&repo_path, msg);
            let _ = git::push_branch(&repo_path, &self.change_id);
        }

        true
    }

    /// Applies a substitution or regex change to a single file.
    fn process_file(&self, full_path: &Path, change: &Change, buffer: usize, commit: bool) -> Option<String> {
        let content = read_to_string(full_path).ok()?;

        let updated_content = match change {
            Change::Sub(pattern, replacement) => {
                if !content.contains(pattern) {
                    return None;
                }
                content.replace(pattern, replacement)
            }
            Change::Regex(pattern, replacement) => {
                let regex = Regex::new(pattern).ok()?;
                if !regex.is_match(&content) {
                    return None;
                }
                regex.replace_all(&content, replacement).to_string()
            }
        };

        if updated_content == content {
            return None;
        }

        let diff = self.generate_diff(&content, &updated_content, buffer);

        if commit {
            let _ = write(full_path, &updated_content);
        }

        Some(diff)
    }

    pub fn parse_unified_diff(&self, diff_text: &str) -> Vec<(String, String, String)> {
        let mut result = Vec::new();
        let mut current_file: Option<(String, Vec<String>, Vec<String>)> = None;

        for line in diff_text.lines() {
            if line.starts_with("diff --git ") {
                // If we were processing a file, save its accumulated content
                if let Some((filename, old_content, new_content)) = current_file.take() {
                    if !filename.is_empty() {
                        result.push((filename, old_content.join("\n"), new_content.join("\n")));
                    }
                }
                current_file = Some(("".to_string(), Vec::new(), Vec::new()));
            } else if line.starts_with("+++ b/") {
                if let Some(file) = current_file.as_mut() {
                    file.0 = line.trim_start_matches("+++ b/").to_string();
                }
            } else if let Some(file) = current_file.as_mut() {
                if line.starts_with('-') && !line.starts_with("---") {
                    file.1.push(line[1..].to_string());
                } else if line.starts_with('+') && !line.starts_with("+++") {
                    file.2.push(line[1..].to_string());
                } else if line.starts_with(' ') {
                    file.1.push(line[1..].to_string());
                    file.2.push(line[1..].to_string());
                }
            }
        }

        // Push any remaining file being processed
        if let Some((filename, old_content, new_content)) = current_file {
            if !filename.is_empty() {
                result.push((filename, old_content.join("\n"), new_content.join("\n")));
            }
        }

        if result.is_empty() {
            log::warn!(
                "parse_unified_diff: No meaningful diffs were extracted for repo '{}'",
                self.reponame
            );
        }

        result
    }

    /// Generates a formatted diff string.
    pub fn generate_diff(&self, original: &str, updated: &str, buffer: usize) -> String {
        let diff = TextDiff::from_lines(original, updated);
        let mut result = String::new();

        for group in diff.grouped_ops(buffer) {
            for op in group {
                for change in diff.iter_changes(&op) {
                    match change.tag() {
                        ChangeTag::Delete => {
                            result.push_str(&format!(
                                "{} | {}\n",
                                format!("-{:4}", change.old_index().unwrap() + 1).red(),
                                change.to_string().trim_end().red()
                            ));
                        }
                        ChangeTag::Insert => {
                            result.push_str(&format!(
                                "{} | {}\n",
                                format!("+{:4}", change.new_index().unwrap() + 1).green(),
                                change.to_string().trim_end().green()
                            ));
                        }
                        ChangeTag::Equal => {
                            result.push_str(&format!(
                                "{} | {}\n",
                                format!(" {:4}", change.old_index().unwrap() + 1).dimmed(),
                                change.to_string().trim_end().dimmed()
                            ));
                        }
                    }
                }
            }
        }

        result
    }

    /// Finds files matching a glob pattern within a repository.
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

    /// Approves a remote PR.
    pub fn approve_pr_remote(&self) -> bool {
        match git::approve_pr(&self.reponame, &self.change_id) {
            Ok(_) => {
                info!("PR approved for '{}'", self.reponame);
                true
            }
            Err(e) => {
                warn!("Failed to approve PR for '{}': {}", self.reponame, e);
                false
            }
        }
    }

    /// Merges a remote PR.
    pub fn merge_pr_remote(&self, admin_override: bool) -> bool {
        match git::merge_pr(&self.reponame, &self.change_id, admin_override) {
            Ok(_) => {
                info!("PR merged for '{}'", self.reponame);
                true
            }
            Err(e) => {
                warn!("Failed to merge PR for '{}': {}", self.reponame, e);
                false
            }
        }
    }
}
