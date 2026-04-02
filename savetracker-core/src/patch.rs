use alloc::vec::Vec;

/// A single changed region in a binary diff.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    pub offset: u32,
    pub data: Vec<u8>,
}

/// A binary patch: a list of changed regions.
#[derive(Debug, Clone, PartialEq)]
pub struct Patch {
    pub regions: Vec<Region>,
}

impl Patch {
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }

    pub fn encoded_size(&self) -> usize {
        self.regions.iter().map(|r| 4 + 2 + r.data.len()).sum()
    }
}

/// Compute a binary patch from old to new.
pub fn diff(old: &[u8], new: &[u8]) -> Patch {
    let mut regions = Vec::new();
    let min_len = old.len().min(new.len());

    let mut i = 0;
    while i < min_len {
        if old[i] != new[i] {
            let start = i;
            while i < min_len && old[i] != new[i] && (i - start) < u16::MAX as usize {
                i += 1;
            }
            regions.push(Region {
                offset: start as u32,
                data: new[start..i].to_vec(),
            });
        } else {
            i += 1;
        }
    }

    // If new is longer, the tail is a new region
    if new.len() > min_len {
        regions.push(Region {
            offset: min_len as u32,
            data: new[min_len..].to_vec(),
        });
    }

    Patch { regions }
}

/// Apply a patch to a base snapshot, producing the new version.
pub fn apply(base: &[u8], patch: &Patch) -> Vec<u8> {
    let max_end = patch
        .regions
        .iter()
        .map(|r| r.offset as usize + r.data.len())
        .max()
        .unwrap_or(0);

    let mut result = base.to_vec();

    if max_end > result.len() {
        result.resize(max_end, 0);
    }

    for region in &patch.regions {
        let offset = region.offset as usize;
        result[offset..offset + region.data.len()].copy_from_slice(&region.data);
    }

    result
}

/// Encode a patch to bytes.
/// Format: repeated [offset: u32 LE][length: u16 LE][data: length bytes]
pub fn encode(patch: &Patch) -> Vec<u8> {
    let mut buf = Vec::with_capacity(patch.encoded_size());

    for region in &patch.regions {
        buf.extend_from_slice(&region.offset.to_le_bytes());
        let len = region.data.len() as u16;
        buf.extend_from_slice(&len.to_le_bytes());
        buf.extend_from_slice(&region.data);
    }

    buf
}

/// Decode a patch from bytes.
pub fn decode(data: &[u8]) -> Option<Patch> {
    let mut regions = Vec::new();
    let mut pos = 0;

    while pos < data.len() {
        if pos + 6 > data.len() {
            return None;
        }

        let offset = u32::from_le_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]);
        pos += 4;

        let length = u16::from_le_bytes([data[pos], data[pos + 1]]) as usize;
        pos += 2;

        if pos + length > data.len() {
            return None;
        }

        regions.push(Region {
            offset,
            data: data[pos..pos + length].to_vec(),
        });
        pos += length;
    }

    Some(Patch { regions })
}

/// Returns true if new differs from old in a way that warrants a patch.
/// If the files are identical, returns false.
/// If new is shorter than old (truncation), returns false — use a full snapshot instead.
pub fn should_patch(old: &[u8], new: &[u8]) -> bool {
    if old == new {
        return false;
    }

    // Truncation can't be represented as a patch — need a full snapshot
    if new.len() < old.len() {
        return false;
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_files_produce_empty_patch() {
        let data = b"hello world";
        let patch = diff(data, data);
        assert!(patch.is_empty());
    }

    #[test]
    fn single_byte_change() {
        let old = b"hello";
        let new = b"hxllo";
        let patch = diff(old, new);
        assert_eq!(patch.regions.len(), 1);
        assert_eq!(patch.regions[0].offset, 1);
        assert_eq!(patch.regions[0].data, b"x");
    }

    #[test]
    fn multiple_changed_regions() {
        let old = b"aabbcc";
        let new = b"aXbbYc";
        let patch = diff(old, new);
        assert_eq!(patch.regions.len(), 2);
        assert_eq!(patch.regions[0].offset, 1);
        assert_eq!(patch.regions[0].data, b"X");
        assert_eq!(patch.regions[1].offset, 4);
        assert_eq!(patch.regions[1].data, b"Y");
    }

    #[test]
    fn appended_data() {
        let old = b"abc";
        let new = b"abcdef";
        let patch = diff(old, new);
        assert_eq!(patch.regions.len(), 1);
        assert_eq!(patch.regions[0].offset, 3);
        assert_eq!(patch.regions[0].data, b"def");
    }

    #[test]
    fn apply_produces_new_version() {
        let old = b"hello world";
        let new = b"hello WORLD";
        let patch = diff(old, new);
        let result = apply(old, &patch);
        assert_eq!(result, new);
    }

    #[test]
    fn apply_with_appended_data() {
        let old = b"abc";
        let new = b"abcXYZ";
        let patch = diff(old, new);
        let result = apply(old, &patch);
        assert_eq!(result, new);
    }

    #[test]
    fn encode_decode_roundtrip() {
        let old = b"the quick brown fox";
        let new = b"the slow  brown cat";
        let patch = diff(old, new);
        let encoded = encode(&patch);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(patch, decoded);
    }

    #[test]
    fn decode_empty() {
        let patch = decode(&[]).unwrap();
        assert!(patch.is_empty());
    }

    #[test]
    fn decode_truncated_header_returns_none() {
        assert!(decode(&[0x00, 0x00]).is_none());
    }

    #[test]
    fn decode_truncated_data_returns_none() {
        // Header says 10 bytes of data but only 2 provided
        let mut buf = Vec::new();
        buf.extend_from_slice(&0u32.to_le_bytes());
        buf.extend_from_slice(&10u16.to_le_bytes());
        buf.extend_from_slice(&[0x00, 0x00]);
        assert!(decode(&buf).is_none());
    }

    #[test]
    fn full_roundtrip_diff_encode_decode_apply() {
        let old = b"save: level=5 gold=100 hp=80";
        let new = b"save: level=6 gold=250 hp=80";
        let patch = diff(old, new);
        let encoded = encode(&patch);
        let decoded = decode(&encoded).unwrap();
        let result = apply(old, &decoded);
        assert_eq!(result, new);
    }

    #[test]
    fn should_patch_identical() {
        assert!(!should_patch(b"same", b"same"));
    }

    #[test]
    fn should_patch_changed() {
        assert!(should_patch(b"old", b"new"));
    }

    #[test]
    fn should_patch_truncation_returns_false() {
        assert!(!should_patch(b"longer", b"short"));
    }

    #[test]
    fn should_patch_appended() {
        assert!(should_patch(b"abc", b"abcdef"));
    }

    #[test]
    fn encoded_size_matches() {
        let patch = diff(b"aaaa", b"aXYa");
        assert_eq!(patch.encoded_size(), encode(&patch).len());
    }

    #[test]
    fn contiguous_changes_merge_into_one_region() {
        let old = b"abcd";
        let new = b"XXXX";
        let patch = diff(old, new);
        assert_eq!(patch.regions.len(), 1);
        assert_eq!(patch.regions[0].data, b"XXXX");
    }
}
