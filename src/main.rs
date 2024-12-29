// src/main.rs

use clap::Parser;
use eyre::Result;
use glob::glob;
use log::{debug, info};
use similar::{ChangeTag, TextDiff};
use std::{
    env,
    fs::{self, read_to_string, write},
    path::{Path, PathBuf},
};

/// Slam: Operates on multiple repositories
#[derive(Parser, Debug)]
#[command(name = "slam", about = "Finds and operates on repositories")]
struct SlamOpts {
    /// Root directory to search for repositories
    #[arg(short = 'R', long, help = "Root directory to search for repositories")]
    root: Option<PathBuf>,

    /// Glob pattern to find files within each repository
    #[arg(short = 'f', long, help = "Glob pattern to find files within each repository")]
    files: Option<String>,

    /// Pattern to match in files
    #[arg(short = 'p', long, help = "Pattern to match in files")]
    pattern: Option<String>,

    /// Value to replace the matched pattern with
    #[arg(short = 'r', long, help = "Value to replace the matched pattern with")]
    replace: Option<String>,

    /// Execute the changes instead of a dry run
    #[arg(short = 'e', long, help = "Execute changes instead of a dry run")]
    execute: bool,
}

/// Representation of a repository
struct Repo {
    reponame: String,                // Full slug (e.g., scottidler/ssl)
    change: Option<(String, String)>, // (pattern, replacement)
    files: Vec<String>,              // List of matching files
}

impl Repo {
    /// Output the repository information to the console
    fn output(&self, filter_files: bool, root: &Path, execute: bool) {
        // Skip repositories with no matching files
        if filter_files && self.files.is_empty() {
            return;
        }

        // Print repository name
        println!("{}", self.reponame);

        if filter_files {
            for file in &self.files {
                println!("  {}", file);

                // If `change` is specified, generate and optionally apply the diff
                if let Some((pattern, replacement)) = &self.change {
                    let full_path = root.join(&self.reponame).join(file);
                    if let Some(diff) = self.process_file(&full_path, pattern, replacement, execute)
                    {
                        for line in diff.lines() {
                            println!("    {}", line);
                        }
                    }
                }
            }
        }
    }

    /// Process a file to generate a diff and optionally apply changes
    fn process_file(
        &self,
        full_path: &Path,
        pattern: &str,
        replacement: &str,
        execute: bool,
    ) -> Option<String> {
        debug!("Processing file '{}'", full_path.display());

        // Read the file content
        let content = match read_to_string(full_path) {
            Ok(content) => content,
            Err(err) => {
                debug!("Failed to read file '{}': {}", full_path.display(), err);
                return None;
            }
        };

        // Check if the pattern exists in the file
        if !content.contains(pattern) {
            debug!(
                "Pattern '{}' not found in file '{}'",
                pattern,
                full_path.display()
            );
            return None;
        }

        debug!(
            "Pattern '{}' found in file '{}'. Preparing to apply replacement.",
            pattern,
            full_path.display()
        );

        // Prepare updated content
        let updated_content = content.replace(pattern, replacement);

        if updated_content == content {
            debug!(
                "Replacement resulted in no changes for file '{}'. Skipping.",
                full_path.display()
            );
            return None;
        }

        // Generate the diff
        let diff = self.generate_diff(&content, &updated_content);

        if execute {
            // Apply changes if execute is true
            if let Err(err) = write(full_path, &updated_content) {
                debug!(
                    "Failed to write updated content to '{}': {}",
                    full_path.display(),
                    err
                );
                return None;
            }
        }

        Some(diff)
    }

    /*
    /// Generate a unified diff between original and updated content
    fn generate_diff(&self, original: &str, updated: &str) -> String {
        let diff = TextDiff::from_lines(original, updated);
        let mut result = String::new();

        for change in diff.iter_all_changes() {
            let symbol = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => " ",
            };

            result.push_str(&format!("{}{}", symbol, change));
        }

        result
    }
    */

    fn generate_diff(&self, original: &str, updated: &str) -> String {
        let diff = TextDiff::from_lines(original, updated);
        let mut result = String::new();

        for (group_index, group) in diff.grouped_ops(3).iter().enumerate() {
            if group_index > 0 {
                result.push_str("\n...\n"); // Separator between groups of changes
            }

            for op in group {
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Delete => {
                            result.push_str(&format!("-{:4} | {}", change.old_index().unwrap() + 1, change));
                        }
                        ChangeTag::Insert => {
                            result.push_str(&format!("+{:4} | {}", change.new_index().unwrap() + 1, change));
                        }
                        ChangeTag::Equal => {
                            // Add context lines
                            result.push_str(&format!(" {:4} | {}", change.old_index().unwrap() + 1, change));
                        }
                    }
                }
            }
        }

        result
    }
}

fn main() -> Result<()> {
    env_logger::init(); // Initialize the logger

    let opts = SlamOpts::parse();

    let root = opts
        .root
        .unwrap_or_else(|| env::current_dir().expect("Failed to get current directory"));

    info!("Starting search in root directory: {}", root.display());

    let repos = find_git_repositories(&root)?;
    let mut repo_list = Vec::new();

    for repo in repos {
        if let Ok(relative_repo) = repo.strip_prefix(&root) {
            let reponame = relative_repo.display().to_string();
            let mut files = Vec::new();

            if let Some(ref pattern) = opts.files {
                let matched_files = find_files_in_repo(&repo, pattern)?;
                files.extend(
                    matched_files
                        .into_iter()
                        .map(|f| f.display().to_string())
                        .collect::<Vec<_>>(),
                );
            }

            files.sort();

            debug!(
                "Repository '{}' has {} matching files.",
                reponame,
                files.len()
            );

            let repo_entry = Repo {
                reponame,
                change: opts.pattern.clone().zip(opts.replace.clone()), // Combine pattern and replacement if both exist
                files,
            };

            repo_list.push(repo_entry);
        }
    }

    // Determine whether to filter based on files
    let filter_files = opts.files.is_some();

    // Output all repositories
    for repo in &repo_list {
        repo.output(filter_files, &root, opts.execute);
    }

    Ok(())
}

fn find_git_repositories(root: &Path) -> Result<Vec<PathBuf>> {
    let mut repos = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() && path.join(".git").is_dir() {
            repos.push(path);
        } else if path.is_dir() {
            repos.extend(find_git_repositories(&path)?);
        }
    }
    repos.sort();
    Ok(repos)
}

fn find_files_in_repo(repo: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    let mut matches = Vec::new();
    let search_pattern = repo.join(pattern).to_string_lossy().to_string();

    debug!("Searching for files matching '{}' in '{}'", pattern, repo.display());

    for entry in glob(&search_pattern)? {
        if let Ok(path) = entry {
            matches.push(path.strip_prefix(repo)?.to_path_buf());
        }
    }
    Ok(matches)
}
