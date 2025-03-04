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

fn default_branch_name() -> String {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    format!("SLAM-{}", date)
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
        long,
        value_names = &["PTN", "REPL"],
        num_args = 2,
        help = "Substring and replacement (requires two arguments)",
        group = "change_type"
    )]
    sub: Option<Vec<String>>,

    #[arg(
        short = 'r',
        long,
        value_names = &["PTN", "REPL"],
        num_args = 2,
        help = "Regex pattern and replacement (requires two arguments)",
        group = "change_type"
    )]
    regex: Option<Vec<String>>,

    #[arg(
        short = 'b',
        long,
        help = "Branch to create and commit changes on (default: 'SLAM-<YYYY-MM-DD>')",
        default_value_t = default_branch_name()
    )]
    branch: String,

    #[arg(short = 'B', long, default_value_t = 1, help = "Number of context lines in the diff output")]
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
    fn output(&self, root: &Path, commit_msg: Option<&str>, buffer: usize, branch_name: &str) -> bool {
        let repo_path = root.join(&self.reponame);

        // Ensure we're on the correct branch BEFORE making modifications
        if !self.create_or_switch_branch(&repo_path, branch_name) {
            println!("Skipping {} due to branch switching failure.", repo_path.display());
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
            self.commit_changes(&repo_path, commit_msg, branch_name);
        }

        true
    }

    fn is_working_tree_clean(&self, repo_path: &Path) -> bool {
        let output = Command::new("git")
            .current_dir(repo_path)
            .args(["status", "--porcelain"])
            .output();

        match output {
            Ok(output) => {
                if output.stdout.is_empty() {
                    debug!("Repo '{}' is clean.", repo_path.display());
                    true
                } else {
                    debug!(
                        "Repo '{}' has uncommitted changes:\n{}",
                        repo_path.display(),
                        String::from_utf8_lossy(&output.stdout)
                    );
                    false
                }
            }
            Err(e) => {
                eprintln!(
                    "Failed to check working tree status in '{}': {}",
                    repo_path.display(),
                    e
                );
                false
            }
        }
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

    fn commit_changes(&self, repo_path: &Path, user_message: &str, branch_name: &str) {
        // Ensure we're on the correct branch before committing
        if !self.create_or_switch_branch(repo_path, branch_name) {
            println!("Skipping {} due to branch switching failure.", repo_path.display());
            return;
        }

        // Debugging: Check current branch
        let branch_check = Command::new("git")
            .current_dir(repo_path)
            .args(["branch", "--show-current"])
            .output()
            .expect("Failed to check branch");

        println!(
            "DEBUG: Current branch in '{}': {}",
            repo_path.display(),
            String::from_utf8_lossy(&branch_check.stdout).trim()
        );

        // Debugging: Check repo state
        let rev_parse = Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "--verify", "HEAD"])
            .output();

        if let Err(e) = rev_parse {
            println!("DEBUG: Git rev-parse failed in '{}': {:?}", repo_path.display(), e);
        }

        // Stage all files before checking for uncommitted changes
        self.stage_files(repo_path);

        if !self.is_working_tree_clean(repo_path) {
            println!(
                "Skipping {} due to uncommitted changes before commit. Resolve manually.",
                repo_path.display()
            );
            return;
        }

        self.commit(repo_path, user_message);
    }

    fn create_or_switch_branch(&self, repo_path: &Path, branch_name: &str) -> bool {
        // Ensure the repo is on a valid branch
        let head_output = Command::new("git")
            .current_dir(repo_path)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output();

        let current_branch = match head_output {
            Ok(output) if output.status.success() => {
                String::from_utf8_lossy(&output.stdout).trim().to_string()
            }
            _ => {
                println!(
                    "Skipping {}: Not on a valid branch or detached HEAD.",
                    repo_path.display()
                );
                return false;
            }
        };

        println!(
            "Repository '{}' is currently on branch '{}'.",
            repo_path.display(),
            current_branch
        );

        // Check if the target branch already exists
        let branch_exists = Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "--verify", branch_name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !branch_exists {
            println!("Creating and switching to branch '{}' in '{}'", branch_name, repo_path.display());
            let status = Command::new("git")
                .current_dir(repo_path)
                .args(["checkout", "-b", branch_name])
                .status();

            if let Err(err) = status {
                eprintln!("Error creating branch {}: {}", branch_name, err);
                return false;
            }
        } else {
            println!("Switching to existing branch '{}' in '{}'", branch_name, repo_path.display());
            let status = Command::new("git")
                .current_dir(repo_path)
                .args(["checkout", branch_name])
                .status();

            if let Err(err) = status {
                eprintln!("Error switching to branch {}: {}", branch_name, err);
                return false;
            }
        }

        true
    }

    fn stage_files(&self, repo_path: &Path) -> bool {
        let status = Command::new("git")
            .current_dir(repo_path)
            .args(["add", "."])
            .status();

        if let Ok(status) = status {
            if status.success() {
                return true;
            }
            debug!("Git add failed.");
        } else {
            debug!("Git add command failed to execute.");
        }

        false
    }

    fn commit(&self, repo_path: &Path, user_message: &str) {
        let title = if user_message.is_empty() {
            "SLAM: Changes applied by slam".to_string()
        } else {
            format!("SLAM: {}", user_message)
        };

        let commit_message = format!("{}\ndocs: https://github.com/scottidler/slam/blob/main/README.md", title);

        let commit_output = Command::new("git")
            .current_dir(repo_path)
            .args(["commit", "-m", &commit_message])
            .output();

        match commit_output {
            Ok(output) => {
                if output.status.success() {
                    println!(
                        "✅ Successfully committed changes in '{}':\n{}",
                        repo_path.display(),
                        String::from_utf8_lossy(&output.stdout)
                    );
                } else {
                    eprintln!(
                        "❌ Failed to commit changes in '{}':\n{}",
                        repo_path.display(),
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
            }
            Err(e) => {
                eprintln!("❌ Failed to execute git commit in '{}': {}", repo_path.display(), e);
            }
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
                    if repo.output(&root, cli.commit.as_deref(), cli.buffer, &cli.branch) {
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
