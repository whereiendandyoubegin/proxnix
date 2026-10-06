use serde_json::Value;

use crate::types::{AppError, ParsedWebhook, Result};

fn is_commit_hash(s: &str) -> bool {
    s.len() == 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_ssh_repo_url(s: &str) -> bool {
    s.contains("ssh://") && s.contains(".git")
}

fn field(webhook: &Value, pointer: &str, predicate: &impl Fn(&str) -> bool) -> Option<String> {
    webhook
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|s| predicate(s))
        .map(str::to_string)
}

pub fn webhook_parse(webhook: serde_json::Value) -> Result<ParsedWebhook> {
    let hash = field(&webhook, "/after", &is_commit_hash)
        .or_else(|| find_string(&webhook, &is_commit_hash))
        .ok_or(AppError::ParsingModuleError(
            "could not find commit hash".to_string(),
        ))?;

    let repo = field(&webhook, "/repository/ssh_url", &is_ssh_repo_url)
        .or_else(|| find_string(&webhook, &is_ssh_repo_url))
        .ok_or(AppError::ParsingModuleError(
            "could not find repo url".to_string(),
        ))?;

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
            for a in array {
                let result = find_string(a, predicate);
                if result.is_some() {
                    return result;
                }
            }
            None
        }
        Value::Object(map) => {
            for v in map.values() {
                let result = find_string(v, predicate);
                if result.is_some() {
                    return result;
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const AFTER: &str = "767f66531ee8b3d07e53ef6e2a1a7b0e1b3c0d4f";
    const BEFORE: &str = "5d0ae7351ee8b3d07e53ef6e2a1a7b0e1b3c0d4f";

    fn push(message: &str) -> Value {
        json!({
            "after": AFTER,
            "before": BEFORE,
            "commits": [{ "id": AFTER, "message": message }],
            "ref": "refs/heads/main",
            "repository": {
                "clone_url": "https://git.thesta.rs/dan/nixology.git",
                "ssh_url": "ssh://git@forgejo.thesta.rs:2222/dan/nixology.git"
            }
        })
    }

    #[test]
    fn reads_the_pushed_commit_and_repo_ssh_url() {
        let parsed = webhook_parse(push("bump")).unwrap();
        assert_eq!(parsed.hash, AFTER);
        assert_eq!(
            parsed.repository,
            "ssh://git@forgejo.thesta.rs:2222/dan/nixology.git"
        );
    }

    #[test]
    fn a_commit_message_quoting_an_ssh_url_does_not_redirect_the_clone() {
        let parsed = webhook_parse(push("point updater at ssh://git@evil.example/x.git")).unwrap();
        assert_eq!(
            parsed.repository,
            "ssh://git@forgejo.thesta.rs:2222/dan/nixology.git"
        );
    }

    #[test]
    fn falls_back_to_searching_when_the_usual_fields_are_missing() {
        let parsed = webhook_parse(json!({
            "head": { "sha": AFTER },
            "repo": { "ssh": "ssh://git@forgejo.thesta.rs:2222/dan/nixology.git" }
        }))
        .unwrap();
        assert_eq!(parsed.hash, AFTER);
        assert_eq!(
            parsed.repository,
            "ssh://git@forgejo.thesta.rs:2222/dan/nixology.git"
        );
    }

    #[test]
    fn a_payload_without_a_repo_url_is_rejected() {
        assert!(webhook_parse(json!({ "after": AFTER })).is_err());
    }
}
