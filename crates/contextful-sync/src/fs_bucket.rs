//! A bucket on a filesystem directory: keys are paths under it, an object's ETag is the
//! sha256 of its bytes, and every conditional put runs under one advisory lock on the
//! bucket, so conditional writes are linearizable across the processes of one machine.
//! Across the clients of a network share that lock guarantees nothing, so a bucket on a
//! volume off the local allowlist reports a machine-local `CasScope`
//! (`store.probe.network-volume`).

use contextful_core::run::journal::sha256_hex;
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// The mount types whose advisory lock the bucket trusts: local disk and memory
/// filesystems. Every other type, a FUSE or unrecognised one included, is a network volume.
const LOCAL_TYPES: [&str; 8] = ["apfs", "hfs", "ext4", "xfs", "btrfs", "zfs", "tmpfs", "overlay"];

/// The volume a bucket directory sits on, by its mount type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeClass {
    Local(String),
    Network(String),
}

impl VolumeClass {
    /// Classify a mount type name against the local allowlist.
    pub fn of(fs_type: &str) -> VolumeClass {
        if LOCAL_TYPES.contains(&fs_type) {
            VolumeClass::Local(fs_type.to_string())
        } else {
            VolumeClass::Network(fs_type.to_string())
        }
    }

    /// The class of the volume holding `path`; a failed or unsupported query reads as `unknown`.
    pub fn detect(path: &Path) -> VolumeClass {
        VolumeClass::of(&mount_type(path).unwrap_or_else(|| "unknown".to_string()))
    }
}

#[cfg(target_vendor = "apple")]
fn mount_type(path: &Path) -> Option<String> {
    let st = rustix::fs::statfs(path).ok()?;
    let name: Vec<u8> = st.f_fstypename.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    Some(String::from_utf8_lossy(&name).into_owned())
}

#[cfg(target_os = "linux")]
fn mount_type(path: &Path) -> Option<String> {
    let st = rustix::fs::statfs(path).ok()?;
    // `f_type` is a signed or unsigned word by architecture; the magic numbers are 32-bit.
    #[allow(clippy::unnecessary_cast)]
    let magic = st.f_type as u32;
    Some(
        match magic {
            0xEF53 => "ext4",
            0x5846_5342 => "xfs",
            0x9123_683E => "btrfs",
            0x2FC1_2FC1 => "zfs",
            0x0102_1994 => "tmpfs",
            0x794C_7630 => "overlay",
            0x6969 => "nfs",
            0x517B => "smb",
            0xFF53_4D42 => "cifs",
            0xFE53_4D42 => "smb2",
            0x6573_5546 => "fuse",
            0x0102_1997 => "9p",
            other => return Some(format!("0x{other:x}")),
        }
        .to_string(),
    )
}

#[cfg(not(any(target_vendor = "apple", target_os = "linux")))]
fn mount_type(_path: &Path) -> Option<String> {
    None
}

/// The filesystem bucket `<root>/<bucket>/`.
#[derive(Debug, Clone)]
pub struct FsBucket {
    root: PathBuf,
    volume: VolumeClass,
}

fn transport(path: &Path, e: std::io::Error) -> ObjectError {
    ObjectError::Transport(format!("{}: {e}", path.display()))
}

impl FsBucket {
    /// Open the bucket named `bucket` under the directory `root`, classifying its volume
    /// by the mount type.
    pub fn open(root: &Path, bucket: &str) -> Result<FsBucket, ObjectError> {
        let dir = FsBucket::create(root, bucket)?;
        let volume = VolumeClass::detect(&dir);
        Ok(FsBucket { root: dir, volume })
    }

    /// Open the bucket as [`FsBucket::open`] does, on a volume of the given class.
    pub fn open_with_volume(root: &Path, bucket: &str, volume: VolumeClass) -> Result<FsBucket, ObjectError> {
        Ok(FsBucket { root: FsBucket::create(root, bucket)?, volume })
    }

    fn create(root: &Path, bucket: &str) -> Result<PathBuf, ObjectError> {
        if bucket.is_empty() || bucket.contains('/') || bucket.starts_with('.') {
            return Err(ObjectError::Unsupported(format!("`{bucket}` is not a bucket name")));
        }
        let root = root.join(bucket);
        fs::create_dir_all(&root).map_err(|e| transport(&root, e))?;
        Ok(root)
    }

    /// The volume the bucket directory sits on.
    pub fn volume(&self) -> &VolumeClass {
        &self.volume
    }

    fn path(&self, key: &str) -> Result<PathBuf, ObjectError> {
        if key.is_empty() || key.starts_with('/') || key.split('/').any(|s| s.is_empty() || s == "." || s == ".." || s.starts_with(".bucket")) {
            return Err(ObjectError::Unsupported(format!("`{key}` is not an object key")));
        }
        Ok(self.root.join(key))
    }

    fn lock(&self) -> Result<fs::File, ObjectError> {
        let path = self.root.join(".bucket.lock");
        let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(|e| transport(&path, e))?;
        file.lock().map_err(|e| transport(&path, e))?;
        Ok(file)
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<(), ObjectError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| transport(dir, e))?;
        }
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| ObjectError::Transport(e.to_string()))?;
        let tmp = path.with_file_name(format!(".bucket-tmp-{}", nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()));
        fs::write(&tmp, bytes).map_err(|e| transport(&tmp, e))?;
        fs::rename(&tmp, path).map_err(|e| transport(path, e))
    }
}

impl ObjectStore for FsBucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        let path = self.path(key)?;
        match fs::read(&path) {
            Ok(bytes) => {
                let etag = sha256_hex(&bytes);
                Ok(Some((bytes, etag)))
            }
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(transport(&path, e)),
        }
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        let path = self.path(key)?;
        let _lock = self.lock()?;
        let current = match fs::read(&path) {
            Ok(b) => Some(sha256_hex(&b)),
            Err(e) if e.kind() == ErrorKind::NotFound => None,
            Err(e) => return Err(transport(&path, e)),
        };
        let holds = match &condition {
            Condition::None => true,
            Condition::IfNoneMatch => current.is_none(),
            Condition::IfMatch(etag) => current.as_deref() == Some(etag.as_str()),
        };
        if !holds {
            return Ok(Put::ConditionFailed);
        }
        self.write(&path, bytes)?;
        Ok(Put::Applied(sha256_hex(bytes)))
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        let path = self.path(key)?;
        let _lock = self.lock()?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(transport(&path, e)),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        let mut out = Vec::new();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(transport(&dir, e)),
            };
            for entry in entries {
                let path = entry.map_err(|e| transport(&dir, e))?.path();
                if path.file_name().is_some_and(|n| n.to_string_lossy().starts_with(".bucket")) {
                    continue;
                }
                if path.is_dir() {
                    stack.push(path);
                } else if let Ok(rel) = path.strip_prefix(&self.root) {
                    let key = rel.to_string_lossy().replace('\\', "/");
                    if key.starts_with(prefix) {
                        out.push(key);
                    }
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn cas_scope(&self) -> CasScope {
        match &self.volume {
            VolumeClass::Local(_) => CasScope::Backend,
            VolumeClass::Network(t) => CasScope::Machine(format!("the bucket sits on a `{t}` volume, off the local allowlist, where its lock holds on one machine only")),
        }
    }
}
