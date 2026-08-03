/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use serde_json::Value;

const SCRIPT: &str = include_str!("readiness.js");
const TIMEOUT_TOKEN: &str = "__PLIEGO_TIMEOUT_MS__";

pub const HOST_EVALUATION_EXPRESSION: &str = "JSON.stringify(window.__pliegoReadiness ?? null)";

pub fn document_start_script(timeout_ms: u64) -> String {
    SCRIPT.replace(TIMEOUT_TOKEN, &timeout_ms.to_string())
}

#[derive(Debug, PartialEq)]
pub enum Readiness {
    Pending,
    Ready { payload: Value },
    Failed { error: ReadinessError },
}

#[derive(Debug, PartialEq)]
pub struct ReadinessError {
    pub code: String,
    pub message: String,
}

pub fn parse_snapshot(json: &str) -> Result<Readiness, String> {
    let value: Value =
        serde_json::from_str(json).map_err(|error| format!("invalid readiness JSON: {error}"))?;
    let snapshot = value
        .as_object()
        .ok_or_else(|| "readiness shim is not installed".to_owned())?;
    let status = snapshot
        .get("status")
        .and_then(Value::as_str)
        .ok_or_else(|| "readiness snapshot has no status".to_owned())?;

    match status {
        "pending" => Ok(Readiness::Pending),
        "ready" => {
            if snapshot.get("font_status").and_then(Value::as_str) != Some("loaded") {
                return Err("ready snapshot has no loaded font proof".to_owned());
            }
            Ok(Readiness::Ready {
                payload: snapshot
                    .get("payload")
                    .cloned()
                    .ok_or_else(|| "ready snapshot has no payload".to_owned())?,
            })
        },
        "failed" => {
            let error = snapshot
                .get("error")
                .and_then(Value::as_object)
                .ok_or_else(|| "failed snapshot has no error".to_owned())?;
            let field = |name| {
                error
                    .get(name)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| format!("readiness error has no {name}"))
            };
            Ok(Readiness::Failed {
                error: ReadinessError {
                    code: field("code")?,
                    message: field("message")?,
                },
            })
        },
        other => Err(format!("unknown readiness status: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Readiness, ReadinessError, document_start_script, parse_snapshot};

    #[test]
    fn builds_a_script_with_the_requested_timeout() {
        let script = document_start_script(2500);
        assert!(!script.contains("__PLIEGO_TIMEOUT_MS__"));
        assert!(script.contains("timed out after 2500 ms"));
        assert!(script.contains("}), 2500);"));
        assert!(script.contains("classList.add(\"test-wait\")"));
        assert!(script.contains("document.fonts.ready.then"));
        assert!(script.contains("addEventListener(\"load\", waitForFonts"));
        assert!(script.contains("font_status: \"loaded\""));
    }

    #[test]
    fn parses_pending_ready_and_failed_snapshots() {
        assert_eq!(
            parse_snapshot(r#"{"status":"pending"}"#).unwrap(),
            Readiness::Pending
        );
        assert_eq!(
            parse_snapshot(r#"{"status":"ready","payload":{"pages":2},"font_status":"loaded"}"#)
                .unwrap(),
            Readiness::Ready {
                payload: json!({ "pages": 2 })
            }
        );
        assert_eq!(
            parse_snapshot(
                r#"{"status":"failed","error":{"code":"NO_DATA","message":"missing rows"}}"#
            )
            .unwrap(),
            Readiness::Failed {
                error: ReadinessError {
                    code: "NO_DATA".to_owned(),
                    message: "missing rows".to_owned(),
                }
            }
        );
    }

    #[test]
    fn rejects_missing_or_malformed_snapshots() {
        assert!(parse_snapshot("null").is_err());
        assert!(parse_snapshot(r#"{"status":"ready"}"#).is_err());
        assert!(
            parse_snapshot(r#"{"status":"ready","payload":null,"font_status":"loading"}"#).is_err()
        );
        assert!(parse_snapshot(r#"{"status":"unknown"}"#).is_err());
        assert!(parse_snapshot("not-json").is_err());
    }
}
