// src/main.rs

use clap::{ArgGroup, Parser};
use eyre::Result;
use glob::glob;
use log::{info, debug, warn, error};
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
    let branch_name = format!("SLAM-{}", date);

    debug!("Generated default branch name: {}", branch_name);

    branch_name
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
        info!(
            "Processing repository '{}' at '{}'",
            self.reponame, repo_path.display()
        );

        // Ensure we're on the correct branch BEFORE making modifications
        if !self.create_or_switch_branch(&repo_path, branch_name) {
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

        if let Some(commit_msg) = commit_msg {
            info!(
                "Committing changes in '{}' with message: '{}'",
                repo_path.display(),
                commit_msg
            );
            self.commit_changes(&repo_path, commit_msg, branch_name);

            // Step 1: Push branch to remote
            if !self.push_branch(&repo_path, branch_name) {
                warn!("Skipping PR creation due to push failure.");
                return false;
            }

            // Step 2: Create PR
            if let Some(pr_url) = self.create_pr(&repo_path, branch_name) {
                info!("PR created successfully: {}", pr_url);

                // Step 3: Merge PR with admin rights
                if self.merge_pr(&repo_path) {
                    info!("PR merged successfully.");
                } else {
                    warn!("Failed to merge PR for repository '{}'", self.reponame);
                }
            } else {
                warn!("Failed to create PR for repository '{}'", self.reponame);
            }
        }

        true
    }

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
                "Uncommitted changes found in '{}'. Staged: {}, Unstaged: {}",
                repo_path.display(),
                !staged_clean,
                !unstaged_clean
            );
            false
        }
    }

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
                        "Substring '{}' not found in file '{}', skipping.",
                        pattern,
                        full_path.display()
                    );
                    return None;
                }
                info!(
                    "Applying substring replacement '{}' -> '{}' in '{}'",
                    pattern, replacement, full_path.display()
                );
                content.replace(pattern, replacement)
            }
            Change::Regex(pattern, replacement) => {
                let regex = match Regex::new(pattern) {
                    Ok(re) => re,
                    Err(err) => {
                        error!(
                            "Failed to compile regex '{}' for file '{}': {}",
                            pattern, full_path.display(), err
                        );
                        return None;
                    }
                };
                if !regex.is_match(&content) {
                    debug!(
                        "Regex '{}' did not match in file '{}', skipping.",
                        pattern,
                        full_path.display()
                    );
                    return None;
                }
                info!(
                    "Applying regex replacement '{}' -> '{}' in '{}'",
                    pattern, replacement, full_path.display()
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

    fn generate_diff(&self, original: &str, updated: &str, buffer: usize) -> String {
        info!(
            "Generating diff with buffer size {} for changes",
            buffer
        );

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
                            debug!(
                                "Deleted line {}: {}",
                                change.old_index().unwrap() + 1,
                                change.to_string().trim_end()
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

    fn commit_changes(&self, repo_path: &Path, user_message: &str, branch_name: &str) {
        info!(
            "Attempting to commit changes in repository '{}' on branch '{}'",
            repo_path.display(),
            branch_name
        );

        // Ensure we're on the correct branch before committing
        if !self.create_or_switch_branch(repo_path, branch_name) {
            warn!(
                "Skipping commit in '{}' due to branch switching failure.",
                repo_path.display()
            );
            return;
        }

        // Stage all changes
        if !self.stage_files(repo_path) {
            warn!("Skipping commit in '{}' due to failure in staging files.", repo_path.display());
            return;
        }

        // Verify that we have staged changes and no uncommitted changes
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

    fn create_or_switch_branch(&self, repo_path: &Path, branch_name: &str) -> bool {
        info!(
            "Ensuring repository '{}' is on the correct branch '{}'",
            repo_path.display(),
            branch_name
        );

        // Ensure the repo is on a valid branch
        let head_output = Command::new("git")
            .current_dir(repo_path)
            .args(["symbolic-ref", "--short", "HEAD"])
            .output();

        let _current_branch = match head_output {
            Ok(output) if output.status.success() => {
                let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
                debug!(
                    "Current branch in '{}': '{}'",
                    repo_path.display(),
                    branch
                );
                branch
            }
            _ => {
                warn!(
                    "Skipping repository '{}': Not on a valid branch or in detached HEAD state.",
                    repo_path.display()
                );
                return false;
            }
        };

        // Check if the target branch already exists
        let branch_exists = Command::new("git")
            .current_dir(repo_path)
            .args(["rev-parse", "--verify", branch_name])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !branch_exists {
            info!(
                "Creating and switching to new branch '{}' in '{}'",
                branch_name,
                repo_path.display()
            );
            let status = Command::new("git")
                .current_dir(repo_path)
                .args(["checkout", "-b", branch_name])
                .status();

            if let Err(err) = status {
                error!(
                    "Error creating branch '{}' in '{}': {}",
                    branch_name,
                    repo_path.display(),
                    err
                );
                return false;
            }
        } else {
            info!(
                "Switching to existing branch '{}' in '{}'",
                branch_name,
                repo_path.display()
            );
            let status = Command::new("git")
                .current_dir(repo_path)
                .args(["checkout", branch_name])
                .status();

            if let Err(err) = status {
                error!(
                    "Error switching to branch '{}' in '{}': {}",
                    branch_name,
                    repo_path.display(),
                    err
                );
                return false;
            }
        }

        info!(
            "Successfully switched to branch '{}' in '{}'",
            branch_name,
            repo_path.display()
        );

        true
    }

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

    fn commit(&self, repo_path: &Path, user_message: &str) {
        info!(
            "Attempting to commit changes in repository '{}' with message '{}'",
            repo_path.display(),
            user_message
        );

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

    fn push_branch(&self, repo_path: &Path, branch_name: &str) -> bool {
        info!("Pushing branch '{}' to remote in '{}'", branch_name, repo_path.display());

        let status = Command::new("git")
            .current_dir(repo_path)
            .args(["push", "--set-upstream", "origin", branch_name])
            .status();

        if let Err(err) = status {
            error!("Failed to push branch '{}' in '{}': {}", branch_name, repo_path.display(), err);
            return false;
        }

        info!("Successfully pushed branch '{}' in '{}'", branch_name, repo_path.display());
        true
    }

    fn create_pr(&self, repo_path: &Path, branch_name: &str) -> Option<String> {
        info!("Creating pull request for '{}' on branch '{}'", repo_path.display(), branch_name);

        let pr_output = Command::new("gh")
            .current_dir(repo_path)
            .args([
                "pr", "create",
                "--title", "SLAM: Automated Update",
                "--body", "Automated update generated by SLAM.\ndocs: https://github.com/scottidler/slam/blob/main/README.md",
                "--base", "main",
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

    fn merge_pr(&self, repo_path: &Path) -> bool {
        info!("Merging pull request with admin rights in '{}'", repo_path.display());

        let merge_output = Command::new("gh")
            .current_dir(repo_path)
            .args(["pr", "merge", "--admin", "--squash", "--delete-branch"])
            .status();

        if let Err(err) = merge_output {
            error!("Failed to merge PR: {}", err);
            return false;
        }

        info!("PR successfully merged and branch deleted.");
        true
    }
}

fn get_change(cli: &SlamCli) -> Option<Change> {
    debug!("Parsing change arguments from CLI");

    if let Some(sub_args) = &cli.sub {
        info!(
            "Using substring replacement: '{}' -> '{}'",
            sub_args[0], sub_args[1]
        );
        return Some(Change::Sub(sub_args[0].clone(), sub_args[1].clone()));
    }

    if let Some(regex_args) = &cli.regex {
        info!(
            "Using regex replacement: '{}' -> '{}'",
            regex_args[0], regex_args[1]
        );
        return Some(Change::Regex(regex_args[0].clone(), regex_args[1].clone()));
    }

    debug!("No change argument provided");
    None
}

fn create_repo(repo: &Path, root: &Path, change: &Option<Change>, files_pattern: &Option<String>) -> Option<Repo> {
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
        debug!(
            "Searching for files matching '{}' in repository '{}'",
            pattern, repo.display()
        );

        match find_files_in_repo(repo, pattern) {
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
    }

    if files_pattern.is_some() && files.is_empty() {
        debug!(
            "Skipping repository '{}' as no files matched the pattern '{}'",
            repo.display(),
            files_pattern.as_deref().unwrap_or("None")
        );
        return None;
    }

    info!(
        "Repository '{}' added with {} matching files.",
        relative_repo,
        files.len()
    );

    Some(Repo {
        reponame: relative_repo,
        change: change.clone(),
        files,
    })
}

fn main() -> Result<()> {
    // Set default log level to INFO if RUST_LOG is not set
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    env_logger::init();
    info!("Starting SLAM");

    let cli = SlamCli::parse();
    debug!("Parsed CLI arguments: {:?}", cli);

    let change = get_change(&cli);
    debug!("Parsed change argument: {:?}", change);

    let root = env::current_dir().expect("Failed to get current directory");
    info!("Starting search in root directory: {}", root.display());

    let repos = find_git_repositories(&root)?;
    info!("Found {} repositories", repos.len());

    let mut repo_list = Vec::new();

    for repo in repos {
        if let Some(repo_entry) = create_repo(&repo, &root, &change, &cli.files) {
            if cli.repos.is_empty() || cli.repos.iter().any(|arg| repo_entry.reponame.contains(arg)) {
                repo_list.push(repo_entry);
            }
        }
    }

    info!("Processing {} repositories", repo_list.len());

    if let Some(change) = &change {
        for repo in &repo_list {
            match change {
                Change::Sub(pattern, replacement) | Change::Regex(pattern, replacement) => {
                    if repo.output(&root, cli.commit.as_deref(), cli.buffer, &cli.branch) {
                        info!(
                            "Applied pattern '{}' with replacement '{}' in repo '{}'.",
                            pattern, replacement, repo.reponame
                        );
                    }
                }
            }
        }
    } else if cli.files.is_some() {
        for repo in &repo_list {
            if !repo.files.is_empty() {
                info!("Repo: {}", repo.reponame);
                for file in &repo.files {
                    debug!("  File: {}", file);
                }
            }
        }
    } else {
        for repo in &repo_list {
            info!("Repo: {}", repo.reponame);
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

fn find_git_repositories(root: &Path) -> Result<Vec<PathBuf>> {
    info!("Searching for git repositories in '{}'", root.display());

    let mut repos = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() && path.join(".git").is_dir() {
            info!("Found git repository: '{}'", path.display());
            repos.push(path);
        } else if path.is_dir() {
            debug!("Recursively searching in '{}'", path.display());
            let nested_repos = find_git_repositories(&path)?;
            repos.extend(nested_repos);
        }
    }

    repos.sort();
    info!("Total repositories found: {}", repos.len());

    Ok(repos)
}

fn find_files_in_repo(repo: &Path, pattern: &str) -> Result<Vec<PathBuf>> {
    info!(
        "Searching for files matching '{}' in repository '{}'",
        pattern, repo.display()
    );

    let mut matches = Vec::new();
    let search_pattern = repo.join(pattern).to_string_lossy().to_string();

    debug!("Using search pattern: '{}'", search_pattern);

    for entry in glob(&search_pattern)? {
        match entry {
            Ok(path) => {
                let relative_path = path.strip_prefix(repo)?.to_path_buf();
                debug!("Matched file: '{}'", relative_path.display());
                matches.push(relative_path);
            }
            Err(e) => {
                warn!("Failed to match file with pattern '{}': {}", search_pattern, e);
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
