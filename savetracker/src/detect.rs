use crate::decompress::{self, CompressionType};

#[derive(Debug, Clone, PartialEq)]
pub enum FileFormat {
    Compressed(CompressionType, Box<FileFormat>),
    Json,
    Yaml,
    Toml,
    Xml,
    Ini,
    Binary,
}

impl std::fmt::Display for FileFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compressed(ct, inner) => write!(f, "{ct}({inner})"),
            Self::Json => write!(f, "JSON"),
            Self::Yaml => write!(f, "YAML"),
            Self::Toml => write!(f, "TOML"),
            Self::Xml => write!(f, "XML"),
            Self::Ini => write!(f, "INI"),
            Self::Binary => write!(f, "Binary"),
        }
    }
}

fn detect_compression(data: &[u8]) -> Option<CompressionType> {
    if data.len() < 2 {
        return None;
    }

    if data[0] == 0x1f && data[1] == 0x8b {
        return Some(CompressionType::Gzip);
    }

    if data.len() >= 4 && data[0] == 0x28 && data[1] == 0xb5 && data[2] == 0x2f && data[3] == 0xfd {
        return Some(CompressionType::Zstd);
    }

    if data.len() >= 4 && data[0] == 0x04 && data[1] == 0x22 && data[2] == 0x4d && data[3] == 0x18 {
        return Some(CompressionType::Lz4);
    }

    // zlib: first byte is 0x78, second is 0x01/0x5e/0x9c/0xda
    if data[0] == 0x78 && matches!(data[1], 0x01 | 0x5e | 0x9c | 0xda) {
        return Some(CompressionType::Zlib);
    }

    None
}

fn detect_text_format(text: &str) -> FileFormat {
    let trimmed = text.trim();

    if serde_json::from_str::<serde_json::Value>(trimmed).is_ok() {
        return FileFormat::Json;
    }

    if trimmed.starts_with('<') && trimmed.ends_with('>') {
        let mut reader = quick_xml::Reader::from_str(trimmed);
        let mut valid = true;
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Err(_) => {
                    valid = false;
                    break;
                }
                _ => {}
            }
        }
        if valid {
            return FileFormat::Xml;
        }
    }

    if toml::from_str::<toml::Value>(trimmed).is_ok() && trimmed.contains('=') {
        return FileFormat::Toml;
    }

    if serde_yaml::from_str::<serde_yaml::Value>(trimmed).is_ok()
        && (trimmed.contains(':') || trimmed.starts_with('-'))
    {
        return FileFormat::Yaml;
    }

    if looks_like_ini(trimmed) {
        return FileFormat::Ini;
    }

    FileFormat::Binary
}

fn looks_like_ini(text: &str) -> bool {
    let mut has_section = false;
    let mut has_kv = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            has_section = true;
        } else if line.contains('=') {
            has_kv = true;
        }
    }

    has_section && has_kv
}

pub fn detect(data: &[u8]) -> FileFormat {
    if let Some(compression) = detect_compression(data) {
        if let Ok(decompressed) = decompress::decompress(data, compression) {
            let inner = detect(&decompressed);
            return FileFormat::Compressed(compression, Box::new(inner));
        }
    }

    if let Ok(text) = std::str::from_utf8(data) {
        return detect_text_format(text);
    }

    FileFormat::Binary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_json() {
        let data = br#"{"player": "test", "level": 5}"#;
        assert_eq!(detect(data), FileFormat::Json);
    }

    #[test]
    fn detects_yaml() {
        let data = b"player: test\nlevel: 5\n";
        assert_eq!(detect(data), FileFormat::Yaml);
    }

    #[test]
    fn detects_toml() {
        let data = b"[player]\nname = \"test\"\nlevel = 5\n";
        assert_eq!(detect(data), FileFormat::Toml);
    }

    #[test]
    fn detects_xml() {
        let data = b"<save><player name=\"test\" level=\"5\"/></save>";
        assert_eq!(detect(data), FileFormat::Xml);
    }

    #[test]
    fn detects_ini() {
        let data = b"[General]\nplayer = test\nlevel = 5\n";
        assert_eq!(detect(data), FileFormat::Ini);
    }

    #[test]
    fn detects_binary() {
        let data = &[0x00, 0x01, 0x02, 0xFF, 0xFE, 0xFD];
        assert_eq!(detect(data), FileFormat::Binary);
    }

    #[test]
    fn detects_gzip_json() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let json = br#"{"player": "test"}"#;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(json).unwrap();
        let compressed = encoder.finish().unwrap();

        assert_eq!(
            detect(&compressed),
            FileFormat::Compressed(CompressionType::Gzip, Box::new(FileFormat::Json))
        );
    }
}
