//! Thin wrappers over the `gphoto2` crate that turn its types into the
//! shapes the pipeline already speaks.

use anyhow::{anyhow, Context as _, Result};
use chrono::{DateTime, Utc};
use gphoto2::filesys::FileInfo;
use gphoto2::widget::TextWidget;
use gphoto2::Camera;
use sha1::{Digest, Sha1};
use std::io::Write;
use tempfile::NamedTempFile;
use tracing::debug;

use super::object_info::{AssetKind, ObjectInfo};

pub struct DownloadedFile {
    pub file: NamedTempFile,
    pub sha1_hex: String,
    pub bytes_written: u64,
}

/// Turn a libgphoto2 `FileInfo` (+ the filename, which is reported
/// separately by `CameraFS::list_files`) into our pipeline-facing
/// `ObjectInfo`.
///
/// libgphoto2's `mtime` is already correct UTC unix seconds: it parses
/// the PTP `CaptureDate` string (camera's wall-clock literal, no TZ)
/// via `mktime()`, which respects the daemon's `$TZ`. So as long as
/// the daemon runs with `$TZ` matching the camera's clock — which is
/// what the env var requirement enforces — `mtime` decodes directly
/// to the right moment.
pub fn digest_info(info: &FileInfo, filename: &str) -> Result<ObjectInfo> {
    let file = info.file();
    let size = file
        .size()
        .ok_or_else(|| anyhow!("FileInfo missing size for {filename}"))?;
    let mtime = file
        .mtime()
        .ok_or_else(|| anyhow!("FileInfo missing mtime for {filename}"))?;
    let date_created_utc = DateTime::<Utc>::from_timestamp(mtime, 0)
        .ok_or_else(|| anyhow!("invalid mtime={mtime} for {filename}"))?;
    Ok(ObjectInfo {
        filename: filename.to_owned(),
        size,
        date_created_utc,
        kind: AssetKind::from_filename(filename),
    })
}

/// Read the camera's manufacturer string from its PTP `DeviceInfo`,
/// exposed by libgphoto2 as the `manufacturer` config key. Returns
/// `None` if the key isn't supported or is empty — caller falls back
/// to caching all Immich assets unfiltered.
///
/// This is what gphoto2's `--summary` shows as `Manufacturer:` and
/// matches Immich's EXIF `Make` value exactly (verified against the
/// X-T3: both report `FUJIFILM`).
pub async fn detect_camera_make(camera: &Camera) -> Option<String> {
    match camera.config_key::<TextWidget>("manufacturer").await {
        Ok(w) => {
            let v = w.value().trim().to_owned();
            (!v.is_empty()).then_some(v)
        }
        Err(e) => {
            debug!(error = ?e, "could not read manufacturer from camera config");
            None
        }
    }
}

/// Spool the camera-file's bytes into a tempfile and hash on the way.
pub fn spool_to_tempfile(data: &[u8]) -> Result<DownloadedFile> {
    let mut tempfile = NamedTempFile::new().context("creating tempfile for download")?;
    tempfile
        .write_all(data)
        .context("writing download to tempfile")?;
    tempfile.flush().context("flushing tempfile")?;
    let sha1_hex = hex::encode(Sha1::digest(data));
    Ok(DownloadedFile {
        file: tempfile,
        sha1_hex,
        bytes_written: data.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spool_writes_bytes_and_hash() {
        let dl = spool_to_tempfile(b"hello, world!").unwrap();
        assert_eq!(dl.bytes_written, 13);
        let expected = hex::encode(Sha1::digest(b"hello, world!"));
        assert_eq!(dl.sha1_hex, expected);
        let read = std::fs::read(dl.file.path()).unwrap();
        assert_eq!(read, b"hello, world!");
    }
}
