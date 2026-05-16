//! Streaming download wrapper for `fuji_download_file`.
//!
//! libfuji invokes our `extern "C"` callback repeatedly as bytes stream in
//! from the camera. The callback writes them to a `NamedTempFile` and feeds
//! a `sha1::Sha1` hasher so we have the checksum ready for Immich's
//! `x-immich-checksum` header by the time the download finishes.

use std::ffi::{c_int, c_void};
use std::io::Write;

use anyhow::{anyhow, Result};
use sha1::{Digest, Sha1};
use tempfile::NamedTempFile;
use tracing::warn;

use super::ffi::sys;

/// State accessible to the streaming callback.
struct CallbackContext {
    file: NamedTempFile,
    hasher: Sha1,
    /// Set to `Err` on the first write/IO failure; we let libfuji finish the
    /// transfer (returning -1 from the callback aborts) and surface the error.
    error: Option<std::io::Error>,
    bytes_written: u64,
}

/// The successful result of downloading one object.
pub struct DownloadedFile {
    pub file: NamedTempFile,
    pub sha1_hex: String,
    pub bytes_written: u64,
}

/// SAFETY:
/// - `runtime` must be a valid `PtpRuntime *` returned by `ptp_new` and set
///   up for an active Fuji session.
/// - Must be called from the thread that owns the libfuji mutex (the camera
///   thread). libfuji's callback runs synchronously on this same thread.
pub unsafe fn download_to_tempfile(
    runtime: *mut sys::PtpRuntime,
    handle: c_int,
    file_size: c_int,
) -> Result<DownloadedFile> {
    let file = NamedTempFile::new()
        .map_err(|e| anyhow!("failed to allocate tempfile for download: {e}"))?;

    let mut ctx = CallbackContext {
        file,
        hasher: Sha1::new(),
        error: None,
        bytes_written: 0,
    };

    let rc = unsafe {
        sys::fuji_download_file(
            runtime,
            handle,
            file_size,
            Some(stream_callback),
            &mut ctx as *mut _ as *mut c_void,
        )
    };

    if let Some(err) = ctx.error {
        return Err(anyhow!("write error during download: {err}"));
    }
    if rc != 0 {
        return Err(anyhow!(
            "fuji_download_file returned rc={rc} (handle={handle}, size={file_size})"
        ));
    }
    if file_size >= 0 && (ctx.bytes_written as i64) != file_size as i64 {
        warn!(
            handle,
            expected = file_size,
            actual = ctx.bytes_written,
            "download finished with unexpected byte count"
        );
    }

    let sha1_hex = hex::encode(ctx.hasher.finalize());
    Ok(DownloadedFile {
        file: ctx.file,
        sha1_hex,
        bytes_written: ctx.bytes_written,
    })
}

/// libfuji callback: `int handle_add(void *arg, void *payload, int size, int offset)`.
/// Return 0 on success, non-zero to abort the transfer.
unsafe extern "C" fn stream_callback(
    arg: *mut c_void,
    payload: *mut c_void,
    payload_size: c_int,
    _offset: c_int,
) -> c_int {
    if arg.is_null() || payload.is_null() || payload_size <= 0 {
        return -1;
    }
    let ctx = unsafe { &mut *(arg as *mut CallbackContext) };
    // If we've already failed once, stop accepting more bytes.
    if ctx.error.is_some() {
        return -1;
    }
    let bytes = unsafe { std::slice::from_raw_parts(payload as *const u8, payload_size as usize) };
    if let Err(e) = ctx.file.write_all(bytes) {
        ctx.error = Some(e);
        return -1;
    }
    ctx.hasher.update(bytes);
    ctx.bytes_written += bytes.len() as u64;
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Drive the callback directly, simulating libfuji handing us byte chunks.
    /// Verifies the file ends up with the right contents AND SHA1 hex matches.
    #[test]
    fn callback_assembles_file_and_sha1() {
        let file = NamedTempFile::new().unwrap();
        let mut ctx = CallbackContext {
            file,
            hasher: Sha1::new(),
            error: None,
            bytes_written: 0,
        };

        let chunks: &[&[u8]] = &[b"hello, ", b"world!"];
        for (i, chunk) in chunks.iter().enumerate() {
            let rc = unsafe {
                stream_callback(
                    &mut ctx as *mut _ as *mut c_void,
                    chunk.as_ptr() as *mut c_void,
                    chunk.len() as c_int,
                    i as c_int,
                )
            };
            assert_eq!(rc, 0);
        }

        assert!(ctx.error.is_none());
        assert_eq!(ctx.bytes_written, 13);

        let mut written = String::new();
        ctx.file
            .reopen()
            .unwrap()
            .read_to_string(&mut written)
            .unwrap();
        assert_eq!(written, "hello, world!");

        let expected = hex::encode(Sha1::digest(b"hello, world!"));
        assert_eq!(hex::encode(ctx.hasher.finalize()), expected);
    }

    #[test]
    fn callback_rejects_null_and_zero() {
        let mut ctx = CallbackContext {
            file: NamedTempFile::new().unwrap(),
            hasher: Sha1::new(),
            error: None,
            bytes_written: 0,
        };
        unsafe {
            assert_eq!(
                stream_callback(
                    &mut ctx as *mut _ as *mut c_void,
                    std::ptr::null_mut(),
                    10,
                    0
                ),
                -1
            );
            assert_eq!(
                stream_callback(
                    &mut ctx as *mut _ as *mut c_void,
                    b"x".as_ptr() as *mut c_void,
                    0,
                    0
                ),
                -1
            );
        }
        assert_eq!(ctx.bytes_written, 0);
    }
}
