//! Owning wrapper around `*mut PtpRuntime` plus the connect / setup /
//! teardown sequence for the WIRELESS_COMM transport.
//!
//! libfuji is blocking and not Send-safe; all methods here must be called
//! from the dedicated camera thread.

use std::ffi::{c_int, CString};
use std::ptr::NonNull;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use tracing::{debug, info, warn};

use super::ffi::sys;
use super::object_info::ObjectInfo;

const FUJI_PTP_PORT: c_int = 55740;
const CONNECT_EXTRA_TIMEOUT_S: c_int = 5;
const PTP_OK: c_int = 0;

/// RAII handle for `PtpRuntime`. `Drop` calls `ptp_close` so leaks aren't
/// silent. Owned exclusively by the camera thread.
pub struct Runtime {
    raw: NonNull<sys::PtpRuntime>,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        // PTP_IP_USB is the connection style libfuji uses for WIRELESS_COMM
        // (TCP-based but with USB-style packet framing per libpict's enum).
        let raw = unsafe { sys::ptp_new(sys::PtpConnType::PTP_IP_USB as _) };
        let raw = NonNull::new(raw).ok_or_else(|| anyhow!("ptp_new returned NULL"))?;
        Ok(Runtime { raw })
    }

    pub fn as_ptr(&self) -> *mut sys::PtpRuntime {
        self.raw.as_ptr()
    }

    /// Reset PTP session state and configure the WIRELESS_COMM transport for
    /// the given camera IP.
    pub fn prepare_wireless(&mut self, ip: &str) -> Result<()> {
        let rc = unsafe { sys::fuji_reset_ptp(self.raw.as_ptr()) };
        check_rc("fuji_reset_ptp", rc)?;

        let knowledge = unsafe { sys::fuji_get(self.raw.as_ptr()) };
        if knowledge.is_null() {
            bail!("fuji_get returned NULL after fuji_reset_ptp");
        }
        // SAFETY: knowledge points to the runtime's owned FujiDeviceKnowledge.
        // Writing to its fields is what libfuji's reference frontend does in
        // `backend.c::cTryConnectWiFi`.
        unsafe {
            (*knowledge).transport = sys::FujiTransport::FUJI_FEATURE_WIRELESS_COMM;
            write_cstring_field(&mut (*knowledge).ip_address, ip)?;
        }
        Ok(())
    }

    pub fn ptpip_connect(&mut self, ip: &str) -> Result<()> {
        let c_ip = CString::new(ip).map_err(|e| anyhow!("invalid camera IP {ip:?}: {e}"))?;
        let rc = unsafe {
            sys::ptpip_connect(
                self.raw.as_ptr(),
                c_ip.as_ptr(),
                FUJI_PTP_PORT,
                CONNECT_EXTRA_TIMEOUT_S,
            )
        };
        check_rc("ptpip_connect", rc)
    }

    /// Drive the full Fuji setup state machine: `ptpip_fuji_init_req`,
    /// session open, the "press OK on camera" first-pair handshake, version
    /// negotiation, and remote-mode entry if supported.
    pub fn fuji_setup(&mut self) -> Result<()> {
        let rc = unsafe { sys::fuji_setup(self.raw.as_ptr()) };
        check_rc("fuji_setup", rc)
    }

    pub fn num_objects(&self) -> Result<i32> {
        let knowledge = unsafe { sys::fuji_get(self.raw.as_ptr()) };
        if knowledge.is_null() {
            bail!("fuji_get returned NULL");
        }
        Ok(unsafe { (*knowledge).num_objects })
    }

    /// Poll one round of events (analog of libfuji's `fuji_get_events`).
    /// Returns the current `num_objects` count so callers can compare against
    /// their last-seen value to detect new photos.
    pub fn poll_events(&mut self) -> Result<i32> {
        let rc = unsafe { sys::fuji_get_events(self.raw.as_ptr()) };
        check_rc("fuji_get_events", rc)?;
        self.num_objects()
    }

    /// Fetch metadata for a single object handle.
    ///
    /// libfuji's `fuji_begin_download_get_object_info` sets `EnableCorrectFileSize=1`
    /// internally so the returned `compressed_size` is accurate.
    pub fn object_info(&mut self, handle: i32, tz: Tz) -> Result<ObjectInfo> {
        let mut oi = sys::PtpObjectInfo::default();
        let rc = unsafe {
            sys::fuji_begin_download_get_object_info(self.raw.as_ptr(), handle, &mut oi)
        };
        check_rc("fuji_begin_download_get_object_info", rc)?;
        // SAFETY: rc == 0 means libfuji wrote into `oi`.
        unsafe { ObjectInfo::from_raw(&oi, tz) }
    }

}

impl Drop for Runtime {
    fn drop(&mut self) {
        // libpict's `ptp_close` calls `free(r)` — it destroys the entire
        // runtime, not just the socket. Only call it once, at the very end of
        // the camera thread's lifetime. Retry between connection attempts
        // happens via `fuji_reset_ptp`, which resets without freeing.
        unsafe {
            sys::ptp_close(self.raw.as_ptr());
        }
    }
}

fn check_rc(op: &str, rc: c_int) -> Result<()> {
    if rc == PTP_OK {
        Ok(())
    } else {
        Err(anyhow!("{op} failed with rc={rc}"))
    }
}

/// Write a Rust string into a fixed-size C `char[N]` field, NUL-terminated.
unsafe fn write_cstring_field(buf: &mut [i8], s: &str) -> Result<()> {
    if s.as_bytes().iter().any(|&b| b == 0) {
        bail!("string contains NUL byte: {s:?}");
    }
    if s.len() >= buf.len() {
        bail!(
            "string too long for C buffer ({} bytes, buffer holds {})",
            s.len(),
            buf.len()
        );
    }
    for (dst, src) in buf.iter_mut().zip(s.as_bytes().iter()) {
        *dst = *src as i8;
    }
    buf[s.len()] = 0;
    Ok(())
}

/// Try to connect once. Returns `Ok` only when the camera is fully set up.
pub fn try_connect(runtime: &mut Runtime, ip: &str) -> Result<()> {
    info!(camera_ip = %ip, "connecting");
    runtime.prepare_wireless(ip)?;
    runtime.ptpip_connect(ip)?;
    debug!("ptpip_connect ok, running fuji_setup");
    runtime.fuji_setup()?;
    info!("camera setup complete");
    Ok(())
}

/// Connect with exponential backoff between attempts. Always returns `Ok`
/// once connected; only propagates a fatal error if the runtime is unusable.
pub fn connect_with_backoff(
    runtime: &mut Runtime,
    ip: &str,
    shutdown: &dyn Fn() -> bool,
) -> Result<()> {
    let mut delay = Duration::from_secs(1);
    let max_delay = Duration::from_secs(30);
    loop {
        if shutdown() {
            bail!("shutdown requested during connect");
        }
        match try_connect(runtime, ip) {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(error = %e, retry_in_s = delay.as_secs(), "connect failed");
                // libpict closes its own socket on failed connects; the next
                // `fuji_reset_ptp` call inside try_connect resets state.
                std::thread::sleep(delay);
                delay = (delay * 2).min(max_delay);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BackfillCutoff {
    pub not_before: Option<DateTime<Utc>>,
}

impl BackfillCutoff {
    pub fn accept(&self, info: &ObjectInfo) -> bool {
        match self.not_before {
            Some(t) => info.date_created_utc >= t,
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn cutoff_accepts_when_no_floor() {
        let cutoff = BackfillCutoff { not_before: None };
        let info = ObjectInfo {
            filename: "X.JPG".into(),
            compressed_size: 1,
            date_created_utc: Utc.with_ymd_and_hms(2020, 1, 1, 0, 0, 0).unwrap(),
            obj_format: 0x3801,
            kind: super::super::AssetKind::Jpeg,
        };
        assert!(cutoff.accept(&info));
    }

    #[test]
    fn cutoff_filters_old_assets() {
        let cutoff = BackfillCutoff {
            not_before: Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()),
        };
        let old = ObjectInfo {
            filename: "OLD.JPG".into(),
            compressed_size: 1,
            date_created_utc: Utc.with_ymd_and_hms(2025, 6, 1, 0, 0, 0).unwrap(),
            obj_format: 0x3801,
            kind: super::super::AssetKind::Jpeg,
        };
        let new = ObjectInfo {
            filename: "NEW.JPG".into(),
            compressed_size: 1,
            date_created_utc: Utc.with_ymd_and_hms(2026, 3, 1, 0, 0, 0).unwrap(),
            obj_format: 0x3801,
            kind: super::super::AssetKind::Jpeg,
        };
        assert!(!cutoff.accept(&old));
        assert!(cutoff.accept(&new));
    }

    #[test]
    fn write_cstring_rejects_overflow() {
        let mut buf = [0i8; 4];
        let result = unsafe { write_cstring_field(&mut buf, "this is too long") };
        assert!(result.is_err());
    }

    #[test]
    fn write_cstring_writes_and_terminates() {
        let mut buf = [0i8; 16];
        unsafe { write_cstring_field(&mut buf, "1.2.3.4").unwrap() };
        let bytes: Vec<u8> = buf.iter().map(|&b| b as u8).collect();
        assert_eq!(&bytes[..7], b"1.2.3.4");
        assert_eq!(bytes[7], 0);
    }
}
