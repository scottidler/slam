// src/main.rs

use std::{
    env,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
use eyre::{eyre, Result};
use log::{debug, info, warn, LevelFilter};
use serde_json::{from_str, Value};
use env_logger::Target;
use rayon::prelude::*;


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

        #[arg(
            short = 'b',
            long,
            default_value_t = 1,
            help = "Number of context lines in the diff output"
        )]
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
            short = 'b',
            long,
            default_value_t = 1,
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

    let success = "✅";
    let failure = "❌";
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
                    success, tool_bin, version_line.trim()
                ));
            }
            _ => {
                output_string.push_str(&format!(
                    "{} {} (missing or broken)\n",
                    failure, tool_bin
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

    fs::create_dir_all("/var/log/messages/slam")?;
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/var/log/messages/slam/slam.log")
        .expect("Failed to open /var/log/messages/slam/slam.log");

    env_logger::Builder::new()
        .filter_level(LevelFilter::Info)
        .target(Target::Pipe(Box::new(log_file)))
        .format(|buf, record| {
            writeln!(
                buf,
                "[{level}] {timestamp} - {msg}",
                level = record.level(),
                timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                msg = record.args()
            )
        })
        .init();

    let mut cmd = SlamCli::command();
    cmd = cmd.after_help(get_cli_tool_status());

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
            change_id,
            org,
            approve,
            merge,
            admin_override,
            buffer,
            repos,
        } => {
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
    user_repo_specs: Vec<String>,
) -> Result<()> {
    // A) gather all repos in the org
    let repo_names = find_repos_in_org(&org)?;
    info!("Found {} repos in '{}'", repo_names.len(), org);

    // B) filter by user input.
    //    If you have a function that works on strings instead of Repos, do that:
    let filtered_names: Vec<_> = repo_names
        .into_iter()
        .filter(|full_name| {
            // your old "any(|uf| full_name.contains(uf))" logic,
            // or if user_repo_specs is empty => keep them all
            user_repo_specs.is_empty()
                || user_repo_specs.iter().any(|pat| full_name.contains(pat))
        })
        .collect();
    info!(
        "After user input filter, {} remain",
        filtered_names.len()
    );

    // C) In parallel, find who has an open PR on change_id.
    //    For each matching repo, we build a Repo struct with pr_number.
    let filtered_repos: Vec<Repo> = filtered_names
        .par_iter()
        .filter_map(|name| {
            let pr_number = get_pr_number_for_repo(name, &change_id);
            if pr_number == 0 {
                // no open PR => skip
                None
            } else {
                // build the real Repo
                Some(Repo::create_repo_from_remote_with_pr(name, &change_id, pr_number))
            }
        })
        .collect();

    info!(
        "{} repos actually have an open PR for branch '{}'",
        filtered_repos.len(),
        change_id
    );

    // D) For each final repo => show diffs, optionally approve/merge
    for filtred_repo in &filtered_repos {
        // show diffs
        show_repo_diff(filtred_repo, buffer);

        // if not approving => skip merge
        if !approve {
            info!("No --approve, skipping '{}'", filtred_repo.reponame);
            continue;
        }
        if !filtred_repo.approve_pr_remote() {
            warn!("Failed to approve PR for '{}', skipping merge", filtred_repo.reponame);
            continue;
        }

        // if not merging => done
        if !merge {
            info!("No --merge, skipping '{}'", filtred_repo.reponame);
            continue;
        }
        let merged = filtred_repo.merge_pr_remote(admin_override);
        if !merged {
            warn!("Failed to merge PR for '{}'", filtred_repo.reponame);
        } else {
            info!("Successfully merged {}", filtred_repo.reponame);
        }
    }

    Ok(())
}

fn show_repo_diff(repo: &Repo, buffer: usize) {
    // 1) fetch the patch
    let diff_text = match get_pr_diff(&repo.reponame, repo.pr_number) {
        Ok(txt) => txt,
        Err(e) => {
            warn!("Could not fetch PR diff for '{}': {}", repo.reponame, e);
            return;
        }
    };
    // 2) parse
    let file_patches = parse_unified_diff(&diff_text);
    if file_patches.is_empty() {
        return;
    }

    // 3) show diffs
    println!("Repo: {}", repo.reponame);
    for (filename, old_text, new_text) in file_patches {
        println!("  Modified file: {}", filename);
        let short_diff = repo.generate_diff(&old_text, &new_text, buffer);
        for line in short_diff.lines() {
            println!("    {}", line);
        }
    }
}

fn find_repos_in_org(org: &str) -> Result<Vec<String>> {
    // Use a high limit (e.g. 1000) to fetch all repos.
    let output = std::process::Command::new("gh")
        .args(&["repo", "list", org, "--limit", "1000", "--json", "name"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(eyre!("Failed to list repos in org '{}': {}", org, stderr.trim()));
    }

    let stdout_str = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = from_str(&stdout_str)?;

    let mut repo_names = Vec::new();
    if let Some(arr) = parsed.as_array() {
        for obj in arr {
            if let Some(name) = obj.get("name").and_then(|n| n.as_str()) {
                // Construct the full repo name as "org/name"
                repo_names.push(format!("{}/{}", org, name));
            }
        }
    }

    Ok(repo_names)
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

fn get_pr_number_for_repo(repo_name: &str, change_id: &str) -> u64 {
    use std::process::Command;

    let output = Command::new("gh")
        .args([
            "pr", "list",
            "--repo", repo_name,
            "--head", change_id,
            "--state", "open",
            "--json", "number",
            "--limit", "1",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() && !o.stdout.is_empty() => {
            // e.g. `[ { "number": 123 } ]`
            let stdout_str = String::from_utf8_lossy(&o.stdout);
            if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&stdout_str) {
                if let Some(arr) = json_val.as_array() {
                    if let Some(first) = arr.first() {
                        if let Some(num) = first.get("number").and_then(|v| v.as_u64()) {
                            return num;
                        }
                    }
                }
            }
            0
        }
        _ => 0,
    }
}

fn get_pr_diff(repo: &str, pr_number: u64) -> Result<String> {
    use std::process::Command;

    let output = Command::new("gh")
        .args(["pr", "diff", &pr_number.to_string(), "-R", repo, "--patch"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(eyre!("gh pr diff command failed: {}", stderr.trim()))
    } else {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

