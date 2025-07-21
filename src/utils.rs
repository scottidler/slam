pub fn indent(s: &str, indent: usize) -> String {
    let pad = " ".repeat(indent);
    s.lines()
      .map(|line| format!("{}{}", pad, line))
      .collect::<Vec<_>>()
      .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_indent_single_line() {
        let input = "hello world";
        let result = indent(input, 4);
        assert_eq!(result, "    hello world");
    }

    #[test]
    fn test_indent_multiple_lines() {
        let input = "line1\nline2\nline3";
        let result = indent(input, 2);
        assert_eq!(result, "  line1\n  line2\n  line3");
    }

    #[test]
    fn test_indent_empty_string() {
        let input = "";
        let result = indent(input, 3);
        assert_eq!(result, "");
    }

    #[test]
    fn test_indent_zero_indent() {
        let input = "no indent";
        let result = indent(input, 0);
        assert_eq!(result, "no indent");
    }

    #[test]
    fn test_indent_with_empty_lines() {
        let input = "line1\n\nline3";
        let result = indent(input, 2);
        assert_eq!(result, "  line1\n  \n  line3");
    }
}
