// src/main.rs

use clap::Parser;
use eyre::Result;
use glob::glob;
use log::{debug, info};
use colored::*;
use similar::{ChangeTag, TextDiff};
use std::{
    env,
    fs::{self, read_to_string, write},
    path::{Path, PathBuf},
};

#[derive(Parser, Debug)]
#[command(name = "slam", about = "Finds and operates on repositories")]
struct SlamOpts {
    #[arg(short = 'f', long, help = "Glob pattern to find files within each repository")]
    files: Option<String>,

    #[arg(short = 'p', long, help = "Pattern to match in files")]
    pattern: Option<String>,

    #[arg(short = 'r', long, help = "Value to replace the matched pattern with")]
    replace: Option<String>,

    #[arg(short = 'b', long, default_value_t = 1, help = "Number of context lines in the diff output")]
    buffer: usize,

    #[arg(short = 'e', long, help = "Execute changes instead of a dry run")]
    execute: bool,

    #[arg(help = "Repository names to filter", value_name = "REPOS", default_value = "")]
    repos: Vec<String>,
}


/// Representation of a repository
struct Repo {
    reponame: String,                   // Full slug (e.g., scottidler/ssl)
    change: Option<(String, String)>,   // (pattern, replacement)
    files: Vec<String>,                 // List of matching files
}

impl Repo {
    fn output(&self, root: &Path, execute: bool, buffer: usize) -> bool {
        let mut changed_files = Vec::new();

        for file in &self.files {
            if let Some((pattern, replacement)) = &self.change {
                let full_path = root.join(&self.reponame).join(file);
                if let Some(diff) = self.process_file(&full_path, pattern, replacement, execute, buffer) {
                    changed_files.push((file.clone(), diff));
                }
            }
        }

        if changed_files.is_empty() {
            return false;
        }

        println!("{}", self.reponame);
        for (file, diff) in changed_files {
            println!("  {}", file);
            for line in diff.lines() {
                println!("    {}", line);
            }
        }

        true
    }

    fn process_file(
        &self,
        full_path: &Path,
        pattern: &str,
        replacement: &str,
        execute: bool,
        buffer: usize,
    ) -> Option<String> {
        debug!("Processing file '{}'", full_path.display());

        let content = match read_to_string(full_path) {
            Ok(content) => content,
            Err(err) => {
                debug!("Failed to read file '{}': {}", full_path.display(), err);
                return None;
            }
        };

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

        let updated_content = content.replace(pattern, replacement);

        if updated_content == content {
            debug!(
                "Replacement resulted in no changes for file '{}'. Skipping.",
                full_path.display()
            );
            return None;
        }

        let diff = self.generate_diff(&content, &updated_content, buffer);

        if execute {
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

    fn generate_diff(&self, original: &str, updated: &str, buffer: usize) -> String {
        let diff = TextDiff::from_lines(original, updated);
        let mut result = String::new();

        for (group_index, group) in diff.grouped_ops(buffer).iter().enumerate() {
            if group_index > 0 {
                result.push_str("\n...\n");
            }

            for op in group {
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Delete => {
                            result.push_str(
                                &format!(
                                    "{} | {}",
                                    format!("-{:4}", change.old_index().unwrap() + 1).red(),
                                    change.to_string().red()
                                )
                            );
                        }
                        ChangeTag::Insert => {
                            result.push_str(
                                &format!(
                                    "{} | {}",
                                    format!("+{:4}", change.new_index().unwrap() + 1).green(),
                                    change.to_string().green()
                                )
                            );
                        }
                        ChangeTag::Equal => {
                            result.push_str(&format!(
                                " {:4} | {}",
                                change.old_index().unwrap() + 1,
                                change
                            ));
                        }
                    }
                }
            }
        }

        result
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let opts = SlamOpts::parse();

    let root = env::current_dir().expect("Failed to get current directory");
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
                files.sort();
            }

            if opts.files.is_some() && files.is_empty() {
                continue;
            }

            debug!(
                "Repository '{}' has {} matching files.",
                reponame,
                files.len()
            );

            let repo_entry = Repo {
                reponame: reponame.clone(),
                change: opts.pattern.clone().zip(opts.replace.clone()),
                files,
            };

            if opts.repos.is_empty() || opts.repos.iter().any(|arg| reponame.contains(arg)) {
                repo_list.push(repo_entry);
            }
        }
    }

    if opts.pattern.is_some() && opts.replace.is_some() {
        for repo in &repo_list {
            if repo.output(&root, opts.execute, opts.buffer) {
                continue;
            }
        }
    } else if opts.files.is_some() {
        for repo in &repo_list {
            if !repo.files.is_empty() {
                println!("{}", repo.reponame);
                for file in &repo.files {
                    println!("  {}", file);
                }
            }
        }
    } else {
        for repo in &repo_list {
            println!("{}", repo.reponame);
        }
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
