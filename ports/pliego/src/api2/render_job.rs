/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Profile-null mapping from a validated API 2 request into controlled runtime types.

#[cfg(test)]
use std::collections::BTreeMap;

use layout::pages::PageDefinition;
use serde_json::Value;

use super::input_job::ResolvedInputJob;
#[cfg(test)]
use super::input_job::resolve_input_job_for_test;
use super::{
    ENVIRONMENT_FIELDS, InvocationError, LIMIT_FIELDS, MARGIN_FIELDS, PAGE_FIELDS, RESOURCE_FIELDS,
    SETTLEMENT_FIELDS, TIME_FIELDS, closed_object, required, required_string, required_u64,
    validate_page_size, validate_request,
};
use crate::render_environment::RenderEnvironment;
use crate::resource_policy::ResourcePolicy;
use crate::runtime_policy::{
    DeterministicRuntimePolicy, DocumentSettlementLimits, DocumentSettlementPolicy,
    DocumentTimePolicy, InfiniteSourcePolicy,
};

#[derive(Debug)]
pub(crate) struct ResolvedRenderJob {
    input: ResolvedInputJob,
    environment: RenderEnvironment,
    page: PageDefinition,
    resources: ResourcePolicy,
    allow_host_fonts: bool,
    runtime_policy: DeterministicRuntimePolicy,
}

pub(crate) struct ResolvedRenderJobParts {
    pub(crate) input: ResolvedInputJob,
    pub(crate) environment: RenderEnvironment,
    pub(crate) page: PageDefinition,
    pub(crate) resources: ResourcePolicy,
    pub(crate) allow_host_fonts: bool,
    pub(crate) runtime_policy: DeterministicRuntimePolicy,
}

impl ResolvedRenderJob {
    pub(crate) fn from_resolved_input(
        request: Value,
        input: ResolvedInputJob,
    ) -> Result<Self, InvocationError> {
        validate_request(&request).map_err(InvocationError::new)?;
        input.require_request_binding(&request)?;
        let root =
            closed_object(&request, "$", super::TOP_LEVEL_FIELDS).map_err(InvocationError::new)?;
        if !required(root, "$", "profile")
            .map_err(InvocationError::new)?
            .is_null()
        {
            return Err(InvocationError::new(
                "the inactive render plan supports only profile: null",
            ));
        }

        let environment = closed_object(
            required(root, "$", "environment").map_err(InvocationError::new)?,
            "$.environment",
            ENVIRONMENT_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let locale = match required_string(environment, "$.environment", "locale")
            .map_err(InvocationError::new)?
        {
            "en-US" => "en-US",
            "es-MX" => "es-MX",
            _ => unreachable!("validate_request accepted an unknown locale"),
        };
        let timezone = match required_string(environment, "$.environment", "timezone")
            .map_err(InvocationError::new)?
        {
            "UTC" => "UTC",
            "America/Tijuana" => "America/Tijuana",
            _ => unreachable!("validate_request accepted an unknown timezone"),
        };

        let page = closed_object(
            required(root, "$", "page").map_err(InvocationError::new)?,
            "$.page",
            PAGE_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let (width, height) =
            validate_page_size(required(page, "$.page", "size").map_err(InvocationError::new)?)
                .map_err(InvocationError::new)?;
        let margins = closed_object(
            required(page, "$.page", "margins_app_units").map_err(InvocationError::new)?,
            "$.page.margins_app_units",
            MARGIN_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let app_unit = |field| {
            required_u64(
                margins,
                "$.page.margins_app_units",
                field,
                0,
                i32::MAX as u64,
            )
            .map_err(InvocationError::new)
            .and_then(|value| {
                i32::try_from(value)
                    .map_err(|_| InvocationError::new("validated page app unit did not fit i32"))
            })
        };
        let page = PageDefinition::from_app_units(
            i32::try_from(width)
                .map_err(|_| InvocationError::new("validated page width did not fit i32"))?,
            i32::try_from(height)
                .map_err(|_| InvocationError::new("validated page height did not fit i32"))?,
            [
                app_unit("top")?,
                app_unit("right")?,
                app_unit("bottom")?,
                app_unit("left")?,
            ],
        )
        .map_err(|error| InvocationError::new(format!("invalid API 2 page geometry: {error}")))?;

        let resources = closed_object(
            required(root, "$", "resources").map_err(InvocationError::new)?,
            "$.resources",
            RESOURCE_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let resource_policy = match (
            required_string(resources, "$.resources", "network").map_err(InvocationError::new)?,
            required_string(resources, "$.resources", "host_fonts")
                .map_err(InvocationError::new)?,
        ) {
            ("deny", "deny") => ResourcePolicy::default(),
            _ => unreachable!("validate_request accepted an unsupported resource policy"),
        };

        let time = closed_object(
            required(root, "$", "time").map_err(InvocationError::new)?,
            "$.time",
            TIME_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let epoch_unix_ms = required(time, "$.time", "epoch_unix_ms")
            .map_err(InvocationError::new)?
            .as_i64()
            .expect("validate_request accepted a non-i64 epoch");
        let settlement = closed_object(
            required(root, "$", "settlement").map_err(InvocationError::new)?,
            "$.settlement",
            SETTLEMENT_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let limits = closed_object(
            required(settlement, "$.settlement", "limits").map_err(InvocationError::new)?,
            "$.settlement.limits",
            LIMIT_FIELDS,
        )
        .map_err(InvocationError::new)?;
        let limit = |field, minimum| {
            required_u64(
                limits,
                "$.settlement.limits",
                field,
                minimum,
                if field == "virtual_span_ms" {
                    9_007_199_254_740_991
                } else {
                    u64::from(u32::MAX)
                },
            )
            .map_err(InvocationError::new)
        };
        let runtime_policy = DeterministicRuntimePolicy {
            time: DocumentTimePolicy {
                epoch_unix_ms,
                initial_offset_ns: 0,
            },
            settlement: DocumentSettlementPolicy {
                infinite_source_policy: InfiniteSourcePolicy::Fail,
                empty_checkpoints: 2,
                limits: DocumentSettlementLimits {
                    virtual_span_ms: limit("virtual_span_ms", 1)?,
                    ordinary_tasks: limit("ordinary_tasks", 1)?,
                    microtasks: limit("microtasks", 1)?,
                    rendering_opportunities: limit("rendering_opportunities", 1)?,
                    mutations: limit("mutations", 1)?,
                    post_readiness_resources: 1_024,
                    process_cpu_ms: 30_000,
                    host_wall_ms: limit("host_wall_ms", 1)?,
                },
            },
        }
        .validate()
        .map_err(|error| InvocationError::new(error.to_string()))?;

        Ok(Self {
            input,
            environment: RenderEnvironment { locale, timezone },
            page,
            resources: resource_policy,
            allow_host_fonts: false,
            runtime_policy,
        })
    }

    pub(crate) fn into_parts(self) -> ResolvedRenderJobParts {
        ResolvedRenderJobParts {
            input: self.input,
            environment: self.environment,
            page: self.page,
            resources: self.resources,
            allow_host_fonts: self.allow_host_fonts,
            runtime_policy: self.runtime_policy,
        }
    }
}

#[cfg(test)]
pub(crate) fn resolve_render_job_for_test(
    request: &Value,
    canonical_manifest: &[u8],
    bodies: BTreeMap<String, Vec<u8>>,
) -> Result<ResolvedRenderJob, InvocationError> {
    let input = resolve_input_job_for_test(request, canonical_manifest, bodies)?;
    ResolvedRenderJob::from_resolved_input(request.clone(), input)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::api2::decode_render_request;

    const A4_REQUEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/goldens/accepted/render-request.a4.json"
    ));
    const EXPLICIT_REQUEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/goldens/accepted/render-request.explicit-page.json"
    ));

    fn fixture_job(frame: &[u8]) -> ResolvedRenderJob {
        let mut reader: &[u8] = frame;
        let request = decode_render_request(&mut reader).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/api2/fixtures");
        let manifest = std::fs::read(root.join("input-manifest.json")).unwrap();
        let mut bodies = BTreeMap::new();
        for path in [
            "assets/fixture-font.bin",
            "assets/mark.svg",
            "document.html",
            "styles.css",
        ] {
            bodies.insert(
                path.to_owned(),
                std::fs::read(root.join("input").join(path)).unwrap(),
            );
        }
        resolve_render_job_for_test(&request, &manifest, bodies).unwrap()
    }

    #[test]
    fn maps_the_complete_profile_null_request_to_the_enforced_runtime_policy() {
        let a4 = fixture_job(A4_REQUEST).into_parts();
        assert_eq!(a4.environment, RenderEnvironment::default());
        assert_eq!(a4.page.width(), 47_622.0 / 60.0);
        assert_eq!(a4.page.height(), 67_351.0 / 60.0);
        assert!(a4.resources.allowed_http_roots.is_empty());
        assert!(a4.resources.virtual_resources.is_empty());
        assert!(a4.resources.resolved_document_root().is_none());
        assert!(!a4.allow_host_fonts);
        assert_eq!(a4.runtime_policy, DeterministicRuntimePolicy::default());

        let explicit = fixture_job(EXPLICIT_REQUEST).into_parts();
        assert_eq!(
            explicit.environment,
            RenderEnvironment {
                locale: "es-MX",
                timezone: "America/Tijuana",
            }
        );
        assert_eq!(explicit.page.width(), 612.0);
        assert_eq!(explicit.page.height(), 792.0);
        assert_eq!(explicit.page.margins().top, 36.0);
        assert_eq!(explicit.runtime_policy.time.epoch_unix_ms, 0);
        assert_eq!(
            explicit.runtime_policy.settlement.limits,
            DocumentSettlementLimits {
                virtual_span_ms: 172_800_000,
                ordinary_tasks: 200_000,
                microtasks: 2_000_000,
                rendering_opportunities: 20_000,
                mutations: 2_000_000,
                post_readiness_resources: 1_024,
                process_cpu_ms: 30_000,
                host_wall_ms: 120_000,
            }
        );
    }

    #[test]
    fn inactive_plan_rejects_a_profile_before_a_session_can_start() {
        let mut reader: &[u8] = A4_REQUEST;
        let mut request = decode_render_request(&mut reader).unwrap();
        request["profile"] = serde_json::json!({
            "schema": "pliego.profile.future",
            "version": 1,
        });
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/api2/fixtures");
        let manifest = std::fs::read(root.join("input-manifest.json")).unwrap();
        let mut bodies = BTreeMap::new();
        for path in [
            "assets/fixture-font.bin",
            "assets/mark.svg",
            "document.html",
            "styles.css",
        ] {
            bodies.insert(
                path.to_owned(),
                std::fs::read(root.join("input").join(path)).unwrap(),
            );
        }
        let error = resolve_render_job_for_test(&request, &manifest, bodies).unwrap_err();
        assert!(error.to_string().contains("supports only profile: null"));
    }

    #[test]
    fn resolved_input_cannot_be_repaired_with_a_different_request_descriptor() {
        let mut reader: &[u8] = A4_REQUEST;
        let request = decode_render_request(&mut reader).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contracts/api2/fixtures");
        let manifest = std::fs::read(root.join("input-manifest.json")).unwrap();
        let mut bodies = BTreeMap::new();
        for path in [
            "assets/fixture-font.bin",
            "assets/mark.svg",
            "document.html",
            "styles.css",
        ] {
            bodies.insert(
                path.to_owned(),
                std::fs::read(root.join("input").join(path)).unwrap(),
            );
        }
        let input = resolve_input_job_for_test(&request, &manifest, bodies).unwrap();
        let mut changed = request;
        changed["input"]["manifest"]["sha256"] =
            serde_json::Value::from(format!("sha256:{}", "0".repeat(64)));
        let error = ResolvedRenderJob::from_resolved_input(changed, input).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("$.input.manifest: SHA-256 does not match supplied manifest bytes")
        );
    }
}
