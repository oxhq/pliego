/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::PathBuf;

use layout::pages::PageDefinition;

use super::{DEFAULT_LOCALE, DEFAULT_TIMEZONE, READINESS_TIMEOUT_MS};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RenderEnvironment {
    pub locale: &'static str,
    pub timezone: &'static str,
}

impl Default for RenderEnvironment {
    fn default() -> Self {
        Self {
            locale: DEFAULT_LOCALE,
            timezone: DEFAULT_TIMEZONE,
        }
    }
}

impl RenderEnvironment {
    pub(crate) fn artifact(self) -> serde_json::Value {
        serde_json::json!({
            "locale": {
                "requested": self.locale,
                "resolved": self.locale,
            },
            "timezone": {
                "requested": self.timezone,
                "resolved": self.timezone,
            },
        })
    }
}

#[derive(Debug, PartialEq)]
pub struct ExplicitRenderPaths {
    pub output: PathBuf,
    pub artifacts: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ResourcePolicyConfig {
    pub allowed_http_roots: Vec<url::Url>,
    pub virtual_resources: Vec<VirtualResourceSpec>,
    pub asset_manifest: Option<PathBuf>,
    pub timeout_ms: u64,
}

impl Default for ResourcePolicyConfig {
    fn default() -> Self {
        Self {
            allowed_http_roots: Vec::new(),
            virtual_resources: Vec::new(),
            asset_manifest: None,
            timeout_ms: READINESS_TIMEOUT_MS,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VirtualResourceSpec {
    pub url: url::Url,
    pub path: PathBuf,
}

#[derive(Debug, PartialEq)]
pub struct RenderRequest {
    pub input: PathBuf,
    pub environment: RenderEnvironment,
    pub page: PageDefinition,
    pub resources: ResourcePolicyConfig,
    pub allow_host_fonts: bool,
    pub allow_partial_scene: bool,
    pub explicit_paths: Option<ExplicitRenderPaths>,
}

#[derive(Debug)]
pub struct RenderOutcome {
    pub summary: serde_json::Value,
}

#[derive(Debug, PartialEq)]
pub struct RenderError {
    pub code: String,
    pub message: String,
    pub exit_code: i32,
    pub artifacts: Option<PathBuf>,
    pub document_pdf: Option<PathBuf>,
    pub render_id: Option<String>,
    pub warnings: Vec<String>,
}

impl RenderError {
    pub fn request(code: &str, message: impl Into<String>) -> Self {
        Self::without_publication(code, message, 2)
    }

    pub fn without_publication(code: &str, message: impl Into<String>, exit_code: i32) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            exit_code,
            artifacts: None,
            document_pdf: None,
            render_id: None,
            warnings: Vec::new(),
        }
    }

    pub fn session(
        artifacts: &std::path::Path,
        document_pdf: &std::path::Path,
        render_id: &str,
        code: &str,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.to_owned(),
            message: message.into(),
            exit_code: 1,
            artifacts: Some(artifacts.to_path_buf()),
            document_pdf: Some(document_pdf.to_path_buf()),
            render_id: Some(render_id.to_owned()),
            warnings: Vec::new(),
        }
    }
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RenderError {}

pub struct DocumentEngine;

impl DocumentEngine {
    pub fn render(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
        crate::render(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_input_returns_the_exact_typed_request_error() {
        let input = PathBuf::from("__pliego_document_engine_missing__.html");
        let expected_message = format!(
            "document is unavailable: {}",
            std::path::Path::new(".")
                .canonicalize()
                .unwrap()
                .join(&input)
                .display()
        );
        let error = DocumentEngine::render(RenderRequest {
            input,
            environment: RenderEnvironment::default(),
            page: crate::default_page(),
            resources: ResourcePolicyConfig::default(),
            allow_host_fonts: false,
            allow_partial_scene: false,
            explicit_paths: None,
        })
        .unwrap_err();

        assert_eq!(error.code, "INVALID_REQUEST");
        assert_eq!(error.message, expected_message);
        assert_eq!(error.exit_code, 2);
        assert_eq!(error.artifacts, None);
        assert_eq!(error.document_pdf, None);
        assert_eq!(error.render_id, None);
        assert!(error.warnings.is_empty());
    }
}
