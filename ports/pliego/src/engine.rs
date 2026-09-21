/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::path::PathBuf;

pub use super::render_environment::RenderEnvironment;
use super::resource_policy::ResourcePolicyConfig;
pub use super::runtime_policy::DeterministicRuntimePolicy;
use crate::PageDefinition;

#[derive(Debug, PartialEq)]
pub struct ExplicitRenderPaths {
    pub output: PathBuf,
    pub artifacts: PathBuf,
}

#[derive(Debug, PartialEq)]
pub struct RenderRequest {
    pub input: PathBuf,
    pub environment: RenderEnvironment,
    pub page: PageDefinition,
    pub resources: ResourcePolicyConfig,
    /// Full normalized deterministic-time and settlement identity.
    pub runtime_policy: DeterministicRuntimePolicy,
    pub allow_host_fonts: bool,
    pub allow_partial_scene: bool,
    pub explicit_paths: Option<ExplicitRenderPaths>,
}

#[derive(Debug)]
pub struct RenderOutcome {
    #[allow(dead_code)]
    // Direct renderer callers consume the structured result; the CLI writes cli_bytes.
    pub summary: serde_json::Value,
    pub(crate) cli_bytes: Vec<u8>,
}

impl RenderOutcome {
    pub(crate) fn from_summary(summary: serde_json::Value) -> Result<Self, serde_json::Error> {
        let mut cli_bytes = serde_json::to_vec(&summary)?;
        cli_bytes.push(b'\n');
        let summary = serde_json::from_slice(&cli_bytes)?;
        Ok(Self { summary, cli_bytes })
    }

    pub(crate) fn from_sealed(summary: serde_json::Value, cli_bytes: Vec<u8>) -> Self {
        Self { summary, cli_bytes }
    }
}

#[derive(Debug, PartialEq)]
pub struct RenderError {
    pub code: String,
    pub message: String,
    pub exit_code: u8,
    pub artifacts: Option<PathBuf>,
    pub document_pdf: Option<PathBuf>,
    pub render_id: Option<String>,
    pub warnings: Vec<String>,
}

impl RenderError {
    pub fn request(code: &str, message: impl Into<String>) -> Self {
        Self::without_publication(code, message, 2)
    }

    pub fn without_publication(code: &str, message: impl Into<String>, exit_code: u8) -> Self {
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

#[cfg(all(
    feature = "document-session",
    not(any(target_os = "android", target_env = "ohos"))
))]
impl From<super::document_session::SessionError> for RenderError {
    fn from(error: super::document_session::SessionError) -> Self {
        if error.code == "INVALID_REQUEST" {
            Self::request(&error.code, error.message)
        } else {
            Self::without_publication(&error.code, error.message, 1)
        }
    }
}

pub struct DocumentEngine;

impl DocumentEngine {
    /// Render with the selected runtime. DocumentSession wins whenever it is
    /// enabled; an oracle-only build preserves the pre-cutover explicit API.
    #[cfg(any(feature = "document-session", feature = "shell-oracle"))]
    #[allow(dead_code)]
    pub fn render(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
        #[cfg(feature = "document-session")]
        {
            crate::render(request)
        }
        #[cfg(all(not(feature = "document-session"), feature = "shell-oracle"))]
        {
            crate::render_with_shell_oracle(request)
        }
    }

    /// Render through the generation-bound controlled capture transaction used by the default CLI
    /// and supported SDK path. It never falls back to realtime capture.
    #[cfg(feature = "document-session")]
    pub fn render_controlled(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
        crate::render_controlled(request)
    }

    /// Exercise the pre-cutover servoshell path as an explicit, nonproduction
    /// parity oracle. This entrypoint does not participate in default builds.
    #[cfg(feature = "shell-oracle")]
    pub fn render_with_shell_oracle(request: RenderRequest) -> Result<RenderOutcome, RenderError> {
        crate::render_with_shell_oracle(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(all(feature = "shell-oracle", not(feature = "document-session")))]
    #[test]
    fn oracle_only_build_preserves_the_render_api_and_typed_oracle_entrypoint() {
        let _: fn(RenderRequest) -> Result<RenderOutcome, RenderError> = DocumentEngine::render;
        let _: fn(RenderRequest) -> Result<RenderOutcome, RenderError> =
            DocumentEngine::render_with_shell_oracle;
    }

    #[cfg(feature = "document-session")]
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
            runtime_policy: DeterministicRuntimePolicy::default(),
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

    #[cfg(feature = "shell-oracle")]
    #[test]
    fn shell_oracle_missing_input_returns_the_exact_typed_request_error() {
        let input = PathBuf::from("__pliego_shell_oracle_missing__.html");
        let expected_message = format!(
            "document is unavailable: {}",
            std::path::Path::new(".")
                .canonicalize()
                .unwrap()
                .join(&input)
                .display()
        );
        let error = DocumentEngine::render_with_shell_oracle(RenderRequest {
            input,
            environment: RenderEnvironment::default(),
            page: crate::default_page(),
            resources: ResourcePolicyConfig::default(),
            runtime_policy: DeterministicRuntimePolicy::default(),
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

    #[cfg(all(
        feature = "document-session",
        not(any(target_os = "android", target_env = "ohos"))
    ))]
    #[test]
    fn session_failures_preserve_the_engine_error_classes() {
        let request_error = super::super::document_session::DocumentSession::new(
            "__pliego_document_session_missing__.html",
            RenderEnvironment::default(),
            crate::default_page(),
            ResourcePolicyConfig::default(),
            false,
            super::super::readiness::ReadinessPolicy::default(),
        )
        .err()
        .expect("missing input should return a session error");
        let expected_request_message = request_error.message.clone();
        let request_error: RenderError = request_error.into();

        assert_eq!(request_error.code, "INVALID_REQUEST");
        assert_eq!(request_error.message, expected_request_message);
        assert_eq!(request_error.exit_code, 2);
        assert_eq!(request_error.artifacts, None);
        assert_eq!(request_error.document_pdf, None);
        assert_eq!(request_error.render_id, None);
        assert!(request_error.warnings.is_empty());

        let layout_error: RenderError = super::super::document_session::SessionError::new(
            "LAYOUT_CONFIGURATION_FAILED",
            "already configured",
        )
        .into();
        assert_eq!(layout_error.code, "LAYOUT_CONFIGURATION_FAILED");
        assert_eq!(layout_error.message, "already configured");
        assert_eq!(layout_error.exit_code, 1);
        assert_eq!(layout_error.artifacts, None);
        assert_eq!(layout_error.document_pdf, None);
        assert_eq!(layout_error.render_id, None);
        assert!(layout_error.warnings.is_empty());

        let session_error: RenderError =
            super::super::document_session::SessionError::new("RESOURCE_DENIED", "blocked").into();

        assert_eq!(session_error.code, "RESOURCE_DENIED");
        assert_eq!(session_error.message, "blocked");
        assert_eq!(session_error.exit_code, 1);
        assert_eq!(session_error.artifacts, None);
        assert_eq!(session_error.document_pdf, None);
        assert_eq!(session_error.render_id, None);
        assert!(session_error.warnings.is_empty());
    }
}
