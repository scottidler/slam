// src/main.rs

use clap::{ArgGroup, Parser};
use eyre::Result;
use glob::glob;
use log::{debug, info};
use colored::*;
use similar::{ChangeTag, TextDiff};
use std::{
    env,
    fs::{self, read_to_string, write},
    path::{Path, PathBuf},
    process::Command,
};
use regex::Regex;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

#[derive(Debug, Clone)]
pub enum Change {
    Sub(String, String),
    Regex(String, String),
}

#[derive(Parser, Debug)]
#[command(
    name = "slam",
    about = "Finds and operates on repositories",
    version = built_info::GIT_DESCRIBE
)]
#[command(group = ArgGroup::new("change").required(false).args(["sub", "regex"]))]
struct SlamCli {
    #[arg(short = 'f', long, help = "Glob pattern to find files within each repository")]
    files: Option<String>,

    #[arg(
        short = 's',
        long = "sub",
        value_names = &["PTN", "REPL"],
        num_args = 2,
        help = "Substring and replacement (requires two arguments)",
        group = "change_type"
    )]
    sub: Option<Vec<String>>,

    #[arg(
        short = 'r',
        long = "regex",
        value_names = &["PTN", "REPL"],
        num_args = 2,
        help = "Regex pattern and replacement (requires two arguments)",
        group = "change_type"
    )]
    regex: Option<Vec<String>>,

    #[arg(short = 'b', long, default_value_t = 1, help = "Number of context lines in the diff output")]
    buffer: usize,

    #[arg(
        short = 'c',
        long,
        help = "Commit changes with an optional message",
        default_missing_value = "",
        num_args(0..=1)
    )]
    commit: Option<String>,

    #[arg(help = "Repository names to filter", value_name = "REPOS", default_value = "")]
    repos: Vec<String>,
}

struct Repo {
    reponame: String,
    change: Option<Change>,
    files: Vec<String>,
}

impl Repo {
    fn output(&self, root: &Path, commit_msg: Option<&str>, buffer: usize) -> bool {
        let mut changed_files = Vec::new();

        for file in &self.files {
            if let Some(change) = &self.change {
                let full_path = root.join(&self.reponame).join(file);
                if let Some(diff) = self.process_file(&full_path, change, buffer, commit_msg.is_some()) {
                    changed_files.push((file.clone(), diff));
                }
            }
        }

        if changed_files.is_empty() {
            return false;
        }

        println!("{}", self.reponame);
        for (file, diff) in &changed_files {
            println!("  {}", file);
            for line in diff.lines() {
                println!("    {}", line);
            }
        }

        if let Some(commit_msg) = commit_msg {
            self.commit_changes(&root.join(&self.reponame), commit_msg);
        }

        true
    }

    fn process_file(
        &self,
        full_path: &Path,
        change: &Change,
        buffer: usize,
        commit: bool,
    ) -> Option<String> {
        debug!("Processing file '{}'", full_path.display());

        let content = match read_to_string(full_path) {
            Ok(content) => content,
            Err(err) => {
                debug!("Failed to read file '{}': {}", full_path.display(), err);
                return None;
            }
        };

        let updated_content = match change {
            Change::Sub(pattern, replacement) => {
                if !content.contains(pattern) {
                    debug!(
                        "Substring '{}' not found in file '{}'",
                        pattern,
                        full_path.display()
                    );
                    return None;
                }
                content.replace(pattern, replacement)
            }
            Change::Regex(pattern, replacement) => {
                let regex = match Regex::new(pattern) {
                    Ok(re) => re,
                    Err(err) => {
                        debug!(
                            "Failed to compile regex '{}' for file '{}': {}",
                            pattern, full_path.display(), err
                        );
                        return None;
                    }
                };
                if !regex.is_match(&content) {
                    debug!(
                        "Regex '{}' did not match in file '{}'",
                        pattern,
                        full_path.display()
                    );
                    return None;
                }
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

        if commit {
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

        for (index, group) in diff.grouped_ops(buffer).iter().enumerate() {
            if index > 0 {
                result.push_str("\n...\n");
            }

            for op in group {
                for change in diff.iter_changes(op) {
                    match change.tag() {
                        ChangeTag::Delete => {
                            result.push_str(
                                &format!(
                                    "{} | {}\n",
                                    format!("-{:4}", change.old_index().unwrap() + 1).red(),
                                    change.to_string().trim_end().red()
                                )
                            );
                        }
                        ChangeTag::Insert => {
                            result.push_str(
                                &format!(
                                    "{} | {}\n",
                                    format!("+{:4}", change.new_index().unwrap() + 1).green(),
                                    change.to_string().trim_end().green()
                                )
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

        result
    }

    fn commit_changes(&self, repo_path: &Path, user_message: &str) {
        let title = if user_message.is_empty() {
            "SLAM: Changes applied by slam".to_string()
        } else {
            format!("SLAM: {}", user_message)
        };

        let mut commit_body = String::new();
        if let Some(change) = &self.change {
            match change {
                Change::Sub(pattern, replacement) | Change::Regex(pattern, replacement) => {
                    let diff = self.generate_diff(
                        &pattern.replace("\"", ""),
                        &replacement.replace("\"", ""),
                        1,
                    );
                    commit_body.push_str(&diff);
                }
            }
        }
        commit_body.push_str("\ndocs: https://github.com/scottidler/slam/blob/main/README.md");

        let commit_message = format!("{}\n\n{}", title, commit_body);

        debug!("Committing changes to '{}'", repo_path.display());

        let status = Command::new("git")
            .current_dir(repo_path)
            .args(["add", "."])
            .status();

        if let Ok(status) = status {
            if status.success() {
                let commit = Command::new("git")
                    .current_dir(repo_path)
                    .args(["commit", "-m", &commit_message])
                    .status();

                if let Err(err) = commit {
                    debug!("Failed to commit changes: {}", err);
                }
            } else {
                debug!("Git add failed.");
            }
        } else {
            debug!("Git add command failed to execute.");
        }
    }
}

fn get_change(cli: &SlamCli) -> Option<Change> {
    if let Some(sub_args) = &cli.sub {
        Some(Change::Sub(sub_args[0].clone(), sub_args[1].clone()))
    } else if let Some(regex_args) = &cli.regex {
        Some(Change::Regex(regex_args[0].clone(), regex_args[1].clone()))
    } else {
        None
    }
}

fn create_repo(repo: &Path, root: &Path, change: &Option<Change>, files_pattern: &Option<String>) -> Option<Repo> {
    if let Ok(relative_repo) = repo.strip_prefix(root) {
        let reponame = relative_repo.display().to_string();
        let mut files = Vec::new();

        if let Some(pattern) = files_pattern {
            let matched_files = find_files_in_repo(repo, pattern).ok()?;
            files.extend(
                matched_files
                    .into_iter()
                    .map(|f| f.display().to_string())
                    .collect::<Vec<_>>(),
            );
            files.sort();
        }

        if files_pattern.is_some() && files.is_empty() {
            return None;
        }

        debug!(
            "Repository '{}' has {} matching files.",
            reponame,
            files.len()
        );

        Some(Repo {
            reponame,
            change: change.clone(),
            files,
        })
    } else {
        None
    }
}

fn main() -> Result<()> {
    env_logger::init();

    let cli = SlamCli::parse();
    let change = get_change(&cli);

    let root = env::current_dir().expect("Failed to get current directory");
    info!("Starting search in root directory: {}", root.display());

    let repos = find_git_repositories(&root)?;
    let mut repo_list = Vec::new();

    for repo in repos {
        if let Some(repo_entry) = create_repo(&repo, &root, &change, &cli.files) {
            if cli.repos.is_empty() || cli.repos.iter().any(|arg| repo_entry.reponame.contains(arg)) {
                repo_list.push(repo_entry);
            }
        }
    }

    if let Some(change) = &change {
        for repo in &repo_list {
            match change {
                Change::Sub(pattern, replacement) | Change::Regex(pattern, replacement) => {
                    if repo.output(&root, cli.commit.as_deref(), cli.buffer) {
                        println!(
                            "Applying pattern '{}' with replacement '{}' in repo '{}'.",
                            pattern, replacement, repo.reponame
                        );
                    }
                }
            }
        }
    } else if cli.files.is_some() {
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
