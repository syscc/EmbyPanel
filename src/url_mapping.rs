use regex::{Regex, escape};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone)]
pub struct UrlMappingRule {
    from: Regex,
    to: String,
}

impl UrlMappingRule {
    fn parse(line: &str, line_number: usize) -> AppResult<Option<Self>> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }

        let Some((from, to)) = line.split_once("=>") else {
            return Err(AppError::Config(format!(
                "invalid strm_url_mappings line {line_number}: expected `from => to`"
            )));
        };

        let from = from.trim();
        let to = to.trim();
        if from.is_empty() || to.is_empty() {
            return Err(AppError::Config(format!(
                "invalid strm_url_mappings line {line_number}: from and to cannot be empty"
            )));
        }

        let pattern = from
            .strip_prefix("regex:")
            .map(str::trim)
            .map(str::to_string)
            .unwrap_or_else(|| normalize_literal_pattern(from));
        let from = Regex::new(&pattern).map_err(|err| {
            AppError::Config(format!(
                "invalid strm_url_mappings regex at line {line_number}: {err}"
            ))
        })?;

        Ok(Some(Self {
            from,
            to: to.to_string(),
        }))
    }

    fn apply(&self, value: &str) -> String {
        self.from.replace_all(value, self.to.as_str()).into_owned()
    }
}

pub fn parse_rules(text: &str) -> AppResult<Vec<UrlMappingRule>> {
    text.lines()
        .enumerate()
        .filter_map(|(index, line)| UrlMappingRule::parse(line, index + 1).transpose())
        .collect()
}

pub fn apply_rules(value: &str, rules: &[UrlMappingRule]) -> String {
    rules
        .iter()
        .fold(value.to_string(), |current, rule| rule.apply(&current))
}

fn normalize_literal_pattern(value: &str) -> String {
    escape(&value.replace("\\.", "."))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_applies_url_mapping_rules() {
        let rules =
            parse_rules("https://source.example.com => http://media-gateway.local:5244").unwrap();
        let mapped = apply_rules("https://source.example.com/d/videos/movie.mkv", &rules);

        assert_eq!(mapped, "http://media-gateway.local:5244/d/videos/movie.mkv");
    }

    #[test]
    fn keeps_compatibility_with_escaped_dot_literal_rules() {
        let rules =
            parse_rules("https://source\\.example\\.com => http://media-gateway.local:5244")
                .unwrap();
        let mapped = apply_rules("https://source.example.com/d/videos/movie.mkv", &rules);

        assert_eq!(mapped, "http://media-gateway.local:5244/d/videos/movie.mkv");
    }

    #[test]
    fn supports_explicit_regex_rules() {
        let rules = parse_rules(
            "regex:https://(source|mirror)\\.example\\.test => http://media-gateway.local:5244",
        )
        .unwrap();
        let mapped = apply_rules("https://mirror.example.test/d/videos/movie.mkv", &rules);

        assert_eq!(mapped, "http://media-gateway.local:5244/d/videos/movie.mkv");
    }

    #[test]
    fn ignores_blank_and_comment_lines() {
        let rules = parse_rules("\n# comment\n").unwrap();
        assert!(rules.is_empty());
    }

    #[test]
    fn rejects_invalid_mapping_lines() {
        assert!(parse_rules("https://example.test").is_err());
    }
}
