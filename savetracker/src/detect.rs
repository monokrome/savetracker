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

impl FileFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Compressed(_, inner) => inner.extension(),
            Self::Json => "json",
            Self::Yaml => "yaml",
            Self::Toml => "toml",
            Self::Xml => "xml",
            Self::Ini => "ini",
            Self::Binary => "dat",
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Compressed(_, _) => "Compressed",
            Self::Json => "JSON",
            Self::Yaml => "YAML",
            Self::Toml => "TOML",
            Self::Xml => "XML",
            Self::Ini => "INI",
            Self::Binary => "Binary",
        }
    }
}

impl std::fmt::Display for FileFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Compressed(ct, inner) => write!(f, "{ct}({inner})"),
            _ => f.write_str(self.name()),
        }
    }
}

const MAGIC_BYTES: &[(&[u8], CompressionType)] = &[
    (&[0x1f, 0x8b], CompressionType::Gzip),
    (&[0x28, 0xb5, 0x2f, 0xfd], CompressionType::Zstd),
    (&[0x04, 0x22, 0x4d, 0x18], CompressionType::Lz4),
];

fn detect_compression(data: &[u8]) -> Option<CompressionType> {
    for (magic, ct) in MAGIC_BYTES {
        if data.len() >= magic.len() && data.starts_with(magic) {
            return Some(*ct);
        }
    }

    // zlib: first byte is 0x78, second varies by compression level
    if data.len() >= 2 && data[0] == 0x78 && matches!(data[1], 0x01 | 0x5e | 0x9c | 0xda) {
        return Some(CompressionType::Zlib);
    }

    None
}

type TextDetector = fn(&str) -> bool;

const TEXT_DETECTORS: &[(FileFormat, TextDetector)] = &[
    (FileFormat::Json, is_json),
    (FileFormat::Xml, is_xml),
    (FileFormat::Toml, is_toml),
    (FileFormat::Yaml, is_yaml),
    (FileFormat::Ini, is_ini),
];

fn is_json(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text).is_ok()
}

fn is_xml(text: &str) -> bool {
    if !text.starts_with('<') || !text.ends_with('>') {
        return false;
    }
    let mut reader = quick_xml::Reader::from_str(text);
    loop {
        match reader.read_event() {
            Ok(quick_xml::events::Event::Eof) => return true,
            Err(_) => return false,
            _ => {}
        }
    }
}

fn is_toml(text: &str) -> bool {
    text.contains('=') && toml::from_str::<toml::Value>(text).is_ok()
}

fn is_yaml(text: &str) -> bool {
    (text.contains(':') || text.starts_with('-'))
        && serde_yaml::from_str::<serde_yaml::Value>(text).is_ok()
}

fn is_ini(text: &str) -> bool {
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

fn detect_text_format(text: &str) -> FileFormat {
    let trimmed = text.trim();
    TEXT_DETECTORS
        .iter()
        .find(|(_, detect)| detect(trimmed))
        .map(|(fmt, _)| fmt.clone())
        .unwrap_or(FileFormat::Binary)
}

pub fn detect(data: &[u8]) -> FileFormat {
    if let Some(compression) = detect_compression(data) {
        if let Ok(decompressed) = decompress::decompress(data, compression) {
            let inner = detect(&decompressed);
            return FileFormat::Compressed(compression, Box::new(inner));
        }
    }

    std::str::from_utf8(data)
        .map(detect_text_format)
        .unwrap_or(FileFormat::Binary)
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

    #[test]
    fn display_format() {
        assert_eq!(FileFormat::Json.to_string(), "JSON");
        assert_eq!(FileFormat::Binary.to_string(), "Binary");
        assert_eq!(
            FileFormat::Compressed(CompressionType::Gzip, Box::new(FileFormat::Yaml)).to_string(),
            "gzip(YAML)"
        );
    }

    #[test]
    fn extension_through_compression() {
        let fmt = FileFormat::Compressed(CompressionType::Zstd, Box::new(FileFormat::Json));
        assert_eq!(fmt.extension(), "json");
    }
}
