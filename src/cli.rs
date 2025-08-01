use clap::{Parser, Subcommand};
use chrono::Local;

use crate::repo::Change;

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
    /// Sandbox commands for local workspace with every repo checked out
    Sandbox {
        #[arg(
            short = 'r',
            long,
            help = "Patterns for repo filtering"
        )]
        repo_ptns: Vec<String>,
        #[command(subcommand)]
        action: SandboxAction,
    },

    /// Create new <change-id> (branches/PRs) with updates
    Create {
        #[arg(
            short = 'f',
            long,
            help = "Glob pattern to find files within each repository"
        )]
        files: Vec<String>,

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

        #[arg(
            short = 'r',
            long,
            help = "Patterns for repo filtering",
        )]
        repo_ptns: Vec<String>,

        #[command(subcommand)]
        action: Option<CreateAction>,
    },

    /// Review <change-id> (PRs per repo) and merge them
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
            help = "Patterns for repo filtering",
            default_value = ""
        )]
        repo_ptns: Vec<String>,

        #[command(subcommand)]
        action: ReviewAction,
    },
}

#[derive(Subcommand, Debug)]
pub enum CreateAction {
    /// Add a file with specified contents
    Add {
        #[arg(value_name = "PATH", help = "Relative path for the new file")]
        path: String,
        #[arg(value_name = "CONTENT", help = "Contents to write into the file")]
        content: String,
        #[arg(
            short = 'c',
            long,
            help = "Commit changes with an optional message",
            num_args = 0..=1,
            default_missing_value = "Automated update generated by SLAM"
        )]
        commit: Option<String>,
        #[arg(
            short = 's',
            long,
            help = "Do not display diff output; only list matched files"
        )]
        simplified: bool,
    },

    /// Delete matching files
    Delete {
        #[arg(
            short = 'c',
            long,
            help = "Commit deletion with an optional message",
            num_args = 0..=1,
            default_missing_value = "Automated update generated by SLAM"
        )]
        commit: Option<String>,
        #[arg(
            short = 's',
            long,
            help = "Do not display diff output; only list matched files"
        )]
        simplified: bool,
    },

    /// Substring and replacement (requires two arguments)
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
        #[arg(
            short = 's',
            long,
            help = "Do not display diff output; only list matched files"
        )]
        simplified: bool,
    },

    /// Regex pattern and replacement (requires two arguments)
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
        #[arg(
            short = 's',
            long,
            help = "Do not display diff output; only list matched files"
        )]
        simplified: bool,
    },
}

impl CreateAction {
    pub fn decompose(self) -> (Change, Option<String>, bool) {
        match self {
            CreateAction::Delete { commit, simplified } => (Change::Delete, commit, simplified),
            CreateAction::Add { path, content, commit, simplified } => (Change::Add(path, content), commit, simplified),
            CreateAction::Sub { ptn, repl, commit, simplified } => (Change::Sub(ptn, repl), commit, simplified),
            CreateAction::Regex { ptn, repl, commit, simplified } => (Change::Regex(ptn, repl), commit, simplified),
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum ReviewAction {
    #[command(about = "List Change IDs matching the given pattern")]
    Ls {
        #[arg(
            value_name = "CHANGE_ID_PTNS",
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
    #[command(about = "Clone all repos that have an open PR for the given Change ID")]
    Clone {
        #[arg(
            value_name = "CHANGE_ID",
            help = "Change ID used to find the PR (exact match required)"
        )]
        change_id: String,

        #[arg(
            short,
            long,
            help = "Pass `--all` to clone all repos, even with closed PRs"
        )]
        all: bool,
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
    #[command(about = "Purge: close every PR and delete every remote branch prefixed with SLAM for each matching repo")]
    Purge {},
}

#[derive(Subcommand, Debug)]
pub enum SandboxAction {
    /// Set up sandbox environment
    Setup {},
    /// Refresh sandbox by resetting and pulling repositories
    Refresh {},
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_change_id_format() {
        let change_id = default_change_id();
        assert!(change_id.starts_with("SLAM-"));

        // Should be in format SLAM-YYYY-MM-DDTHH-MM-SS
        let timestamp_part = change_id.strip_prefix("SLAM-").unwrap();
        assert_eq!(timestamp_part.len(), 19); // YYYY-MM-DDTHH-MM-SS
        assert_eq!(timestamp_part.chars().nth(4), Some('-'));
        assert_eq!(timestamp_part.chars().nth(7), Some('-'));
        assert_eq!(timestamp_part.chars().nth(10), Some('T'));
        assert_eq!(timestamp_part.chars().nth(13), Some('-'));
        assert_eq!(timestamp_part.chars().nth(16), Some('-'));
    }

    #[test]
    fn test_default_change_id_uniqueness() {
        let id1 = default_change_id();
        std::thread::sleep(std::time::Duration::from_millis(1001)); // Ensure different second
        let id2 = default_change_id();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_validate_buffer_valid_values() {
        assert_eq!(validate_buffer("1"), Ok(1));
        assert_eq!(validate_buffer("2"), Ok(2));
        assert_eq!(validate_buffer("3"), Ok(3));
    }

    #[test]
    fn test_validate_buffer_invalid_values() {
        assert!(validate_buffer("0").is_err());
        assert!(validate_buffer("4").is_err());
        assert!(validate_buffer("-1").is_err());
        assert!(validate_buffer("abc").is_err());
        assert!(validate_buffer("").is_err());
    }

    #[test]
    fn test_validate_buffer_error_messages() {
        let err = validate_buffer("abc").unwrap_err();
        assert!(err.contains("isn't a valid number"));

        let err = validate_buffer("0").unwrap_err();
        assert!(err.contains("Buffer must be between 1 and 3"));

        let err = validate_buffer("4").unwrap_err();
        assert!(err.contains("Buffer must be between 1 and 3"));
    }

    #[test]
    fn test_create_action_decompose_delete() {
        let action = CreateAction::Delete {
            commit: Some("test commit".to_string()),
            simplified: true,
        };

        let (change, commit, simplified) = action.decompose();
        assert!(matches!(change, Change::Delete));
        assert_eq!(commit, Some("test commit".to_string()));
        assert!(simplified);
    }

    #[test]
    fn test_create_action_decompose_add() {
        let action = CreateAction::Add {
            path: "test.txt".to_string(),
            content: "test content".to_string(),
            commit: None,
            simplified: false,
        };

        let (change, commit, simplified) = action.decompose();
        assert!(matches!(change, Change::Add(path, content) if path == "test.txt" && content == "test content"));
        assert_eq!(commit, None);
        assert!(!simplified);
    }

    #[test]
    fn test_create_action_decompose_sub() {
        let action = CreateAction::Sub {
            ptn: "old".to_string(),
            repl: "new".to_string(),
            commit: Some("sub commit".to_string()),
            simplified: false,
        };

        let (change, commit, simplified) = action.decompose();
        assert!(matches!(change, Change::Sub(ptn, repl) if ptn == "old" && repl == "new"));
        assert_eq!(commit, Some("sub commit".to_string()));
        assert!(!simplified);
    }

    #[test]
    fn test_create_action_decompose_regex() {
        let action = CreateAction::Regex {
            ptn: "foo".to_string(),
            repl: "bar".to_string(),
            commit: Some("regex commit".to_string()),
            simplified: true,
        };

        let (change, commit, simplified) = action.decompose();
        assert!(matches!(change, Change::Regex(ptn, repl) if ptn == "foo" && repl == "bar"));
        assert_eq!(commit, Some("regex commit".to_string()));
        assert!(simplified);
    }

    // Note: Testing CLI parsing would require integration tests with clap
    // since the Parser derive macro generates the parsing logic

    #[test]
    fn test_sandbox_action_debug() {
        let setup = SandboxAction::Setup {};
        let refresh = SandboxAction::Refresh {};

        // Ensure Debug is implemented
        assert!(!format!("{:?}", setup).is_empty());
        assert!(!format!("{:?}", refresh).is_empty());
    }

    #[test]
    fn test_review_action_debug() {
        let ls = ReviewAction::Ls {
            change_id_ptns: vec!["SLAM-test".to_string()],
            buffer: 2,
        };

        let clone = ReviewAction::Clone {
            change_id: "SLAM-test".to_string(),
            all: true,
        };

        let approve = ReviewAction::Approve {
            change_id: "SLAM-test".to_string(),
            admin_override: false,
        };

        let delete = ReviewAction::Delete {
            change_id: "SLAM-test".to_string(),
        };

        let purge = ReviewAction::Purge {};

        // Ensure Debug is implemented for all variants
        assert!(!format!("{:?}", ls).is_empty());
        assert!(!format!("{:?}", clone).is_empty());
        assert!(!format!("{:?}", approve).is_empty());
        assert!(!format!("{:?}", delete).is_empty());
        assert!(!format!("{:?}", purge).is_empty());
    }
}
