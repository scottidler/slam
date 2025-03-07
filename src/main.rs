use std::{
    env,
    fs,
    io,
    io::Write,
    path::Path,
};

use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
use eyre::Result;
use log::{info, debug, warn};
use rayon::prelude::*;
use std::process::Command;
use itertools::Itertools;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

mod git;
mod diff;
mod repo;

use repo::{Change, Repo};

fn default_change_id() -> String {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let change_id = format!("SLAM-{}", date);
    debug!("Generated default change_id: {}", change_id);
    change_id
}

fn get_cli_tool_status() -> String {
    let success = "✅";
    let failure = "❌";
    let tools = [("git", &["--version"]), ("gh", &["--version"])];

    let mut output_string = String::new();
    output_string.push('\n');
    for (tool, args) in &tools {
        match Command::new(tool).args(args.iter()).output() {
            Ok(cmd_output) if cmd_output.status.success() => {
                let stdout = String::from_utf8_lossy(&cmd_output.stdout);
                let version = stdout.lines().next().unwrap_or("Unknown Version");
                output_string.push_str(&format!("{} {}\n", success, version.trim()));
            }
            _ => {
                output_string.push_str(&format!("{} {} (missing or broken)\n", failure, tool));
            }
        }
    }
    let log_status = {
        let log_dir = Path::new("/var/log/messages/slam");
        if log_dir.exists() && log_dir.is_dir() {
            match fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("slam.log"))
            {
                Ok(_) => format!("{} {} (writable)\n", success, log_dir.display()),
                Err(_) => format!("{} {} (!writable)\n", failure, log_dir.display()),
            }
        } else {
            format!("{} {} (not found)\n", failure, log_dir.display())
        }
    };
    output_string.push_str(&log_status);
    output_string.push('\n');
    output_string
}

#[derive(Parser, Debug)]
#[command(
    name = "slam",
    about = "HPA: horizontal PR autoscaler",
    version = built_info::GIT_DESCRIBE
)]
struct SlamCli {
    #[command(subcommand)]
    command: SlamCommand,
}

#[derive(Subcommand, Debug)]
enum SlamCommand {
    #[command(alias = "alleyoop")]
    #[command(group = ArgGroup::new("change").required(false).args(["delete", "sub", "regex"]))]
    Create {
        #[arg(short = 'f', long, help = "Glob pattern to find files within each repository")]
        files: Option<String>,

        #[arg(
            short = 'd',
            long,
            help = "Match and delete whole files"
        )]
        delete: bool,

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

fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    let log_dir = Path::new("/var/log/messages/slam");
    let log_file_path = log_dir.join("slam.log");

    let log_writer: Box<dyn Write + Send> = if log_dir.exists() && log_dir.is_dir() {
        match fs::OpenOptions::new().create(true).append(true).open(&log_file_path) {
            Ok(file) => Box::new(file),
            Err(err) => {
                eprintln!(
                    "Log directory exists but is not writable: {}. Falling back to stdout.",
                    err
                );
                Box::new(io::stdout())
            }
        }
    } else {
        eprintln!(
            "Log directory {} not found. Falling back to stdout.",
            log_dir.display()
        );
        Box::new(io::stdout())
    };

    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .target(env_logger::Target::Pipe(log_writer))
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

    match cli.command {
        SlamCommand::Create {
            files,
            delete,
            sub,
            regex,
            change_id,
            buffer,
            commit,
            repos,
        } => {
            let change = Change::from_args(delete, &sub, &regex);
            process_create_command(files, change, change_id, buffer, commit, repos)?;
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
            process_review_command(org, change_id, approve, merge, admin_override, buffer, repos)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

fn process_create_command(
    files: Option<String>,
    change: Option<Change>,
    change_id: String,
    buffer: usize,
    commit: Option<String>,
    user_repo_specs: Vec<String>,
) -> eyre::Result<()> {
    let root = std::env::current_dir()?;
    let discovered_paths = git::find_git_repositories(&root)?;

    let mut discovered_repos = Vec::new();
    for path in discovered_paths {
        if let Some(repo) = Repo::create_repo_from_local(&path, &root, &change, &files, &change_id) {
            discovered_repos.push(repo);
        }
    }

    let filtered_repos: Vec<_> = discovered_repos
        .into_iter()
        .filter(|repo| {
            user_repo_specs.is_empty()
                || user_repo_specs.iter().any(|spec| repo.reponame.contains(spec))
        })
        .sorted_by(|a, b| a.reponame.cmp(&b.reponame))
        .collect();

    for repo in &filtered_repos {
        repo.show_create_diff(&root, buffer, commit.is_some());
        if let Some(commit_msg) = commit.as_deref() {
            git::stage_files(&root)?;
            if !git::is_working_tree_clean(&root) {
                git::commit_changes(&root, commit_msg)?;
                git::push_branch(&root, &change_id)?;
            }
        }
    }

    Ok(())
}

fn process_review_command(
    org: String,
    change_id: String,
    approve: bool,
    merge: bool,
    admin_override: bool,
    buffer: usize,
    user_repo_specs: Vec<String>,
) -> eyre::Result<()> {
    let repo_names = git::find_repos_in_org(&org)?;
    log::info!("Found {} repos in '{}'", repo_names.len(), org);

    let filtered_names: Vec<_> = repo_names
        .into_iter()
        .filter(|full_name| {
            user_repo_specs.is_empty() || user_repo_specs.iter().any(|pat| full_name.contains(pat))
        })
        .collect();
    log::info!("After user input filter, {} remain", filtered_names.len());

    let mut filtered_repos: Vec<Repo> = filtered_names
        .par_iter()
        .filter_map(|repo_name| {
            match git::get_pr_number_for_repo(repo_name, &change_id) {
                Ok(pr_number) if pr_number > 0 => {
                    info!("Found PR #{} for repo '{}'", pr_number, repo_name);
                    Some(Repo::create_repo_from_remote_with_pr(repo_name, &change_id, pr_number))
                }
                Ok(_) => {
                    debug!("No open PR found for '{}'", repo_name);
                    None
                }
                Err(err) => {
                    warn!("Error fetching PR for '{}': {}", repo_name, err);
                    None
                }
            }
        })
        .collect();

    filtered_repos.sort_by(|a, b| a.reponame.cmp(&b.reponame));

    if filtered_repos.is_empty() {
        warn!("No repositories found with an open PR for '{}'", change_id);
        return Ok(());
    }

    info!(
        "{} repositories have an open PR for '{}'",
        filtered_repos.len(),
        change_id
    );

    let mut processed_count = 0;
    for repo in &filtered_repos {
        repo.show_review_diff(buffer);

        if !approve {
            info!("No --approve flag, skipping review actions for '{}'", repo.reponame);
            continue;
        }

        if let Err(e) = git::approve_pr(&repo.reponame, &repo.change_id) {
            warn!("Failed to approve PR for '{}': {}. Skipping merge.", repo.reponame, e);
            continue;
        }

        if !merge {
            info!("No --merge flag, skipping merge for '{}'", repo.reponame);
            continue;
        }

        match git::merge_pr(&repo.reponame, &repo.change_id, admin_override) {
            Ok(_) => {
                info!("Successfully merged '{}'", repo.reponame);
                processed_count += 1;
            }
            Err(e) => {
                warn!("Failed to merge PR for '{}': {}", repo.reponame, e);
            }
        }
    }

    info!(
        "Review completed. PRs Approved: {}, PRs Merged: {}",
        if approve { processed_count } else { 0 },
        if merge { processed_count } else { 0 }
    );
    Ok(())
}
