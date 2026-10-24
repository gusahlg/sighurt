/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Crash-safe commits of the public cookie jar and HSTS list.
//!
//! One writer thread owns every commit. It snapshots under the jar's lock, then, outside
//! that lock, writes an exclusive temporary file in the same directory (mode 0600), flushes,
//! fsyncs the file, renames it over the destination, and fsyncs the directory. A failure
//! leaves the generation dirty, retries on the next wake, and is logged once until a commit
//! succeeds. Session and expired cookies are never written. HTTP authentication is not
//! written, and an existing `auth_cache.json` is deleted at load.
//!
//! The resource thread stops and joins this writer on `Exit`, before Servo's shutdown
//! returns. Private browsing state is never passed in.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::cookie_storage::CookieStorage;
use crate::hsts::HstsList;

const COOKIE_FILE: &str = "cookie_jar.json";
const HSTS_FILE: &str = "hsts_list.json";
const AUTH_FILE: &str = "auth_cache.json";
/// Upper bound on how long a dirty jar waits before a commit. Shutdown does not wait this out.
pub(crate) const COMMIT_INTERVAL: Duration = Duration::from_secs(5);
const MAX_PROFILE_BYTES: u64 = 32 * 1024 * 1024;

/// What the writer reads. Generations change only when the jar or the HSTS list changes.
pub(crate) trait ProfileSnapshot: Send + Sync {
    fn cookie_generation(&self) -> u64;
    fn hsts_generation(&self) -> u64;
    /// Generation and the persistent, unexpired cookies, taken under one lock hold.
    fn snapshot_cookies(&self) -> (u64, CookieStorage);
    fn snapshot_hsts(&self) -> (u64, HstsList);
}

pub(crate) struct ProfileWriter {
    stop: Option<mpsc::Sender<()>>,
    handle: Option<JoinHandle<()>>,
}

impl ProfileWriter {
    pub(crate) fn spawn<S>(dir: PathBuf, state: Arc<S>) -> Self
    where
        S: ProfileSnapshot + 'static,
    {
        Self::spawn_every(dir, state, COMMIT_INTERVAL)
    }

    pub(crate) fn spawn_every<S>(dir: PathBuf, state: Arc<S>, interval: Duration) -> Self
    where
        S: ProfileSnapshot + 'static,
    {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("profile-writer".to_owned())
            .spawn(move || run(dir, state, interval, rx))
            .expect("profile writer thread");
        Self {
            stop: Some(tx),
            handle: Some(handle),
        }
    }

    /// Wake the writer for its final commit and wait until that commit has finished.
    pub(crate) fn join(&mut self) {
        self.stop.take();
        if let Some(handle) = self.handle.take() &&
            handle.join().is_err()
        {
            log::warn!("profile writer stopped after a panic");
        }
    }
}

impl Drop for ProfileWriter {
    fn drop(&mut self) {
        self.join();
    }
}

fn run<S: ProfileSnapshot>(
    dir: PathBuf,
    state: Arc<S>,
    interval: Duration,
    stop: mpsc::Receiver<()>,
) {
    remove_stale_temps(&dir, COOKIE_FILE);
    remove_stale_temps(&dir, HSTS_FILE);
    let mut cookies_at = 0u64;
    let mut hsts_at = 0u64;
    let mut cookies_logged = false;
    let mut hsts_logged = false;
    loop {
        commit(
            state.as_ref(),
            &dir,
            &mut cookies_at,
            &mut hsts_at,
            &mut cookies_logged,
            &mut hsts_logged,
        );
        match stop.recv_timeout(interval) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                commit(
                    state.as_ref(),
                    &dir,
                    &mut cookies_at,
                    &mut hsts_at,
                    &mut cookies_logged,
                    &mut hsts_logged,
                );
                break;
            },
            Err(RecvTimeoutError::Timeout) => {},
        }
    }
}

fn commit<S: ProfileSnapshot>(
    state: &S,
    dir: &Path,
    cookies_at: &mut u64,
    hsts_at: &mut u64,
    cookies_logged: &mut bool,
    hsts_logged: &mut bool,
) {
    if state.cookie_generation() != *cookies_at {
        let (generation, jar) = state.snapshot_cookies();
        store(dir, COOKIE_FILE, generation, cookies_at, &jar, cookies_logged);
    }
    if state.hsts_generation() != *hsts_at {
        let (generation, list) = state.snapshot_hsts();
        store(dir, HSTS_FILE, generation, hsts_at, &list, hsts_logged);
    }
}

fn store<T: Serialize>(
    dir: &Path,
    name: &str,
    generation: u64,
    committed: &mut u64,
    value: &T,
    logged: &mut bool,
) {
    if generation == *committed {
        return;
    }
    let bytes = match encode(value) {
        Ok(bytes) => bytes,
        Err(error) => {
            note_failure(name, &error, logged);
            return;
        },
    };
    match write_atomic(dir, name, &bytes) {
        Ok(()) => {
            *committed = generation;
            *logged = false;
        },
        Err(error) => note_failure(name, &error, logged),
    }
}

fn note_failure(name: &str, error: &io::Error, logged: &mut bool) {
    if !*logged {
        log::warn!("could not persist {name}: {error}");
        *logged = true;
    }
}

/// Deletes a persisted HTTP auth cache and any incomplete commit from a previous process.
pub(crate) fn prepare_profile_dir(dir: &Path) {
    let auth = dir.join(AUTH_FILE);
    match fs::remove_file(&auth) {
        Ok(()) => log::debug!("removed persisted HTTP authentication cache"),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(error) => {
            log::warn!("could not remove persisted HTTP authentication cache: {error}");
        },
    }
    remove_stale_temps(dir, COOKIE_FILE);
    remove_stale_temps(dir, HSTS_FILE);
}

/// Loads the jar, dropping session and expired cookies. A damaged file is renamed aside.
pub(crate) fn load_cookies(dir: &Path) -> CookieStorage {
    match read_profile::<CookieStorage>(&dir.join(COOKIE_FILE)) {
        Loaded::Value(mut jar) => {
            jar.drop_session_and_expired();
            jar
        },
        Loaded::Missing | Loaded::Damaged => CookieStorage::new(150),
    }
}

/// Loads the HSTS list. A damaged file is renamed aside. The preload list is untouched.
pub(crate) fn load_hsts(dir: &Path) -> HstsList {
    match read_profile::<HstsList>(&dir.join(HSTS_FILE)) {
        Loaded::Value(list) => list,
        Loaded::Missing | Loaded::Damaged => HstsList::default(),
    }
}

enum Loaded<T> {
    Missing,
    Value(T),
    Damaged,
}

fn read_profile<T: DeserializeOwned>(path: &Path) -> Loaded<T> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Loaded::Missing,
        Err(error) => {
            log::warn!("could not read {}: {error}", path.display());
            quarantine(path);
            return Loaded::Damaged;
        },
    };
    if !meta.file_type().is_file() {
        log::warn!(
            "profile file {} is not a regular file; setting it aside",
            path.display()
        );
        quarantine(path);
        return Loaded::Damaged;
    }
    if meta.len() > MAX_PROFILE_BYTES {
        log::warn!(
            "profile file {} is too large; setting it aside",
            path.display()
        );
        quarantine(path);
        return Loaded::Damaged;
    }
    let mut perms = meta.permissions();
    perms.set_mode(0o600);
    if let Err(error) = fs::set_permissions(path, perms) {
        log::warn!("could not restrict {}: {error}", path.display());
    }
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            log::warn!("could not read {}: {error}", path.display());
            quarantine(path);
            return Loaded::Damaged;
        },
    };
    let mut buf = String::new();
    if let Err(error) = file.read_to_string(&mut buf) {
        log::warn!("could not read {}: {error}", path.display());
        drop(file);
        quarantine(path);
        return Loaded::Damaged;
    }
    drop(file);
    match serde_json::from_str(&buf) {
        Ok(value) => Loaded::Value(value),
        Err(error) => {
            log::warn!("damaged profile file {}: {error}", path.display());
            quarantine(path);
            Loaded::Damaged
        },
    }
}

fn quarantine(path: &Path) {
    let mut damaged = path.as_os_str().to_owned();
    damaged.push(".damaged");
    let dest = PathBuf::from(damaged);
    if let Err(error) = fs::rename(path, &dest) {
        log::warn!(
            "damaged profile file {} could not be set aside: {error}",
            path.display()
        );
    } else {
        log::warn!("damaged profile file set aside at {}", dest.display());
    }
}

fn remove_stale_temps(dir: &Path, name: &str) {
    let prefix = format!(".{name}.");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(fname) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if fname.starts_with(&prefix) && fname.ends_with(".tmp") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

fn encode<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    serde_json::to_vec_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
fn write_state(dir: &Path, name: &str, value: &impl Serialize) -> io::Result<()> {
    let bytes = encode(value)?;
    write_atomic(dir, name, &bytes)
}

/// Exclusive same-directory temp file, mode 0600, then write, flush, fsync, rename, fsync dir.
fn write_atomic(dir: &Path, name: &str, bytes: &[u8]) -> io::Result<()> {
    let dest = dir.join(name);
    let tmp = temp_path(dir, name);
    let created = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        let mut perms = file.metadata()?.permissions();
        perms.set_mode(0o600);
        file.set_permissions(perms)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = created {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    File::open(dir)?.sync_all()?;
    Ok(())
}

fn temp_path(dir: &Path, name: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{name}.{}.{n}.tmp", std::process::id()))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::time::SystemTime;

    use std::collections::HashMap;

    use cookie::Cookie;

    use super::*;
    use crate::cookie::ServoCookie;
    use crate::hsts::HstsEntry;

    const PERSIST: &str = "persist_value_9f3a";
    const SESSION: &str = "session_value_9f3a";
    const EXPIRED: &str = "expired_value_9f3a";
    const EXPIRY: u64 = 1_900_000_000;

    struct MemoryProfile {
        cookies: Mutex<CookieStorage>,
        hsts: Mutex<HstsList>,
    }

    impl ProfileSnapshot for MemoryProfile {
        fn cookie_generation(&self) -> u64 {
            self.cookies.lock().unwrap().generation()
        }

        fn hsts_generation(&self) -> u64 {
            self.hsts.lock().unwrap().generation()
        }

        fn snapshot_cookies(&self) -> (u64, CookieStorage) {
            let jar = self.cookies.lock().unwrap();
            (jar.generation(), jar.persistent_snapshot())
        }

        fn snapshot_hsts(&self) -> (u64, HstsList) {
            let list = self.hsts.lock().unwrap();
            (list.generation(), list.clone())
        }
    }

    struct Temp {
        path: PathBuf,
    }

    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "sighurt-net-profile-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let mut perms = fs::metadata(&self.path).unwrap().permissions();
            perms.set_mode(0o700);
            let _ = fs::set_permissions(&self.path, perms);
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn expiry() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(EXPIRY)
    }

    fn cookie(name: &str, value: &str, persistent: bool, expiry_time: Option<SystemTime>) -> ServoCookie {
        let built = Cookie::build((name.to_owned(), value.to_owned()))
            .path("/secret")
            .domain("example.com")
            .secure(true)
            .http_only(true)
            .max_age(time::Duration::seconds(100))
            .build();
        ServoCookie {
            cookie: built,
            host_only: true,
            persistent,
            creation_time: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            last_access: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_010),
            expiry_time,
        }
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn temps(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter_map(|entry| {
                let name = entry.file_name().to_str()?.to_owned();
                name.ends_with(".tmp").then_some(name)
            })
            .collect()
    }

    #[test]
    fn persistent_unexpired_cookies_are_written_atomically() {
        let dir = Temp::new();
        let mut jar = CookieStorage::new(150);
        jar.insert_stored(
            "example.com".to_owned(),
            cookie("persist", PERSIST, true, Some(expiry())),
        );
        jar.insert_stored(
            "example.com".to_owned(),
            cookie("session", SESSION, false, None),
        );
        jar.insert_stored(
            "example.com".to_owned(),
            cookie("expired", EXPIRED, true, Some(SystemTime::UNIX_EPOCH)),
        );
        let snap = jar.persistent_snapshot();
        write_state(dir.path(), COOKIE_FILE, &snap).unwrap();

        let path = dir.path().join(COOKIE_FILE);
        assert_eq!(mode(&path), 0o600);
        assert!(temps(dir.path()).is_empty(), "no temp file left behind");
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains(PERSIST), "{text}");
        assert!(!text.contains(SESSION), "a session cookie was written: {text}");
        assert!(!text.contains(EXPIRED), "an expired cookie was written: {text}");

        let loaded = load_cookies(dir.path());
        let stored = loaded.stored_cookies();
        assert_eq!(stored.len(), 1);
        let saved = &stored[0];
        assert_eq!(saved.cookie.name(), "persist");
        assert_eq!(saved.cookie.value(), PERSIST);
        assert_eq!(saved.cookie.path(), Some("/secret"));
        assert_eq!(saved.cookie.domain(), Some("example.com"));
        assert_eq!(saved.cookie.secure(), Some(true));
        assert_eq!(saved.cookie.http_only(), Some(true));
        assert_eq!(saved.cookie.max_age(), Some(time::Duration::seconds(100)));
        assert!(saved.host_only);
        assert!(saved.persistent);
        assert_eq!(saved.expiry_time, Some(expiry()));
        assert_eq!(
            saved.creation_time,
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000)
        );
    }

    #[test]
    fn session_cookie_never_reaches_disk() {
        let dir = Temp::new();
        let mut legacy = CookieStorage::new(150);
        legacy.insert_stored(
            "example.com".to_owned(),
            cookie("session", SESSION, false, None),
        );
        legacy.insert_stored(
            "example.com".to_owned(),
            cookie("persist", PERSIST, true, Some(expiry())),
        );
        write_state(dir.path(), COOKIE_FILE, &legacy).unwrap();
        let legacy_text = fs::read_to_string(dir.path().join(COOKIE_FILE)).unwrap();
        assert!(legacy_text.contains(SESSION));

        let loaded = load_cookies(dir.path());
        assert!(
            !loaded
                .stored_cookies()
                .iter()
                .any(|cookie| cookie.cookie.value() == SESSION)
        );
        assert!(loaded.generation() > 0, "dropping session cookies stays dirty");
        let mut entries = HashMap::new();
        entries.insert(
            "example.com".to_owned(),
            vec![HstsEntry {
                host: "example.com".to_owned(),
                include_subdomains: false,
                expires_at: None,
            }],
        );
        let mut hsts = HstsList::from_entries(entries);
        hsts.force_dirty();
        let state = Arc::new(MemoryProfile {
            cookies: Mutex::new(loaded),
            hsts: Mutex::new(hsts),
        });
        let mut writer = ProfileWriter::spawn_every(
            dir.path().to_owned(),
            state,
            Duration::from_millis(20),
        );
        let text = wait_until(&dir.path().join(COOKIE_FILE), |text| {
            text.contains(PERSIST) && !text.contains(SESSION)
        });
        writer.join();
        assert!(!text.contains(SESSION), "{text}");
        assert!(text.contains(PERSIST), "{text}");
        let hsts_text = fs::read_to_string(dir.path().join(HSTS_FILE)).unwrap();
        assert!(hsts_text.contains("example.com"), "{hsts_text}");
    }

    #[test]
    fn damaged_profile_file_is_set_aside() {
        let dir = Temp::new();
        let path = dir.path().join(COOKIE_FILE);
        fs::write(&path, b"{not json").unwrap();
        let loaded = load_cookies(dir.path());
        assert!(loaded.stored_cookies().is_empty());
        assert!(!path.exists(), "the damaged file is no longer in place");
        let aside = dir.path().join("cookie_jar.json.damaged");
        assert_eq!(fs::read(&aside).unwrap(), b"{not json");

        let secret = dir.path().join("secret");
        fs::write(&secret, b"secret-cookie-value").unwrap();
        std::os::unix::fs::symlink(&secret, &path).unwrap();
        let loaded = load_cookies(dir.path());
        assert!(loaded.stored_cookies().is_empty());
        assert_eq!(fs::read(&secret).unwrap(), b"secret-cookie-value");
        assert!(!path.exists());
    }

    #[test]
    fn prepare_profile_dir_deletes_auth_cache() {
        let dir = Temp::new();
        let auth = dir.path().join(AUTH_FILE);
        fs::write(&auth, b"{\"version\":1,\"entries\":{}}").unwrap();
        prepare_profile_dir(dir.path());
        assert!(!auth.exists());
    }

    #[test]
    fn failed_commit_keeps_state_and_retries() {
        let dir = Temp::new();
        let path = dir.path().join(COOKIE_FILE);
        fs::write(&path, b"original").unwrap();
        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(dir.path(), perms).unwrap();
        let error = write_atomic(dir.path(), COOKIE_FILE, b"{\"version\":1}").unwrap_err();
        assert_eq!(fs::read(&path).unwrap(), b"original", "{error}");
        assert!(temps(dir.path()).is_empty());

        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir.path(), perms).unwrap();
        fs::remove_file(&path).unwrap();

        let mut jar = CookieStorage::new(150);
        jar.insert_stored(
            "example.com".to_owned(),
            cookie("persist", PERSIST, true, Some(expiry())),
        );
        let state = Arc::new(MemoryProfile {
            cookies: Mutex::new(jar),
            hsts: Mutex::new(HstsList::default()),
        });
        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o555);
        fs::set_permissions(dir.path(), perms).unwrap();
        let mut writer = ProfileWriter::spawn_every(
            dir.path().to_owned(),
            Arc::clone(&state),
            Duration::from_millis(30),
        );
        std::thread::sleep(Duration::from_millis(150));
        assert!(!path.exists(), "a failed commit must not replace the jar");
        assert_eq!(state.cookies.lock().unwrap().stored_cookies().len(), 1);

        let mut perms = fs::metadata(dir.path()).unwrap().permissions();
        perms.set_mode(0o700);
        fs::set_permissions(dir.path(), perms).unwrap();
        let text = wait_for(&path, PERSIST);
        writer.join();
        assert!(text.contains(PERSIST), "{text}");
        assert!(!text.contains(SESSION));
        assert_eq!(mode(&path), 0o600);
        assert_eq!(state.cookies.lock().unwrap().stored_cookies().len(), 1);
    }

    fn wait_for(path: &Path, needle: &str) -> String {
        wait_until(path, |text| text.contains(needle))
    }

    fn wait_until(path: &Path, ready: impl Fn(&str) -> bool) -> String {
        let start = std::time::Instant::now();
        loop {
            if let Ok(text) = fs::read_to_string(path) &&
                ready(&text)
            {
                return text;
            }
            if start.elapsed() > Duration::from_secs(5) {
                let got = fs::read_to_string(path).unwrap_or_default();
                panic!("timed out waiting for {} ({got})", path.display());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
