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

    pub fn detect(
        &self,
        file_path: &str,
        data: &[u8],
    ) -> Option<&FormatDefinition> {
        self.formats
            .iter()
            .filter_map(|def| {
                let score = score_match(def, file_path, data);
                if score > 0 {
                    Some((def, score))
                } else {
                    None
                }
            })
            .max_by_key(|(_, score)| *score)
            .map(|(def, _)| def)
    }

    pub fn all(&self) -> &[FormatDefinition] {
        &self.formats
    }
}

pub fn extract_params(
    def: &FormatDefinition,
    file_path: &str,
) -> HashMap<String, String> {
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

fn match_and_extract(pattern_parts: &[&str], path_parts: &[&str]) -> Option<String> {
    let mut pi = 0;
    let mut captured = None;

    for pp in pattern_parts {
        if *pp == "**" {
            let remaining_pattern = &pattern_parts[pattern_parts.iter().position(|x| *x == "**").unwrap() + 1..];
            if remaining_pattern.is_empty() {
                return captured;
            }

            for start in pi..path_parts.len() {
                if let Some(c) = match_and_extract(remaining_pattern, &path_parts[start..]) {
                    return Some(c);
                }
                if captured.is_none() {
                    if let Some(c) = try_match_segment(remaining_pattern, &path_parts[start..]) {
                        return Some(c);
                    }
                }
            }
            return None;
        }

        if pi >= path_parts.len() {
            return None;
        }

        if *pp == "{}" {
            captured = Some(path_parts[pi].to_string());
            pi += 1;
        } else if *pp == "*" || pp.eq_ignore_ascii_case(path_parts[pi]) {
            pi += 1;
        } else {
            return None;
        }
    }

    captured
}

fn try_match_segment(pattern_parts: &[&str], path_parts: &[&str]) -> Option<String> {
    if pattern_parts.is_empty() || path_parts.is_empty() {
        return None;
    }
    match_and_extract(pattern_parts, path_parts)
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn score_match(def: &FormatDefinition, file_path: &str, data: &[u8]) -> u32 {
    let mut score = 0u32;
    let path = Path::new(file_path);
    let normalized = normalize_path(file_path);

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let dot_ext = format!(".{ext}");
        if def
            .detect
            .extensions
            .iter()
            .any(|e| e.eq_ignore_ascii_case(&dot_ext))
        {
            score += 1;
        }
    }

    let patterns = platform_patterns(&def.detect);
    for pat in &patterns {
        if glob_matches(pat, &normalized) {
            score += 5;
            break;
        }
    }

    if let Some(ref hex_str) = def.detect.magic_bytes {
        if let Ok(magic) = hex::decode(hex_str) {
            if data.starts_with(&magic) {
                score += 10;
            }
        }
    }

    score
}

fn platform_patterns(detect: &DetectionRules) -> Vec<&str> {
    let platform_key = if cfg!(windows) { "windows" } else { "linux" };
    let mut patterns: Vec<&str> =
        detect.path_patterns.iter().map(|s| s.as_str()).collect();
    if let Some(plat) = detect.platform.get(platform_key) {
        patterns.extend(plat.path_patterns.iter().map(|s| s.as_str()));
    }
    patterns
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    glob::Pattern::new(pattern)
        .map(|p| {
            let opts = glob::MatchOptions {
                case_sensitive: false,
                require_literal_separator: false,
                require_literal_leading_dot: false,
            };
            p.matches_with(path, opts)
        })
        .unwrap_or(false)
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
        assert!(params.get("steam_id").is_none());
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
