use colored::*;
use similar::{ChangeTag, TextDiff};
use log::warn;

use crate::git;


pub fn show_repo_diff(reponame: &str, pr_number: u64, buffer: usize) {
    let diff_text = match git::get_pr_diff(&reponame, pr_number) {
        Ok(txt) => txt,
        Err(e) => {
            warn!("Could not fetch PR diff for '{}': {}", reponame, e);
            return;
        }
    };

    let file_patches = parse_unified_diff(&diff_text);
    if file_patches.is_empty() {
        return;
    }

    println!("\nRepo: {}", reponame); // <-- Ensures a blank line before

    for (filename, old_text, new_text) in file_patches {
        println!("  Modified file: {}", filename);
        let short_diff = generate_diff(&old_text, &new_text, buffer);
        for line in short_diff.lines() {
            println!("    {}", line);
        }
    }
}

pub fn parse_unified_diff(diff_text: &str) -> Vec<(String, String, String)> {
    let mut result = Vec::new();
    let mut current_file: Option<(String, Vec<String>, Vec<String>)> = None;

    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            if let Some((filename, old_content, new_content)) = current_file.take() {
                if !filename.is_empty() {
                    result.push((filename, old_content.join("\n"), new_content.join("\n")));
                }
            }
            current_file = Some(("".to_string(), Vec::new(), Vec::new()));
        } else if line.starts_with("+++ b/") {
            if let Some(file) = current_file.as_mut() {
                file.0 = line.trim_start_matches("+++ b/").to_string();
            }
        } else if let Some(file) = current_file.as_mut() {
            if line.starts_with('-') && !line.starts_with("---") {
                file.1.push(line[1..].to_string());
            } else if line.starts_with('+') && !line.starts_with("+++") {
                file.2.push(line[1..].to_string());
            } else if line.starts_with(' ') {
                file.1.push(line[1..].to_string());
                file.2.push(line[1..].to_string());
            }
        }
    }

    if let Some((filename, old_content, new_content)) = current_file {
        if !filename.is_empty() {
            result.push((filename, old_content.join("\n"), new_content.join("\n")));
        }
    }

    if result.is_empty() {
        log::warn!(
            "parse_unified_diff: No meaningful diffs were extracted for repo '{}'",
            "DONKEY"
        );
    }

    result
}

pub fn generate_diff(original: &str, updated: &str, buffer: usize) -> String {
    let diff = TextDiff::from_lines(original, updated);
    let mut result = String::new();

    for group in diff.grouped_ops(buffer) {
        for op in group {
            for change in diff.iter_changes(&op) {
                match change.tag() {
                    ChangeTag::Delete => {
                        result.push_str(&format!(
                            "{} | {}\n",
                            format!("-{:4}", change.old_index().unwrap() + 1).red(),
                            change.to_string().trim_end().red()
                        ));
                    }
                    ChangeTag::Insert => {
                        result.push_str(&format!(
                            "{} | {}\n",
                            format!("+{:4}", change.new_index().unwrap() + 1).green(),
                            change.to_string().trim_end().green()
                        ));
                    }
                    ChangeTag::Equal => {
                        result.push_str(&format!(
                            "{} | {}\n",
                            format!(" {:4}", change.old_index().unwrap() + 1).dimmed(),
                            change.to_string().trim_end().dimmed()
                        ));
                    }
                }
            }
        }
    }

    result
}

