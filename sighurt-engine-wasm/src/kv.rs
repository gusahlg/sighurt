//! The persistent key-value store behind `api_kv_store_*`.
//!
//! Every page runs in its own `sig-wasm` process, so the store must work with any number of
//! processes using it at once, without locks. Each origin gets a directory and each key a file
//! holding the raw value, both named by a SHA-256 hash (so any key is a valid file name and
//! origins cannot reach each other's files). Writes go to a temporary file that is then renamed
//! over the old value, which is atomic: readers see the old value or the new one, never a torn
//! write, and the last writer wins. Writes are not synced to disk, which would cost a
//! millisecond or more each: a crash may lose the latest values, but never tears one.
//!
//! An origin may keep up to [`MAX_KEYS`] keys with values of up to [`MAX_VALUE_LEN`] bytes.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

/// Most keys one origin may keep.
pub const MAX_KEYS: usize = 10_000;
/// Largest value, in bytes.
pub const MAX_VALUE_LEN: usize = 16 * 1024 * 1024;

/// One origin's keys.
pub struct KvStore {
    dir: PathBuf,
}

impl KvStore {
    /// The store for `origin` under `root`, usually [`default_root`].
    pub fn new(root: &Path, origin: &str) -> Self {
        Self {
            dir: root.join(hash(origin)),
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
    /// [`MAX_VALUE_LEN`], or a new key once the origin has [`MAX_KEYS`].
    pub fn set(&self, key: &str, value: &[u8]) -> io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let quota = |what| io::Error::new(io::ErrorKind::QuotaExceeded, what);
        if value.len() > MAX_VALUE_LEN {
            return Err(quota(format!("values are at most {MAX_VALUE_LEN} bytes")));
        }
        std::fs::create_dir_all(&self.dir)?;
        let path = self.path(key);
        if !path.exists() && self.len()? >= MAX_KEYS {
            return Err(quota(format!("an origin keeps at most {MAX_KEYS} keys")));
        }
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
        }
        written
    }

    /// Removes `key`; removing a key that isn't there succeeds.
    pub fn delete(&self, key: &str) -> io::Result<()> {
        match std::fs::remove_file(self.path(key)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// How many keys the origin has. Temporary files, the only names with a dot, don't count.
    fn len(&self) -> io::Result<usize> {
        let mut keys = 0;
        for entry in std::fs::read_dir(&self.dir)? {
            keys += usize::from(!entry?.file_name().to_string_lossy().contains('.'));
        }
        Ok(keys)
    }

    fn path(&self, key: &str) -> PathBuf {
        self.dir.join(hash(key))
    }
}

/// `{data_dir}/sighurt/wasm/kv`.
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

    #[test]
    fn set_get_overwrite_delete() {
        let root = tempfile::tempdir().unwrap();
        let kv = KvStore::new(root.path(), "https://a.test:443");
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
        let a = KvStore::new(root.path(), "https://a.test:443");
        let b = KvStore::new(root.path(), "https://b.test:443");
        for key in ["", "../../etc/passwd", "a/b\\c", "ключ", &"x".repeat(1000)] {
            a.set(key, key.as_bytes()).unwrap();
            assert_eq!(a.get(key).unwrap().as_deref(), Some(key.as_bytes()));
            assert_eq!(b.get(key).unwrap(), None);
        }
    }

    #[test]
    fn origins_have_a_quota() {
        let root = tempfile::tempdir().unwrap();
        let kv = KvStore::new(root.path(), "o");
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

    /// Two pages of the same origin are two processes with a store each on the same directory.
    #[test]
    fn stores_on_the_same_directory_share_values() {
        let root = tempfile::tempdir().unwrap();
        let one = KvStore::new(root.path(), "https://a.test:443");
        let two = KvStore::new(root.path(), "https://a.test:443");
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
                let kv = KvStore::new(root.path(), "o");
                s.spawn(move || {
                    for _ in 0..20 {
                        kv.set("k", value).unwrap();
                    }
                });
            }
            let kv = KvStore::new(root.path(), "o");
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
