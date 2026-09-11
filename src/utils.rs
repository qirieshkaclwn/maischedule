/// Escapes special characters for safe inclusion into Telegram HTML messages.
pub fn escape_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '&' => output.push_str("&amp;"),
            '<' => output.push_str("&lt;"),
            '>' => output.push_str("&gt;"),
            '"' => output.push_str("&quot;"),
            _ => output.push(c),
        }
    }
    output
}

/// Returns true if a character is an emoji or pictorial symbol.
#[allow(dead_code)]
pub fn is_emoji(c: char) -> bool {
    matches!(c,
        '\u{1F600}'..='\u{1F64F}' | // Emoticons
        '\u{1F300}'..='\u{1F5FF}' | // Misc Symbols and Pictographs
        '\u{1F680}'..='\u{1F6FF}' | // Transport and Map
        '\u{1F700}'..='\u{1F77F}' | // Alchemical Symbols
        '\u{1F780}'..='\u{1F7FF}' | // Geometric Shapes Extended
        '\u{1F800}'..='\u{1F8FF}' | // Supplemental Arrows-C
        '\u{1F900}'..='\u{1F9FF}' | // Supplemental Symbols and Pictographs
        '\u{1FA00}'..='\u{1FA6F}' | // Chess Symbols
        '\u{1FA70}'..='\u{1FAFF}' | // Symbols and Pictographs Extended-A
        '\u{2600}'..='\u{26FF}'   | // Miscellaneous Symbols
        '\u{2700}'..='\u{27BF}'   | // Dingbats
        '\u{2300}'..='\u{23FF}'   | // Miscellaneous Technical
        '\u{2B50}' | '\u{2B55}'
    )
}

/// Removes all emoji characters from a string slice.
#[allow(dead_code)]
pub fn strip_emojis(input: &str) -> String {
    input.chars().filter(|&c| !is_emoji(c)).collect()
}

/// Percent-encodes a string for safe inclusion in URLs.
pub fn url_encode(input: &str) -> String {
    let mut result = String::new();
    for b in input.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
}

/// Decodes a percent-encoded URL string.
pub fn url_decode(input: &str) -> String {
    let mut bytes = Vec::new();
    let mut chars = input.bytes().peekable();
    while let Some(b) = chars.next() {
        if b == b'%' {
            let h1 = chars.next();
            let h2 = chars.next();
            if let (Some(c1), Some(c2)) = (h1, h2) {
                if let Ok(val) = u8::from_str_radix(std::str::from_utf8(&[c1, c2]).unwrap_or(""), 16) {
                    bytes.push(val);
                    continue;
                }
                bytes.push(b'%');
                bytes.push(c1);
                bytes.push(c2);
            } else {
                bytes.push(b'%');
            }
        } else if b == b'+' {
            bytes.push(b' ');
        } else {
            bytes.push(b);
        }
    }
    String::from_utf8_lossy(&bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    #[test]
    fn test_escape_html() {
        assert_eq!(escape_html("Hello World"), "Hello World");
        assert_eq!(escape_html("<b>Bold & Italic</b>"), "&lt;b&gt;Bold &amp; Italic&lt;/b&gt;");
        assert_eq!(escape_html("\"Quoted\""), "&quot;Quoted&quot;");
    }

    #[test]
    fn test_strip_emojis() {
        assert_eq!(strip_emojis("Текст без эмодзи"), "Текст без эмодзи");
        assert_eq!(strip_emojis("Тест 123 !?"), "Тест 123 !?");
    }

    #[test]
    fn test_url_encode_decode() {
        let original = "М3О-309БВ-24";
        let encoded = url_encode(original);
        assert!(encoded.contains('%'));
        let decoded = url_decode(&encoded);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_no_emojis_in_source_and_docs() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let check_files = [
            "README.md",
            "Agents.md",
            "Cargo.toml",
            "Dockerfile",
            "docker-compose.yml",
            ".env.example",
            ".gitignore",
            "deploy.ps1",
            "scripts/deploy.ps1",
            "scripts/deploy.sh",
            "scripts/server_deploy.sh",
            "scripts/renew_cert.sh",
            "nginx/default.conf.template",
            "src/main.rs",
            "src/api.rs",
            "src/calendar.rs",
            "src/config.rs",
            "src/db.rs",
            "src/diff.rs",
            "src/models.rs",
            "src/scheduler.rs",
            "src/server.rs",
            "src/miniapp.rs",
            "src/telegram.rs",
            "src/utils.rs",
        ];

        for file_rel in &check_files {
            let path = root.join(file_rel);
            if !path.exists() {
                continue;
            }
            let content = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("Failed to read {}: {}", file_rel, e));
            for (line_idx, line) in content.lines().enumerate() {
                for c in line.chars() {
                    // Check non-utils files or lines outside is_emoji definition
                    if file_rel == &"src/utils.rs" && line_idx < 35 {
                        continue;
                    }
                    assert!(
                        !is_emoji(c),
                        "Found emoji '{}' (U+{:04X}) in {} at line {}",
                        c,
                        c as u32,
                        file_rel,
                        line_idx + 1
                    );
                }
            }
        }
    }
}
