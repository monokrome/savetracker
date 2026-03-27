use crate::detect::FileFormat;
use similar::{ChangeTag, TextDiff};

#[derive(Debug, Clone)]
pub struct FileDiff {
    pub format: FileFormat,
    pub summary: String,
    pub detail: String,
}

pub fn diff(old: &[u8], new: &[u8], format: &FileFormat) -> FileDiff {
    let inner_format = unwrap_compression(format);

    match inner_format {
        FileFormat::Json
        | FileFormat::Yaml
        | FileFormat::Toml
        | FileFormat::Xml
        | FileFormat::Ini => text_diff(old, new, format),
        _ => binary_diff(old, new, format),
    }
}

fn unwrap_compression(format: &FileFormat) -> &FileFormat {
    match format {
        FileFormat::Compressed(_, inner) => unwrap_compression(inner),
        other => other,
    }
}

const CONTEXT_LINES: usize = 3;

fn text_diff(old: &[u8], new: &[u8], format: &FileFormat) -> FileDiff {
    let old_text = String::from_utf8_lossy(old);
    let new_text = String::from_utf8_lossy(new);

    let diff = TextDiff::from_lines(old_text.as_ref(), new_text.as_ref());

    let mut added = 0usize;
    let mut removed = 0usize;

    let changes: Vec<_> = diff.iter_all_changes().collect();

    for change in &changes {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => {}
        }
    }

    // Build set of line indices that should be shown (changed lines + context)
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
        let sign = match change.tag() {
            ChangeTag::Insert => "+",
            ChangeTag::Delete => "-",
            ChangeTag::Equal => " ",
        };
        detail.push_str(sign);
        detail.push_str(change.as_str().unwrap_or(""));
        if !change.as_str().unwrap_or("").ends_with('\n') {
            detail.push('\n');
        }
    }

    let summary = format!("{format} diff: +{added} lines, -{removed} lines");

    FileDiff {
        format: format.clone(),
        summary,
        detail,
    }
}

fn binary_diff(old: &[u8], new: &[u8], format: &FileFormat) -> FileDiff {
    let size_diff = new.len() as i64 - old.len() as i64;
    let mut changed_regions: Vec<(usize, u8, u8)> = Vec::new();

    let min_len = old.len().min(new.len());
    for i in 0..min_len {
        if old[i] != new[i] {
            changed_regions.push((i, old[i], new[i]));
        }
    }

    let mut detail = String::new();

    if !changed_regions.is_empty() {
        detail.push_str("Changed bytes:\n");
        let display_limit = changed_regions.len().min(100);
        for &(offset, old_byte, new_byte) in &changed_regions[..display_limit] {
            detail.push_str(&format!(
                "  0x{offset:08X}: {old_byte:02X} -> {new_byte:02X}\n",
            ));
        }
        if changed_regions.len() > 100 {
            detail.push_str(&format!(
                "  ... and {} more changed bytes\n",
                changed_regions.len() - 100
            ));
        }
    }

    if new.len() > old.len() {
        detail.push_str(&format!(
            "Appended {} bytes (file grew from {} to {})\n",
            new.len() - old.len(),
            old.len(),
            new.len()
        ));
    } else if old.len() > new.len() {
        detail.push_str(&format!(
            "Truncated {} bytes (file shrank from {} to {})\n",
            old.len() - new.len(),
            old.len(),
            new.len()
        ));
    }

    let summary = format!(
        "Binary diff: {} bytes changed, size delta {size_diff:+}",
        changed_regions.len(),
    );

    FileDiff {
        format: format.clone(),
        summary,
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_diff_shows_changes() {
        let old = br#"{"level": 1, "gold": 100}"#;
        let new = br#"{"level": 2, "gold": 150}"#;
        let result = diff(old, new, &FileFormat::Json);
        assert!(result.summary.contains("+1"));
        assert!(result.summary.contains("-1"));
    }

    #[test]
    fn binary_diff_detects_changed_bytes() {
        let old = &[0x00, 0x01, 0x02, 0x03];
        let new = &[0x00, 0xFF, 0x02, 0x03];
        let result = diff(old, new, &FileFormat::Binary);
        assert!(result.detail.contains("01 -> FF"));
    }

    #[test]
    fn binary_diff_detects_size_change() {
        let old = &[0x00, 0x01];
        let new = &[0x00, 0x01, 0x02, 0x03];
        let result = diff(old, new, &FileFormat::Binary);
        assert!(result.detail.contains("Appended 2 bytes"));
    }
}
