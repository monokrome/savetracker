use std::collections::HashMap;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct FormatDefinition {
    pub format: FormatMeta,
    pub detect: DetectionRules,
    #[serde(default)]
    pub pipeline: Vec<PipelineLayer>,
    #[serde(default)]
    pub output: OutputSpec,
    #[serde(default)]
    pub params: HashMap<String, ParamSpec>,
    #[serde(default)]
    pub transform: TransformSpec,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TransformSpec {
    #[serde(default)]
    pub to_content: Option<Vec<String>>,
    #[serde(default)]
    pub to_save: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormatMeta {
    pub name: String,
    pub display_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DetectionRules {
    #[serde(default)]
    pub extensions: Vec<String>,
    #[serde(default)]
    pub path_patterns: Vec<String>,
    #[serde(default)]
    pub magic_bytes: Option<String>,
    #[serde(default)]
    pub platform: HashMap<String, PlatformDetect>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformDetect {
    #[serde(default)]
    pub path_patterns: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PipelineLayer {
    GzipDecompress,
    ZlibDecompress,
    ZstdDecompress,
    Lz4Decompress,
    AesEcbDecrypt {
        key_hex: String,
        #[serde(default)]
        key_transform: Option<String>,
        #[serde(default)]
        key_transform_param: Option<String>,
        #[serde(default)]
        key_transform_bytes: Option<usize>,
    },
    AesCbcDecrypt {
        key_hex: String,
        iv_hex: String,
        #[serde(default)]
        key_transform: Option<String>,
        #[serde(default)]
        key_transform_param: Option<String>,
        #[serde(default)]
        key_transform_bytes: Option<usize>,
    },
    Pkcs7Unpad,
    Xor {
        key_hex: String,
    },
    SkipBytes {
        count: usize,
    },
    TakeBytes {
        offset: usize,
        length: usize,
    },
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct OutputSpec {
    #[serde(default)]
    pub format: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParamSpec {
    #[serde(default)]
    pub flag: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub extract_from_path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_bl4_definition() {
        let toml_str = include_str!("../../etc/formats/borderlands4.toml");
        let def: FormatDefinition =
            toml::from_str(toml_str).expect("failed to parse BL4 definition");
        assert_eq!(def.format.name, "borderlands4");
        assert_eq!(def.format.display_name, "Borderlands 4");
        assert_eq!(def.detect.extensions, vec![".sav"]);
        assert_eq!(def.pipeline.len(), 3);
        assert!(def.output.format.as_deref() == Some("yaml"));
        assert_eq!(def.params.len(), 1);
        let steam_param = def.params.get("steam_id").expect("missing steam_id param");
        assert!(steam_param.required);
        assert!(steam_param.extract_from_path.is_some());
    }

    #[test]
    fn parse_minimal_definition() {
        let toml_str = r#"
[format]
name = "test"
display_name = "Test Format"

[detect]
extensions = [".dat"]
"#;
        let def: FormatDefinition = toml::from_str(toml_str).expect("failed to parse minimal def");
        assert_eq!(def.format.name, "test");
        assert!(def.pipeline.is_empty());
        assert!(def.params.is_empty());
    }

    #[test]
    fn parse_transform_spec() {
        let toml_str = r#"
[format]
name = "pokemon"
display_name = "Pokémon Emerald"

[detect]
extensions = [".sav"]

[transform]
to_content = ["pksav", "decode", "--format", "json"]
to_save = ["pksav", "encode", "--format", "json"]
"#;
        let def: FormatDefinition = toml::from_str(toml_str).expect("failed to parse transform def");
        assert_eq!(
            def.transform.to_content.as_deref(),
            Some(["pksav", "decode", "--format", "json"].map(String::from).as_slice())
        );
        assert_eq!(
            def.transform.to_save.as_deref(),
            Some(["pksav", "encode", "--format", "json"].map(String::from).as_slice())
        );
    }

    #[test]
    fn parse_transform_absent_defaults_to_none() {
        let toml_str = r#"
[format]
name = "test"
display_name = "Test"

[detect]
extensions = [".dat"]
"#;
        let def: FormatDefinition = toml::from_str(toml_str).expect("failed to parse");
        assert!(def.transform.to_content.is_none());
        assert!(def.transform.to_save.is_none());
    }

    #[test]
    fn parse_pipeline_layers() {
        let toml_str = r#"
[format]
name = "layered"
display_name = "Layered Format"

[detect]
extensions = [".bin"]

[[pipeline]]
type = "skip_bytes"
count = 16

[[pipeline]]
type = "xor"
key_hex = "ff"

[[pipeline]]
type = "gzip_decompress"

[output]
format = "json"
"#;
        let def: FormatDefinition = toml::from_str(toml_str).expect("failed to parse layered def");
        assert_eq!(def.pipeline.len(), 3);
        assert!(matches!(
            def.pipeline[0],
            PipelineLayer::SkipBytes { count: 16 }
        ));
        assert!(matches!(def.pipeline[1], PipelineLayer::Xor { .. }));
        assert!(matches!(def.pipeline[2], PipelineLayer::GzipDecompress));
    }
}
