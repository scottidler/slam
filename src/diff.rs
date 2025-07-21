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
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 4 {
                current_filename = parts[2].trim_start_matches("a/").to_string();
            }
        } else if line.starts_with("+++ ") {
            if line.trim() != "+++ /dev/null" {
                current_filename = line.trim_start_matches("+++ b/").to_string();
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_diff_empty_updated() {
        let original = "line1\nline2\nline3";
        let updated = "";
        let result = generate_diff(original, updated, 1);

        // Should show all original lines as deletions (ignoring color codes)
        assert!(result.contains("-   1"));
        assert!(result.contains("-   2"));
        assert!(result.contains("-   3"));
        assert!(result.contains("line1"));
        assert!(result.contains("line2"));
        assert!(result.contains("line3"));
    }

    #[test]
    fn test_generate_diff_no_changes() {
        let original = "line1\nline2\nline3";
        let updated = "line1\nline2\nline3";
        let result = generate_diff(original, updated, 1);

        // When there are no changes, the diff should be empty
        assert!(result.is_empty());
    }

    #[test]
    fn test_generate_diff_with_changes() {
        let original = "line1\nline2\nline3";
        let updated = "line1\nmodified_line2\nline3";
        let result = generate_diff(original, updated, 1);

        // Should show deletion and insertion (ignoring color codes)
        assert!(result.contains("-   2"));
        assert!(result.contains("+   2"));
        assert!(result.contains("line2"));
        assert!(result.contains("modified_line2"));
    }

    #[test]
    fn test_generate_diff_empty_original() {
        let original = "";
        let updated = "new_line1\nnew_line2";
        let result = generate_diff(original, updated, 1);

        // Should show all lines as insertions (ignoring color codes)
        assert!(result.contains("+   1"));
        assert!(result.contains("+   2"));
        assert!(result.contains("new_line1"));
        assert!(result.contains("new_line2"));
    }

    #[test]
    fn test_reconstruct_files_from_unified_diff_simple() {
        let diff_text = r#"diff --git a/file1.txt b/file1.txt
index 1234567..abcdefg 100644
--- a/file1.txt
+++ b/file1.txt
@@ -1,3 +1,3 @@
 line1
-old_line2
+new_line2
 line3"#;

        let result = reconstruct_files_from_unified_diff(diff_text);
        assert_eq!(result.len(), 1);

        let (filename, orig, upd) = &result[0];
        assert_eq!(filename, "file1.txt");
        assert_eq!(orig, "line1\nold_line2\nline3");
        assert_eq!(upd, "line1\nnew_line2\nline3");
    }

    #[test]
    fn test_reconstruct_files_from_unified_diff_multiple_files() {
        let diff_text = r#"diff --git a/file1.txt b/file1.txt
index 1234567..abcdefg 100644
--- a/file1.txt
+++ b/file1.txt
@@ -1,2 +1,2 @@
 line1
-old_line2
+new_line2
diff --git a/file2.txt b/file2.txt
index 2345678..bcdefgh 100644
--- a/file2.txt
+++ b/file2.txt
@@ -1,2 +1,2 @@
 another_line1
-another_old_line2
+another_new_line2"#;

        let result = reconstruct_files_from_unified_diff(diff_text);
        assert_eq!(result.len(), 2);

        let (filename1, orig1, upd1) = &result[0];
        assert_eq!(filename1, "file1.txt");
        assert_eq!(orig1, "line1\nold_line2");
        assert_eq!(upd1, "line1\nnew_line2");

        let (filename2, orig2, upd2) = &result[1];
        assert_eq!(filename2, "file2.txt");
        assert_eq!(orig2, "another_line1\nanother_old_line2");
        assert_eq!(upd2, "another_line1\nanother_new_line2");
    }

    #[test]
    fn test_reconstruct_files_from_unified_diff_empty() {
        let diff_text = "";
        let result = reconstruct_files_from_unified_diff(diff_text);
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_reconstruct_files_from_unified_diff_new_file() {
        let diff_text = r#"diff --git a/newfile.txt b/newfile.txt
new file mode 100644
index 0000000..1234567
--- /dev/null
+++ b/newfile.txt
@@ -0,0 +1,2 @@
+new_line1
+new_line2"#;

        let result = reconstruct_files_from_unified_diff(diff_text);
        assert_eq!(result.len(), 1);

        let (filename, orig, upd) = &result[0];
        assert_eq!(filename, "newfile.txt");
        assert_eq!(orig, "");
        assert_eq!(upd, "new_line1\nnew_line2");
    }

    #[test]
    fn test_reconstruct_files_from_unified_diff_deleted_file() {
        let diff_text = r#"diff --git a/deletedfile.txt b/deletedfile.txt
deleted file mode 100644
index 1234567..0000000
--- a/deletedfile.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-deleted_line1
-deleted_line2"#;

        let result = reconstruct_files_from_unified_diff(diff_text);
        assert_eq!(result.len(), 1);

        let (filename, orig, upd) = &result[0];
        assert_eq!(filename, "deletedfile.txt");
        assert_eq!(orig, "deleted_line1\ndeleted_line2");
        assert_eq!(upd, "");
    }

    #[test]
    fn test_reconstruct_files_from_unified_diff_context_lines() {
        let diff_text = r#"diff --git a/file.txt b/file.txt
index 1234567..abcdefg 100644
--- a/file.txt
+++ b/file.txt
@@ -2,5 +2,5 @@
 context_line1
 context_line2
-old_line
+new_line
 context_line3
 context_line4"#;

        let result = reconstruct_files_from_unified_diff(diff_text);
        assert_eq!(result.len(), 1);

        let (filename, orig, upd) = &result[0];
        assert_eq!(filename, "file.txt");
        assert_eq!(orig, "\ncontext_line1\ncontext_line2\nold_line\ncontext_line3\ncontext_line4");
        assert_eq!(upd, "\ncontext_line1\ncontext_line2\nnew_line\ncontext_line3\ncontext_line4");
    }
}
