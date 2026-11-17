//! What the owner loop and `--dump` do differently on Unix and on Windows: pointing stderr at
//! the log file, and waking the loop for `sig`'s signals and for the configuration watch. On
//! Unix those are descriptors calloop polls; on Windows `sig`'s signals and the configuration
//! watch each ring a calloop ping from a thread ([`Signals::set_wake`],
//! [`ConfigWatch::set_wake`]). The terminal's own sources are [`crate::term`]'s.
//!
//! Owns: those calls. Must not own: the loop, the terminal's I/O, or what a request does.

use std::fs::File;
use std::io;
use std::path::Path;

use calloop::LoopHandle;
use sighurt_runtime::persistence::ConfigWatch;
use sighurt_runtime::signals::{Request, Signals};

/// stderr before [`point_stderr_at`].
#[cfg(unix)]
pub(crate) type SavedStderr = std::os::fd::OwnedFd;

/// stderr before [`point_stderr_at`]: the process's standard handle, which this does not own.
#[cfg(windows)]
pub(crate) struct SavedStderr(usize);

/// Points stderr (fd 2, or Windows' standard error handle, which Rust's `stderr` asks for on
/// each write) at `file`, and returns what it was.
#[cfg(unix)]
pub(crate) fn point_stderr_at(file: &File) -> io::Result<SavedStderr> {
    use std::os::fd::AsFd;
    let saved = io::stderr().as_fd().try_clone_to_owned()?;
    rustix::stdio::dup2_stderr(file)?;
    Ok(saved)
}

/// Points stderr (fd 2, or Windows' standard error handle, which Rust's `stderr` asks for on
/// each write) at `file`, and returns what it was. The handle stderr gets stays open until the
/// process ends.
#[cfg(windows)]
pub(crate) fn point_stderr_at(file: &File) -> io::Result<SavedStderr> {
    use std::os::windows::io::IntoRawHandle;
    use windows_sys::Win32::System::Console::{GetStdHandle, SetStdHandle, STD_ERROR_HANDLE};
    let handle = file.try_clone()?.into_raw_handle();
    let saved = unsafe { GetStdHandle(STD_ERROR_HANDLE) };
    if unsafe { SetStdHandle(STD_ERROR_HANDLE, handle as _) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(SavedStderr(saved as usize))
}

/// Points stderr back at what [`point_stderr_at`] returned.
pub(crate) fn restore_stderr(saved: &SavedStderr) {
    #[cfg(unix)]
    {
        let _ = rustix::stdio::dup2_stderr(saved);
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Console::{SetStdHandle, STD_ERROR_HANDLE};
        unsafe { SetStdHandle(STD_ERROR_HANDLE, saved.0 as _) };
    }
}

/// Adds `signals` to the loop: `on` gets what arrived each time it wakes the loop.
pub(crate) fn insert_signals<D: 'static>(
    handle: &LoopHandle<'static, D>,
    signals: Signals,
    on: fn(&mut D, Vec<Request>),
) -> Result<(), String> {
    #[cfg(unix)]
    {
        use calloop::generic::Generic;
        use calloop::{Interest, Mode, PostAction};
        let source = Generic::new(signals, Interest::READ, Mode::Level);
        handle
            .insert_source(source, move |_, signals, data| {
                on(data, signals.take());
                Ok(PostAction::Continue)
            })
            .map_err(|e| e.error.to_string())?;
    }
    #[cfg(windows)]
    {
        let (ping, source) = calloop::ping::make_ping().map_err(|e| e.to_string())?;
        signals.set_wake(move || ping.ping());
        handle
            .insert_source(source, move |(), &mut (), data| on(data, signals.take()))
            .map_err(|e| e.error.to_string())?;
    }
    Ok(())
}

/// Watches the configuration at `path`: `on` gets whether the file changed each time the
/// watch wakes the loop. A watch that can't be made (no directory yet) is logged, and the file
/// is read at startup only; on a system with no watch at all that is a notice for the user,
/// which this returns.
pub(crate) fn insert_config_watch<D: 'static>(
    handle: &LoopHandle<'static, D>,
    path: &Path,
    on: fn(&mut D, bool),
) -> Result<Option<String>, String> {
    let watch = match ConfigWatch::new(path) {
        Ok(watch) => watch,
        Err(error) if error.kind() == io::ErrorKind::Unsupported => {
            log::info!("not watching {}: {error}", path.display());
            return Ok(Some(NO_WATCH.into()));
        }
        Err(error) => {
            log::info!("not watching {}: {error}", path.display());
            return Ok(None);
        }
    };
    #[cfg(unix)]
    {
        use calloop::generic::Generic;
        use calloop::{Interest, Mode, PostAction};
        let source = Generic::new(watch, Interest::READ, Mode::Level);
        handle
            .insert_source(source, move |_, watch, data| {
                on(data, watch.changed());
                Ok(PostAction::Continue)
            })
            .map_err(|e| e.error.to_string())?;
    }
    #[cfg(windows)]
    {
        let (ping, source) = calloop::ping::make_ping().map_err(|e| e.to_string())?;
        watch.set_wake(move || ping.ping());
        handle
            .insert_source(source, move |(), &mut (), data| on(data, watch.changed()))
            .map_err(|e| e.to_string())?;
    }
    Ok(None)
}

/// The notice where the configuration is not watched.
pub(crate) const NO_WATCH: &str =
    "sig reads its configuration at startup only here: restart it after a change";
