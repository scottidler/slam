use std::{
    fs,
    path::PathBuf,
    process::Command,
};

use clap::{Parser, Subcommand};
use chrono::Local;

use crate::utils;

pub fn default_change_id() -> String {
    let now = Local::now();
    let ts = now.format("%Y-%m-%dT%H-%M-%S").to_string();
    format!("SLAM-{}", ts)
}

fn validate_buffer(s: &str) -> Result<usize, String> {
    s.parse::<usize>()
        .map_err(|_| format!("`{}` isn't a valid number", s))
        .and_then(|v| {
            if (1..=3).contains(&v) {
                Ok(v)
            } else {
                Err(format!("Buffer must be between 1 and 3, but got {}", v))
            }
        })
}

#[derive(Parser, Debug)]
#[command(
    name = "slam",
    about = "HPA: horizontal PR autoscaler",
    version = crate::built_info::GIT_DESCRIBE
)]
pub struct SlamCli {
    #[command(subcommand)]
    pub command: SlamCommand,
}

#[derive(Subcommand, Debug)]
pub enum SlamCommand {
    #[command(alias = "alleyoop", about = "Create new PR branches with file updates")]
    Create {
        #[arg(short = 'f', long, help = "Glob pattern to find files within each repository")]
        files: Option<String>,

        #[arg(
            short = 'x',
            long,
            help = "Change ID used to create branches and PRs (default: 'SLAM-<YYYY-MM-DDT..>')",
            default_value_t = default_change_id()
        )]
        change_id: String,

        #[arg(
            short = 'b',
            long,
            default_value_t = 1,
            value_parser = validate_buffer,
            help = "Number of context lines in the diff output (must be between 1 and 3)"
        )]
        buffer: usize,

        #[arg(help = "Repository names to filter", value_name = "REPOS", default_value = "")]
        repos: Vec<String>,

        #[command(subcommand)]
        action: CreateAction,
    },

    #[command(about = "Review PRs and merge them")]
    Review {
        #[arg(short = 'o', long, default_value = "tatari-tv", help = "GitHub organization to search for branches")]
        org: String,

        #[arg(short = 'r', long, help = "Repository names to filter", default_value = "")]
        repos: Vec<String>,

        #[command(subcommand)]
        action: ReviewAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum CreateAction {
    #[command(about = "Delete matching files")]
    Delete {
        #[arg(
            short = 'c',
            long,
            help = "Commit deletion with an optional message",
            num_args = 0..=1,
            default_missing_value = "Automated update generated by SLAM"
        )]
        commit: Option<String>,
    },
    #[command(about = "Substring and replacement (requires two arguments)")]
    Sub {
        #[arg(value_name = "PTN", help = "Substring pattern to match")]
        ptn: String,
        #[arg(value_name = "REPL", help = "Replacement string")]
        repl: String,
        #[arg(
            short = 'c',
            long,
            help = "Commit changes with an optional message",
            num_args = 0..=1,
            default_missing_value = "Automated update generated by SLAM"
        )]
        commit: Option<String>,
    },
    #[command(about = "Regex pattern and replacement (requires two arguments)")]
    Regex {
        #[arg(value_name = "PTN", help = "Regex pattern to match")]
        ptn: String,
        #[arg(value_name = "REPL", help = "Replacement string")]
        repl: String,
        #[arg(
            short = 'c',
            long,
            help = "Commit changes with an optional message",
            num_args = 0..=1,
            default_missing_value = "Automated update generated by SLAM"
        )]
        commit: Option<String>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ReviewAction {
    #[command(about = "List Change IDs matching the given pattern")]
    Ls {
        #[arg(
            value_name = "CHANGE_ID_PTNS",
            default_value = "SLAM*",
            num_args = 0..,
            help = "Optional list of Change IDs to filter by. Uses prefix matching (e.g. Change IDs starting with SLAM)"
        )]
        change_id_ptns: Vec<String>,

        #[arg(
            short = 'b',
            long,
            default_value_t = 1,
            value_parser = validate_buffer,
            help = "Number of context lines in the diff output (must be between 1 and 3)"
        )]
        buffer: usize,
    },
    #[command(about = "Approve a specific PR & merge it per matched repos, identified by its Change ID")]
    Approve {
        #[arg(value_name = "CHANGE_ID", help = "Change ID used to find the PR (exact match required)")]
        change_id: String,
        #[arg(long, help = "Pass `--admin` to `gh pr merge` to bypass failing checks")]
        admin_override: bool,
    },
    #[command(about = "Delete a PR & branches per matched repos, identified by its Change ID")]
    Delete {
        #[arg(value_name = "CHANGE_ID", help = "Change ID used to find the PR to delete (exact match required)")]
        change_id: String,
    },
}

pub fn get_cli_tool_status() -> String {
    let success = "✅";
    let failure = "❌";
    let tools = [("git", &["--version"]), ("gh", &["--version"])];

    let mut output_string = String::new();
    output_string.push('\n');
    for (tool, args) in &tools {
        match Command::new(tool).args(*args).output() {
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

    let log_dir: PathBuf = utils::get_or_create_log_dir();
    let log_file = log_dir.join("slam.log");
    let log_status = if log_dir.exists() && log_dir.is_dir() {
        match fs::OpenOptions::new().create(true).append(true).open(&log_file) {
            Ok(_) => format!("{} {} (writable)\n", success, log_dir.display()),
            Err(_) => format!("{} {} (!writable)\n", failure, log_dir.display()),
        }
    } else {
        format!("{} {} (not found)\n", failure, log_dir.display())
    };

    output_string.push_str(&log_status);
    output_string.push('\n');
    output_string
}
