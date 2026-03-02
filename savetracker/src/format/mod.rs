pub mod definition;
pub mod pipeline;
pub mod registry;
pub mod transform;

use std::collections::HashMap;
use std::path::Path;

use thiserror::Error;

use crate::decompress;
use crate::detect::{self, FileFormat};
use definition::FormatDefinition;
use pipeline::PipelineError;
pub use registry::FormatRegistry;

const EMBEDDED_FORMATS: &[&str] = &[include_str!("../../etc/formats/borderlands4.toml")];

#[derive(Debug, Error)]
pub enum FormatError {
    #[error("unknown format: {0}")]
    UnknownFormat(String),

    #[error("pipeline error: {0}")]
    Pipeline(#[from] PipelineError),

    #[error("missing required parameter: {0}")]
    MissingParam(String),

    #[error("invalid format definition: {0}")]
    InvalidDefinition(String),
}

pub fn build_registry() -> FormatRegistry {
    let mut reg = FormatRegistry::new();

    for toml_str in EMBEDDED_FORMATS {
        match toml::from_str::<FormatDefinition>(toml_str) {
            Ok(def) => reg.register(def),
            Err(e) => eprintln!("warning: failed to parse embedded format definition: {e}"),
        }
    }

    if let Some(config_dir) = dirs::config_dir() {
        load_from_directory(&mut reg, &config_dir.join("savetracker").join("formats"));
    }

    reg
}

fn load_from_directory(reg: &mut FormatRegistry, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };

        if let Ok(def) = toml::from_str::<FormatDefinition>(&content) {
            reg.register(def);
        }
    }
}

pub fn decode_file(
    registry: &FormatRegistry,
    forced_format: Option<&str>,
    file_path: &str,
    data: &[u8],
    format_params: &HashMap<String, String>,
) -> (Vec<u8>, FileFormat) {
    match decode_or_detect(registry, forced_format, file_path, data, format_params) {
        Ok(result) => (result.data, result.format),
        Err(_) => {
            let fmt = detect::detect(data);
            let decoded = match &fmt {
                FileFormat::Compressed(ct, _) => {
                    decompress::decompress(data, *ct).unwrap_or_else(|_| data.to_vec())
                }
                _ => data.to_vec(),
            };
            (decoded, fmt)
        }
    }
}

pub struct DecodeResult {
    pub data: Vec<u8>,
    pub format: FileFormat,
    pub definition_name: Option<String>,
}

pub fn decode_or_detect(
    reg: &FormatRegistry,
    forced_format: Option<&str>,
    file_path: &str,
    data: &[u8],
    cli_params: &HashMap<String, String>,
) -> Result<DecodeResult, FormatError> {
    let def = match forced_format {
        Some(name) => Some(
            reg.get_by_name(name)
                .ok_or_else(|| FormatError::UnknownFormat(name.to_string()))?,
        ),
        None => reg.detect(file_path, data),
    };

    if let Some(def) = def {
        let mut params = registry::extract_params(def, file_path);
        for (k, v) in cli_params {
            params.entry(k.clone()).or_insert_with(|| v.clone());
        }

        for (name, spec) in &def.params {
            if spec.required && !params.contains_key(name) {
                return Err(FormatError::MissingParam(name.clone()));
            }
        }

        let decoded = pipeline::execute(&def.pipeline, data, &params)?;

        let format = match def.output.format.as_deref() {
            Some("json") => FileFormat::Json,
            Some("yaml") => FileFormat::Yaml,
            Some("toml") => FileFormat::Toml,
            Some("xml") => FileFormat::Xml,
            Some("ini") => FileFormat::Ini,
            _ => detect::detect(&decoded),
        };

        return Ok(DecodeResult {
            data: decoded,
            format,
            definition_name: Some(def.format.name.clone()),
        });
    }

    let format = detect::detect(data);
    let decoded = match &format {
        FileFormat::Compressed(ct, _) => {
            decompress::decompress(data, *ct).unwrap_or_else(|_| data.to_vec())
        }
        _ => data.to_vec(),
    };

    Ok(DecodeResult {
        data: decoded,
        format,
        definition_name: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_registry_has_bl4() {
        let reg = build_registry();
        assert!(reg.get_by_name("borderlands4").is_some());
    }

    #[test]
    fn decode_or_detect_fallback() {
        let reg = build_registry();
        let data = br#"{"player": "test"}"#;
        let params = HashMap::new();
        let result = decode_or_detect(&reg, None, "unknown.json", data, &params).unwrap();
        assert_eq!(result.format, FileFormat::Json);
        assert!(result.definition_name.is_none());
    }

    #[test]
    fn decode_or_detect_forced_unknown() {
        let reg = build_registry();
        let params = HashMap::new();
        let result = decode_or_detect(&reg, Some("nonexistent"), "test.sav", &[], &params);
        assert!(result.is_err());
    }

    #[test]
    fn decode_or_detect_bl4_missing_param() {
        let reg = build_registry();
        let params = HashMap::new();
        let data = vec![0u8; 32];
        let result = decode_or_detect(&reg, Some("borderlands4"), "test.sav", &data, &params);
        assert!(matches!(result, Err(FormatError::MissingParam(_))));
    }

    #[test]
    fn decode_or_detect_bl4_full_roundtrip() {
        use aes::cipher::generic_array::GenericArray;
        use aes::cipher::{BlockEncrypt, KeyInit};
        use aes::Aes256;
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let yaml_data = b"player:\n  name: Test\n  level: 42\n";

        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(yaml_data).unwrap();
        let compressed = encoder.finish().unwrap();

        let pad_len = 16 - (compressed.len() % 16);
        let mut padded = compressed;
        padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));

        let steam_id = "76561198012345678";
        let base_key = "35ec3377f35db0eabe6b83115403ebfb2725642ed549062905078bd60ba4a787";
        let mut key_bytes = hex::decode(base_key).unwrap();
        let id_num: u64 = steam_id.parse().unwrap();
        let id_bytes = id_num.to_le_bytes();
        for i in 0..8 {
            key_bytes[i] ^= id_bytes[i];
        }
        let cipher = Aes256::new(GenericArray::from_slice(&key_bytes));
        for chunk in padded.chunks_exact_mut(16) {
            let block = GenericArray::from_mut_slice(chunk);
            cipher.encrypt_block(block);
        }

        let reg = build_registry();
        let path = "C:/Users/me/My Games/Borderlands 4/Saved/SaveGames/76561198012345678/Profiles/client/1.sav";
        let params = HashMap::new();

        let result = decode_or_detect(&reg, None, path, &padded, &params).unwrap();
        assert_eq!(result.data, yaml_data);
        assert_eq!(result.format, FileFormat::Yaml);
        assert_eq!(result.definition_name.as_deref(), Some("borderlands4"));
    }
}
