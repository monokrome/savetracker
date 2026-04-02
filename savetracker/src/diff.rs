use crate::detect::FileFormat;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub format: FileFormat,
    pub summary: String,
    pub detail: String,
}

pub fn diff(old: &[u8], new: &[u8], format: &FileFormat) -> FileDiff {
    match format {
        FileFormat::Compressed(_, inner) => diff(old, new, inner),
        FileFormat::Binary => binary_diff(old, new, format),
        _ => text_diff(old, new, format),
    }
}

// --- Text diffing ---

const CONTEXT_LINES: usize = 3;

struct TextStats {
    added: usize,
    removed: usize,
}

fn count_changes(changes: &[similar::Change<&str>]) -> TextStats {
    let mut stats = TextStats {
        added: 0,
        removed: 0,
    };
    for change in changes {
        match change.tag() {
            ChangeTag::Insert => stats.added += 1,
            ChangeTag::Delete => stats.removed += 1,
            ChangeTag::Equal => {}
        }
    }
    stats
}

fn build_visibility_mask(changes: &[similar::Change<&str>]) -> Vec<bool> {
    let mut visible = vec![false; changes.len()];
    for (i, change) in changes.iter().enumerate() {
        if change.tag() != ChangeTag::Equal {
            let start = i.saturating_sub(CONTEXT_LINES);
            let end = (i + CONTEXT_LINES + 1).min(changes.len());
            for v in &mut visible[start..end] {
                *v = true;
            }
        }
    }
    visible
}

fn change_prefix(tag: ChangeTag) -> &'static str {
    match tag {
        ChangeTag::Insert => "+",
        ChangeTag::Delete => "-",
        ChangeTag::Equal => " ",
    }
}

fn render_text_detail(changes: &[similar::Change<&str>], visible: &[bool]) -> String {
    if !visible.iter().any(|&v| v) {
        return String::new();
    }

    let mut detail = String::new();
    let mut in_gap = false;

    for (i, change) in changes.iter().enumerate() {
        if !visible[i] {
            if !in_gap && i > 0 {
                detail.push_str("───\n");
                in_gap = true;
            }
            continue;
        }

        in_gap = false;
        detail.push_str(change_prefix(change.tag()));
        let text = change.as_str().unwrap_or("");
        detail.push_str(text);
        if !text.ends_with('\n') {
            detail.push('\n');
        }
    }

    detail
}

fn build_result(format: &FileFormat, summary_body: &str, detail: String) -> FileDiff {
    FileDiff {
        format: format.clone(),
        summary: format!("{format} diff: {summary_body}"),
        detail,
    }
}

fn text_diff(old: &[u8], new: &[u8], format: &FileFormat) -> FileDiff {
    let old_text = String::from_utf8_lossy(old);
    let new_text = String::from_utf8_lossy(new);
    let td = TextDiff::from_lines(old_text.as_ref(), new_text.as_ref());
    let changes: Vec<_> = td.iter_all_changes().collect();

    let stats = count_changes(&changes);
    let visible = build_visibility_mask(&changes);
    let detail = render_text_detail(&changes, &visible);

    build_result(
        format,
        &format!("+{} lines, -{} lines", stats.added, stats.removed),
        detail,
    )
}

// --- Binary diffing ---

const CHANGED_BYTE_DISPLAY_LIMIT: usize = 100;

struct ChangedByte {
    offset: usize,
    old: u8,
    new: u8,
}

fn find_changed_bytes(old: &[u8], new: &[u8]) -> Vec<ChangedByte> {
    let min_len = old.len().min(new.len());
    (0..min_len)
        .filter(|&i| old[i] != new[i])
        .map(|i| ChangedByte {
            offset: i,
            old: old[i],
            new: new[i],
        })
        .collect()
}

fn render_size_change(old_len: usize, new_len: usize) -> Option<String> {
    if new_len > old_len {
        Some(format!(
            "Appended {} bytes (file grew from {old_len} to {new_len})\n",
            new_len - old_len,
        ))
    } else if old_len > new_len {
        Some(format!(
            "Truncated {} bytes (file shrank from {old_len} to {new_len})\n",
            old_len - new_len,
        ))
    } else {
        None
    }
}

fn render_binary_detail(changed: &[ChangedByte], old_len: usize, new_len: usize) -> String {
    let mut detail = String::new();

    if changed.is_empty() {
        if let Some(size_info) = render_size_change(old_len, new_len) {
            detail.push_str(&size_info);
        }
        return detail;
    }

    detail.push_str("Changed bytes:\n");
    let limit = changed.len().min(CHANGED_BYTE_DISPLAY_LIMIT);
    for cb in &changed[..limit] {
        detail.push_str(&format!(
            "  0x{:08X}: {:02X} -> {:02X}\n",
            cb.offset, cb.old, cb.new,
        ));
    }
    if changed.len() > CHANGED_BYTE_DISPLAY_LIMIT {
        detail.push_str(&format!(
            "  ... and {} more changed bytes\n",
            changed.len() - CHANGED_BYTE_DISPLAY_LIMIT,
        ));
    }

    if let Some(size_info) = render_size_change(old_len, new_len) {
        detail.push_str(&size_info);
    }

    detail
}

fn binary_diff(old: &[u8], new: &[u8], format: &FileFormat) -> FileDiff {
    let changed = find_changed_bytes(old, new);
    let size_diff = new.len() as i64 - old.len() as i64;
    let detail = render_binary_detail(&changed, old.len(), new.len());

    build_result(
        format,
        &format!("{} bytes changed, size delta {size_diff:+}", changed.len()),
        detail,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decompress::CompressionType;

    // --- text diff tests ---

    #[test]
    fn text_diff_counts_added_and_removed() {
        let old = b"line1\nline2\nline3\n";
        let new = b"line1\nchanged\nline3\nnew line\n";
        let result = diff(old, new, &FileFormat::Yaml);
        assert!(result.summary.contains("+2"));
        assert!(result.summary.contains("-1"));
    }

    #[test]
    fn text_diff_detail_shows_plus_minus() {
        let old = b"aaa\n";
        let new = b"bbb\n";
        let result = diff(old, new, &FileFormat::Json);
        assert!(result.detail.contains("-aaa"));
        assert!(result.detail.contains("+bbb"));
    }

    #[test]
    fn text_diff_identical_files() {
        let data = b"same\ncontent\n";
        let result = diff(data, data, &FileFormat::Toml);
        assert!(result.summary.contains("+0"));
        assert!(result.summary.contains("-0"));
        assert!(result.detail.is_empty());
    }

    #[test]
    fn text_diff_context_lines() {
        let old_lines: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        let mut new_lines = old_lines.clone();
        new_lines[10] = "CHANGED".to_string();

        let old = old_lines.join("\n") + "\n";
        let new = new_lines.join("\n") + "\n";

        let result = diff(old.as_bytes(), new.as_bytes(), &FileFormat::Ini);
        // Context should include surrounding lines but not line 0
        assert!(result.detail.contains("line 8") || result.detail.contains("line 9"));
        assert!(!result.detail.contains("line 0\n") || result.detail.contains("───"));
    }

    #[test]
    fn text_diff_gap_separator() {
        let old_lines: Vec<String> = (0..30).map(|i| format!("line {i}")).collect();
        let mut new_lines = old_lines.clone();
        new_lines[5] = "CHANGED_A".to_string();
        new_lines[25] = "CHANGED_B".to_string();

        let old = old_lines.join("\n") + "\n";
        let new = new_lines.join("\n") + "\n";

        let result = diff(old.as_bytes(), new.as_bytes(), &FileFormat::Xml);
        assert!(result.detail.contains("───"));
    }

    #[test]
    fn text_diff_format_in_summary() {
        let result = diff(b"a\n", b"b\n", &FileFormat::Json);
        assert!(result.summary.starts_with("JSON"));
    }

    #[test]
    fn text_diff_compressed_unwraps() {
        let fmt = FileFormat::Compressed(CompressionType::Gzip, Box::new(FileFormat::Yaml));
        let result = diff(b"a: 1\n", b"a: 2\n", &fmt);
        // Should do text diff, not binary
        assert!(result.detail.contains("-a: 1"));
        assert!(result.detail.contains("+a: 2"));
    }

    // --- binary diff tests ---

    #[test]
    fn binary_diff_detects_changed_bytes() {
        let old = &[0x00, 0x01, 0x02, 0x03];
        let new = &[0x00, 0xFF, 0x02, 0x03];
        let result = diff(old, new, &FileFormat::Binary);
        assert!(result.detail.contains("01 -> FF"));
        assert!(result.summary.contains("1 bytes changed"));
    }

    #[test]
    fn binary_diff_multiple_changes() {
        let old = &[0xAA, 0xBB, 0xCC];
        let new = &[0x11, 0xBB, 0x22];
        let result = diff(old, new, &FileFormat::Binary);
        assert!(result.detail.contains("AA -> 11"));
        assert!(result.detail.contains("CC -> 22"));
        assert!(result.summary.contains("2 bytes changed"));
    }

    #[test]
    fn binary_diff_appended_bytes() {
        let old = &[0x00, 0x01];
        let new = &[0x00, 0x01, 0x02, 0x03];
        let result = diff(old, new, &FileFormat::Binary);
        assert!(result.detail.contains("Appended 2 bytes"));
        assert!(result.summary.contains("size delta +2"));
    }

    #[test]
    fn binary_diff_truncated_bytes() {
        let old = &[0x00, 0x01, 0x02, 0x03];
        let new = &[0x00, 0x01];
        let result = diff(old, new, &FileFormat::Binary);
        assert!(result.detail.contains("Truncated 2 bytes"));
        assert!(result.summary.contains("size delta -2"));
    }

    #[test]
    fn binary_diff_identical_files() {
        let data = &[0x00, 0x01, 0x02];
        let result = diff(data, data, &FileFormat::Binary);
        assert!(result.summary.contains("0 bytes changed"));
        assert!(result.summary.contains("size delta +0"));
        assert!(result.detail.is_empty());
    }

    #[test]
    fn binary_diff_truncates_display() {
        let old: Vec<u8> = (0u8..=200).collect();
        let new: Vec<u8> = old.iter().map(|b| b.wrapping_add(1)).collect();
        let result = diff(&old, &new, &FileFormat::Binary);
        assert!(result.detail.contains("... and"));
    }

    #[test]
    fn binary_diff_empty_to_data() {
        let result = diff(&[], &[0x42], &FileFormat::Binary);
        assert!(result.detail.contains("Appended 1 bytes"));
    }

    #[test]
    fn binary_diff_data_to_empty() {
        let result = diff(&[0x42], &[], &FileFormat::Binary);
        assert!(result.detail.contains("Truncated 1 bytes"));
    }
}
