use crate::git;
use eyre::Result;
use log::{debug, warn};
use regex::Regex;
use std::fs::{read_to_string, write};
use std::path::{Path, PathBuf};

use crate::diff;

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

    /// Display the diff for local file changes (used in the create flow).
    pub fn show_create_diff(&self, root: &Path, buffer: usize, commit: bool) {
        let repo_path = root.join(&self.reponame);
        println!("Repo: {}", self.reponame);
        for file in &self.files {
            let full_path = repo_path.join(file);
            if let Some(change) = self.change.as_ref() {
                if let Some(diff) = process_file(&full_path, change, buffer, commit) {
                    println!("  Modified file: {}", file);
                    for line in diff.lines() {
                        println!("    {}", line);
                    }
                }
            }
        }
    }

    /// Display the diff for a pull request (used in the review flow).
    pub fn show_review_diff(&self, buffer: usize) {
        println!("Repo: {}", self.reponame);
        match git::get_pr_diff(&self.reponame, self.pr_number) {
            Ok(diff_text) => {
                let file_patches = diff::reconstruct_files_from_unified_diff(&diff_text);
                for (filename, orig_text, upd_text) in file_patches {
                    println!("  Modified file: {}", filename);
                    let colored_diff = diff::generate_diff(&orig_text, &upd_text, buffer);
                    for line in colored_diff.lines() {
                        println!("    {}", line);
                    }
                }
            }
            Err(e) => {
                warn!("Could not fetch PR diff for '{}': {}", self.reponame, e);
            }
        }
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

    let diff = diff::generate_diff(&content, &updated_content, buffer);

    if commit {
        let _ = write(full_path, &updated_content);
    }

    Some(diff)
}
