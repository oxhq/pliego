/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Unreleased API 2 executable framing and strict request decoding.
//!
//! The current executable advertises no complete render tuple. This module deliberately stops
//! after strict request decoding so it cannot accidentally route an API 2 request through API 1 or
//! the nonproduction servoshell oracle.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::File;
use std::io::{self, Read, Write};

use serde::Serialize;
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};

pub(crate) const API_VERSION: u32 = 2;
pub(crate) const REQUEST_MAX_BYTES: usize = 1_048_576;
pub(crate) const INVOCATION_ERROR_EXIT_CODE: i32 = 64;

const SOURCE_COMMIT: &str = env!("PLIEGO_SOURCE_COMMIT");
const BUILD_TARGET: &str = env!("PLIEGO_BUILD_TARGET");

const TOP_LEVEL_FIELDS: &[&str] = &[
    "schema",
    "version",
    "api",
    "profile",
    "input",
    "environment",
    "page",
    "resources",
    "time",
    "settlement",
    "diagnostics",
];
const PROFILE_FIELDS: &[&str] = &["schema", "version"];
const INPUT_FIELDS: &[&str] = &["entrypoint", "manifest"];
const MANIFEST_FIELDS: &[&str] = &["path", "media_type", "sha256", "bytes"];
const ENVIRONMENT_FIELDS: &[&str] = &["locale", "timezone"];
const PAGE_FIELDS: &[&str] = &["size", "margins_app_units", "css_page_precedence"];
const NAMED_PAGE_FIELDS: &[&str] = &["name"];
const EXPLICIT_PAGE_FIELDS: &[&str] = &["width_app_units", "height_app_units"];
const MARGIN_FIELDS: &[&str] = &["top", "right", "bottom", "left"];
const RESOURCE_FIELDS: &[&str] = &["network", "host_fonts"];
const TIME_FIELDS: &[&str] = &["policy_version", "epoch_unix_ms", "initial_offset_ns"];
const SETTLEMENT_FIELDS: &[&str] = &[
    "policy_version",
    "infinite_source_policy",
    "empty_checkpoints",
    "limits",
];
const LIMIT_FIELDS: &[&str] = &[
    "virtual_span_ms",
    "ordinary_tasks",
    "microtasks",
    "rendering_opportunities",
    "mutations",
    "post_readiness_resources",
    "process_cpu_ms",
    "host_wall_ms",
];
const DIAGNOSTIC_FIELDS: &[&str] = &["retention"];

#[derive(Debug)]
pub(crate) struct InvocationError(String);

impl InvocationError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self::framing(message)
    }

    fn framing(message: impl Into<String>) -> Self {
        Self(format!(
            "pliego: API 2 invocation error: {}",
            message.into()
        ))
    }

    pub(crate) fn unsupported() -> Self {
        Self::framing("no complete API 2 contract tuple is advertised")
    }

    pub(crate) fn write_stderr_line(&self, writer: &mut impl Write) -> io::Result<()> {
        let one_line = self.0.replace(['\r', '\n'], " ");
        writer.write_all(one_line.as_bytes())?;
        writer.write_all(b"\n")
    }
}

impl fmt::Display for InvocationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for InvocationError {}

/// Serialize the exact current executable identity and its unavailable API 2 foundation.
pub(crate) fn write_contract_probe(
    writer: &mut impl Write,
    servo_base: &str,
) -> Result<(), InvocationError> {
    validate_build_identity()?;
    let executable = std::env::current_exe().map_err(|error| {
        InvocationError::framing(format!("cannot locate current executable: {error}"))
    })?;
    let mut executable = File::open(&executable).map_err(|error| {
        InvocationError::framing(format!("cannot read current executable: {error}"))
    })?;
    let mut hasher = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = executable.read(&mut chunk).map_err(|error| {
            InvocationError::framing(format!("cannot hash current executable: {error}"))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&chunk[..read]);
    }
    let binary_sha256 = format!("sha256:{}", hex_lower(&hasher.finalize()));

    let probe = RuntimeContractProbe {
        schema: "pliego.runtime-contract",
        version: 1,
        engine: EngineIdentity {
            name: "pliego",
            version: env!("CARGO_PKG_VERSION"),
            api: API_VERSION,
            source_commit: SOURCE_COMMIT,
            runtime: RuntimeIdentity {
                mode: "one-shot",
                target: BUILD_TARGET,
                binary_sha256: &binary_sha256,
                servo_base,
            },
        },
        contracts: [],
        invocation: InvocationContract {
            request_transport: "stdin-single-json",
            request_max_bytes: REQUEST_MAX_BYTES,
            result_transport: "stdout-single-json",
            invocation_error_transport: "stderr-utf8-line",
            success_exit_code: 0,
            failed_exit_code: 1,
            invocation_error_exit_code: INVOCATION_ERROR_EXIT_CODE,
        },
    };
    let mut frame = serde_json::to_vec(&probe).map_err(|error| {
        InvocationError::framing(format!("cannot serialize contract probe: {error}"))
    })?;
    frame.push(b'\n');
    writer
        .write_all(&frame)
        .map_err(|error| InvocationError::framing(format!("cannot write contract probe: {error}")))
}

#[derive(Serialize)]
struct RuntimeContractProbe<'a> {
    schema: &'static str,
    version: u32,
    engine: EngineIdentity<'a>,
    contracts: [(); 0],
    invocation: InvocationContract,
}

#[derive(Serialize)]
struct EngineIdentity<'a> {
    name: &'static str,
    version: &'static str,
    api: u32,
    source_commit: &'static str,
    runtime: RuntimeIdentity<'a>,
}

#[derive(Serialize)]
struct RuntimeIdentity<'a> {
    mode: &'static str,
    target: &'static str,
    binary_sha256: &'a str,
    servo_base: &'a str,
}

#[derive(Serialize)]
struct InvocationContract {
    request_transport: &'static str,
    request_max_bytes: usize,
    result_transport: &'static str,
    invocation_error_transport: &'static str,
    success_exit_code: i32,
    failed_exit_code: i32,
    invocation_error_exit_code: i32,
}

/// Read one bounded frame and perform strict lexical plus typed API 2 request validation.
pub(crate) fn decode_render_request(reader: &mut impl Read) -> Result<Value, InvocationError> {
    let mut frame = Vec::with_capacity(8 * 1024);
    reader
        .take((REQUEST_MAX_BYTES as u64) + 1)
        .read_to_end(&mut frame)
        .map_err(|error| InvocationError::framing(format!("cannot read stdin: {error}")))?;
    if frame.len() > REQUEST_MAX_BYTES {
        return Err(InvocationError::framing(format!(
            "stdin exceeds request_max_bytes ({REQUEST_MAX_BYTES})"
        )));
    }
    if frame.starts_with(&[0xef, 0xbb, 0xbf]) {
        return Err(InvocationError::framing("UTF-8 BOM is not permitted"));
    }
    if frame.is_empty() {
        return Err(InvocationError::framing("stdin is empty"));
    }
    reject_negative_zero(&frame)?;

    let mut decoder = serde_json::Deserializer::from_slice(&frame);
    let StrictJson(value) = StrictJson::deserialize(&mut decoder)
        .map_err(|error| InvocationError::framing(format!("invalid request JSON: {error}")))?;
    decoder
        .end()
        .map_err(|error| InvocationError::framing(format!("invalid request framing: {error}")))?;
    validate_request(&value).map_err(InvocationError::framing)?;
    Ok(value)
}

fn reject_negative_zero(frame: &[u8]) -> Result<(), InvocationError> {
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < frame.len() {
        let byte = frame[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            index += 1;
            continue;
        }
        if byte == b'-' && frame.get(index + 1) == Some(&b'0') {
            let terminator = frame.get(index + 2).copied();
            if terminator.is_none_or(|next| {
                matches!(next, b' ' | b'\t' | b'\r' | b'\n' | b',' | b']' | b'}')
            }) {
                return Err(InvocationError::framing(
                    "negative zero is not canonical JSON",
                ));
            }
        }
        index += 1;
    }
    Ok(())
}

fn validate_build_identity() -> Result<(), InvocationError> {
    if !is_lower_hex(SOURCE_COMMIT, 40) {
        return Err(InvocationError::framing(
            "build source commit is not a full lowercase Git object id",
        ));
    }
    if !is_target_triple(BUILD_TARGET) {
        return Err(InvocationError::framing(
            "build target triple is not canonical",
        ));
    }
    Ok(())
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_lower(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from(DIGITS[usize::from(byte >> 4)]));
        encoded.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn is_target_triple(value: &str) -> bool {
    let components = value.split('-').collect::<Vec<_>>();
    (3..=4).contains(&components.len()) && components.into_iter().all(is_target_component)
}

fn is_target_component(value: &str) -> bool {
    !value.is_empty()
        && value.split('_').all(|atom| {
            !atom.is_empty()
                && atom
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

#[derive(Debug)]
struct StrictJson(Value);

impl<'de> Deserialize<'de> for StrictJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(StrictJsonVisitor)
    }
}

struct StrictJsonVisitor;

impl<'de> Visitor<'de> for StrictJsonVisitor {
    type Value = StrictJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("JSON without duplicate object names or floating-point numbers")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(StrictJson(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(StrictJson(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(StrictJson(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Err(E::custom("floating-point JSON numbers are not permitted"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(StrictJson(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(StrictJson(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJson(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(StrictJson(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::with_capacity(sequence.size_hint().unwrap_or(0).min(1024));
        while let Some(StrictJson(value)) = sequence.next_element::<StrictJson>()? {
            values.push(value);
        }
        Ok(StrictJson(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut names = BTreeSet::new();
        let mut values = Map::with_capacity(object.size_hint().unwrap_or(0).min(1024));
        while let Some(name) = object.next_key::<String>()? {
            if !names.insert(name.clone()) {
                return Err(de::Error::custom(format_args!(
                    "duplicate JSON object member {name:?}"
                )));
            }
            let StrictJson(value) = object.next_value::<StrictJson>()?;
            values.insert(name, value);
        }
        Ok(StrictJson(Value::Object(values)))
    }
}

fn validate_request(request: &Value) -> Result<(), String> {
    let request = closed_object(request, "$", TOP_LEVEL_FIELDS)?;
    exact_string(request, "$", "schema", "pliego.render-request")?;
    exact_u64(request, "$", "version", 1)?;
    exact_u64(request, "$", "api", u64::from(API_VERSION))?;

    match required(request, "$", "profile")? {
        Value::Null => {},
        value => validate_profile(value, "$.profile")?,
    }

    let input = closed_object(required(request, "$", "input")?, "$.input", INPUT_FIELDS)?;
    portable_path(
        required_string(input, "$.input", "entrypoint")?,
        "$.input.entrypoint",
    )?;
    let manifest = closed_object(
        required(input, "$.input", "manifest")?,
        "$.input.manifest",
        MANIFEST_FIELDS,
    )?;
    exact_string(manifest, "$.input.manifest", "path", "input-manifest.json")?;
    exact_string(
        manifest,
        "$.input.manifest",
        "media_type",
        "application/vnd.pliego.input-manifest+json",
    )?;
    content_address(
        required_string(manifest, "$.input.manifest", "sha256")?,
        "$.input.manifest.sha256",
    )?;
    integer_range(
        required(manifest, "$.input.manifest", "bytes")?,
        "$.input.manifest.bytes",
        1,
        9_007_199_254_740_991,
    )?;

    let environment = closed_object(
        required(request, "$", "environment")?,
        "$.environment",
        ENVIRONMENT_FIELDS,
    )?;
    enum_string(
        required_string(environment, "$.environment", "locale")?,
        "$.environment.locale",
        &["en-US", "es-MX"],
    )?;
    enum_string(
        required_string(environment, "$.environment", "timezone")?,
        "$.environment.timezone",
        &["UTC", "America/Tijuana"],
    )?;

    let page = closed_object(required(request, "$", "page")?, "$.page", PAGE_FIELDS)?;
    let size = required(page, "$.page", "size")?;
    let (width, height) = validate_page_size(size)?;
    let margins = closed_object(
        required(page, "$.page", "margins_app_units")?,
        "$.page.margins_app_units",
        MARGIN_FIELDS,
    )?;
    let top = required_u64(
        margins,
        "$.page.margins_app_units",
        "top",
        0,
        i32::MAX as u64,
    )?;
    let right = required_u64(
        margins,
        "$.page.margins_app_units",
        "right",
        0,
        i32::MAX as u64,
    )?;
    let bottom = required_u64(
        margins,
        "$.page.margins_app_units",
        "bottom",
        0,
        i32::MAX as u64,
    )?;
    let left = required_u64(
        margins,
        "$.page.margins_app_units",
        "left",
        0,
        i32::MAX as u64,
    )?;
    if left + right >= width {
        return Err("$.page.margins_app_units: horizontal margins consume the page".into());
    }
    if top + bottom >= height {
        return Err("$.page.margins_app_units: vertical margins consume the page".into());
    }
    exact_string(
        page,
        "$.page",
        "css_page_precedence",
        "css-page-over-request-defaults",
    )?;

    let resources = closed_object(
        required(request, "$", "resources")?,
        "$.resources",
        RESOURCE_FIELDS,
    )?;
    exact_string(resources, "$.resources", "network", "deny")?;
    exact_string(resources, "$.resources", "host_fonts", "deny")?;

    let time = closed_object(required(request, "$", "time")?, "$.time", TIME_FIELDS)?;
    exact_u64(time, "$.time", "policy_version", 1)?;
    integer_range(
        required(time, "$.time", "epoch_unix_ms")?,
        "$.time.epoch_unix_ms",
        -8_640_000_000_000_000,
        8_640_000_000_000_000,
    )?;
    exact_u64(time, "$.time", "initial_offset_ns", 0)?;

    let settlement = closed_object(
        required(request, "$", "settlement")?,
        "$.settlement",
        SETTLEMENT_FIELDS,
    )?;
    exact_u64(settlement, "$.settlement", "policy_version", 1)?;
    exact_string(settlement, "$.settlement", "infinite_source_policy", "fail")?;
    exact_u64(settlement, "$.settlement", "empty_checkpoints", 2)?;
    let limits = closed_object(
        required(settlement, "$.settlement", "limits")?,
        "$.settlement.limits",
        LIMIT_FIELDS,
    )?;
    required_u64(
        limits,
        "$.settlement.limits",
        "virtual_span_ms",
        1,
        9_007_199_254_740_991,
    )?;
    for field in [
        "ordinary_tasks",
        "microtasks",
        "rendering_opportunities",
        "mutations",
        "process_cpu_ms",
        "host_wall_ms",
    ] {
        required_u64(limits, "$.settlement.limits", field, 1, u64::from(u32::MAX))?;
    }
    required_u64(
        limits,
        "$.settlement.limits",
        "post_readiness_resources",
        0,
        u64::from(u32::MAX),
    )?;

    let diagnostics = closed_object(
        required(request, "$", "diagnostics")?,
        "$.diagnostics",
        DIAGNOSTIC_FIELDS,
    )?;
    enum_string(
        required_string(diagnostics, "$.diagnostics", "retention")?,
        "$.diagnostics.retention",
        &["none", "on-failure", "always"],
    )?;
    Ok(())
}

fn validate_profile(value: &Value, path: &str) -> Result<(), String> {
    let profile = closed_object(value, path, PROFILE_FIELDS)?;
    let schema = required_string(profile, path, "schema")?;
    let suffix = schema
        .strip_prefix("pliego.profile.")
        .ok_or_else(|| format!("{path}.schema: unsupported profile schema"))?;
    if suffix.is_empty()
        || suffix.len() > 128
        || !suffix.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'-')
        })
        || !suffix.as_bytes()[0].is_ascii_lowercase() && !suffix.as_bytes()[0].is_ascii_digit()
    {
        return Err(format!("{path}.schema: unsupported profile schema"));
    }
    required_u64(profile, path, "version", 1, u64::from(u32::MAX))?;
    Ok(())
}

fn validate_page_size(size: &Value) -> Result<(u64, u64), String> {
    let size = size
        .as_object()
        .ok_or_else(|| "$.page.size: expected object".to_owned())?;
    if size.contains_key("name") {
        let size = closed_map(size, "$.page.size", NAMED_PAGE_FIELDS)?;
        exact_string(size, "$.page.size", "name", "A4")?;
        Ok((47_622, 67_351))
    } else {
        let size = closed_map(size, "$.page.size", EXPLICIT_PAGE_FIELDS)?;
        let width = required_u64(size, "$.page.size", "width_app_units", 1, i32::MAX as u64)?;
        let height = required_u64(size, "$.page.size", "height_app_units", 1, i32::MAX as u64)?;
        Ok((width, height))
    }
}

fn closed_object<'a>(
    value: &'a Value,
    path: &str,
    fields: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    let object = value
        .as_object()
        .ok_or_else(|| format!("{path}: expected object"))?;
    closed_map(object, path, fields)
}

fn closed_map<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    fields: &[&str],
) -> Result<&'a Map<String, Value>, String> {
    if let Some(unknown) = object.keys().find(|name| !fields.contains(&name.as_str())) {
        return Err(format!("{path}: unexpected property {unknown:?}"));
    }
    Ok(object)
}

fn required<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    field: &str,
) -> Result<&'a Value, String> {
    object
        .get(field)
        .ok_or_else(|| format!("{path}: missing required property {field:?}"))
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    path: &str,
    field: &str,
) -> Result<&'a str, String> {
    required(object, path, field)?
        .as_str()
        .ok_or_else(|| format!("{path}.{field}: expected string"))
}

fn exact_string(
    object: &Map<String, Value>,
    path: &str,
    field: &str,
    expected: &str,
) -> Result<(), String> {
    let actual = required_string(object, path, field)?;
    if actual == expected {
        Ok(())
    } else {
        Err(format!("{path}.{field}: expected {expected:?}"))
    }
}

fn exact_u64(
    object: &Map<String, Value>,
    path: &str,
    field: &str,
    expected: u64,
) -> Result<(), String> {
    let actual = required_u64(object, path, field, expected, expected)?;
    debug_assert_eq!(actual, expected);
    Ok(())
}

fn required_u64(
    object: &Map<String, Value>,
    path: &str,
    field: &str,
    minimum: u64,
    maximum: u64,
) -> Result<u64, String> {
    let value = required(object, path, field)?;
    let actual = value
        .as_u64()
        .ok_or_else(|| format!("{path}.{field}: expected nonnegative integer"))?;
    if (minimum..=maximum).contains(&actual) {
        Ok(actual)
    } else {
        Err(format!(
            "{path}.{field}: integer is outside {minimum}..={maximum}"
        ))
    }
}

fn integer_range(value: &Value, path: &str, minimum: i64, maximum: i64) -> Result<(), String> {
    let actual = value
        .as_i64()
        .ok_or_else(|| format!("{path}: expected signed integer"))?;
    if (minimum..=maximum).contains(&actual) {
        Ok(())
    } else {
        Err(format!("{path}: integer is outside {minimum}..={maximum}"))
    }
}

fn enum_string(value: &str, path: &str, accepted: &[&str]) -> Result<(), String> {
    if accepted.contains(&value) {
        Ok(())
    } else {
        Err(format!("{path}: unsupported value {value:?}"))
    }
}

fn content_address(value: &str, path: &str) -> Result<(), String> {
    value
        .strip_prefix("sha256:")
        .filter(|digest| is_lower_hex(digest, 64))
        .map(|_| ())
        .ok_or_else(|| format!("{path}: expected lowercase SHA-256 content address"))
}

fn portable_path(value: &str, path: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 240
        || !value.is_ascii()
        || !value.as_bytes()[0].is_ascii_alphanumeric()
        || value.starts_with('/')
        || value.contains('\\')
    {
        return Err(format!("{path}: path is not portable"));
    }
    for segment in value.split('/') {
        if segment.is_empty()
            || matches!(segment, "." | "..")
            || segment.len() > 100
            || segment.ends_with(['.', ' '])
            || !segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return Err(format!("{path}: path is not portable"));
        }
        let stem = segment
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if matches!(stem.as_str(), "aux" | "con" | "nul" | "prn")
            || stem.len() == 4
                && (stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.as_bytes()[3].is_ascii_digit()
                && stem.as_bytes()[3] != b'0'
        {
            return Err(format!("{path}: path is not portable"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../contracts/api2/goldens/accepted/render-request.a4.json"
    ));

    #[test]
    fn decodes_the_accepted_null_profile_request_without_rendering() {
        let request = decode_render_request(&mut &REQUEST[..]).unwrap();
        assert_eq!(request["schema"], "pliego.render-request");
        assert_eq!(request["profile"], Value::Null);
    }

    #[test]
    fn rejects_duplicate_names_float_negative_zero_bom_and_trailing_json() {
        for (label, mut frame, expected) in [
            (
                "duplicate",
                br#"{"schema":"a","schema":"b"}"#.as_slice(),
                "duplicate JSON object member",
            ),
            (
                "float",
                br#"{"value":1.0}"#.as_slice(),
                "floating-point JSON numbers",
            ),
            (
                "negative zero",
                br#"{"value":-0}"#.as_slice(),
                "negative zero",
            ),
            ("BOM", b"\xef\xbb\xbf{}".as_slice(), "BOM"),
            ("trailing", b"{} {}".as_slice(), "trailing"),
        ] {
            let error = decode_render_request(&mut frame).unwrap_err();
            assert!(error.to_string().contains(expected), "{label}: {error}");
        }
    }

    #[test]
    fn rejects_unknown_members_at_every_typed_level() {
        let mut request: Value = serde_json::from_slice(REQUEST).unwrap();
        request["settlement"]["limits"]["future"] = Value::from(1);
        let encoded = serde_json::to_vec(&request).unwrap();
        assert!(
            decode_render_request(&mut encoded.as_slice())
                .unwrap_err()
                .to_string()
                .contains("unexpected property")
        );
    }

    #[test]
    fn rejects_paths_whose_first_byte_is_not_ascii_alphanumeric() {
        for entrypoint in ["_document.html", "-document.html"] {
            let mut request: Value = serde_json::from_slice(REQUEST).unwrap();
            request["input"]["entrypoint"] = Value::from(entrypoint);
            let encoded = serde_json::to_vec(&request).unwrap();
            assert!(
                decode_render_request(&mut encoded.as_slice())
                    .unwrap_err()
                    .to_string()
                    .contains("path is not portable")
            );
        }
    }

    #[test]
    fn framing_limit_is_inclusive_and_reads_only_limit_plus_one() {
        let mut exact = vec![b' '; REQUEST_MAX_BYTES];
        exact[..2].copy_from_slice(b"{}");
        let error = decode_render_request(&mut exact.as_slice()).unwrap_err();
        assert!(!error.to_string().contains("exceeds request_max_bytes"));

        let over = vec![b' '; REQUEST_MAX_BYTES + 1];
        assert!(
            decode_render_request(&mut over.as_slice())
                .unwrap_err()
                .to_string()
                .contains("exceeds request_max_bytes")
        );
    }

    #[test]
    fn unavailable_foundation_reports_an_empty_contract_array_and_exact_tuple_shape() {
        let mut output = Vec::new();
        write_contract_probe(&mut output, super::super::SERVO_BASE_SHA).unwrap();
        assert_eq!(output.last(), Some(&b'\n'));
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        let serialized = std::str::from_utf8(&output).unwrap();
        assert!(
            serialized.starts_with(concat!(
                r#"{"schema":"pliego.runtime-contract","version":1,"engine":{"name":"pliego","version":""#,
                env!("CARGO_PKG_VERSION"),
                r#"","api":2,"source_commit":""#
            )),
            "unexpected probe order: {serialized}"
        );
        assert!(serialized.contains(&format!(
            r#""contracts":[],"invocation":{{"request_transport":"stdin-single-json","request_max_bytes":{REQUEST_MAX_BYTES},"result_transport":"stdout-single-json","invocation_error_transport":"stderr-utf8-line","success_exit_code":0,"failed_exit_code":1,"invocation_error_exit_code":64}}"#
        )));
        let probe: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(probe["contracts"], serde_json::json!([]));
        assert_eq!(probe["engine"]["api"], API_VERSION);
        assert_eq!(probe["engine"]["source_commit"], SOURCE_COMMIT);
        assert_eq!(probe["engine"]["runtime"]["target"], BUILD_TARGET);
        assert_eq!(probe["invocation"]["request_max_bytes"], REQUEST_MAX_BYTES);
        assert_eq!(probe["invocation"]["invocation_error_exit_code"], 64);
        assert!(
            probe["engine"]["runtime"]["binary_sha256"]
                .as_str()
                .is_some_and(|value| value.starts_with("sha256:") && value.len() == 71)
        );
    }

    #[test]
    fn contract_probe_buffers_the_complete_frame_before_touching_the_writer() {
        #[derive(Default)]
        struct RecordingWriter {
            calls: usize,
            bytes: Vec<u8>,
        }

        impl Write for RecordingWriter {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                self.calls += 1;
                self.bytes.extend_from_slice(buffer);
                Ok(buffer.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = RecordingWriter::default();
        write_contract_probe(&mut writer, super::super::SERVO_BASE_SHA).unwrap();

        assert_eq!(writer.calls, 1);
        assert_eq!(writer.bytes.last(), Some(&b'\n'));
        assert_eq!(
            writer.bytes.iter().filter(|byte| **byte == b'\n').count(),
            1
        );
        let probe: Value = serde_json::from_slice(&writer.bytes).unwrap();
        assert_eq!(probe["contracts"], serde_json::json!([]));
    }

    #[test]
    fn invocation_error_is_exactly_one_utf8_line() {
        let mut stderr = Vec::new();
        InvocationError::framing("first\r\nsecond")
            .write_stderr_line(&mut stderr)
            .unwrap();
        assert_eq!(
            String::from_utf8(stderr).unwrap(),
            "pliego: API 2 invocation error: first  second\n"
        );
    }
}
