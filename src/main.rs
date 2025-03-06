use std::{
    env,
    fs,
    io::Write,
};

use clap::{ArgGroup, CommandFactory, FromArgMatches, Parser, Subcommand};
use eyre::{Result};
use log::{debug, info, LevelFilter};
use env_logger::Target;
use rayon::prelude::*;

mod built_info {
    include!(concat!(env!("OUT_DIR"), "/git_describe.rs"));
}

mod repo;
mod git;

use repo::{Change, Repo};

/// Returns a default "change ID" in the format `SLAM-YYYY-MM-DD`
fn default_change_id() -> String {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let change_id = format!("SLAM-{}", date);
    debug!("Generated default change_id: {}", change_id);
    change_id
}

use std::process::Command;

fn get_cli_tool_status() -> String {
    let success = "✅";
    let failure = "❌";
    let tools = [("git", &["--version"]), ("gh", &["--version"])];

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

fn main() -> Result<()> {
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }

    fs::create_dir_all("/var/log/messages/slam")?;
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("/var/log/messages/slam/slam.log")
        .expect("Failed to open log file");

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
            process_review_command(org, change_id, approve, merge, admin_override, buffer, repos)?;
        }
    }

    info!("SLAM execution complete.");
    Ok(())
}

fn process_create_command(
    files: Option<String>,
    sub: Option<Vec<String>>,
    regex: Option<Vec<String>>,
    change_id: String,
    buffer: usize,
    commit: Option<String>,
    user_repo_specs: Vec<String>,
) -> Result<()> {
    let change = Change::from_args(&sub, &regex);
    let root = env::current_dir()?;
    let discovered_paths = git::find_git_repositories(&root)?;

    let mut discovered_repos = Vec::new();
    for path in discovered_paths {
        if let Some(repo) = Repo::create_repo_from_local(&path, &root, &change, &files, &change_id) {
            discovered_repos.push(repo);
        }
    }

    let filtered_repos = discovered_repos
        .into_iter()
        .filter(|repo| user_repo_specs.is_empty() || user_repo_specs.iter().any(|spec| repo.reponame.contains(spec)))
        .collect::<Vec<_>>();

    for repo in &filtered_repos {
        if let Some(commit_msg) = commit.as_deref() {
            git::stage_files(&root)?;
            if !git::is_working_tree_clean(&root) {
                git::commit_changes(&root, commit_msg)?;
                git::push_branch(&root, &change_id)?;
            }
        }
        repo.output(&root, commit.as_deref(), buffer);
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
) -> Result<()> {
    let repo_names = git::find_repos_in_org(&org)?;
    let filtered_names: Vec<_> = repo_names
        .into_iter()
        .filter(|full_name| user_repo_specs.is_empty() || user_repo_specs.iter().any(|pat| full_name.contains(pat)))
        .collect();

    let filtered_repos: Vec<Repo> = filtered_names
        .par_iter()
        .filter_map(|name| git::get_pr_number_for_repo(name, &change_id).ok().map(|pr| Repo::create_repo_from_remote_with_pr(name, &change_id, pr)))
        .collect();

    for repo in &filtered_repos {
        let diff = git::get_pr_diff(&repo.reponame, repo.pr_number)?;
        let formatted_diff = repo.generate_diff(&diff, &diff, buffer);
        println!("Repo: {}", repo.reponame);
        println!("{}", formatted_diff);

        if approve {
            repo.approve_pr_remote();
        }
        if merge {
            repo.merge_pr_remote(admin_override);
        }
    }

    Ok(())
}
