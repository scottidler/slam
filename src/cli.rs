use clap::{Parser, Subcommand};
use chrono::Local;

pub fn default_change_id() -> String {
    let date = Local::now().format("%Y-%m-%d").to_string();
    format!("SLAM-{}", date)
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

        #[arg(short = 'd', long, help = "Match and delete whole files")]
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
            long = "commit",
            help = "Commit changes with an optional message",
            num_args = 0..=1,
            default_missing_value = "Automated update generated by SLAM"
        )]
        commit: Option<String>,

        #[arg(help = "Repository names to filter", value_name = "REPOS", default_value = "")]
        repos: Vec<String>,
    },

    #[command(about = "Review PRs and merge them")]
    Review {
        #[arg(
            short = 'o',
            long,
            default_value = "tatari-tv",
            help = "GitHub organization to search for branches"
        )]
        org: String,

        #[arg(
            short = 'r',
            long,
            help = "Repository names to filter",
            default_value = ""
        )]
        repos: Vec<String>,

        #[command(subcommand)]
        action: Action,
    },
}

#[derive(Subcommand, Debug)]
pub enum Action {
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
            help = "Number of context lines in the diff output"
        )]
        buffer: usize,
    },

    #[command(about = "Approve a specific PR & merge it per matched repos, identified by its Change ID")]
    Approve {
        #[arg(
            value_name = "CHANGE_ID",
            help = "Change ID used to find the PR (exact match required)"
        )]
        change_id: String,

        #[arg(
            long,
            help = "Pass `--admin` to `gh pr merge` to bypass failing checks"
        )]
        admin_override: bool,
    },

    #[command(about = "Delete a PR & branches per matched repos, identified by its Change ID")]
    Delete {
        #[arg(
            value_name = "CHANGE_ID",
            help = "Change ID used to find the PR to delete (exact match required)"
        )]
        change_id: String,
    },
}

pub fn get_cli_tool_status() -> String {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

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
    let log_status = {
        let log_dir = Path::new("/var/log/messages/slam");
        if log_dir.exists() && log_dir.is_dir() {
            match fs::OpenOptions::new().create(true).append(true).open(log_dir.join("slam.log")) {
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
