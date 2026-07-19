/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::error::Error;
use std::ffi::OsString;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use sha2::{Digest, Sha256};

const BUNDLE_FILE_NAME: &str = "bundle.json";

#[derive(Clone, Debug, PartialEq, Serialize)]
struct BundleEntry {
    path: String,
    sha256: String,
    bytes: u64,
}

#[derive(Debug, Serialize)]
struct BundleManifest<'a> {
    schema: &'static str,
    version: u32,
    render_id: &'a str,
    entries: Vec<BundleEntry>,
    output: BundleEntry,
}

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
                "render_id": self.render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": request_id,
                "url": url,
                "status": "requested",
                "bytes": null,
            }),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_resource_failure(
        &self,
        code: &str,
        status: &str,
        url: &str,
        method: &str,
        destination: &str,
        referrer_url: Option<&str>,
        is_for_main_frame: bool,
        is_redirect: bool,
        reason: &str,
    ) -> io::Result<()> {
        self.append(
            "resources.jsonl",
            serde_json::json!({
                "timestamp_ms": timestamp_ms(),
                "render_id": self.render_id,
                "policy": "pliego.resource-policy.v1",
                "request_id": null,
                "url": url,
                "status": status,
                "code": code,
                "method": method,
                "destination": destination,
                "referrer_url": referrer_url,
                "is_for_main_frame": is_for_main_frame,
                "is_redirect": is_redirect,
                "reason": reason,
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
                "render_id": self.render_id,
                "policy": "pliego.resource-policy.v1",
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

    pub fn write_scene_previews(&self, pages: &[Vec<u8>]) -> io::Result<Vec<PathBuf>> {
        if pages.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "scene preview requires at least one page",
            ));
        }
        if pages.len() == 1 {
            self.write_scene_preview(&pages[0])?;
            return Ok(vec![self.directory.join("scene-preview.png")]);
        }

        let directory = self.directory.join("pages");
        create_private_directory(&directory)?;
        pages
            .iter()
            .enumerate()
            .map(|(index, png)| {
                let path = directory.join(format!("page-{:04}.png", index + 1));
                let mut file = open_private_file(&path)?;
                file.write_all(png)?;
                Ok(path)
            })
            .collect()
    }

    pub fn write_pages(&self, pages: &serde_json::Value) -> io::Result<()> {
        self.write_json("pages.json", pages)
    }

    pub fn write_document_pdf(&self, pdf: &[u8]) -> io::Result<()> {
        self.write_bytes("document.pdf", pdf)
    }

    /// Publish the diagnostic PDF without replacing an existing caller-owned path.
    pub fn publish_document_pdf(&self, destination: impl AsRef<Path>) -> io::Result<()> {
        publish_new_file(&self.directory.join("document.pdf"), destination.as_ref())
    }

    /// Bind the completed diagnostic artifacts and published PDF to this render ID.
    pub fn write_bundle(&self, output: impl AsRef<Path>) -> io::Result<PathBuf> {
        require_rendered_terminal_state(&self.directory.join("session-state.jsonl"))?;
        require_directory_without_symlink(&self.directory)?;

        let mut entries = Vec::new();
        collect_bundle_entries(&self.directory, &self.directory, &mut entries)?;
        entries.sort_by(|left, right| left.path.cmp(&right.path));

        let output_path = output.as_ref();
        let output_path_string = output_path.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "published output path is not valid UTF-8: {}",
                    output_path.display()
                ),
            )
        })?;
        let (output_sha256, output_bytes) = hash_regular_file(output_path)?;
        let manifest = BundleManifest {
            schema: "pliego.bundle",
            version: 1,
            render_id: &self.render_id,
            entries,
            output: BundleEntry {
                path: output_path_string.to_owned(),
                sha256: output_sha256,
                bytes: output_bytes,
            },
        };

        let bundle_path = self.directory.join(BUNDLE_FILE_NAME);
        let mut bundle = private_file_options()
            .write(true)
            .create_new(true)
            .open(&bundle_path)?;
        serde_json::to_writer_pretty(&mut bundle, &manifest).map_err(io::Error::other)?;
        bundle.write_all(b"\n")?;
        bundle.sync_all()?;
        Ok(bundle_path)
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

    pub fn write_failure(&self, code: &str, message: &str) -> io::Result<()> {
        self.write_json(
            "failure.json",
            &serde_json::json!({
                "status": "failed",
                "render_id": self.render_id,
                "error": {
                    "code": code,
                    "message": message,
                },
            }),
        )
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

fn require_rendered_terminal_state(path: &Path) -> io::Result<()> {
    let contents = std::fs::read_to_string(path)?;
    let event = contents
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "session state has no terminal event",
            )
        })?;
    let event: serde_json::Value = serde_json::from_str(event).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("session terminal state is invalid JSON: {error}"),
        )
    })?;
    if event.get("state").and_then(serde_json::Value::as_str) != Some("rendered") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle may only be written after the rendered terminal state",
        ));
    }
    Ok(())
}

fn require_directory_without_symlink(path: &Path) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle artifact root must be a directory, not a symlink or special file: {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn collect_bundle_entries(
    root: &Path,
    directory: &Path,
    entries: &mut Vec<BundleEntry>,
) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bundle artifacts may not contain symlinks: {}",
                    path.display()
                ),
            ));
        }
        if metadata.is_dir() {
            collect_bundle_entries(root, &path, entries)?;
            continue;
        }
        if !metadata.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bundle artifacts may only contain regular files and directories: {}",
                    path.display()
                ),
            ));
        }

        let relative = normalized_relative_path(root, &path)?;
        if is_bundle_excluded(&relative) {
            continue;
        }
        let (sha256, bytes) = hash_regular_file(&path)?;
        entries.push(BundleEntry {
            path: relative,
            sha256,
            bytes,
        });
    }
    Ok(())
}

fn normalized_relative_path(root: &Path, path: &Path) -> io::Result<String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle artifact escapes its root {}: {}",
                root.display(),
                path.display()
            ),
        )
    })?;
    let mut normalized = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle artifact path is unsafe: {}", path.display()),
            ));
        };
        let component = component.to_str().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "bundle artifact path is not valid UTF-8: {}",
                    path.display()
                ),
            )
        })?;
        if component.is_empty() || component.contains('\\') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle artifact path is unsafe: {}", path.display()),
            ));
        }
        normalized.push(component);
    }
    if normalized.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "bundle artifact path may not be the artifact root",
        ));
    }
    Ok(normalized.join("/"))
}

fn is_bundle_excluded(relative: &str) -> bool {
    if relative == BUNDLE_FILE_NAME {
        return true;
    }
    let file_name = relative.rsplit('/').next().unwrap_or(relative);
    file_name.starts_with('.') && file_name.contains(".pliego-") && file_name.ends_with(".tmp")
}

fn hash_regular_file(path: &Path) -> io::Result<(String, u64)> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "bundle entry must be a regular file, not a symlink or special file: {}",
                path.display()
            ),
        ));
    }

    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        bytes = bytes.checked_add(read as u64).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("bundle entry is too large to count: {}", path.display()),
            )
        })?;
    }
    if bytes != metadata.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bundle entry changed while hashing: {}", path.display()),
        ));
    }
    Ok((
        format!("sha256:{}", lowercase_hex(&hasher.finalize())),
        bytes,
    ))
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
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
            .record_resource_failure(
                "RESOURCE_DENIED",
                "denied",
                "https://example.test/font.woff2",
                "GET",
                "Font",
                Some("file:///index.html"),
                false,
                false,
                "network access is disabled",
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
        assert_eq!(resources.len(), 3);
        assert_eq!(resources[0]["status"], "requested");
        assert_eq!(resources[0]["render_id"], artifacts.render_id());
        assert_eq!(resources[0]["policy"], "pliego.resource-policy.v1");
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
        assert_eq!(resources[2]["status"], "denied");
        assert_eq!(resources[2]["code"], "RESOURCE_DENIED");
        assert_eq!(resources[2]["request_id"], serde_json::Value::Null);
        assert_eq!(resources[2]["url"], "https://example.test/font.woff2");
        assert_eq!(resources[2]["destination"], "Font");
        assert_eq!(resources[2]["reason"], "network access is disabled");
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
    fn writes_a_typed_failure_bound_to_the_render_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "pliego-failure-artifact-{}-{unique}",
            std::process::id()
        ));
        let artifacts =
            SessionArtifacts::create_with_render_id(&directory, "sha256:failed-fixture").unwrap();

        artifacts
            .write_failure("FIXTURE_FAILED", "fixture failure")
            .unwrap();
        let failure: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("failure.json")).unwrap()).unwrap();
        assert_eq!(failure["status"], "failed");
        assert_eq!(failure["render_id"], "sha256:failed-fixture");
        assert_eq!(failure["error"]["code"], "FIXTURE_FAILED");
        assert_eq!(failure["error"]["message"], "fixture failure");

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

    #[test]
    fn binds_sorted_artifacts_and_the_published_pdf_to_the_render_id() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sandbox =
            std::env::temp_dir().join(format!("pliego-bundle-{}-{unique}", std::process::id()));
        fs::create_dir(&sandbox).unwrap();
        let artifacts = SessionArtifacts::create_with_render_id(
            sandbox.join("artifacts"),
            "sha256:bundle-fixture",
        )
        .unwrap();
        let output = sandbox.join("invoice.pdf");

        artifacts.write_scene(b"{}\n").unwrap();
        artifacts.write_document_pdf(b"%PDF-bundle").unwrap();
        artifacts.publish_document_pdf(&output).unwrap();
        artifacts.record_state("started", None).unwrap();
        artifacts.record_state("rendered", None).unwrap();
        let bundle_path = artifacts.write_bundle(&output).unwrap();
        let bundle: serde_json::Value =
            serde_json::from_slice(&fs::read(bundle_path).unwrap()).unwrap();

        assert_eq!(bundle["schema"], "pliego.bundle");
        assert_eq!(bundle["version"], 1);
        assert_eq!(bundle["render_id"], "sha256:bundle-fixture");
        assert_eq!(bundle["output"]["path"], output.to_string_lossy().as_ref());
        assert_eq!(bundle["output"]["bytes"], 11);
        assert_eq!(
            bundle["output"]["sha256"],
            "sha256:1e3325b692c5c5d3a7e354870e4ee26947d6d4614f48e5e4d2125bb944eeae16"
        );
        let paths: Vec<_> = bundle["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect();
        assert_eq!(
            paths,
            [
                "console.jsonl",
                "document.pdf",
                "resources.jsonl",
                "scene.json",
                "session-state.jsonl",
            ]
        );

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
