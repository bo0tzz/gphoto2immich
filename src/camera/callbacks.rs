//! Strong implementations of libfuji's weak `app_*` / `plat_*` / `tester_*`
//! callbacks. These override the defaults in `lib/plat.c` of libfuji.
//!
//! Important: the variadic logging callbacks (`plat_dbg`, `app_print`,
//! `tester_log`, `tester_fail`) are declared here with non-variadic signatures.
//! The caller-cleans C ABI on x86_64 / aarch64 makes this safe: any varargs
//! passed by libfuji land in registers/stack and are simply ignored. We log
//! only the format string, which is enough breadcrumb to trace what libfuji is
//! doing without pulling in `c_variadic` or libc's `vsnprintf`.

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::sync::OnceLock;

/// Set the client name returned to libfuji from `app_get_client_name`.
/// Call once at startup from `config`. libfuji takes ownership of the returned
/// pointer (it calls `free` on it), so we leak a fresh `CString` on each call.
///
/// Also forces all weak-symbol override functions to stay in the binary by
/// touching `FORCE_KEEP`. Without this, the linker GCs our overrides and
/// libpict/libfuji resolve to their own weak defaults at link time.
pub fn set_client_name(name: &str) {
    let cstr = CString::new(name).expect("client name must not contain NUL");
    CLIENT_NAME
        .set(cstr)
        .expect("client name set more than once");
    // Read the table to defeat --gc-sections. The compiler can't prove these
    // function pointers are never observed externally.
    force_link_overrides();
}

/// Touch every override's address through `black_box` so the linker considers
/// the symbols live. The Rust compiler can't see that they're "really" called
/// from libpict/libfuji's C code, so without this they get GC'd.
fn force_link_overrides() {
    use std::hint::black_box;
    black_box(plat_dbg as *const ());
    black_box(app_print as *const ());
    black_box(tester_log as *const ());
    black_box(tester_fail as *const ());
    black_box(app_get_client_name as *const ());
    black_box(app_send_cam_name as *const ());
    black_box(plat_update_object_info as *const ());
    black_box(fuji_discover_ask_connect as *const ());
    black_box(fuji_discovery_check_cancel as *const ());
    black_box(app_get_os_network_handle as *const ());
    black_box(app_get_wifi_network_handle as *const ());
    black_box(app_bind_socket_to_network as *const ());
    black_box(app_increment_progress_bar as *const ());
    black_box(app_report_download_speed as *const ());
    black_box(app_downloading_file as *const ());
    black_box(app_downloaded_file as *const ());
    black_box(app_check_thread_cancel as *const ());
    black_box(app_get_file_path as *const ());
    black_box(app_get_tether_file_path as *const ());
    black_box(ptp_verbose_log as *const ());
    black_box(ptp_error_log as *const ());
    black_box(ptp_panic as *const ());
}

static CLIENT_NAME: OnceLock<CString> = OnceLock::new();

fn fmt_to_str<'a>(fmt: *const c_char) -> &'a str {
    if fmt.is_null() {
        return "<null>";
    }
    unsafe { CStr::from_ptr(fmt) }.to_str().unwrap_or("<utf8>")
}

#[unsafe(no_mangle)]
pub extern "C" fn plat_dbg(fmt: *const c_char) {
    tracing::trace!(target: "libfuji", "{}", fmt_to_str(fmt));
}

/// libpict logging defaults (compiled out via `PTP_DEFAULT_LOGGING=OFF`).
/// `ptp_verbose_log` and `ptp_error_log` are weak in libpict's log.c, but
/// since log.c is excluded entirely we must provide them.
#[unsafe(no_mangle)]
pub extern "C" fn ptp_verbose_log(fmt: *const c_char) {
    tracing::debug!(target: "libpict", "{}", fmt_to_str(fmt));
}

#[unsafe(no_mangle)]
pub extern "C" fn ptp_error_log(fmt: *const c_char) {
    tracing::warn!(target: "libpict", "{}", fmt_to_str(fmt));
}

/// `ptp_panic` is not weak in libpict — required to be provided when log.c is
/// excluded. libpict declares it `__attribute__((noreturn))`, so we must abort.
#[unsafe(no_mangle)]
pub extern "C" fn ptp_panic(fmt: *const c_char) -> ! {
    tracing::error!(target: "libpict", "PANIC: {}", fmt_to_str(fmt));
    std::process::abort();
}

#[unsafe(no_mangle)]
pub extern "C" fn app_print(_r: *mut c_void, fmt: *const c_char) {
    tracing::debug!(target: "libfuji", "{}", fmt_to_str(fmt));
}

#[unsafe(no_mangle)]
pub extern "C" fn tester_log(_r: *mut c_void, fmt: *const c_char) {
    tracing::debug!(target: "libfuji::test", "{}", fmt_to_str(fmt));
}

#[unsafe(no_mangle)]
pub extern "C" fn tester_fail(_r: *mut c_void, fmt: *const c_char) {
    tracing::error!(target: "libfuji::test", "{}", fmt_to_str(fmt));
}

#[unsafe(no_mangle)]
pub extern "C" fn app_get_client_name(_r: *mut c_void) -> *mut c_char {
    // libfuji frees this pointer, so we hand it a heap copy each call.
    let src = CLIENT_NAME
        .get()
        .map(|c| c.as_c_str())
        .unwrap_or(c"fujimmich");
    // SAFETY: src is a valid NUL-terminated C string; libc::strdup or our own
    // dup via CString::into_raw produces an allocation libfuji can free().
    let dup = CString::from(src).into_raw();
    dup
}

#[unsafe(no_mangle)]
pub extern "C" fn app_send_cam_name(_r: *mut c_void, name: *const c_char) {
    let name = if name.is_null() {
        "<null>"
    } else {
        unsafe { CStr::from_ptr(name) }.to_str().unwrap_or("<utf8>")
    };
    tracing::info!("camera identified itself as {name:?}");
}

#[unsafe(no_mangle)]
pub extern "C" fn plat_update_object_info(
    _r: *mut c_void,
    _handle: c_int,
    _oi: *const c_void,
) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn fuji_discover_ask_connect(_r: *mut c_void, _info: *mut c_void) -> c_int {
    // Discovery (broadcast pairing) path — we don't use it (we connect via a
    // known IP), but if libfuji ever calls this on the WIRELESS_COMM path,
    // accept by default.
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn fuji_discovery_check_cancel(_r: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn app_get_os_network_handle(_h: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn app_get_wifi_network_handle(_h: *mut c_void) -> c_int {
    // Returning -1 mirrors the libfuji default; signals "no specific wifi
    // interface — let the OS routing table pick".
    -1
}

#[unsafe(no_mangle)]
pub extern "C" fn app_bind_socket_to_network(_fd: c_int, _h: *mut c_void) -> c_int {
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn app_increment_progress_bar(_r: *mut c_void, _read: c_int) {}

#[unsafe(no_mangle)]
pub extern "C" fn app_report_download_speed(_r: *mut c_void, _time: i64, _size: usize) {}

#[unsafe(no_mangle)]
pub extern "C" fn app_downloading_file(_r: *mut c_void, _oi: *const c_void) {}

#[unsafe(no_mangle)]
pub extern "C" fn app_downloaded_file(
    _r: *mut c_void,
    _oi: *const c_void,
    _path: *const c_char,
) {
}

#[unsafe(no_mangle)]
pub extern "C" fn app_check_thread_cancel(_r: *mut c_void) -> c_int {
    0
}

/// The default in `lib/plat.c` calls `abort()`. We don't use the import path
/// that triggers this, but we override to a tempdir-based fallback so a stray
/// call doesn't kill the process.
#[unsafe(no_mangle)]
pub extern "C" fn app_get_file_path(
    _r: *mut c_void,
    buffer: *mut c_char,
    filename: *const c_char,
) {
    write_tempfile_path(buffer, filename, "fujimmich-fallback");
}

#[unsafe(no_mangle)]
pub extern "C" fn app_get_tether_file_path(_r: *mut c_void, buffer: *mut c_char) {
    write_tempfile_path(buffer, std::ptr::null(), "fujimmich-tether-fallback");
}

fn write_tempfile_path(buffer: *mut c_char, filename: *const c_char, fallback_name: &str) {
    let mut path = std::env::temp_dir();
    let name = if !filename.is_null() {
        unsafe { CStr::from_ptr(filename) }
            .to_str()
            .unwrap_or(fallback_name)
            .to_owned()
    } else {
        fallback_name.to_owned()
    };
    path.push(name);
    let mut bytes = path.to_string_lossy().into_owned().into_bytes();
    // Buffer is `char[256]` in libfuji; truncate to fit with room for NUL.
    bytes.truncate(255);
    bytes.push(0);
    // SAFETY: caller guarantees buffer is a 256-byte char array.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buffer, bytes.len());
    }
}
