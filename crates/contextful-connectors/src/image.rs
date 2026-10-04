//! Local image headers become metadata rows without decoding pixel data.

use contextful_core::connector::ConnectorError;
use contextful_core::run::ports::{Cancellation, PullRequest, Source};
use contextful_core::run::{Failure, FailureTag, RunError};
use contextful_core::time::Instant;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const NAME: &str = "image";
pub const TABLE: &str = "images";
const PNG_SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

#[derive(Debug, Clone)]
pub struct ImageConfig {
    pub root: PathBuf,
}

impl ImageConfig {
    pub fn parse(value: &Value) -> Result<Self, RunError> {
        let object = value.as_object().ok_or_else(|| RunError::Invalid("`image` config is an object".into()))?;
        if let Some(key) = object.keys().find(|key| key.as_str() != "root") {
            return Err(RunError::PipelineUnknownConfigKey(format!("the `image` source reads no key `{key}`; it reads root")));
        }
        let root = object.get("root").and_then(Value::as_str).filter(|s| !s.is_empty())
            .ok_or_else(|| RunError::Invalid("`image` source names a non-empty `root` directory".into()))?;
        Ok(Self { root: PathBuf::from(root) })
    }

    pub fn table(&self, name: &str) -> Result<(), ConnectorError> {
        if name == TABLE { Ok(()) } else {
            Err(ConnectorError::ConnectorTableUnmatched(format!("the `{NAME}` source serves `{TABLE}`, not `{name}`")))
        }
    }
}

pub struct ImageSource {
    root: PathBuf,
}

impl ImageSource {
    pub fn new(config: ImageConfig, base: &Path) -> Self {
        let root = if config.root.is_absolute() { config.root } else { base.join(config.root) };
        Self { root }
    }

    fn files(&self, cancel: &dyn Cancellation) -> Result<Vec<(String, PathBuf)>, Failure> {
        let root = std::fs::metadata(&self.root).map_err(|e| fault(&self.root, e))?;
        if !root.is_dir() {
            return Err(Failure::deterministic(FailureTag::Config, format!("`image` root `{}` is not a directory", self.root.display())));
        }
        let mut files = Vec::new();
        let mut stack = vec![(String::new(), self.root.clone())];
        while let Some((prefix, dir)) = stack.pop() {
            if cancel.requested() { return Err(Failure::canceled("stopped during image walk")); }
            let mut entries = std::fs::read_dir(&dir).map_err(|e| fault(&dir, e))?
                .map(|entry| entry.map_err(|e| fault(&dir, e))).collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            let mut dirs = Vec::new();
            for entry in entries {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') { continue; }
                let path = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
                let absolute = entry.path();
                let meta = std::fs::symlink_metadata(&absolute).map_err(|e| fault(&absolute, e))?;
                if meta.is_dir() { dirs.push((path, absolute)); }
                else if meta.is_file() && name.to_ascii_lowercase().ends_with(".png") { files.push((path, absolute)); }
            }
            stack.extend(dirs.into_iter().rev());
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(files)
    }
}

fn fault(path: &Path, error: std::io::Error) -> Failure {
    Failure::new(FailureTag::Permanent, format!("`image` source reading `{}`: {error}", path.display()))
}

fn header(path: &str, bytes: &[u8]) -> Result<(u32, u32), Failure> {
    let valid = bytes.len() >= 24 && bytes.starts_with(PNG_SIGNATURE) && bytes[8..12] == 13u32.to_be_bytes()
        && &bytes[12..16] == b"IHDR";
    if !valid {
        return Err(Failure::deterministic(FailureTag::Permanent,
            ConnectorError::ConnectorImageHeaderUnreadable(format!("`{path}` has no readable PNG header")).to_string()));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("checked PNG header length"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("checked PNG header length"));
    if width == 0 || height == 0 {
        return Err(Failure::deterministic(FailureTag::Permanent,
            ConnectorError::ConnectorImageHeaderUnreadable(format!("`{path}` has a zero PNG dimension")).to_string()));
    }
    Ok((width, height))
}

impl Source for ImageSource {
    fn pull(&mut self, _request: &PullRequest, cancel: &dyn Cancellation) -> Result<Vec<u8>, Failure> {
        let mut rows = Vec::new();
        for (path, absolute) in self.files(cancel)? {
            if cancel.requested() { return Err(Failure::canceled("stopped during image read")); }
            let mut file = std::fs::File::open(&absolute).map_err(|e| fault(&absolute, e))?;
            let mut first = [0u8; 24];
            let mut n = 0;
            while n < first.len() {
                let count = file.read(&mut first[n..]).map_err(|e| fault(&absolute, e))?;
                if count == 0 { break; }
                n += count;
            }
            let (width, height) = header(&path, &first[..n])?;
            let mut hash = Sha256::new();
            hash.update(&first[..n]);
            let mut buffer = [0u8; 8192];
            loop {
                let n = file.read(&mut buffer).map_err(|e| fault(&absolute, e))?;
                if n == 0 { break; }
                hash.update(&buffer[..n]);
            }
            let modified = std::fs::metadata(&absolute).map_err(|e| fault(&absolute, e))?.modified().map_err(|e| fault(&absolute, e))?;
            let secs = modified.duration_since(std::time::UNIX_EPOCH).map_err(|e| Failure::new(FailureTag::Permanent, format!("`{path}` modification time: {e}")))?.as_secs();
            let modified_at = Instant::from_unix_secs(i64::try_from(secs).map_err(|e| Failure::new(FailureTag::Permanent, format!("`{path}` modification time: {e}")))?)
                .map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))?.to_rfc3339();
            rows.push(json!({"path": path, "sha256": format!("{:x}", hash.finalize()), "width": width,
                "height": height, "capture_at": null, "modified_at": modified_at, "modality": "image", "body": null, "quotability": null}));
        }
        serde_json::to_vec(&json!({"rows": rows, "more": false, "types": {
            "path": "utf8", "sha256": "utf8", "width": "int64", "height": "int64",
            "capture_at": "timestamp", "modified_at": "timestamp", "modality": "utf8",
            "body": "utf8", "quotability": "utf8"
        }})).map_err(|e| Failure::new(FailureTag::Permanent, e.to_string()))
    }
}
