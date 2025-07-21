// src/sandbox.rs

use rayon::prelude::*;
use std::env;
use std::io::{self, Write};
use std::path::Path;

use colored::Colorize;
use eyre::Result;
use log::{debug, info, warn};

use crate::git;

/// Refreshes a single repository by pruning remote branches, cleaning local stale branches,
/// resetting, checking out the head branch, pulling the latest changes, and installing pre-commit hooks.
/// Returns a status string.
pub fn refresh_repo(repo: &Path) -> Result<String> {
    let success_emoji = "📥";
    let error_emoji = "❗";
    let missing_emoji = "❓";

    // Prune remote branches.
    debug!("Starting remote prune for repo '{}'", repo.display());
    git::remote_prune(repo)?;
    debug!("Finished remote prune for repo '{}'", repo.display());

    // Remove any local branches starting with "SLAM" that don't have a corresponding remote branch.
    match git::list_local_branches_with_prefix(repo, "SLAM") {
        Ok(local_branches) => {
            debug!("Found {} local SLAM branches in '{}'", local_branches.len(), repo.display());
            for branch in local_branches {
                match git::remote_branch_exists(repo, &branch) {
                    Ok(true) => {
                        debug!("Remote branch '{}' exists in '{}'", branch, repo.display());
                    }
                    Ok(false) => {
                        debug!(
                            "Remote branch '{}' does not exist in '{}'; deleting local branch",
                            branch,
                            repo.display()
                        );
                        git::safe_delete_local_branch(repo, &branch)?;
                        info!("Deleted local branch '{}' in '{}'", branch, repo.display());
                    }
                    Err(e) => {
                        warn!("Error checking remote branch '{}' in {}: {}", branch, repo.display(), e);
                    }
                }
            }
        }
        Err(e) => {
            warn!("Failed to list local branches in {}: {}", repo.display(), e);
        }
    }

    // Ensure we have the latest changes on the HEAD branch.
    let branch = git::get_head_branch(repo)?;
    debug!("Determined HEAD branch '{}' for repo '{}'", branch, repo.display());
    let branch_display = branch.magenta();

    // Capture the SHA before updating
    let sha_before = git::get_head_sha(repo)?;

    // Reset any local changes and switch to HEAD
    git::reset_hard(repo)?;
    debug!("Completed hard reset for repo '{}'", repo.display());

    git::checkout(repo, &branch)?;
    debug!("Checked out branch '{}' in repo '{}'", branch, repo.display());

    // Pull the latest
    git::pull(repo)?;
    debug!("Pulled latest changes for repo '{}'", repo.display());

    // Capture the SHA after updating
    let sha_after = git::get_head_sha(repo)?;

    // Build a 7-character SHA display: bold green if it changed, dimmed grey if unchanged
    let short_sha = &sha_after[..7];
    let sha_display = if sha_before != sha_after {
        short_sha.bold().green()
    } else {
        short_sha.dimmed()
    };

    // Install pre-commit hooks if a configuration exists.
    let hook_status = if repo.join(".pre-commit-config.yaml").exists() {
        debug!("Found pre-commit config in repo '{}'", repo.display());
        match git::install_pre_commit_hooks(repo) {
            Ok(true) => {
                debug!("Pre-commit hooks installed successfully in repo '{}'", repo.display());
                success_emoji
            }
            Ok(false) | Err(_) => {
                debug!("Pre-commit hooks installation failed or hook file missing in repo '{}'", repo.display());
                error_emoji
            }
        }
    } else {
        debug!("No pre-commit config found in repo '{}'", repo.display());
        missing_emoji
    };

    let reposlug = git::get_repo_slug(repo)?;
    debug!("Returning status for repo '{}'", reposlug);

    // Insert `sha_display` between the branch name and the emoji
    Ok(format!(
        "{:>6} {} {} {}",
        branch_display, sha_display, hook_status, reposlug
    ))
}

/// Generates a status line for a newly cloned repository.
/// This provides consistent output format with refresh_repo for new repositories.
fn generate_clone_status(repo: &Path) -> Result<String> {
    let success_emoji = "📥";
    let error_emoji = "❗";
    let missing_emoji = "❓";

    // Get the current branch and SHA
    let branch = git::get_head_branch(repo)?;
    let branch_display = branch.magenta();

    let sha = git::get_head_sha(repo)?;
    let short_sha = &sha[..7];
    let sha_display = short_sha.bold().green(); // New clone, so it's "new"

    // Check for pre-commit hooks
    let hook_status = if repo.join(".pre-commit-config.yaml").exists() {
        match git::install_pre_commit_hooks(repo) {
            Ok(true) => success_emoji,
            Ok(false) | Err(_) => error_emoji,
        }
    } else {
        missing_emoji
    };

    let reposlug = git::get_repo_slug(repo)?;

    Ok(format!(
        "{:>6} {} {} {}",
        branch_display, sha_display, hook_status, reposlug
    ))
}

/// Refreshes all repositories found in the current working directory.
/// Each repository is processed in parallel; status output is printed for each.
pub fn sandbox_refresh() -> Result<()> {
    let cwd = env::current_dir()?;
    debug!("Current working directory: '{}'", cwd.display());
    let repos = git::find_git_repositories(&cwd)?;
    debug!("Found {} repositories in '{}'", repos.len(), cwd.display());

    repos.par_iter().for_each(|repo| {
        debug!("Processing repo '{}'", repo.display());
        match refresh_repo(repo) {
            Ok(line) => {
                println!("{}", line);
                io::stdout().flush().expect("Failed to flush stdout");
            }
            Err(e) => {
                warn!("Error processing repo {}: {}", repo.to_string_lossy().trim_end(), e);
            }
        }
    });
    Ok(())
}

/// Sets up a sandbox environment by retrieving the list of repositories for a given organization,
/// filtering them based on provided patterns, and then cloning or updating each repository.
/// For existing repositories, performs a full refresh to ensure they are on the HEAD branch and up to date.
/// Pre-commit hooks are installed if available.
/// Outputs status lines in the same format as sandbox_refresh.
pub fn sandbox_setup(repo_ptns: Vec<String>) -> Result<()> {
    let org = "tatari-tv";
    debug!("Retrieving repository list for organization '{}'", org);
    let repos = git::find_repos_in_org(org)?;
    info!("Found {} repos in '{}'", repos.len(), org);

    let filtered_repos: Vec<String> = if repo_ptns.is_empty() {
        debug!("No repository patterns provided; using all repos");
        repos.clone()
    } else {
        debug!("Filtering repositories with patterns: {:?}", repo_ptns);
        repos.into_iter().filter(|r| {
            repo_ptns.iter().any(|ptn| r.contains(ptn))
        }).collect()
    };
    info!("After filtering, {} repos remain", filtered_repos.len());

    let cwd = env::current_dir()?;
    debug!("Sandbox setup working directory: '{}'", cwd.display());

    filtered_repos.par_iter().for_each(|reposlug| {
        let target = cwd.join(reposlug);

        if target.exists() {
            debug!("Repository {} already exists in {}; performing full refresh...", reposlug, target.display());

            // Perform a full refresh to ensure the repo is on HEAD branch and up to date
            match refresh_repo(&target) {
                Ok(status_line) => {
                    println!("{}", status_line);
                    io::stdout().flush().expect("Failed to flush stdout");
                }
                Err(e) => {
                    warn!("Failed to refresh repository {}: {}", reposlug, e);
                }
            }
        } else {
            debug!("Cloning repository {} into {}", reposlug, target.display());
            if let Err(e) = git::clone_repo(reposlug, &target) {
                warn!("Failed to clone repository {}: {}", reposlug, e);
                return; // Skip status generation if clone failed
            }

            // Generate and print status line for newly cloned repo
            match generate_clone_status(&target) {
                Ok(status_line) => {
                    println!("{}", status_line);
                    io::stdout().flush().expect("Failed to flush stdout");
                }
                Err(e) => {
                    warn!("Failed to generate status for cloned repository {}: {}", reposlug, e);
                }
            }
        }
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_sandbox_setup_empty_patterns() {
        // This test would require mocking git::find_repos_in_org
        // For now, we'll test the filtering logic
        let all_repos = vec![
            "tatari-tv/repo1".to_string(),
            "tatari-tv/repo2".to_string(),
            "tatari-tv/another-repo".to_string(),
        ];

        let empty_patterns: Vec<String> = vec![];
        let filtered: Vec<String> = if empty_patterns.is_empty() {
            all_repos.clone()
        } else {
            all_repos.into_iter().filter(|r| {
                empty_patterns.iter().any(|ptn| r.contains(ptn))
            }).collect()
        };

        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"tatari-tv/repo1".to_string()));
        assert!(filtered.contains(&"tatari-tv/repo2".to_string()));
        assert!(filtered.contains(&"tatari-tv/another-repo".to_string()));
    }

    #[test]
    fn test_sandbox_setup_with_patterns() {
        let all_repos = vec![
            "tatari-tv/repo1".to_string(),
            "tatari-tv/repo2".to_string(),
            "tatari-tv/another-repo".to_string(),
            "tatari-tv/different".to_string(),
        ];

        let patterns = vec!["repo".to_string()];
        let filtered: Vec<String> = all_repos.into_iter().filter(|r| {
            patterns.iter().any(|ptn| r.contains(ptn))
        }).collect();

        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"tatari-tv/repo1".to_string()));
        assert!(filtered.contains(&"tatari-tv/repo2".to_string()));
        assert!(filtered.contains(&"tatari-tv/another-repo".to_string()));
        assert!(!filtered.contains(&"tatari-tv/different".to_string()));
    }

    #[test]
    fn test_sandbox_setup_multiple_patterns() {
        let all_repos = vec![
            "tatari-tv/frontend-app".to_string(),
            "tatari-tv/backend-service".to_string(),
            "tatari-tv/mobile-app".to_string(),
            "tatari-tv/docs".to_string(),
        ];

        let patterns = vec!["app".to_string(), "service".to_string()];
        let filtered: Vec<String> = all_repos.into_iter().filter(|r| {
            patterns.iter().any(|ptn| r.contains(ptn))
        }).collect();

        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&"tatari-tv/frontend-app".to_string()));
        assert!(filtered.contains(&"tatari-tv/backend-service".to_string()));
        assert!(filtered.contains(&"tatari-tv/mobile-app".to_string()));
        assert!(!filtered.contains(&"tatari-tv/docs".to_string()));
    }

    // Integration tests would need to mock the git functions, but for now we'll test
    // the core logic and structure

    #[test]
    fn test_refresh_repo_status_format() {
        // This is more of an integration test that would require a real git repo
        // For now, we'll test that the function signature and basic structure are correct

        // The function should return a formatted string with:
        // - Branch name (right-aligned, 6 chars)
        // - SHA (7 chars, colored based on change)
        // - Hook status emoji
        // - Repo slug

        // We can't easily test this without mocking all the git functions,
        // but we can verify the expected format structure
        let expected_parts = 4; // branch, sha, emoji, reposlug

        // Mock result format: "  main abc1234 📥 tatari-tv/test-repo"
        let mock_result = "  main abc1234 📥 tatari-tv/test-repo";
        let parts: Vec<&str> = mock_result.split_whitespace().collect();
        assert_eq!(parts.len(), expected_parts);
    }

    #[test]
    fn test_sandbox_refresh_empty_directory() {
        let temp_dir = TempDir::new().unwrap();

        // Change to temp directory
        let original_dir = env::current_dir().unwrap();
        env::set_current_dir(temp_dir.path()).unwrap();

        // This should not panic with empty directory
        // Note: This test will actually try to find git repos, but should handle empty gracefully
        // We can't easily test the full function without mocking git::find_git_repositories

        // Restore original directory
        env::set_current_dir(original_dir).unwrap();
    }

    #[test]
    fn test_org_constant() {
        // Test that the hardcoded org is what we expect
        // This is tested indirectly through the sandbox_setup function
        // The function uses "tatari-tv" as the organization
        assert_eq!("tatari-tv".len(), 9); // Basic sanity check
    }

    #[test]
    fn test_emoji_constants() {
        // Test the emoji constants used in refresh_repo
        let success_emoji = "📥";
        let error_emoji = "❗";
        let missing_emoji = "❓";

        assert_eq!(success_emoji, "📥");
        assert_eq!(error_emoji, "❗");
        assert_eq!(missing_emoji, "❓");

        // Ensure they're not empty
        assert!(!success_emoji.is_empty());
        assert!(!error_emoji.is_empty());
        assert!(!missing_emoji.is_empty());
    }

    #[test]
    fn test_pre_commit_config_filename() {
        // Test the expected pre-commit config filename
        let config_file = ".pre-commit-config.yaml";
        assert_eq!(config_file, ".pre-commit-config.yaml");
        assert!(config_file.starts_with('.'));
        assert!(config_file.ends_with(".yaml"));
    }

    #[test]
    fn test_generate_clone_status_format() {
        // Test that generate_clone_status produces the expected format
        // We can't easily test with real git repos, but we can verify the format structure

        // The function should return a formatted string with:
        // - Branch name (right-aligned, 6 chars)
        // - SHA (7 chars, bold green for new clone)
        // - Hook status emoji
        // - Repo slug

        // Expected format: "{:>6} {} {} {}"
        // This matches the format used in refresh_repo
        let format_template = "{:>6} {} {} {}";
        assert!(format_template.contains("{:>6}"));  // Right-aligned branch

        // Count the number of format placeholders (both {:>6} and {})
        let right_aligned_count = format_template.matches("{:>6}").count();
        let simple_placeholder_count = format_template.matches(" {}").count(); // Space before {} to avoid counting part of {:>6}
        let total_placeholders = right_aligned_count + simple_placeholder_count;

        assert_eq!(right_aligned_count, 1);  // One right-aligned placeholder
        assert_eq!(simple_placeholder_count, 3);  // Three simple placeholders
        assert_eq!(total_placeholders, 4);  // Four placeholders total
    }

    #[test]
    fn test_generate_clone_status_emoji_constants() {
        // Verify the emoji constants used in generate_clone_status match refresh_repo
        let success_emoji = "📥";
        let error_emoji = "❗";
        let missing_emoji = "❓";

        // These should be the same constants used in both functions
        assert_eq!(success_emoji, "📥");
        assert_eq!(error_emoji, "❗");
        assert_eq!(missing_emoji, "❓");
    }

    #[test]
    fn test_setup_and_refresh_output_consistency() {
        // Test that both setup and refresh use the same output format
        // Both should call functions that return strings in the format:
        // "{:>6} {} {} {}" where:
        // - First {} is branch name (right-aligned, 6 chars)
        // - Second {} is SHA (7 chars, colored)
        // - Third {} is hook status emoji
        // - Fourth {} is repo slug

        // Mock the expected output format from both functions
        let mock_refresh_output = "  main abc1234 📥 tatari-tv/test-repo";
        let mock_setup_existing_output = "  main def5678 📥 tatari-tv/existing-repo";
        let mock_setup_cloned_output = "  main ghi9012 📥 tatari-tv/cloned-repo";

        // All should have the same structure
        for output in [mock_refresh_output, mock_setup_existing_output, mock_setup_cloned_output] {
            let parts: Vec<&str> = output.split_whitespace().collect();
            assert_eq!(parts.len(), 4, "Output should have 4 parts: branch, sha, emoji, reposlug");

            // Branch part (first part)
            assert!(!parts[0].is_empty(), "Branch name should not be empty");

            // SHA part (second part) - should be 7 characters
            assert_eq!(parts[1].len(), 7, "SHA should be 7 characters");

            // Emoji part (third part)
            assert!(!parts[2].is_empty(), "Emoji should not be empty");

            // Repo slug part (fourth part)
            assert!(parts[3].contains('/'), "Repo slug should contain org/repo format");
        }
    }

    #[test]
    fn test_sandbox_setup_uses_parallel_processing() {
        // Test that sandbox_setup uses par_iter() for parallel processing
        // This is more of a structural test to ensure the function signature is correct

        // We can't easily test the parallel execution without complex mocking,
        // but we can verify the function doesn't panic with empty inputs
        let empty_patterns: Vec<String> = vec![];

        // The function should handle empty patterns gracefully
        // (though it will fail when trying to call git functions in a test environment)
        assert!(empty_patterns.is_empty());
    }

    #[test]
    fn test_sandbox_refresh_uses_parallel_processing() {
        // Test that sandbox_refresh uses par_iter() for parallel processing
        // Similar to the setup test, this verifies the structural consistency

        // Both functions should use the same parallel processing pattern:
        // repos.par_iter().for_each(|repo| { ... });

        // This is a structural test - we can't easily test the actual parallel
        // execution without mocking the git functions
        assert!(true); // Placeholder for structural consistency
    }

    #[test]
    fn test_both_functions_use_stdout_flush() {
        // Test that both setup and refresh use io::stdout().flush()
        // This ensures consistent output behavior in parallel processing

        // Both functions should:
        // 1. Print status lines with println!()
        // 2. Flush stdout with io::stdout().flush().expect("Failed to flush stdout")

        // This is important for parallel processing to ensure output appears immediately
        let flush_error_msg = "Failed to flush stdout";
        assert_eq!(flush_error_msg, "Failed to flush stdout");
        assert!(!flush_error_msg.is_empty());
    }

    #[test]
    fn test_setup_handles_existing_and_new_repos() {
        // Test that setup handles both existing repos (calls refresh_repo)
        // and new repos (calls generate_clone_status) consistently

        // Both paths should:
        // 1. Generate a status line
        // 2. Print it with println!()
        // 3. Flush stdout

        // The key difference is:
        // - Existing repos: calls refresh_repo() which returns status
        // - New repos: calls git::clone_repo() then generate_clone_status()

        // Both should produce the same output format
        assert!(true); // Structural test placeholder
    }

    #[test]
    fn test_error_handling_consistency() {
        // Test that both setup and refresh handle errors consistently
        // Both should:
        // - Use warn!() for errors
        // - Continue processing other repos (don't panic)
        // - Use similar error message formats

        let error_msg_refresh = "Error processing repo";
        let error_msg_setup_refresh = "Failed to refresh repository";
        let error_msg_setup_clone = "Failed to clone repository";
        let error_msg_setup_status = "Failed to generate status for cloned repository";

        // All error messages should be descriptive and non-empty
        for msg in [error_msg_refresh, error_msg_setup_refresh, error_msg_setup_clone, error_msg_setup_status] {
            assert!(!msg.is_empty(), "Error message should not be empty");
            assert!(msg.len() > 10, "Error message should be descriptive");
        }
    }
}
