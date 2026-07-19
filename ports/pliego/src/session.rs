/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct LocalDocument {
    root: PathBuf,
    path: PathBuf,
}

impl LocalDocument {
    pub fn resolve(
        root: impl AsRef<Path>,
        requested: impl AsRef<Path>,
    ) -> Result<Self, SessionFailure> {
        let supplied_root = root.as_ref();
        let root =
            supplied_root
                .canonicalize()
                .map_err(|source| SessionFailure::RootUnavailable {
                    path: supplied_root.to_owned(),
                    source,
                })?;

        if !root.is_dir() {
            return Err(SessionFailure::RootNotDirectory(root));
        }

        let requested = requested.as_ref();
        if requested.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(SessionFailure::UnsafeRequestPath(requested.to_owned()));
        }

        let unresolved = root.join(requested);
        let path =
            unresolved
                .canonicalize()
                .map_err(|source| SessionFailure::DocumentUnavailable {
                    path: unresolved,
                    source,
                })?;

        Self::from_canonical_paths(root, path)
    }

    fn from_canonical_paths(root: PathBuf, path: PathBuf) -> Result<Self, SessionFailure> {
        if !path.starts_with(&root) {
            return Err(SessionFailure::OutsideRoot { root, path });
        }

        if !path.is_file() {
            return Err(SessionFailure::DocumentNotFile(path));
        }

        Ok(Self { root, path })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[derive(Debug)]
pub struct SessionArtifacts {
    directory: PathBuf,
    render_id: String,
}

impl SessionArtifacts {
    #[cfg(test)]
    pub fn create(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_owned();
        let render_id = directory
            .file_name()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "session artifact path has no final component",
                )
            })?
            .to_string_lossy()
            .into_owned();
        Self::create_with_render_id(directory, render_id)
    }

    pub fn create_with_render_id(
        directory: impl AsRef<Path>,
        render_id: impl Into<String>,
    ) -> io::Result<Self> {
        let directory = directory.as_ref().to_owned();
        let render_id = render_id.into();
        if render_id.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "render ID may not be empty",
            ));
        }
        create_private_directory(&directory)?;
        create_private_directory(&directory.join("resources"))?;
        for name in ["console.jsonl", "resources.jsonl", "session-state.jsonl"] {
            private_file_options()
                .write(true)
                .create_new(true)
                .open(directory.join(name))?;
        }
        Ok(Self {
            directory,
            render_id,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn render_id(&self) -> String {
        self.render_id.clone()
    }

    pub fn record_state(&self, state: &str, message: Option<&str>) -> io::Result<()> {
        self.append(
            "session-state.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "state": state,
                "message": message,
            }),
        )
    }

    pub fn record_console(&self, level: &str, message: &str) -> io::Result<()> {
        self.append(
            "console.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "level": level,
                "message": message,
            }),
        )
    }

    pub fn record_resource_request(&self, request_id: &str, url: &str) -> io::Result<()> {
        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "request_id": request_id,
                "url": url,
                "status": "requested",
                "bytes": null,
            }),
        )
    }

    pub fn record_loaded_resource(
        &self,
        request_id: &str,
        urls: &[String],
        response_status: Option<u16>,
        content_type: Option<&str>,
        sha256: &str,
        body: &[u8],
    ) -> io::Result<()> {
        let artifact = self.write_resource_digest(sha256, body)?;

        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "request_id": request_id,
                "url": urls.last(),
                "urls": urls,
                "status": "loaded",
                "response_status": response_status,
                "content_type": content_type,
                "bytes": body.len() as u64,
                "sha256": sha256,
                "resource": format!("sha256:{sha256}"),
                "artifact": artifact,
            }),
        )
    }

    pub fn write_content_addressed_resource(
        &self,
        resource: &str,
        body: &[u8],
    ) -> io::Result<String> {
        let digest = resource.strip_prefix("sha256:").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("resource is not a SHA-256 content address: {resource}"),
            )
        })?;
        self.write_resource_digest(digest, body)
    }

    pub fn write_scene(&self, normalized_scene: &[u8]) -> io::Result<()> {
        self.write_bytes("scene.json", normalized_scene)
    }

    pub fn write_fonts(&self, fonts: &serde_json::Value) -> io::Result<()> {
        self.write_json("fonts.json", fonts)
    }

    pub fn write_scene_report(&self, report: &serde_json::Value) -> io::Result<()> {
        self.write_json("scene-report.json", report)
    }

    pub fn write_scene_preview(&self, png: &[u8]) -> io::Result<()> {
        self.write_bytes("scene-preview.png", png)
    }

    pub fn write_document_pdf(&self, pdf: &[u8]) -> io::Result<()> {
        self.write_bytes("document.pdf", pdf)
    }

    /// Publish the diagnostic PDF without replacing an existing caller-owned path.
    pub fn publish_document_pdf(&self, destination: impl AsRef<Path>) -> io::Result<()> {
        publish_new_file(&self.directory.join("document.pdf"), destination.as_ref())
    }

    pub fn write_pdf_structure(&self, structure: &serde_json::Value) -> io::Result<()> {
        self.write_json("pdf-structure.json", structure)
    }

    pub fn write_readiness(&self, readiness: &serde_json::Value) -> io::Result<()> {
        let mut readiness = readiness.clone();
        let object = readiness.as_object_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "readiness must be a JSON object",
            )
        })?;
        object.insert(
            "render_id".into(),
            serde_json::Value::String(self.render_id()),
        );
        self.write_json("readiness.json", &readiness)
    }

    pub fn write_layout_debug(&self, snapshot: &serde_json::Value) -> io::Result<()> {
        self.write_json("layout-debug.json", snapshot)
    }

    pub fn write_environment(&self, environment: &serde_json::Value) -> io::Result<()> {
        self.write_json("environment.json", environment)
    }

    fn write_resource_digest(&self, digest: &str, body: &[u8]) -> io::Result<String> {
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid lowercase SHA-256 digest: {digest}"),
            ));
        }

        let artifact = format!("resources/{digest}");
        let path = self.directory.join(&artifact);
        match private_file_options()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => file.write_all(body)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path)? != body {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("resource digest collision for {digest}"),
                    ));
                }
            },
            Err(error) => return Err(error),
        }
        Ok(artifact)
    }

    fn write_bytes(&self, name: &str, bytes: &[u8]) -> io::Result<()> {
        let mut file = open_private_file(&self.directory.join(name))?;
        file.write_all(bytes)
    }

    fn write_json(&self, name: &str, value: &serde_json::Value) -> io::Result<()> {
        let mut file = open_private_file(&self.directory.join(name))?;
        serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }

    fn append(&self, name: &str, event: serde_json::Value) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .append(true)
            .open(self.directory.join(name))?;
        serde_json::to_writer(&mut file, &event).map_err(io::Error::other)?;
        file.write_all(b"\n")
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = std::fs::DirBuilder::new();
    builder.mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    std::fs::create_dir(path)
}

#[cfg(unix)]
fn private_file_options() -> OpenOptions {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options.mode(0o600);
    options
}

#[cfg(not(unix))]
fn private_file_options() -> OpenOptions {
    OpenOptions::new()
}

fn open_private_file(path: &Path) -> io::Result<File> {
    private_file_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
}

fn publish_new_file(source: &Path, destination: &Path) -> io::Result<()> {
    let file_name = destination.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "output path has no final component",
        )
    })?;
    if destination.try_exists()? {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("output already exists: {}", destination.display()),
        ));
    }
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut source_file = File::open(source)?;

    for attempt in 0..32 {
        let mut temporary_name = OsString::from(".");
        temporary_name.push(file_name);
        temporary_name.push(format!(".pliego-{}-{attempt}.tmp", std::process::id()));
        let temporary_path = parent.join(temporary_name);
        let mut temporary_file = match private_file_options()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        let write_result = (|| {
            io::copy(&mut source_file, &mut temporary_file)?;
            temporary_file.sync_all()
        })();
        drop(temporary_file);
        if let Err(error) = write_result {
            let _ = std::fs::remove_file(&temporary_path);
            return Err(error);
        }

        // A hard-link publish is the stdlib's portable atomic no-clobber operation. The temporary
        // file is a sibling, so supported filesystems keep both names on the same volume.
        match std::fs::hard_link(&temporary_path, destination) {
            Ok(()) => {
                // The destination is fully published at this point. A best-effort temporary-name
                // cleanup must not turn that committed output into a reported render failure.
                let _ = std::fs::remove_file(&temporary_path);
                return Ok(());
            },
            Err(error) => {
                let _ = std::fs::remove_file(&temporary_path);
                return Err(error);
            },
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "all temporary output names already exist beside {}",
            destination.display()
        ),
    ))
}

fn timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[derive(Debug)]
pub enum SessionFailure {
    RootUnavailable { path: PathBuf, source: io::Error },
    RootNotDirectory(PathBuf),
    UnsafeRequestPath(PathBuf),
    DocumentUnavailable { path: PathBuf, source: io::Error },
    DocumentNotFile(PathBuf),
    OutsideRoot { root: PathBuf, path: PathBuf },
}

impl fmt::Display for SessionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RootUnavailable { path, .. } => {
                write!(
                    formatter,
                    "document root is unavailable: {}",
                    path.display()
                )
            },
            Self::RootNotDirectory(path) => {
                write!(
                    formatter,
                    "document root is not a directory: {}",
                    path.display()
                )
            },
            Self::UnsafeRequestPath(path) => {
                write!(
                    formatter,
                    "document path may not be absolute or traverse parents: {}",
                    path.display()
                )
            },
            Self::DocumentUnavailable { path, .. } => {
                write!(formatter, "document is unavailable: {}", path.display())
            },
            Self::DocumentNotFile(path) => {
                write!(formatter, "document is not a file: {}", path.display())
            },
            Self::OutsideRoot { root, path } => write!(
                formatter,
                "document is outside root {}: {}",
                root.display(),
                path.display()
            ),
        }
    }
}

impl Error for SessionFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::RootUnavailable { source, .. } | Self::DocumentUnavailable { source, .. } => {
                Some(source)
            },
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{LocalDocument, SessionArtifacts, SessionFailure};

    #[test]
    fn resolves_a_local_file_and_rejects_escape_paths() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            std::env::temp_dir().join(format!("pliego-session-{}-{unique}", std::process::id()));
        let root = sandbox.join("root");
        let inside = root.join("index.html");
        let outside = sandbox.join("outside.html");
        fs::create_dir_all(&root).unwrap();
        fs::write(&inside, "<title>inside</title>").unwrap();
        fs::write(&outside, "<title>outside</title>").unwrap();

        let document = LocalDocument::resolve(&root, "index.html").unwrap();
        assert_eq!(document.root(), root.canonicalize().unwrap());
        assert_eq!(document.path(), inside.canonicalize().unwrap());
        assert!(matches!(
            LocalDocument::resolve(&root, "../outside.html"),
            Err(SessionFailure::UnsafeRequestPath(_))
        ));
        assert!(matches!(
            LocalDocument::from_canonical_paths(
                root.canonicalize().unwrap(),
                outside.canonicalize().unwrap()
            ),
            Err(SessionFailure::OutsideRoot { .. })
        ));

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[test]
    fn writes_the_three_session_traces_as_json_lines() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("pliego-artifacts-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();

        artifacts.record_state("started", None).unwrap();
        artifacts.record_console("info", "fixture-ready").unwrap();
        artifacts
            .record_resource_request("request-1", "file:///index.html")
            .unwrap();
        let resource_body = b"hello";
        let resource_hash = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        artifacts
            .record_loaded_resource(
                "request-1",
                &["file:///index.html".to_owned()],
                Some(200),
                Some("text/html; charset=utf-8"),
                resource_hash,
                resource_body,
            )
            .unwrap();
        artifacts
            .write_readiness(&serde_json::json!({
                "status": "ready",
                "payload": { "fixture": true }
            }))
            .unwrap();
        artifacts
            .write_layout_debug(&serde_json::json!({
                "boxes": [{ "depth": 0, "kind": "block" }],
                "fragments": [{ "depth": 0, "kind": "box" }]
            }))
            .unwrap();

        assert_eq!(artifacts.directory(), directory);
        let state: serde_json::Value = serde_json::from_str(
            fs::read_to_string(directory.join("session-state.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        let console: serde_json::Value = serde_json::from_str(
            fs::read_to_string(directory.join("console.jsonl"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        let resources: Vec<serde_json::Value> =
            fs::read_to_string(directory.join("resources.jsonl"))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect();
        let readiness: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(directory.join("readiness.json")).unwrap())
                .unwrap();
        let layout_debug: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(directory.join("layout-debug.json")).unwrap())
                .unwrap();
        assert_eq!(state["state"], "started");
        assert_eq!(console["message"], "fixture-ready");
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0]["status"], "requested");
        assert_eq!(resources[0]["request_id"], "request-1");
        assert_eq!(resources[1]["status"], "loaded");
        assert_eq!(resources[1]["request_id"], "request-1");
        assert_eq!(resources[1]["url"], "file:///index.html");
        assert_eq!(resources[1]["urls"][0], "file:///index.html");
        assert_eq!(resources[1]["response_status"], 200);
        assert_eq!(resources[1]["content_type"], "text/html; charset=utf-8");
        assert_eq!(resources[1]["bytes"], resource_body.len());
        assert_eq!(resources[1]["sha256"], resource_hash);
        assert_eq!(resources[1]["resource"], format!("sha256:{resource_hash}"));
        assert_eq!(
            fs::read(directory.join("resources").join(resource_hash)).unwrap(),
            resource_body
        );
        assert_eq!(readiness["payload"]["fixture"], true);
        assert_eq!(readiness["render_id"], artifacts.render_id());
        assert_eq!(layout_debug["boxes"][0]["kind"], "block");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn writes_exact_scene_artifacts_and_verifies_resource_collisions() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("pliego-scene-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let scene = br#"{"schema":"pliego.document-scene","version":1,"pages":[]}"#;
        let fonts = serde_json::json!({
            "resources": [{ "resource": "sha256:font" }],
            "instances": []
        });
        let report = serde_json::json!({
            "capture": { "status": "partial", "unsupported_events": [] },
            "preview": { "status": "rendered", "unsupported": [] }
        });
        let pdf_structure = serde_json::json!({
            "schema": "pliego.pdf-structure",
            "version": 1,
            "pages": [],
        });
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let resource = format!("sha256:{digest}");

        artifacts.write_scene(scene).unwrap();
        artifacts.write_fonts(&fonts).unwrap();
        artifacts.write_scene_report(&report).unwrap();
        artifacts.write_scene_preview(b"\x89PNG\r\n\x1a\n").unwrap();
        artifacts.write_document_pdf(b"%PDF-fixture").unwrap();
        artifacts.write_pdf_structure(&pdf_structure).unwrap();
        assert_eq!(
            artifacts
                .write_content_addressed_resource(&resource, b"hello")
                .unwrap(),
            format!("resources/{digest}")
        );
        artifacts
            .write_content_addressed_resource(&resource, b"hello")
            .unwrap();

        assert_eq!(fs::read(directory.join("scene.json")).unwrap(), scene);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("fonts.json")).unwrap()
            )
            .unwrap(),
            fonts
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("scene-report.json")).unwrap()
            )
            .unwrap(),
            report
        );
        assert_eq!(
            fs::read(directory.join("scene-preview.png")).unwrap(),
            b"\x89PNG\r\n\x1a\n"
        );
        assert_eq!(
            fs::read(directory.join("document.pdf")).unwrap(),
            b"%PDF-fixture"
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(
                &fs::read(directory.join("pdf-structure.json")).unwrap()
            )
            .unwrap(),
            pdf_structure
        );
        assert_eq!(
            fs::read(directory.join("resources").join(digest)).unwrap(),
            b"hello"
        );
        let collision = artifacts
            .write_content_addressed_resource(&resource, b"different")
            .unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn refuses_to_reuse_an_existing_session_directory() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("pliego-exclusive-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        artifacts.record_console("info", "preserve-me").unwrap();
        let original = fs::read(directory.join("console.jsonl")).unwrap();

        let collision = SessionArtifacts::create(&directory).unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(directory.join("console.jsonl")).unwrap(), original);

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn keeps_an_explicit_render_id_independent_of_the_artifact_path() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "pliego-explicit-id-{}-{unique}",
            std::process::id()
        ));
        let artifacts =
            SessionArtifacts::create_with_render_id(&directory, "sha256:stable-fixture").unwrap();

        artifacts
            .write_readiness(&serde_json::json!({ "status": "ready" }))
            .unwrap();
        let readiness: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("readiness.json")).unwrap()).unwrap();
        assert_eq!(artifacts.render_id(), "sha256:stable-fixture");
        assert_eq!(readiness["render_id"], "sha256:stable-fixture");

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn atomically_publishes_a_pdf_without_replacing_an_existing_output() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            std::env::temp_dir().join(format!("pliego-publish-{}-{unique}", std::process::id()));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create(sandbox.join("artifacts")).unwrap();
        let output = sandbox.join("invoice.pdf");
        artifacts.write_document_pdf(b"%PDF-first").unwrap();

        artifacts.publish_document_pdf(&output).unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-first");
        assert_eq!(
            fs::read(artifacts.directory().join("document.pdf")).unwrap(),
            b"%PDF-first"
        );
        let collision = artifacts.publish_document_pdf(&output).unwrap_err();
        assert_eq!(collision.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&output).unwrap(), b"%PDF-first");
        assert!(fs::read_dir(&sandbox).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".pliego-")
        }));

        fs::remove_dir_all(sandbox).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_session_directories_and_files() {
        use std::os::unix::fs::PermissionsExt;

        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("pliego-private-{}-{unique}", std::process::id()));
        let artifacts = SessionArtifacts::create(&directory).unwrap();
        let digest = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

        artifacts.write_scene(b"{}").unwrap();
        artifacts
            .write_environment(&serde_json::json!({ "phase": "initial" }))
            .unwrap();
        artifacts
            .write_environment(&serde_json::json!({ "phase": "final" }))
            .unwrap();
        artifacts
            .write_content_addressed_resource(&format!("sha256:{digest}"), b"hello")
            .unwrap();

        for path in [&directory, &directory.join("resources")] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        for path in [
            directory.join("console.jsonl"),
            directory.join("resources.jsonl"),
            directory.join("session-state.jsonl"),
            directory.join("scene.json"),
            directory.join("environment.json"),
            directory.join("resources").join(digest),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        fs::remove_dir_all(directory).unwrap();
    }
}
