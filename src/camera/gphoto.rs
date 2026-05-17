//! Thin wrappers over the `gphoto2` crate that turn its types into the
//! shapes the pipeline already speaks.

use anyhow::{anyhow, Context as _, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use gphoto2::filesys::FileInfo;
use sha1::{Digest, Sha1};
use std::io::Write;
use tempfile::NamedTempFile;

use super::object_info::{AssetKind, ObjectInfo};

pub struct DownloadedFile {
    pub file: NamedTempFile,
    pub sha1_hex: String,
    pub bytes_written: u64,
}

/// Turn a libgphoto2 `FileInfo` (+ the filename, which is reported separately
/// by `CameraFS::list_files`) into our pipeline-facing `ObjectInfo`.
///
/// libgphoto2 returns `mtime` as the camera's local wall-clock seconds
/// reinterpreted as Unix epoch — so a photo taken at 14:30 in `tz` arrives
/// as if it were 14:30 UTC. We reverse that by treating the unix-epoch
/// reading as a naive local datetime in `tz` and converting to true UTC.
pub fn digest_info(info: &FileInfo, filename: &str, tz: Tz) -> Result<ObjectInfo> {
    let file = info.file();
    let size = file
        .size()
        .ok_or_else(|| anyhow!("FileInfo missing size for {filename}"))?;
    let mtime = file
        .mtime()
        .ok_or_else(|| anyhow!("FileInfo missing mtime for {filename}"))?;
    let date_created_utc = local_clock_secs_to_utc(mtime, tz)
        .with_context(|| format!("interpreting mtime={mtime} for {filename}"))?;
    Ok(ObjectInfo {
        filename: filename.to_owned(),
        size,
        date_created_utc,
        kind: AssetKind::from_filename(filename),
    })
}

fn local_clock_secs_to_utc(secs: libc::time_t, tz: Tz) -> Result<DateTime<Utc>> {
    use chrono::TimeZone;
    let pseudo_utc = DateTime::<Utc>::from_timestamp(secs, 0)
        .ok_or_else(|| anyhow!("invalid timestamp"))?;
    let naive = pseudo_utc.naive_utc();
    let local = tz
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| anyhow!("ambiguous or missing local time in tz {tz:?}"))?;
    Ok(local.with_timezone(&Utc))
}

/// Spool the camera-file's bytes into a tempfile and hash on the way.
pub fn spool_to_tempfile(data: &[u8]) -> Result<DownloadedFile> {
    let mut tempfile = NamedTempFile::new().context("creating tempfile for download")?;
    tempfile.write_all(data).context("writing download to tempfile")?;
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
    use chrono::TimeZone;

    #[test]
    fn local_to_utc_round_trip_la() {
        // Pretend libgphoto2 handed us "14:30:00 on 2026-05-16" as the
        // camera-local wall clock for a camera set to America/Los_Angeles
        // (UTC-7 in May). True UTC should be 21:30.
        let local = chrono::NaiveDate::from_ymd_opt(2026, 5, 16)
            .unwrap()
            .and_hms_opt(14, 30, 0)
            .unwrap();
        let secs = local.and_utc().timestamp() as libc::time_t;
        let utc = local_clock_secs_to_utc(secs, chrono_tz::America::Los_Angeles).unwrap();
        assert_eq!(utc.to_rfc3339(), "2026-05-16T21:30:00+00:00");
    }

    #[test]
    fn spool_writes_bytes_and_hash() {
        let dl = spool_to_tempfile(b"hello, world!").unwrap();
        assert_eq!(dl.bytes_written, 13);
        let expected = hex::encode(Sha1::digest(b"hello, world!"));
        assert_eq!(dl.sha1_hex, expected);
        let read = std::fs::read(dl.file.path()).unwrap();
        assert_eq!(read, b"hello, world!");
        // Used only so it isn't dead-code-eliminated.
        let _ = chrono_tz::UTC;
        let _ = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0);
    }
}
