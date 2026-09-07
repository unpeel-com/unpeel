//! Narrow process-boundary operations that Rust 2024 cannot express safely.
//!
//! Environment removal is process-global and Unix peer credentials require
//! FFI. Keep both operations isolated here so the rest of App Kit remains
//! under `deny(unsafe_code)`.

use std::env::{self, VarError};
use std::ffi::OsString;

/// Reads and immediately removes one inherited environment value.
///
/// App Kit calls this during App startup, before the App starts worker
/// threads. Rust 2024 makes environment mutation unsafe because concurrent
/// foreign environment access cannot be synchronized by the standard
/// library. Unpeel Apps must therefore construct `UiBridge` before spawning
/// threads.
pub(crate) fn take_var_os(key: &str) -> Option<OsString> {
    let value = env::var_os(key);
    // SAFETY: `UiBridge::detect` is a startup operation documented to run
    // before the App creates worker threads or invokes foreign libraries.
    unsafe { env::remove_var(key) };
    value
}

/// UTF-8 variant of [`take_var_os`] that still scrubs a malformed value.
pub(crate) fn take_var(key: &str) -> Result<String, VarError> {
    let value = env::var(key);
    // SAFETY: See `take_var_os`; removal happens in the same startup window.
    unsafe { env::remove_var(key) };
    value
}

#[cfg(unix)]
pub(crate) fn peer_has_current_effective_uid(
    stream: &std::os::unix::net::UnixStream,
) -> std::io::Result<Option<bool>> {
    peer_uid(stream).map(|uid| uid.map(|uid| uid == effective_uid()))
}

#[cfg(unix)]
fn effective_uid() -> libc::uid_t {
    // SAFETY: `geteuid` has no pointer arguments and no preconditions.
    unsafe { libc::geteuid() }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<Option<libc::uid_t>> {
    use std::mem::{MaybeUninit, size_of};
    use std::os::fd::AsRawFd;

    let mut credentials = MaybeUninit::<libc::ucred>::zeroed();
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `credentials` points to writable storage of exactly `length`
    // bytes, and both pointers remain valid for the duration of `getsockopt`.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    if length as usize != size_of::<libc::ucred>() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Unix peer credentials had an unexpected size",
        ));
    }
    // SAFETY: A successful `getsockopt` initialized the complete `ucred`.
    Ok(Some(unsafe { credentials.assume_init() }.uid))
}

#[cfg(any(
    target_vendor = "apple",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn peer_uid(stream: &std::os::unix::net::UnixStream) -> std::io::Result<Option<libc::uid_t>> {
    use std::os::fd::AsRawFd;

    let mut uid = 0;
    let mut gid = 0;
    // SAFETY: Both output pointers are valid and writable for this call.
    let result = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(Some(uid))
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))
))]
fn peer_uid(_stream: &std::os::unix::net::UnixStream) -> std::io::Result<Option<libc::uid_t>> {
    // The App-owned socket is still mode 0600 on Unix targets without a
    // portable peer-credential API supported here.
    Ok(None)
}
