use serde_json::Value;

use crate::types::{AppError, ParsedWebhook, Result};

pub fn webhook_parse(webhook: serde_json::Value) -> Result<ParsedWebhook> {
    let hash = find_string(&webhook, &|s| {
        s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
    })
    .ok_or(AppError::ParsingModuleError(
        "could not find commit hash".to_string(),
    ))?;

    let repo = find_string(&webhook, &|s| s.contains("ssh://") && s.contains(".git")).ok_or(
        AppError::ParsingModuleError("could not find repo url".to_string()),
    )?;

    Ok(ParsedWebhook {
        repository: repo,
        hash,
    })
}

pub fn find_string(json: &serde_json::Value, predicate: &impl Fn(&str) -> bool) -> Option<String> {
    match json {
        Value::String(s) => {
            if predicate(s) {
                Some(s.clone())
            } else {
                None
            }
        }
        Value::Array(array) => {
            for (a) in array {
                let result = find_string(a, predicate);
                if result.is_some() {
                    return result;
                }
            }
            return None;
        }
        Value::Object(map) => {
            for v in map.values() {
                let result = find_string(v, predicate);
                if result.is_some() {
                    return result;
                }
            }
            return None;
        }
        _ => {
            return None;
        }
    }
}
