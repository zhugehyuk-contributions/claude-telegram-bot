//! Formatting utilities (Markdown → Telegram HTML, tool status strings).

use regex::Regex;

/// Escape HTML special characters for Telegram HTML parse mode.
pub fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Convert a minimal markdown subset to Telegram-compatible HTML.
///
/// Telegram HTML supports only a small subset: `<b>`, `<i>`, `<code>`, `<pre>`, `<a href="...">`.
pub fn convert_markdown_to_html(input: &str) -> String {
    let (text, code_blocks) = extract_code_blocks(input);
    let (mut text, inline_codes) = extract_inline_codes(&text);

    // Escape the remaining text first.
    text = escape_html(&text);

    // Line-oriented transforms (avoid cross-line emphasis bugs).
    let mut lines = Vec::new();
    for line in text.split('\n') {
        let mut l = convert_header_line(line);
        l = replace_delimited(&l, "**", "<b>", "</b>");
        l = replace_delimited(&l, "__", "<b>", "</b>");
        l = replace_single_delim(&l, '_', "<i>", "</i>");
        l = replace_single_delim(&l, '*', "<b>", "</b>");
        lines.push(l);
    }
    text = lines.join("\n");

    // Blockquotes (after escaping, `>` becomes `&gt;`).
    text = convert_blockquotes(&text);

    // Bullet lists
    text = text
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix("- ") {
                return format!("• {rest}");
            }
            if let Some(rest) = line.strip_prefix("* ") {
                return format!("• {rest}");
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Horizontal rules
    text = text
        .lines()
        .filter(|line| {
            let t = line.trim();
            !(t.len() >= 3 && t.chars().all(|c| c == '-' || c == '*'))
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Links: [text](url) -> <a href="url">text</a>
    // Note: this is intentionally conservative (no nested brackets).
    let link_re = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex");
    text = link_re
        .replace_all(&text, r#"<a href="$2">$1</a>"#)
        .to_string();

    // Restore code blocks
    for (i, code) in code_blocks.iter().enumerate() {
        let escaped = escape_html(code);
        text = text.replace(
            &format!("\0CODEBLOCK{i}\0"),
            &format!("<pre>{escaped}</pre>"),
        );
    }

    // Restore inline code
    for (i, code) in inline_codes.iter().enumerate() {
        let escaped = escape_html(code);
        text = text.replace(
            &format!("\0INLINECODE{i}\0"),
            &format!("<code>{escaped}</code>"),
        );
    }

    // Collapse multiple newlines
    while text.contains("\n\n\n") {
        text = text.replace("\n\n\n", "\n\n");
    }

    text
}

fn extract_code_blocks(input: &str) -> (String, Vec<String>) {
    let mut blocks = Vec::new();
    let mut out = String::new();

    let mut i = 0usize;
    while let Some(rel) = input[i..].find("```") {
        let start = i + rel;
        out.push_str(&input[i..start]);

        let mut p = start + 3;
        // Optional language identifier: [A-Za-z0-9_]+
        while p < input.len() {
            let b = input.as_bytes()[p];
            if b.is_ascii_alphanumeric() || b == b'_' {
                p += 1;
            } else {
                break;
            }
        }
        // Optional single newline
        if p < input.len() && input.as_bytes()[p] == b'\n' {
            p += 1;
        }

        // Find closing fence
        if let Some(end_rel) = input[p..].find("```") {
            let end = p + end_rel;
            let code = input[p..end].to_string();
            let idx = blocks.len();
            blocks.push(code);
            out.push_str(&format!("\0CODEBLOCK{idx}\0"));
            i = end + 3;
            continue;
        }

        // Unclosed fence: append the rest and stop.
        out.push_str(&input[start..]);
        return (out, blocks);
    }

    out.push_str(&input[i..]);
    (out, blocks)
}

fn extract_inline_codes(input: &str) -> (String, Vec<String>) {
    let mut codes = Vec::new();
    let mut out = String::new();

    let mut i = 0usize;
    while let Some(rel) = input[i..].find('`') {
        let start = i + rel;
        out.push_str(&input[i..start]);

        let content_start = start + 1;
        if let Some(end_rel) = input[content_start..].find('`') {
            let end = content_start + end_rel;
            let code = input[content_start..end].to_string();
            let idx = codes.len();
            codes.push(code);
            out.push_str(&format!("\0INLINECODE{idx}\0"));
            i = end + 1;
            continue;
        }

        // Unclosed: append the rest and stop.
        out.push_str(&input[start..]);
        return (out, codes);
    }

    out.push_str(&input[i..]);
    (out, codes)
}

fn convert_header_line(line: &str) -> String {
    let bytes = line.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i] == b'#' && i < 6 {
        i += 1;
    }
    if i == 0 {
        return line.to_string();
    }
    if i < bytes.len() && bytes[i] == b' ' {
        return format!("<b>{}</b>", &line[i + 1..]);
    }
    line.to_string()
}

fn replace_delimited(text: &str, delim: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut i = 0usize;
    while let Some(rel) = text[i..].find(delim) {
        let start = i + rel;
        out.push_str(&text[i..start]);
        let content_start = start + delim.len();
        if let Some(end_rel) = text[content_start..].find(delim) {
            let end = content_start + end_rel;
            out.push_str(open);
            out.push_str(&text[content_start..end]);
            out.push_str(close);
            i = end + delim.len();
            continue;
        }
        out.push_str(&text[start..]);
        return out;
    }
    out.push_str(&text[i..]);
    out
}

fn replace_single_delim(text: &str, delim: char, open: &str, close: &str) -> String {
    let mut out = String::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;

    while i < chars.len() {
        if chars[i] == delim {
            // Do not treat doubled delimiters as single.
            if (i > 0 && chars[i - 1] == delim) || (i + 1 < chars.len() && chars[i + 1] == delim) {
                out.push(delim);
                i += 1;
                continue;
            }

            // Find matching closing delimiter on the same line (no newlines here).
            let mut j = i + 1;
            while j < chars.len() {
                if chars[j] == '\n' {
                    break;
                }
                if chars[j] == delim
                    && !(j > 0 && chars[j - 1] == delim)
                    && !(j + 1 < chars.len() && chars[j + 1] == delim)
                {
                    out.push_str(open);
                    for c in &chars[i + 1..j] {
                        out.push(*c);
                    }
                    out.push_str(close);
                    i = j + 1;
                    break;
                }
                j += 1;
            }

            if j >= chars.len() || chars.get(j) != Some(&delim) {
                // No closing delimiter found.
                out.push(delim);
                i += 1;
            }
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    out
}

fn convert_blockquotes(text: &str) -> String {
    let mut result: Vec<String> = Vec::new();
    let mut in_block = false;
    let mut block_lines: Vec<String> = Vec::new();

    for line in text.split('\n') {
        if line.starts_with("&gt; ") || line == "&gt;" {
            in_block = true;
            if line == "&gt;" {
                block_lines.push(String::new());
            } else {
                // Strip "&gt; " and remove "#" (Telegram mobile bug workaround).
                let content = line[5..].replace('#', "");
                block_lines.push(content);
            }
            continue;
        }

        if in_block {
            result.push(format!(
                "<blockquote>{}</blockquote>",
                block_lines.join("\n")
            ));
            block_lines.clear();
            in_block = false;
        }
        result.push(line.to_string());
    }

    if in_block {
        result.push(format!(
            "<blockquote>{}</blockquote>",
            block_lines.join("\n")
        ));
    }

    result.join("\n")
}

// ============== Tool Status Formatting ==============

fn shorten_path(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() >= 2 {
        return format!("{}/{}", parts[parts.len() - 2], parts[parts.len() - 1]);
    }
    parts.last().copied().unwrap_or("file").to_string()
}

fn truncate_one_line(text: &str, max_len: usize) -> String {
    let cleaned = text.replace('\n', " ").trim().to_string();
    if cleaned.len() <= max_len {
        return cleaned;
    }
    format!("{}...", cleaned.chars().take(max_len).collect::<String>())
}

fn code(text: &str) -> String {
    format!("<code>{}</code>", escape_html(text))
}

/// Format tool use for display in Telegram (HTML mode).
pub fn format_tool_status(tool_name: &str, tool_input: &serde_json::Value) -> String {
    let emoji_map = [
        ("Read", "📖"),
        ("Write", "📝"),
        ("Edit", "✏️"),
        ("Bash", "▶️"),
        ("Glob", "🔍"),
        ("Grep", "🔎"),
        ("WebSearch", "🔍"),
        ("WebFetch", "🌐"),
        ("Task", "🎯"),
        ("TodoWrite", "📋"),
        ("mcp__", "🔧"),
    ];

    let mut emoji = "🔧";
    for (k, v) in emoji_map {
        if tool_name.contains(k) {
            emoji = v;
            break;
        }
    }

    let get = |k: &str| tool_input.get(k).and_then(|v| v.as_str()).unwrap_or("");

    if tool_name == "Read" {
        let file_path = get("file_path");
        let lower = file_path.to_lowercase();
        let image_exts = [
            ".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp", ".svg", ".ico",
        ];
        if image_exts.iter().any(|ext| lower.ends_with(ext)) {
            return "👀 Viewing".to_string();
        }
        return format!("{emoji} Reading {}", code(&shorten_path(file_path)));
    }

    if tool_name == "Write" {
        let file_path = get("file_path");
        return format!("{emoji} Writing {}", code(&shorten_path(file_path)));
    }

    if tool_name == "Edit" {
        let file_path = get("file_path");
        return format!("{emoji} Editing {}", code(&shorten_path(file_path)));
    }

    if tool_name == "Bash" {
        let cmd = get("command");
        let desc = get("description");
        if !desc.is_empty() {
            return format!("{emoji} {}", escape_html(desc));
        }
        return format!("{emoji} {}", code(&truncate_one_line(cmd, 50)));
    }

    if tool_name == "Grep" {
        let pattern = get("pattern");
        let path = get("path");
        if !path.is_empty() {
            return format!(
                "{emoji} Searching {} in {}",
                code(&truncate_one_line(pattern, 30)),
                code(&shorten_path(path))
            );
        }
        return format!(
            "{emoji} Searching {}",
            code(&truncate_one_line(pattern, 40))
        );
    }

    if tool_name == "Glob" {
        let pattern = get("pattern");
        return format!("{emoji} Finding {}", code(&truncate_one_line(pattern, 50)));
    }

    if tool_name == "WebSearch" {
        let query = get("query");
        return format!(
            "{emoji} Searching: {}",
            escape_html(&truncate_one_line(query, 50))
        );
    }

    if tool_name == "WebFetch" {
        let url = get("url");
        return format!("{emoji} Fetching {}", code(&truncate_one_line(url, 50)));
    }

    if tool_name == "Task" {
        let desc = get("description");
        if !desc.is_empty() {
            return format!("{emoji} Agent: {}", escape_html(desc));
        }
    }

    // Fallback
    if tool_input.is_object() {
        return format!("{emoji} {}", escape_html(tool_name));
    }
    format!("{emoji} {}", escape_html(tool_name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_html() {
        let s = r#"<a href="x&y">"#;
        assert_eq!(escape_html(s), "&lt;a href=&quot;x&amp;y&quot;&gt;");
    }

    #[test]
    fn converts_code_blocks_without_touching_contents() {
        let md = "hi\n```js\nconst x = '<b>';\n```\nbye";
        let html = convert_markdown_to_html(md);
        assert!(html.contains("<pre>"));
        assert!(html.contains("const x = '&lt;b&gt;';"));
        assert!(!html.contains("<b>"));
    }

    #[test]
    fn converts_blockquotes_multiline() {
        let md = "> hello\n> world\nok";
        let html = convert_markdown_to_html(md);
        assert!(html.contains("<blockquote>hello\nworld</blockquote>"));
        assert!(html.contains("ok"));
    }

    #[test]
    fn converts_links() {
        let md = "[x](https://example.com)";
        let html = convert_markdown_to_html(md);
        assert_eq!(html, r#"<a href="https://example.com">x</a>"#);
    }

    #[test]
    fn tool_status_read_image() {
        let v = serde_json::json!({"file_path":"/tmp/a.png"});
        assert_eq!(format_tool_status("Read", &v), "👀 Viewing");
    }
}
