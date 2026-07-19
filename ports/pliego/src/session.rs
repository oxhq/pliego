/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
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
}

impl SessionArtifacts {
    pub fn create(directory: impl AsRef<Path>) -> io::Result<Self> {
        let directory = directory.as_ref().to_owned();
        std::fs::create_dir_all(&directory)?;
        std::fs::create_dir_all(directory.join("resources"))?;
        for name in ["console.jsonl", "resources.jsonl", "session-state.jsonl"] {
            File::create(directory.join(name))?;
        }
        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn render_id(&self) -> String {
        self.directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
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
        let artifact = format!("resources/{sha256}");
        let path = self.directory.join(&artifact);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file.write_all(body)?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::read(&path)? != body {
                    return Err(io::Error::new(
                        io::ErrorKind::AlreadyExists,
                        format!("resource digest collision for {sha256}"),
                    ));
                }
            },
            Err(error) => return Err(error),
        }

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

    fn write_json(&self, name: &str, value: &serde_json::Value) -> io::Result<()> {
        let mut file = File::create(self.directory.join(name))?;
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
}
