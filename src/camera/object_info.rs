//! Helpers for interpreting `PtpObjectInfo` fields.

use std::ffi::CStr;

use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;

use super::ffi::sys;

/// PTP standard JPEG format code (`PTP_OF_JPEG` in libpict's `ptp.h`).
pub const PTP_OF_JPEG: u16 = 0x3801;
/// PTP standard MOV/MP4 format code (`PTP_OF_MOV`).
pub const PTP_OF_MOV: u16 = 0x300D;

/// Classification of a Fuji asset for the upload pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetKind {
    Jpeg,
    Raf,
    Mov,
    Other,
}

impl AssetKind {
    /// Classify by PTP object format first, fall back to filename extension.
    /// Fuji's RAW (`.RAF`) doesn't have a standard PTP code, so the extension
    /// fallback is the primary path for it.
    pub fn classify(obj_format: u16, filename: &str) -> Self {
        let ext_upper: String = filename
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_uppercase())
            .unwrap_or_default();
        match (obj_format, ext_upper.as_str()) {
            (PTP_OF_JPEG, _) | (_, "JPG") | (_, "JPEG") => AssetKind::Jpeg,
            (_, "RAF") => AssetKind::Raf,
            (PTP_OF_MOV, _) | (_, "MOV") | (_, "MP4") => AssetKind::Mov,
            _ => AssetKind::Other,
        }
    }
}

/// A digested view of `PtpObjectInfo` we can carry across the FFI boundary
/// into safe Rust code.
#[derive(Debug, Clone)]
pub struct ObjectInfo {
    pub filename: String,
    pub compressed_size: u32,
    pub date_created_utc: DateTime<Utc>,
    pub obj_format: u16,
    pub kind: AssetKind,
}

impl ObjectInfo {
    /// SAFETY: `oi` must point to a valid `PtpObjectInfo` populated by
    /// `fuji_begin_download_get_object_info`.
    pub unsafe fn from_raw(oi: &sys::PtpObjectInfo, camera_tz: Tz) -> Result<Self> {
        let filename = cstr_field(&oi.filename).context("invalid filename in PtpObjectInfo")?;
        let date_str = cstr_field(&oi.date_created)
            .context("invalid date_created in PtpObjectInfo")?;
        let date_created_utc = parse_camera_date(&date_str, camera_tz)
            .with_context(|| format!("parsing date_created={date_str:?}"))?;
        let obj_format = oi.obj_format;
        let kind = AssetKind::classify(obj_format, &filename);
        Ok(ObjectInfo {
            filename,
            compressed_size: oi.compressed_size,
            date_created_utc,
            obj_format,
            kind,
        })
    }

    /// Basename (filename without extension) used for stack pairing.
    pub fn basename(&self) -> &str {
        self.filename
            .rsplit_once('.')
            .map(|(b, _)| b)
            .unwrap_or(&self.filename)
    }
}

fn cstr_field(buf: &[i8]) -> Result<String> {
    // SAFETY: buf is a fixed-size C `char[N]` array; CStr::from_ptr scans for
    // the terminating NUL, which Fuji firmware always writes.
    let s = unsafe { CStr::from_ptr(buf.as_ptr()) }
        .to_str()
        .map_err(|e| anyhow!("non-UTF8: {e}"))?;
    Ok(s.to_owned())
}

/// Parse Fuji's TZ-less `YYYYMMDDTHHMMSS` timestamp using the configured
/// camera timezone.
pub fn parse_camera_date(s: &str, tz: Tz) -> Result<DateTime<Utc>> {
    let naive = NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S")
        .map_err(|e| anyhow!("expected YYYYMMDDTHHMMSS, got {s:?}: {e}"))?;
    let local = tz
        .from_local_datetime(&naive)
        .single()
        .ok_or_else(|| anyhow!("ambiguous or missing local time in tz {tz:?}"))?;
    Ok(local.with_timezone(&Utc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_jpeg_by_ptp_code() {
        assert_eq!(
            AssetKind::classify(PTP_OF_JPEG, "DSCF1234.JPG"),
            AssetKind::Jpeg
        );
    }

    #[test]
    fn classifies_raf_by_extension() {
        // RAF has no standard PTP code; rely on the extension.
        assert_eq!(AssetKind::classify(0xb103, "DSCF1234.RAF"), AssetKind::Raf);
    }

    #[test]
    fn classifies_mov() {
        assert_eq!(
            AssetKind::classify(PTP_OF_MOV, "DSCF0001.MOV"),
            AssetKind::Mov
        );
    }

    #[test]
    fn classifies_unknown_as_other() {
        assert_eq!(AssetKind::classify(0x9999, "weird.bin"), AssetKind::Other);
    }

    #[test]
    fn parses_camera_date_la() {
        // 12:34:56 local in LA on 2026-03-15 → 19:34:56 UTC (PDT, UTC-7).
        let utc = parse_camera_date("20260315T123456", chrono_tz::America::Los_Angeles).unwrap();
        assert_eq!(utc.to_rfc3339(), "2026-03-15T19:34:56+00:00");
    }

    #[test]
    fn rejects_garbage_date() {
        assert!(parse_camera_date("not-a-date", chrono_tz::UTC).is_err());
    }

    #[test]
    fn basename_strips_extension() {
        let info = ObjectInfo {
            filename: "DSCF1234.RAF".into(),
            compressed_size: 0,
            date_created_utc: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            obj_format: 0,
            kind: AssetKind::Raf,
        };
        assert_eq!(info.basename(), "DSCF1234");
    }

    #[test]
    fn basename_without_extension() {
        let info = ObjectInfo {
            filename: "weird".into(),
            compressed_size: 0,
            date_created_utc: Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
            obj_format: 0,
            kind: AssetKind::Other,
        };
        assert_eq!(info.basename(), "weird");
    }
}
