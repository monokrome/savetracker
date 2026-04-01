use std::collections::HashMap;
use std::path::Path;

use super::definition::{DetectionRules, FormatDefinition, ParamSpec};

pub struct FormatRegistry {
    formats: Vec<FormatDefinition>,
    by_name: HashMap<String, usize>,
}

impl Default for FormatRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FormatRegistry {
    pub fn new() -> Self {
        Self {
            formats: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn register(&mut self, def: FormatDefinition) {
        let idx = self.formats.len();
        self.by_name.insert(def.format.name.clone(), idx);
        self.formats.push(def);
    }

    pub fn get_by_name(&self, name: &str) -> Option<&FormatDefinition> {
        self.by_name.get(name).map(|&idx| &self.formats[idx])
    }

    pub fn detect(&self, file_path: &str, data: &[u8]) -> Option<&FormatDefinition> {
        self.formats
            .iter()
            .filter_map(|def| nonzero_score(def, file_path, data))
            .max_by_key(|(_, score)| *score)
            .map(|(def, _)| def)
    }

    pub fn all(&self) -> &[FormatDefinition] {
        &self.formats
    }
}

pub fn extract_params(def: &FormatDefinition, file_path: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();

    for (name, param) in &def.params {
        if let Some(value) = extract_param_from_path(param, file_path) {
            params.insert(name.clone(), value);
        }
    }

    params
}

fn extract_param_from_path(param: &ParamSpec, file_path: &str) -> Option<String> {
    let pattern = param.extract_from_path.as_deref()?;
    let path = normalize_path(file_path);

    let parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    match_and_extract(&parts, &path_parts)
}

fn match_segment(pattern_seg: &str, path_seg: &str) -> bool {
    match pattern_seg {
        "*" | "{}" => true,
        literal => literal.eq_ignore_ascii_case(path_seg),
    }
}

fn match_and_extract(pattern: &[&str], path: &[&str]) -> Option<String> {
    let mut pat_idx = 0;
    let mut path_idx = 0;
    let mut captured = None;

    while pat_idx < pattern.len() {
        if pattern[pat_idx] == "**" {
            return glob_remaining(&pattern[pat_idx + 1..], &path[path_idx..], captured);
        }

        if path_idx >= path.len() {
            return None;
        }

        if !match_segment(pattern[pat_idx], path[path_idx]) {
            return None;
        }

        if pattern[pat_idx] == "{}" {
            captured = Some(path[path_idx].to_string());
        }

        pat_idx += 1;
        path_idx += 1;
    }

    captured
}

fn glob_remaining(rest: &[&str], path: &[&str], captured: Option<String>) -> Option<String> {
    if rest.is_empty() {
        return captured;
    }

    for start in 0..path.len() {
        if let Some(c) = match_and_extract(rest, &path[start..]) {
            return captured.or(Some(c));
        }
    }

    None
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn nonzero_score<'a>(
    def: &'a FormatDefinition,
    file_path: &str,
    data: &[u8],
) -> Option<(&'a FormatDefinition, u32)> {
    let score = score_match(def, file_path, data);
    if score > 0 { Some((def, score)) } else { None }
}

const SCORE_EXTENSION: u32 = 1;
const SCORE_PATH_PATTERN: u32 = 5;
const SCORE_MAGIC_BYTES: u32 = 10;

fn score_match(def: &FormatDefinition, file_path: &str, data: &[u8]) -> u32 {
    let normalized = normalize_path(file_path);

    score_extension(&def.detect, file_path)
        + score_path_pattern(&def.detect, &normalized)
        + score_magic_bytes(&def.detect, data)
}

fn score_extension(detect: &DetectionRules, file_path: &str) -> u32 {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str());

    let Some(ext) = ext else { return 0 };
    let dot_ext = format!(".{ext}");

    if detect.extensions.iter().any(|e| e.eq_ignore_ascii_case(&dot_ext)) {
        SCORE_EXTENSION
    } else {
        0
    }
}

fn score_path_pattern(detect: &DetectionRules, normalized_path: &str) -> u32 {
    let patterns = platform_patterns(detect);

    if patterns.iter().any(|pat| glob_matches(pat, normalized_path)) {
        SCORE_PATH_PATTERN
    } else {
        0
    }
}

fn score_magic_bytes(detect: &DetectionRules, data: &[u8]) -> u32 {
    let Some(ref hex_str) = detect.magic_bytes else { return 0 };
    let Ok(magic) = hex::decode(hex_str) else { return 0 };

    if data.starts_with(&magic) {
        SCORE_MAGIC_BYTES
    } else {
        0
    }
}

fn platform_patterns(detect: &DetectionRules) -> Vec<&str> {
    let platform_key = if cfg!(windows) { "windows" } else { "linux" };
    let mut patterns: Vec<&str> = detect.path_patterns.iter().map(|s| s.as_str()).collect();
    if let Some(plat) = detect.platform.get(platform_key) {
        patterns.extend(plat.path_patterns.iter().map(|s| s.as_str()));
    }
    patterns
}

const GLOB_OPTS: glob::MatchOptions = glob::MatchOptions {
    case_sensitive: false,
    require_literal_separator: false,
    require_literal_leading_dot: false,
};

fn glob_matches(pattern: &str, path: &str) -> bool {
    glob::Pattern::new(pattern)
        .is_ok_and(|p| p.matches_with(path, GLOB_OPTS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bl4_def() -> FormatDefinition {
        let toml_str = include_str!("../../etc/formats/borderlands4.toml");
        toml::from_str(toml_str).unwrap()
    }

    #[test]
    fn register_and_lookup() {
        let mut reg = FormatRegistry::new();
        reg.register(bl4_def());
        assert!(reg.get_by_name("borderlands4").is_some());
        assert!(reg.get_by_name("nonexistent").is_none());
    }

    #[test]
    fn detect_by_extension() {
        let mut reg = FormatRegistry::new();
        reg.register(bl4_def());
        let result = reg.detect("some/path/save.sav", &[]);
        assert!(result.is_some());
        assert_eq!(result.unwrap().format.name, "borderlands4");
    }

    #[test]
    fn detect_no_match() {
        let mut reg = FormatRegistry::new();
        reg.register(bl4_def());
        let result = reg.detect("some/path/save.json", &[]);
        assert!(result.is_none());
    }

    #[test]
    fn extract_steam_id_from_path() {
        let def = bl4_def();
        let path = "C:/Users/me/My Games/Borderlands 4/Saved/SaveGames/76561198012345678/Profiles/client/1.sav";
        let params = extract_params(&def, path);
        assert_eq!(
            params.get("steam_id").map(|s| s.as_str()),
            Some("76561198012345678")
        );
    }

    #[test]
    fn extract_steam_id_backslash_path() {
        let def = bl4_def();
        let path = r"C:\Users\me\My Games\Borderlands 4\Saved\SaveGames\76561198012345678\Profiles\client\1.sav";
        let params = extract_params(&def, path);
        assert_eq!(
            params.get("steam_id").map(|s| s.as_str()),
            Some("76561198012345678")
        );
    }

    #[test]
    fn extract_no_match_returns_empty() {
        let def = bl4_def();
        let path = "C:/Users/me/some_random_dir/1.sav";
        let params = extract_params(&def, path);
        assert!(!params.contains_key("steam_id"));
    }

    #[test]
    fn score_extension_only() {
        let def = bl4_def();
        let score = score_match(&def, "save.sav", &[]);
        assert_eq!(score, 1);
    }

    #[test]
    fn score_path_pattern_higher() {
        let def = bl4_def();
        let path = if cfg!(windows) {
            "C:/Users/me/My Games/Borderlands 4/Saved/SaveGames/12345/test.sav"
        } else {
            "/home/me/.steam/compatdata/1/pfx/something/Borderlands 4/saves/test.sav"
        };
        let score = score_match(&def, path, &[]);
        assert!(score > 1);
    }
}
