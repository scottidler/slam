use colored::*;
use similar::{ChangeTag, TextDiff};
use regex::Regex;

pub fn reconstruct_files_from_unified_diff(diff_text: &str) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let mut current_filename = String::new();
    let mut orig_lines: Vec<String> = Vec::new();
    let mut upd_lines: Vec<String> = Vec::new();
    let mut next_orig_line = 1;
    let mut next_upd_line = 1;

    let hunk_header_re = Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").unwrap();

    for line in diff_text.lines() {
        if line.starts_with("diff --git ") {
            if !current_filename.is_empty() {
                results.push((
                    current_filename.clone(),
                    orig_lines.join("\n"),
                    upd_lines.join("\n"),
                ));
            }
            current_filename.clear();
            orig_lines.clear();
            upd_lines.clear();
            next_orig_line = 1;
            next_upd_line = 1;
        } else if line.starts_with("+++ b/") {
            current_filename = line.trim_start_matches("+++ b/").to_string();
        } else if let Some(caps) = hunk_header_re.captures(line) {
            let hunk_orig_start: usize = caps.get(1).unwrap().as_str().parse().unwrap();
            let hunk_upd_start: usize = caps.get(3).unwrap().as_str().parse().unwrap();

            if hunk_orig_start > next_orig_line {
                let gap = hunk_orig_start - next_orig_line;
                for _ in 0..gap {
                    orig_lines.push(String::new());
                }
                next_orig_line = hunk_orig_start;
            }
            if hunk_upd_start > next_upd_line {
                let gap = hunk_upd_start - next_upd_line;
                for _ in 0..gap {
                    upd_lines.push(String::new());
                }
                next_upd_line = hunk_upd_start;
            }
        } else if line.starts_with(" ") {
            let content = line[1..].to_string();
            orig_lines.push(content.clone());
            upd_lines.push(content);
            next_orig_line += 1;
            next_upd_line += 1;
        } else if line.starts_with("-") && !line.starts_with("---") {
            let content = line[1..].to_string();
            orig_lines.push(content);
            next_orig_line += 1;
        } else if line.starts_with("+") && !line.starts_with("+++") {
            let content = line[1..].to_string();
            upd_lines.push(content);
            next_upd_line += 1;
        }
    }
    if !current_filename.is_empty() {
        results.push((
            current_filename,
            orig_lines.join("\n"),
            upd_lines.join("\n"),
        ));
    }
    results
}

pub fn generate_diff(original: &str, updated: &str, buffer: usize) -> String {
    if updated.is_empty() {
        let mut result = String::new();
        for (i, line) in original.lines().enumerate() {
            result.push_str(&format!(
                "{} | {}\n",
                format!("-{:4}", i + 1).red(),
                line.red()
            ));
        }
        return result;
    }
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

