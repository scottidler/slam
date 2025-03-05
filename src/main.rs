// src/main.rs

use std::{
    env,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{ArgGroup, Parser, Subcommand};
use eyre::Result;
use log::{debug, info, warn};
use serde_json::Value;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

mod repo;
use repo::{Change, Repo};

/// Returns a default "change ID" in the format `SLAM-YYYY-MM-DD`
fn default_change_id() -> String {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let change_id = format!("SLAM-{}", date);
    debug!("Generated default change_id: {}", change_id);
    change_id
}

/// Top-level CLI parser for the `slam` command
#[derive(Parser, Debug)]
#[command(
    name = "slam",
    about = "Finds and operates on repositories",
    version = built_info::GIT_DESCRIBE
)]
struct SlamCli {
    #[command(subcommand)]
    command: SlamCommand,
}

/// Subcommands: Create (local repos) or Approve (remote repos).
#[derive(Subcommand, Debug)]
enum SlamCommand {
    /// Create and commit changes in repositories (alias: alleyoop)
    #[command(alias = "alleyoop")]
    #[command(group = ArgGroup::new("change").required(false).args(["sub", "regex"]))]
    Create {
        #[arg(short = 'f', long, help = "Glob pattern to find files within each repository")]
        files: Option<String>,

        #[arg(
            short = 's',
            long,
            value_names = &["PTN", "REPL"],
            num_args = 2,
            help = "Substring and replacement (requires two arguments)"
        )]
        sub: Option<Vec<String>>,

        #[arg(
            short = 'r',
            long,
            value_names = &["PTN", "REPL"],
            num_args = 2,
            help = "Regex pattern and replacement (requires two arguments)"
        )]
        regex: Option<Vec<String>>,

        #[arg(
            short = 'x',
            long,
            help = "Change ID used to create branches and PRs (default: 'SLAM-<YYYY-MM-DD>')",
            default_value_t = default_change_id()
        )]
        change_id: String,

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
    },

    /// Approve and merge open PRs (alias: dunk)
    #[command(alias = "dunk")]
    Approve {
        #[arg(
            short = 'x',
            long,
            help = "Change ID used to find PRs to approve and merge (default: 'SLAM-<YYYY-MM-DD>')",
            default_value_t = default_change_id()
        )]
        change_id: String,

        #[arg(
            short = 'o',
            long,
            default_value = "tatari-tv",
            help = "GitHub organization to search for branches"
        )]
        org: String,

        #[arg(help = "Repository names to filter", value_name = "REPOS", default_value = "")]
        repos: Vec<String>,
    },
}

fn main() -> Result<()> {
    // Set default log level if not already set
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    info!("Starting SLAM");
    let cli = SlamCli::parse();
    debug!("Parsed CLI arguments: {:?}", cli);

    match cli.command {
        SlamCommand::Create {
            files,
            sub,
            regex,
            change_id,
            buffer,
            commit,
            repos,
        } => {
            process_create_command(files, sub, regex, change_id, buffer, commit, repos)?;
        }
        SlamCommand::Approve {
            change_id,
            org,
            repos,
        } => {
            process_approve_command(change_id, org, repos)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

/// Helper to build a `Change` from the CLI arguments, if any.
fn get_change(sub: &Option<Vec<String>>, regex: &Option<Vec<String>>) -> Option<Change> {
    if let Some(sub_args) = sub {
        info!(
            "Using substring replacement: '{}' -> '{}'",
            sub_args[0], sub_args[1]
        );
        Some(Change::Sub(sub_args[0].clone(), sub_args[1].clone()))
    } else if let Some(regex_args) = regex {
        info!(
            "Using regex replacement: '{}' -> '{}'",
            regex_args[0], regex_args[1]
        );
        Some(Change::Regex(regex_args[0].clone(), regex_args[1].clone()))
    } else {
        debug!("No change argument provided");
        None
    }
}

/// Handles the `slam create` logic: discover local repos on disk, build `Repo` objects,
/// filter them by user input, then optionally apply changes and commit/PR.
fn process_create_command(
    files: Option<String>,
    sub: Option<Vec<String>>,
    regex: Option<Vec<String>>,
    change_id: String,
    buffer: usize,
    commit: Option<String>,
    repos: Vec<String>,
) -> Result<()> {
    let change = get_change(&sub, &regex);

    let root = env::current_dir()?;
    info!("Starting search in root directory: {}", root.display());

    // Find local repos on disk
    let found_repos = find_git_repositories(&root)?;
    info!("Found {} local repositories on disk", found_repos.len());

    // Create `Repo` entries for each directory, potentially with no changes or files
    let mut repo_list = Vec::new();
    for repo_path in found_repos {
        if let Some(repo_obj) =
            Repo::create_repo_from_local(&repo_path, &root, &change, &files, &change_id)
        {
            // If the user specified `repos` (non-empty), filter by substring match
            if repos.is_empty() || repos.iter().any(|arg| repo_obj.reponame.contains(arg)) {
                repo_list.push(repo_obj);
            }
        }
    }

    info!("Processing {} repositories for changes", repo_list.len());

    // If we have a change (substring or regex), apply it; otherwise, just list repos/files
    if let Some(_ch) = &change {
        for r in &repo_list {
            let changes_made = r.output(&root, commit.as_deref(), buffer);
            if changes_made {
                debug!("Changes were applied in '{}'", r.reponame);
            }
        }
    } else if files.is_some() {
        // If user gave a files pattern, but no actual change, just show them matched files
        for r in &repo_list {
            if !r.files.is_empty() {
                info!("Repo: {}", r.reponame);
                for file in &r.files {
                    debug!("  Matched file: {}", file);
                }
            }
        }
    } else {
        // No pattern or changes => just list repositories
        for r in &repo_list {
            info!("Repo: {}", r.reponame);
        }
    }

    Ok(())
}

/// Handles the `slam approve` logic: discover remote repos in a given org that have a PR
/// matching `change_id`, filter by user input, then approve & merge.
fn process_approve_command(change_id: String, org: String, repos: Vec<String>) -> Result<()> {
    info!(
        "Approving and merging PRs in GitHub organization '{}' for branch '{}'",
        org, change_id
    );

    let discovered_repos = find_repos_in_org(&org, &change_id)?;
    info!(
        "Discovered {} repos in org '{}' that have an open PR for branch '{}'",
        discovered_repos.len(),
        org,
        change_id
    );

    // Filter if user specified repos
    let filtered_repos = if repos.is_empty() {
        discovered_repos
    } else {
        discovered_repos
            .into_iter()
            .filter(|r| repos.iter().any(|user_arg| r.contains(user_arg)))
            .collect()
    };

    info!("Filtered down to {} repositories to approve/merge", filtered_repos.len());

    // For each matching remote repo, build a Repo object, approve, then merge
    for remote_name in filtered_repos {
        let repo_obj = Repo::create_repo_from_remote(&remote_name, &change_id);

        // Approve
        if !repo_obj.approve_pr_remote() {
            warn!("Failed to approve PR for '{}', skipping merge.", remote_name);
            continue;
        }
        // Merge
        if !repo_obj.merge_pr_remote() {
            warn!("Failed to merge PR for '{}'.", remote_name);
        } else {
            info!("Successfully merged branch '{}' in '{}'", change_id, remote_name);
        }
    }

    Ok(())
}

/// Uses `gh repo list` to find all repos in a GitHub organization, then checks if each repo
/// has an open PR whose head is `change_id`. Returns a list of all matching `org/repo` names.
fn find_repos_in_org(org: &str, change_id: &str) -> Result<Vec<String>> {
    // `gh repo list <org> --limit 100 --json name`
    let output = Command::new("gh")
        .args(["repo", "list", org, "--limit", "100", "--json", "name"])
        .output()?;

    if !output.status.success() {
        return Err(eyre::eyre!(
            "Failed to list repos in org '{}': {}",
            org,
            String::from_utf8_lossy(&output.stderr)
        ));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(&stdout_str)?;

    let mut matching_repos = Vec::new();

    // The JSON from `gh repo list --json name` is an array like: [ { "name": "repo1" }, { "name": "repo2" }, ... ]
    if let Some(arr) = parsed.as_array() {
        for obj in arr {
            if let Some(repo_name) = obj.get("name").and_then(|n| n.as_str()) {
                let full_repo = format!("{}/{}", org, repo_name);

                // Now check if there's an open PR whose head is our `change_id`
                let pr_list = Command::new("gh")
                    .args([
                        "pr",
                        "list",
                        "--repo",
                        &full_repo,
                        "--head",
                        change_id,
                        "--state",
                        "open",
                        "--json",
                        "url",
                    ])
                    .output()?;

                // If stdout is non-empty and not "[]", we found an open PR on that branch
                if pr_list.status.success() && !pr_list.stdout.is_empty() {
                    let body = String::from_utf8_lossy(&pr_list.stdout).trim().to_string();
                    if body != "[]" {
                        matching_repos.push(full_repo);
                    }
                }
            }
        }
    }

    Ok(matching_repos)
}

/// Recursively looks for directories containing a `.git` folder.
/// Returns a list of local repo paths within the given `root`.
fn find_git_repositories(root: &Path) -> Result<Vec<PathBuf>> {
    info!("Searching for git repositories in '{}'", root.display());

    let mut repos = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();

        // If we find a `.git` folder, treat it as a repo
        if path.is_dir() && path.join(".git").is_dir() {
            info!("Found git repository: '{}'", path.display());
            repos.push(path);
        } else if path.is_dir() {
            // Recurse into subdirectories
            let nested_repos = find_git_repositories(&path)?;
            repos.extend(nested_repos);
        }
    }

    repos.sort();
    info!("Total local repositories found: {}", repos.len());
    Ok(repos)
}
