//! The persistent key-value store behind `api_kv_store_*`.
//!
//! Any number of views, on the runtime's worker threads, and any number of `sig` processes use
//! the store at once, so it works without locks. Each origin gets a directory and each key a file
//! holding the raw value, both named by a SHA-256 hash (so any key is a valid file name and
//! origins cannot reach each other's files). Writes go to a temporary file that is then renamed
//! over the old value, which is atomic: readers see the old value or the new one, never a torn
//! write, and the last writer wins. Writes are not synced to disk, which would cost a
//! millisecond or more each: a crash may lose the latest values, but never tears one.
//!
//! An origin may keep up to [`MAX_KEYS`] keys with values of up to [`MAX_VALUE_LEN`] bytes,
//! [`MAX_ORIGIN_BYTES`] of values in all, and all origins together [`MAX_TOTAL_BYTES`]. What the
//! store holds is counted from the disk once, when a runtime first writes to it, and kept up to
//! date by the runtime's own writes ([`KvQuota`]); another `sig` process writing at the same
//! time counts its writes on top of what it found, so the two together can take an origin past
//! its quota by what one of them wrote meanwhile.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use sha2::{Digest, Sha256};

/// Most keys one origin may keep.
pub const MAX_KEYS: usize = 10_000;
/// Largest value, in bytes.
pub const MAX_VALUE_LEN: usize = 16 * 1024 * 1024;
/// Most bytes of values one origin may keep: a page can't fill the disk.
pub const MAX_ORIGIN_BYTES: u64 = 64 * 1024 * 1024;
/// Most bytes of values all origins may keep together: many origins can't either.
pub const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;

/// What the store under one root holds, per origin and in all, for the quotas. Shared by every
/// view of a runtime; writes go through it one at a time.
pub struct KvQuota {
    root: PathBuf,
    origin_limit: u64,
    total_limit: u64,
    /// Counted on first use.
    usage: Mutex<Option<Usage>>,
}

#[derive(Default)]
struct Usage {
    total: u64,
    /// By origin directory.
    origins: HashMap<PathBuf, Origin>,
}

#[derive(Clone, Copy, Default)]
struct Origin {
    bytes: u64,
    keys: usize,
}

impl KvQuota {
    /// The quota of the store under `root`, usually [`default_root`].
    pub fn new(root: &Path) -> Self {
        Self::with_limits(root, MAX_ORIGIN_BYTES, MAX_TOTAL_BYTES)
    }

    fn with_limits(root: &Path, origin_limit: u64, total_limit: u64) -> Self {
        Self {
            root: root.to_path_buf(),
            origin_limit,
            total_limit,
            usage: Mutex::new(None),
        }
    }

    /// What the store holds, counted from the disk the first time.
    fn usage(&self) -> MutexGuard<'_, Option<Usage>> {
        let mut usage = self.usage.lock().unwrap_or_else(PoisonError::into_inner);
        if usage.is_none() {
            *usage = Some(count(&self.root));
        }
        usage
    }
}

/// Counts the values under `root`: every origin directory's key files.
fn count(root: &Path) -> Usage {
    let mut usage = Usage::default();
    let Ok(origins) = std::fs::read_dir(root) else {
        return usage;
    };
    for origin in origins.flatten() {
        let Ok(keys) = std::fs::read_dir(origin.path()) else {
            continue;
        };
        let mut counted = Origin::default();
        for key in keys.flatten() {
            // Temporary files, the only names with a dot, don't count.
            if key.file_name().to_string_lossy().contains('.') {
                continue;
            }
            counted.keys += 1;
            counted.bytes += key.metadata().map_or(0, |meta| meta.len());
        }
        usage.total += counted.bytes;
        usage.origins.insert(origin.path(), counted);
    }
    usage
}

/// One origin's keys.
pub struct KvStore {
    dir: PathBuf,
    quota: Arc<KvQuota>,
}

impl KvStore {
    /// The store for `origin` under `quota`'s root.
    pub fn new(quota: &Arc<KvQuota>, origin: &str) -> Self {
        Self {
            dir: quota.root.join(hash(origin)),
            quota: quota.clone(),
        }
    }

    pub fn get(&self, key: &str) -> io::Result<Option<Vec<u8>>> {
        match std::fs::read(self.path(key)) {
            Ok(value) => Ok(Some(value)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Sets `key` to `value`. Fails with [`io::ErrorKind::QuotaExceeded`] for a value over
    /// [`MAX_VALUE_LEN`], a new key once the origin has [`MAX_KEYS`], or a value that would take
    /// the origin past [`MAX_ORIGIN_BYTES`] or the store past [`MAX_TOTAL_BYTES`].
    pub fn set(&self, key: &str, value: &[u8]) -> io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let quota = |what| io::Error::new(io::ErrorKind::QuotaExceeded, what);
        if value.len() > MAX_VALUE_LEN {
            return Err(quota(format!("values are at most {MAX_VALUE_LEN} bytes")));
        }
        let mut usage = self.quota.usage();
        let usage = usage.get_or_insert_default();
        let path = self.path(key);
        let old = std::fs::metadata(&path).ok().map(|meta| meta.len());
        let mut origin = usage.origins.get(&self.dir).copied().unwrap_or_default();
        if old.is_none() && origin.keys >= MAX_KEYS {
            return Err(quota(format!("an origin keeps at most {MAX_KEYS} keys")));
        }
        let new = value.len() as u64;
        let old_bytes = old.unwrap_or(0);
        let origin_bytes = origin.bytes.saturating_sub(old_bytes) + new;
        let total = usage.total.saturating_sub(old_bytes) + new;
        if new > old_bytes && origin_bytes > self.quota.origin_limit {
            let limit = self.quota.origin_limit;
            return Err(quota(format!("an origin keeps at most {limit} bytes")));
        }
        if new > old_bytes && total > self.quota.total_limit {
            return Err(quota("the store is full".into()));
        }
        std::fs::create_dir_all(&self.dir)?;
        let tmp = path.with_extension(format!(
            "{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let written = (|| {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(value)?;
            std::fs::rename(&tmp, &path)
        })();
        if written.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return written;
        }
        origin.bytes = origin_bytes;
        origin.keys += usize::from(old.is_none());
        usage.total = total;
        usage.origins.insert(self.dir.clone(), origin);
        Ok(())
    }

    /// Removes `key`; removing a key that isn't there succeeds.
    pub fn delete(&self, key: &str) -> io::Result<()> {
        let mut usage = self.quota.usage();
        let usage = usage.get_or_insert_default();
        let path = self.path(key);
        let old = std::fs::metadata(&path).ok().map(|meta| meta.len());
        match std::fs::remove_file(path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        if let (Some(old), Some(origin)) = (old, usage.origins.get_mut(&self.dir)) {
            origin.bytes = origin.bytes.saturating_sub(old);
            origin.keys = origin.keys.saturating_sub(1);
            usage.total = usage.total.saturating_sub(old);
        }
        Ok(())
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(hash(key))
    }
}

/// `{data_dir}/sighurt/wasm/kv`: where `sig` keeps the store (see
/// [`WasmConfig::kv_root`](crate::engine::WasmConfig::kv_root)).
pub fn default_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("sighurt")
        .join("wasm")
        .join("kv")
}

fn hash(s: &str) -> String {
    format!("{:x}", Sha256::digest(s))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The store of `origin` under `root`, with a quota of its own.
    fn store(root: &Path, origin: &str) -> KvStore {
        KvStore::new(&Arc::new(KvQuota::new(root)), origin)
    }

    #[test]
    fn set_get_overwrite_delete() {
        let root = tempfile::tempdir().unwrap();
        let kv = store(root.path(), "https://a.test:443");
        assert_eq!(kv.get("k").unwrap(), None);
        kv.set("k", b"one").unwrap();
        assert_eq!(kv.get("k").unwrap().as_deref(), Some(&b"one"[..]));
        kv.set("k", b"").unwrap();
        assert_eq!(kv.get("k").unwrap().as_deref(), Some(&b""[..]));
        kv.delete("k").unwrap();
        assert_eq!(kv.get("k").unwrap(), None);
        kv.delete("k").unwrap();
    }

    #[test]
    fn keys_are_any_string_and_origins_are_separate() {
        let root = tempfile::tempdir().unwrap();
        let a = store(root.path(), "https://a.test:443");
        let b = store(root.path(), "https://b.test:443");
        for key in ["", "../../etc/passwd", "a/b\\c", "ключ", &"x".repeat(1000)] {
            a.set(key, key.as_bytes()).unwrap();
            assert_eq!(a.get(key).unwrap().as_deref(), Some(key.as_bytes()));
            assert_eq!(b.get(key).unwrap(), None);
        }
    }

    #[test]
    fn origins_have_a_quota() {
        let root = tempfile::tempdir().unwrap();
        let kv = store(root.path(), "o");
        let too_big = vec![0; MAX_VALUE_LEN + 1];
        let err = kv.set("big", &too_big).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::QuotaExceeded);
        assert_eq!(kv.get("big").unwrap(), None);
        // Fill the origin by hand; writing ten thousand keys through `set` is slow.
        std::fs::create_dir_all(&kv.dir).unwrap();
        for i in 0..MAX_KEYS - 1 {
            std::fs::write(kv.path(&i.to_string()), b"").unwrap();
        }
        kv.set("last", b"fits").unwrap();
        let err = kv.set("one too many", b"").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::QuotaExceeded);
        // Keys already there can still change or go.
        kv.set("last", b"changed").unwrap();
        kv.delete("last").unwrap();
        kv.set("one too many", b"").unwrap();
    }

    #[test]
    fn origins_and_the_store_have_a_byte_quota() {
        let root = tempfile::tempdir().unwrap();
        let quota = Arc::new(KvQuota::with_limits(root.path(), 100, 150));
        let a = KvStore::new(&quota, "a");
        let b = KvStore::new(&quota, "b");
        a.set("one", &[0; 60]).unwrap();
        a.set("two", &[0; 40]).unwrap();
        let err = a.set("three", &[0; 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::QuotaExceeded);
        // A value that shrinks, or replaces one as large, still fits.
        a.set("one", &[0; 50]).unwrap();
        a.set("three", &[0; 10]).unwrap();
        // The whole store is full before b's origin is.
        b.set("one", &[0; 50]).unwrap();
        let err = b.set("two", &[0; 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::QuotaExceeded);
        a.delete("one").unwrap();
        b.set("two", &[0; 50]).unwrap();
        // A new runtime counts what is on the disk.
        let again = Arc::new(KvQuota::with_limits(root.path(), 100, 150));
        let err = KvStore::new(&again, "b").set("three", &[0; 1]).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::QuotaExceeded);
    }

    /// Two pages of the same origin are two processes with a store each on the same directory.
    #[test]
    fn stores_on_the_same_directory_share_values() {
        let root = tempfile::tempdir().unwrap();
        let one = store(root.path(), "https://a.test:443");
        let two = store(root.path(), "https://a.test:443");
        one.set("k", b"from one").unwrap();
        assert_eq!(two.get("k").unwrap().as_deref(), Some(&b"from one"[..]));
        two.set("k", b"from two").unwrap();
        assert_eq!(one.get("k").unwrap().as_deref(), Some(&b"from two"[..]));
        two.delete("k").unwrap();
        assert_eq!(one.get("k").unwrap(), None);
    }

    #[test]
    fn concurrent_writers_never_leave_a_torn_value() {
        let root = tempfile::tempdir().unwrap();
        let values: Vec<Vec<u8>> = (0..4u8).map(|i| vec![i; 64 * 1024]).collect();
        let values = &values;
        std::thread::scope(|s| {
            for value in values {
                let kv = store(root.path(), "o");
                s.spawn(move || {
                    for _ in 0..20 {
                        kv.set("k", value).unwrap();
                    }
                });
            }
            let kv = store(root.path(), "o");
            s.spawn(move || {
                for _ in 0..200 {
                    if let Some(v) = kv.get("k").unwrap() {
                        assert!(values.contains(&v), "torn value");
                    }
                }
            });
        });
        // No temporary files are left behind.
        let dir = root.path().join(hash("o"));
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 1);
    }
}
