// src/main.rs

use std::{
    env,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
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

/// Subcommands: Create (local repos) or Review (remote repos).
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

    /// Review and merge open PRs (alias: dunk)
    Review {
        #[arg(
            short = 'x',
            long,
            help = "Change ID used to find PRs (default: 'SLAM-<YYYY-MM-DD>')",
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

        #[arg(
            long = "approve",
            help = "Add an approving review to each PR"
        )]
        approve: bool,

        #[arg(
            long = "merge",
            help = "Attempt to merge the PR after approving (if checks pass)"
        )]
        merge: bool,

        #[arg(
            long = "admin-override",
            help = "Pass `--admin` to `gh pr merge` to bypass failing checks"
        )]
        admin_override: bool,

        #[arg(
            short = 'B',
            long,
            default_value_t = 3,
            help = "Number of context lines in the diff output"
        )]
        buffer: usize,

        #[arg(help = "Repository names to filter", value_name = "REPOS", default_value = "")]
        repos: Vec<String>,
    },
}

// 1) A helper function that checks `git` and `gh` availability/versions.
//    Returns a multi-line String that will be appended to help text.
fn get_cli_tool_status() -> String {
    use std::process::Command;

    let ok_mark = "✅";
    let fail_mark = "❌";
    let tools = [
        ("git", &["--version"]),
        ("gh", &["--version"]),
    ];

    let mut output_string = String::new();
    output_string.push('\n');
    for (tool_bin, args) in &tools {
        match Command::new(tool_bin).args(args.iter()).output() {
            Ok(cmd_output) if cmd_output.status.success() => {
                let stdout = String::from_utf8_lossy(&cmd_output.stdout);
                let version_line = stdout.lines().next().unwrap_or("Unknown Version");
                output_string.push_str(&format!(
                    "{} {} {}\n",
                    ok_mark, tool_bin, version_line.trim()
                ));
            }
            _ => {
                output_string.push_str(&format!(
                    "{} {} (missing or broken)\n",
                    fail_mark, tool_bin
                ));
            }
        }
    }
    output_string.push('\n');
    output_string
}

fn main() -> Result<()> {
    // Set default log level if not already set
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();
    // Build the Clap command from your parser-deriving struct (whatever you call it).
    let mut cmd = SlamCli::command();

    // Insert dynamic after_help content here. Clap will automatically show it below
    // the main help text when the user does `--help`.
    cmd = cmd.after_help(get_cli_tool_status());

    // If you have a Parser-derived struct named `SlamCli`, parse it like this:
    let cli = SlamCli::from_arg_matches(&cmd.get_matches())?;
    info!("Starting SLAM");
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
        SlamCommand::Review {
            //change_id,
            //org,
            //repos,
            change_id,
            org,
            approve,
            merge,
            admin_override,
            buffer,
            repos,
        } => {
            //process_review_command(change_id, org, repos)?;
            process_review_command(change_id, org, approve, merge, admin_override, buffer, repos)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

/// Constructs a `Change` from substring or regex arguments, if any.
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

/// Filters a list of Repo objects by user-provided partial name matches.
/// If `user_specs` is empty, all repos pass; otherwise, only keep repos
/// whose name contains at least one of the user filters.
fn filter_repo_objects_by_user_input(all_repos: Vec<Repo>, user_specs: &[String]) -> Vec<Repo> {
    if user_specs.is_empty() {
        return all_repos;
    }

    all_repos
        .into_iter()
        .filter(|repo| {
            user_specs.iter().any(|user_filter| repo.reponame.contains(user_filter))
        })
        .collect()
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
    user_repo_specs: Vec<String>,
) -> Result<()> {
    let change = get_change(&sub, &regex);

    let root = env::current_dir()?;
    info!("Starting search in root directory: {}", root.display());

    // 1. Discover local `.git` repos from the filesystem
    let discovered_paths = find_git_repositories(&root)?;
    info!("Discovered {} local .git repos", discovered_paths.len());

    // 2. Construct `Repo` objects
    let mut discovered_repos = Vec::new();
    for path in discovered_paths {
        if let Some(r) = Repo::create_repo_from_local(&path, &root, &change, &files, &change_id) {
            discovered_repos.push(r);
        }
    }
    info!(
        "Constructed {} Repo objects after file matching",
        discovered_repos.len()
    );

    // 3. Filter by user-supplied partial names (if any)
    let filtered_repos = filter_repo_objects_by_user_input(discovered_repos, &user_repo_specs);
    info!("Processing {} repositories for changes", filtered_repos.len());

    // 4. Apply changes or simply list the repos/files
    if change.is_some() {
        for filtered_repo in &filtered_repos {
            let changes_made = filtered_repo.output(&root, commit.as_deref(), buffer);
            if changes_made {
                debug!("Changes applied in '{}'", filtered_repo.reponame);
            }
        }
    } else if files.is_some() {
        // If user gave a files pattern but no actual change, just show matched files
        for filtered_repo in &filtered_repos {
            if !filtered_repo.files.is_empty() {
                info!("Repo: {}", filtered_repo.reponame);
                for file in &filtered_repo.files {
                    debug!("  Matched file: {}", file);
                }
            }
        }
    } else {
        // No pattern or changes => just list repositories
        for filtered_repo in &filtered_repos {
            info!("Repo: {}", filtered_repo.reponame);
        }
    }

    Ok(())
}

fn process_review_command(
    change_id: String,
    org: String,
    approve: bool,
    merge: bool,
    admin_override: bool,
    buffer: usize,
    user_repo_specs: Vec<String>
) -> Result<()> {
    info!(
        "Showing diffs in GitHub organization '{}' for branch '{}'. Approve={}, Merge={}, Admin={}",
        org, change_id, approve, merge, admin_override
    );

    // 1. Discover remote repos that have an open PR with the given head branch
    let discovered_names = find_repos_in_org(&org, &change_id)?;
    info!(
        "Discovered {} remote repos with open PR branch '{}'",
        discovered_names.len(),
        change_id
    );

    // 2. Convert each "org/repo" string into a `Repo` object
    let discovered_repos: Vec<Repo> = discovered_names
        .into_iter()
        .map(|name| Repo::create_repo_from_remote(&name, &change_id))
        .collect();

    // 3. Filter the Repo objects by user-supplied partial names
    let filtered_repos = filter_repo_objects_by_user_input(discovered_repos, &user_repo_specs);
    info!(
        "Filtered down to {} repositories for approval/merge",
        filtered_repos.len()
    );

    for filtered_repo in filtered_repos {
        // a) fetch PR number
        let pr_number = match get_pr_number_for_branch(&filtered_repo.reponame, &change_id) {
            Some(num) => num,
            None => { /* skip, no open PR found */ continue; }
        };

        // b) fetch & parse the diff
        let diff_text = match get_pr_diff(&filtered_repo.reponame, pr_number) {
            Ok(txt) => txt,
            Err(_e) => { /* skip, can't fetch diff */ continue; }
        };
        let file_patches = parse_unified_diff(&diff_text);

        // c) show diffs
        if !file_patches.is_empty() {
            println!("Repo: {}", filtered_repo.reponame);
            for (filename, old_text, new_text) in file_patches {
                println!("  Modified file: {}", filename);
                // Need `pub fn generate_diff` in `repo.rs`
                let short_diff = filtered_repo.generate_diff(&old_text, &new_text, buffer);
                for line in short_diff.lines() {
                    println!("    {}", line);
                }
            }
        }

        // d) Approve only if user wants
        if approve {
            if !filtered_repo.approve_pr_remote() {
                warn!(
                    "Failed to approve PR for '{}', skipping merge.",
                    filtered_repo.reponame
                );
                continue;
            }
        } else {
            // If user isn't approving, skip merging too
            info!("User did not request --approve, skipping actual PR approval/merge.");
            continue;
        }

        // e) Merge only if user wants
        if merge {
            let merged = filtered_repo.merge_pr_remote(admin_override);
            if !merged {
                warn!("Failed to merge PR for '{}'.", filtered_repo.reponame);
            } else {
                info!("Successfully merged branch '{}' in '{}'", change_id, filtered_repo.reponame);
            }
        } else {
            info!("User did not request --merge, skipping merging step.");
        }
    }

    Ok(())
}

/*
/// Handles the `slam review` logic: discover remote repos in a given org that have a PR
/// matching `change_id`, filter by user input, then approve & merge.
fn process_review_command(change_id: String, org: String, user_repo_specs: Vec<String>) -> Result<()> {
    info!(
        "Approving/merging PRs in GitHub organization '{}' for branch '{}'",
        org, change_id
    );

    // 1. Discover remote repos that have an open PR with the given head branch
    let discovered_names = find_repos_in_org(&org, &change_id)?;
    info!(
        "Discovered {} remote repos with open PR branch '{}'",
        discovered_names.len(),
        change_id
    );

    // 2. Convert each "org/repo" string into a `Repo` object
    let discovered_repos: Vec<Repo> = discovered_names
        .into_iter()
        .map(|name| Repo::create_repo_from_remote(&name, &change_id))
        .collect();

    // 3. Filter the Repo objects by user-supplied partial names
    let filtered_repos = filter_repo_objects_by_user_input(discovered_repos, &user_repo_specs);
    info!(
        "Filtered down to {} repositories for approval/merge",
        filtered_repos.len()
    );

    // 4. Approve & merge each matching repo
    for filtered_repo in filtered_repos {
        if !filtered_repo.approve_pr_remote() {
            warn!("Failed to approve PR for '{}', skipping merge.", filtered_repo.reponame);
            continue;
        }
        if !filtered_repo.merge_pr_remote() {
            warn!("Failed to merge PR for '{}'.", filtered_repo.reponame);
        } else {
            info!("Successfully merged branch '{}' in '{}'", change_id, filtered_repo.reponame);
        }
    }

    Ok(())
}
*/

/// Uses `gh repo list` to find all repos in a GitHub organization, then checks if each repo
/// has an open PR whose head is `change_id`. Returns a list of all matching `org/repo` names.
fn find_repos_in_org(org: &str, change_id: &str) -> Result<Vec<String>> {
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

    // Example JSON from `gh repo list --json name`:
    // [ { "name": "repo1" }, { "name": "repo2" }, ... ]
    if let Some(array) = parsed.as_array() {
        for obj in array {
            if let Some(repo_name) = obj.get("name").and_then(|n| n.as_str()) {
                let full_repo = format!("{}/{}", org, repo_name);

                // Check if there's an open PR whose head is our `change_id`
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

                // If stdout is non-empty and != "[]", we have an open PR on that branch
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
            let nested = find_git_repositories(&path)?;
            repos.extend(nested);
        }
    }

    repos.sort();
    info!("Total local repositories found: {}", repos.len());
    Ok(repos)
}






/// Holds the before/after text for a single modified file.
#[derive(Debug)]
struct UnifiedFilePatch {
    filename: String,
    old_content: Vec<String>,
    new_content: Vec<String>,
}

/// Parse a unified diff text into a list of (filename, old_text, new_text).
fn parse_unified_diff(diff_text: &str) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    let mut current_file: Option<UnifiedFilePatch> = None;

    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            if let Some(file) = current_file.take() {
                // Push the previously accumulated file
                result.push((
                    file.filename,
                    file.old_content.join("\n"),
                    file.new_content.join("\n"),
                ));
            }
            current_file = Some(UnifiedFilePatch {
                filename: String::new(),
                old_content: Vec::new(),
                new_content: Vec::new(),
            });
            continue;
        }

        if line.starts_with("+++ b/") {
            if let Some(file) = current_file.as_mut() {
                file.filename = line.trim_start_matches("+++ b/").to_string();
            }
            continue;
        }

        if let Some(file) = current_file.as_mut() {
            if line.starts_with('-') && !line.starts_with("---") {
                file.old_content.push(line[1..].to_string());
            } else if line.starts_with('+') && !line.starts_with("+++") {
                file.new_content.push(line[1..].to_string());
            } else if line.starts_with(' ') {
                file.old_content.push(line[1..].to_string());
                file.new_content.push(line[1..].to_string());
            }
            // We ignore lines starting with "@@", "index", etc.
        }
    }

    // Don’t forget to push the last accumulated file
    if let Some(file) = current_file {
        if !file.filename.is_empty() {
            result.push((
                file.filename,
                file.old_content.join("\n"),
                file.new_content.join("\n"),
            ));
        }
    }

    result
}

fn get_pr_number_for_branch(repo: &str, branch: &str) -> Option<u64> {
    use std::process::Command;

    let output = Command::new("gh")
        .args([
            "pr", "list",
            "--repo", repo,
            "--head", branch,
            "--state", "open",
            "--json", "number",
            "--limit", "1",
        ])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let v: serde_json::Value = serde_json::from_str(&stdout_str).ok()?;
    let arr = v.as_array()?;
    if let Some(obj) = arr.first() {
        obj.get("number").and_then(|n| n.as_u64())
    } else {
        None
    }
}

fn get_pr_diff(repo: &str, pr_number: u64) -> eyre::Result<String> {
    use std::process::Command;

    let output = Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string(), "-R", repo, "--patch"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(eyre::eyre!("gh pr diff command failed: {}", stderr.trim()))
    } else {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

