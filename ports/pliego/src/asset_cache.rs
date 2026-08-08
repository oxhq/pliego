/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

pub const MANIFEST_SCHEMA: &str = "pliego.asset-manifest";
pub const CACHE_SCOPE: &str = "pliego.asset-cache.v1";
const CACHE_DIRECTORY: &str = ".pliego-asset-cache-v1";
const MAX_CACHE_ENTRIES: usize = 128;
pub(crate) const MAX_CACHE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheResult {
    Hit,
    Miss,
    Invalidated,
}

impl CacheResult {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Hit => "hit",
            Self::Miss => "miss",
            Self::Invalidated => "invalidated",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CachedAsset {
    pub url: url::Url,
    pub content_hash: String,
    pub body: Vec<u8>,
    pub cache_result: CacheResult,
}

#[derive(Clone, Debug)]
pub struct AssetStore {
    manifest: PathBuf,
    cache_directory: PathBuf,
    assets: BTreeMap<String, CachedAsset>,
    hits: usize,
    misses: usize,
    invalidations: usize,
    evictions: usize,
}

impl AssetStore {
    pub fn load(path: &Path) -> Result<Self, AssetError> {
        let manifest = path.canonicalize().map_err(|error| {
            AssetError::new(
                "ASSET_MANIFEST_INVALID",
                None,
                format!(
                    "asset manifest is unavailable at {}: {error}",
                    path.display()
                ),
            )
        })?;
        let root = manifest.parent().ok_or_else(|| {
            AssetError::new(
                "ASSET_MANIFEST_INVALID",
                None,
                "asset manifest has no parent directory".into(),
            )
        })?;
        let bytes = fs::read(&manifest).map_err(|error| {
            AssetError::new(
                "ASSET_MANIFEST_INVALID",
                None,
                format!("cannot read asset manifest {}: {error}", manifest.display()),
            )
        })?;
        let mut parsed: Manifest = serde_json::from_slice(&bytes).map_err(|error| {
            AssetError::new(
                "ASSET_MANIFEST_INVALID",
                None,
                format!("invalid asset manifest JSON: {error}"),
            )
        })?;
        if parsed.schema != MANIFEST_SCHEMA || parsed.version != 1 {
            return Err(AssetError::new(
                "ASSET_MANIFEST_INVALID",
                None,
                "asset manifest schema/version must be pliego.asset-manifest/1".into(),
            ));
        }
        parsed
            .assets
            .sort_by(|left, right| left.url.cmp(&right.url));
        let requested_cache_directory = root.join(CACHE_DIRECTORY);
        fs::create_dir_all(&requested_cache_directory).map_err(|error| {
            AssetError::new(
                "ASSET_CACHE_FAILED",
                None,
                format!(
                    "cannot create asset cache {}: {error}",
                    requested_cache_directory.display()
                ),
            )
        })?;
        let cache_directory = requested_cache_directory.canonicalize().map_err(|error| {
            AssetError::new(
                "ASSET_CACHE_FAILED",
                None,
                format!("cannot resolve asset cache directory: {error}"),
            )
        })?;
        if !cache_directory.starts_with(root) {
            return Err(AssetError::new(
                "ASSET_CACHE_FAILED",
                None,
                "asset cache directory resolves outside the manifest directory".into(),
            ));
        }

        let mut urls = HashSet::new();
        let mut assets = BTreeMap::new();
        let mut hits = 0;
        let mut misses = 0;
        let mut invalidations = 0;
        for entry in parsed.assets {
            let url = url::Url::parse(&entry.url).map_err(|error| {
                AssetError::new(
                    "ASSET_MANIFEST_INVALID",
                    Some(entry.url.clone()),
                    format!("asset URL is invalid: {error}"),
                )
            })?;
            if !matches!(url.scheme(), "http" | "https") ||
                url.host_str().is_none() ||
                !url.username().is_empty() ||
                url.password().is_some()
            {
                return Err(AssetError::new(
                    "ASSET_MANIFEST_INVALID",
                    Some(entry.url),
                    "asset URL must be credential-free http(s)".into(),
                ));
            }
            if !urls.insert(url.to_string()) {
                return Err(AssetError::new(
                    "ASSET_MANIFEST_INVALID",
                    Some(url.to_string()),
                    "asset manifest URLs must be unique".into(),
                ));
            }
            validate_digest(&entry.sha256).map_err(|message| {
                AssetError::new("ASSET_MANIFEST_INVALID", Some(url.to_string()), message)
            })?;
            let source = resolve_source(root, &entry.path).map_err(|message| {
                AssetError::new("ASSET_MANIFEST_INVALID", Some(url.to_string()), message)
            })?;
            let object = cache_directory.join(&entry.sha256);
            let (body, cache_result) = match read_verified(&object, &entry.sha256)? {
                CacheRead::Hit(body) => (body, CacheResult::Hit),
                CacheRead::Missing => {
                    let body = read_source(&source, &url, &entry.sha256)?;
                    write_object(&object, &body)?;
                    (body, CacheResult::Miss)
                },
                CacheRead::Invalid => {
                    fs::remove_file(&object).map_err(|error| {
                        AssetError::new(
                            "ASSET_CACHE_FAILED",
                            Some(url.to_string()),
                            format!("cannot invalidate corrupt cached asset: {error}"),
                        )
                    })?;
                    let body = read_source(&source, &url, &entry.sha256)?;
                    write_object(&object, &body)?;
                    (body, CacheResult::Invalidated)
                },
            };
            match cache_result {
                CacheResult::Hit => hits += 1,
                CacheResult::Miss => misses += 1,
                CacheResult::Invalidated => invalidations += 1,
            }
            assets.insert(
                url.to_string(),
                CachedAsset {
                    url,
                    content_hash: format!("sha256:{}", entry.sha256),
                    body,
                    cache_result,
                },
            );
        }
        let evictions = prune_cache(&cache_directory)?;
        Ok(Self {
            manifest,
            cache_directory,
            assets,
            hits,
            misses,
            invalidations,
            evictions,
        })
    }

    pub fn get(&self, url: &url::Url) -> Option<&CachedAsset> {
        self.assets.get(url.as_str())
    }

    pub fn artifact(&self) -> serde_json::Value {
        serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "version": 1,
            "status": "verified",
            "manifest": self.manifest,
            "cache": {
                "scope": CACHE_SCOPE,
                "directory": self.cache_directory,
                "policy": "bounded-lexicographic-sha256",
                "max_entries": MAX_CACHE_ENTRIES,
                "max_bytes": MAX_CACHE_BYTES,
                "hits": self.hits,
                "misses": self.misses,
                "invalidations": self.invalidations,
                "evictions": self.evictions,
            },
            "assets": self.assets.values().map(|asset| serde_json::json!({
                "url": asset.url,
                "content_hash": asset.content_hash,
                "cache_result": asset.cache_result.as_str(),
                "bytes": asset.body.len(),
            })).collect::<Vec<_>>(),
        })
    }

    pub fn identity_entries(&self) -> impl Iterator<Item = (&str, &str)> {
        self.assets
            .values()
            .map(|asset| (asset.url.as_str(), asset.content_hash.as_str()))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AssetError {
    pub code: &'static str,
    pub url: Option<String>,
    pub message: String,
    pub expected: Option<String>,
    pub actual: Option<String>,
}

impl AssetError {
    fn new(code: &'static str, url: Option<String>, message: String) -> Self {
        Self {
            code,
            url,
            message,
            expected: None,
            actual: None,
        }
    }

    fn mismatch(url: &url::Url, expected: &str, actual: &str) -> Self {
        Self {
            code: "ASSET_HASH_MISMATCH",
            url: Some(url.to_string()),
            message: format!(
                "asset bytes hash to sha256:{actual}, manifest declares sha256:{expected}"
            ),
            expected: Some(format!("sha256:{expected}")),
            actual: Some(format!("sha256:{actual}")),
        }
    }

    pub fn artifact(&self, manifest: &Path) -> serde_json::Value {
        serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "version": 1,
            "status": "failed",
            "manifest": manifest,
            "error": {
                "code": self.code,
                "url": self.url,
                "message": self.message,
                "expected": self.expected,
                "actual": self.actual,
            },
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: String,
    version: u32,
    assets: Vec<ManifestAsset>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestAsset {
    url: String,
    path: String,
    sha256: String,
}

enum CacheRead {
    Hit(Vec<u8>),
    Missing,
    Invalid,
}

fn read_verified(path: &Path, expected: &str) -> Result<CacheRead, AssetError> {
    let body = match fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CacheRead::Missing);
        },
        Err(error) => {
            return Err(AssetError::new(
                "ASSET_CACHE_FAILED",
                None,
                format!("cannot read cached asset {}: {error}", path.display()),
            ));
        },
    };
    if body.len() as u64 > MAX_CACHE_BYTES {
        return Ok(CacheRead::Invalid);
    }
    Ok(if sha256_hex(&body) == expected {
        CacheRead::Hit(body)
    } else {
        CacheRead::Invalid
    })
}

fn read_source(path: &Path, url: &url::Url, expected: &str) -> Result<Vec<u8>, AssetError> {
    let metadata = fs::metadata(path).map_err(|error| {
        AssetError::new(
            "ASSET_NOT_FOUND",
            Some(url.to_string()),
            format!(
                "manifest asset is unavailable at {}: {error}",
                path.display()
            ),
        )
    })?;
    if metadata.len() > MAX_CACHE_BYTES {
        return Err(AssetError::new(
            "ASSET_CACHE_LIMIT",
            Some(url.to_string()),
            format!("manifest asset exceeds the {MAX_CACHE_BYTES}-byte cache bound"),
        ));
    }
    let body = fs::read(path).map_err(|error| {
        AssetError::new(
            "ASSET_NOT_FOUND",
            Some(url.to_string()),
            format!("cannot read manifest asset {}: {error}", path.display()),
        )
    })?;
    if body.len() as u64 > MAX_CACHE_BYTES {
        return Err(AssetError::new(
            "ASSET_CACHE_LIMIT",
            Some(url.to_string()),
            format!("manifest asset exceeds the {MAX_CACHE_BYTES}-byte cache bound"),
        ));
    }
    let actual = sha256_hex(&body);
    if actual != expected {
        return Err(AssetError::mismatch(url, expected, &actual));
    }
    Ok(body)
}

fn resolve_source(root: &Path, path: &str) -> Result<PathBuf, String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty() ||
        path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(
            "asset source path must be a non-empty relative path without parent traversal".into(),
        );
    }
    let source = root.join(path).canonicalize().map_err(|error| {
        format!(
            "asset source is unavailable at {}: {error}",
            root.join(path).display()
        )
    })?;
    if !source.starts_with(root) {
        return Err("asset source resolves outside the manifest directory".into());
    }
    Ok(source)
}

fn validate_digest(digest: &str) -> Result<(), String> {
    if digest.len() == 64 &&
        digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err("asset sha256 must be 64 lowercase hexadecimal characters".into())
    }
}

fn write_object(path: &Path, body: &[u8]) -> Result<(), AssetError> {
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file
            .write_all(body)
            .and_then(|_| file.sync_all())
            .map_err(|error| {
                AssetError::new(
                    "ASSET_CACHE_FAILED",
                    None,
                    format!("cannot write cached asset {}: {error}", path.display()),
                )
            }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(AssetError::new(
            "ASSET_CACHE_FAILED",
            None,
            format!("cannot create cached asset {}: {error}", path.display()),
        )),
    }
}

fn prune_cache(directory: &Path) -> Result<usize, AssetError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| {
            AssetError::new(
                "ASSET_CACHE_FAILED",
                None,
                format!("cannot inspect asset cache: {error}"),
            )
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            validate_digest(&name).ok()?;
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((name, entry.path(), metadata.len()))
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| right.0.cmp(&left.0));
    let mut bytes = entries.iter().map(|entry| entry.2).sum::<u64>();
    let mut count = entries.len();
    let mut evictions = 0;
    for (_, path, size) in entries {
        if count <= MAX_CACHE_ENTRIES && bytes <= MAX_CACHE_BYTES {
            break;
        }
        fs::remove_file(&path).map_err(|error| {
            AssetError::new(
                "ASSET_CACHE_FAILED",
                None,
                format!("cannot evict cached asset {}: {error}", path.display()),
            )
        })?;
        count -= 1;
        bytes -= size;
        evictions += 1;
    }
    Ok(evictions)
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(64);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{AssetStore, CacheResult};

    #[test]
    fn shares_content_by_hash_and_rejects_mismatches() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pliego-asset-cache-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a.svg"), b"same").unwrap();
        fs::write(root.join("b.svg"), b"same").unwrap();
        let digest = super::sha256_hex(b"same");
        let manifest = root.join("assets.json");
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.asset-manifest",
                "version": 1,
                "assets": [
                    { "url": "https://assets.test/a.svg", "path": "a.svg", "sha256": digest },
                    { "url": "https://assets.test/b.svg", "path": "b.svg", "sha256": digest },
                ],
            }))
            .unwrap(),
        )
        .unwrap();
        let first = AssetStore::load(&manifest).unwrap();
        assert_eq!(
            first
                .get(&url::Url::parse("https://assets.test/a.svg").unwrap())
                .unwrap()
                .cache_result,
            CacheResult::Miss
        );
        assert_eq!(
            first
                .get(&url::Url::parse("https://assets.test/b.svg").unwrap())
                .unwrap()
                .cache_result,
            CacheResult::Hit
        );
        assert_eq!(first.artifact()["cache"]["hits"], 1);
        assert_eq!(first.artifact()["cache"]["misses"], 1);
        let second = AssetStore::load(&manifest).unwrap();
        assert!(
            second
                .assets
                .values()
                .all(|asset| asset.cache_result == CacheResult::Hit)
        );
        assert_eq!(second.artifact()["cache"]["hits"], 2);
        assert_eq!(second.artifact()["cache"]["misses"], 0);

        fs::write(root.join(super::CACHE_DIRECTORY).join(&digest), b"corrupt").unwrap();
        let recovered = AssetStore::load(&manifest).unwrap();
        assert_eq!(recovered.artifact()["cache"]["hits"], 1);
        assert_eq!(recovered.artifact()["cache"]["invalidations"], 1);

        fs::write(root.join("a.svg"), b"changed").unwrap();
        fs::write(root.join("b.svg"), b"changed").unwrap();
        let changed_digest = super::sha256_hex(b"changed");
        fs::write(
            &manifest,
            serde_json::to_vec(&serde_json::json!({
                "schema": "pliego.asset-manifest",
                "version": 1,
                "assets": [
                    { "url": "https://assets.test/a.svg", "path": "a.svg", "sha256": changed_digest },
                    { "url": "https://assets.test/b.svg", "path": "b.svg", "sha256": changed_digest },
                ],
            }))
            .unwrap(),
        )
        .unwrap();
        let changed = AssetStore::load(&manifest).unwrap();
        assert_eq!(changed.artifact()["cache"]["hits"], 1);
        assert_eq!(changed.artifact()["cache"]["misses"], 1);
        assert!(
            changed
                .assets
                .values()
                .all(|asset| asset.content_hash == format!("sha256:{changed_digest}"))
        );

        fs::write(root.join("bad.svg"), b"changed").unwrap();
        let bad_digest = super::sha256_hex(b"declared-but-absent");
        fs::write(
            &manifest,
            format!(
                r#"{{"schema":"pliego.asset-manifest","version":1,"assets":[{{"url":"https://assets.test/bad.svg","path":"bad.svg","sha256":"{bad_digest}"}}]}}"#
            ),
        )
        .unwrap();
        let error = AssetStore::load(&manifest).unwrap_err();
        assert_eq!(error.code, "ASSET_HASH_MISMATCH");
        assert_eq!(error.expected, Some(format!("sha256:{bad_digest}")));
        assert_ne!(error.expected, error.actual);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn evicts_cache_objects_in_a_fixed_order() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "pliego-asset-eviction-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&root).unwrap();
        for index in 0..=super::MAX_CACHE_ENTRIES {
            fs::write(root.join(format!("{index:064x}")), b"x").unwrap();
        }

        assert_eq!(super::prune_cache(&root).unwrap(), 1);
        assert_eq!(
            fs::read_dir(&root).unwrap().count(),
            super::MAX_CACHE_ENTRIES
        );
        assert!(
            !root
                .join(format!("{:064x}", super::MAX_CACHE_ENTRIES))
                .exists()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
