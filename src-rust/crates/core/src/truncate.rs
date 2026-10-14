//! Text truncation utilities — mirrors src/utils/truncate.ts

/// Truncate `text` to at most `max_chars` characters.
/// If truncated, appends `… (truncated)`.
pub fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.len() <= max_chars {
        return text.to_string();
    }
    // Find a safe char boundary
    let mut end = max_chars;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (truncated)", &text[..end])
}

/// Truncate a list of lines to at most `max_lines`.
/// If truncated, appends a `"… N more lines"` indicator.
pub fn truncate_lines(lines: &[String], max_lines: usize) -> (Vec<String>, bool) {
    if lines.len() <= max_lines {
        return (lines.to_vec(), false);
    }
    let mut out = lines[..max_lines].to_vec();
    let remaining = lines.len() - max_lines;
    out.push(format!("… {} more line{}", remaining, if remaining == 1 { "" } else { "s" }));
    (out, true)
}

/// Cap tool output at about `max_bytes`, keeping the head and the tail (where
/// commands print what they are doing and how they ended) and replacing the
/// middle with a note giving the omitted size. Cuts land on char boundaries.
/// Output within the cap is returned unchanged.
pub fn truncate_tool_output(text: String, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text;
    }
    let half = max_bytes / 2;
    let mut head_end = half;
    while !text.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = text.len() - half;
    while !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n\n[... {} bytes omitted from the middle of this output ...]\n\n{}",
        &text[..head_end],
        tail_start - head_end,
        &text[tail_start..]
    )
}

/// Truncate a file path for display, keeping the filename and shortening the directory.
pub fn truncate_path(path: &str, max_chars: usize) -> String {
    if path.len() <= max_chars {
        return path.to_string();
    }
    let filename = std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path);
    if filename.len() >= max_chars {
        return filename.to_string();
    }
    let prefix_len = max_chars - filename.len() - 4; // 4 for "…/"
    let dir = std::path::Path::new(path)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");
    if dir.len() <= prefix_len {
        return path.to_string();
    }
    format!("…/{}/{}", &dir[dir.len() - prefix_len..], filename)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_text_short() {
        assert_eq!(truncate_text("hello", 10), "hello");
    }

    #[test]
    fn truncate_text_long() {
        let t = truncate_text("hello world", 5);
        assert!(t.starts_with("hello"));
        assert!(t.contains("truncated"));
    }

    #[test]
    fn truncate_tool_output_short_is_unchanged() {
        assert_eq!(truncate_tool_output("ok".to_string(), 10), "ok");
    }

    #[test]
    fn truncate_tool_output_keeps_head_and_tail() {
        let text = format!("HEAD{}TAIL", "x".repeat(10_000));
        let out = truncate_tool_output(text, 1_000);
        assert!(out.starts_with("HEAD"));
        assert!(out.ends_with("TAIL"));
        assert!(out.contains("9008 bytes omitted"), "{}", out);
        assert!(out.len() < 1_100);
    }

    #[test]
    fn truncate_tool_output_cuts_on_char_boundaries() {
        // 3-byte chars: neither cut point may split one.
        let text = "€".repeat(1_000);
        let out = truncate_tool_output(text, 101);
        assert!(out.starts_with('€') && out.ends_with('€'));
    }

    #[test]
    fn truncate_lines_over_limit() {
        let lines: Vec<String> = (0..10).map(|i| format!("line {}", i)).collect();
        let (out, truncated) = truncate_lines(&lines, 5);
        assert!(truncated);
        assert_eq!(out.len(), 6); // 5 lines + 1 indicator
        assert!(out[5].contains("5 more"));
    }
}
