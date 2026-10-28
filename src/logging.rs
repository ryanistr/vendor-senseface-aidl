// FaceHAL/src/logging.rs
//
// logcat output under the tag the stock HIDL HAL used ("FaceHalHW"), so the
// AIDL HAL's lines land in the same place anyone debugging face unlock looks.
use std::os::raw::{c_char, c_int, c_void};
use std::sync::Once;

static INIT: Once = Once::new();

extern "C" {
    fn __android_log_print(prio: c_int, tag: *const c_char, fmt: *const c_char, ...) -> c_int;
    fn __system_property_get(name: *const c_char, value: *mut c_char) -> c_int;
}

const ANDROID_LOG_INFO: c_int = 4;
const ANDROID_LOG_ERROR: c_int = 6;

pub const TAG: &[u8] = b"FaceHalHW\0";

/// Separate tag for rsbinder's own diagnostics, so its messages are
/// distinguishable from the HAL's.
const RSBINDER_TAG: &[u8] = b"FaceHalRsb\0";

pub fn get_prop(name: &[u8]) -> Option<String> {
    let mut value = vec![0u8; 92];
    let len = unsafe {
        __system_property_get(
            name.as_ptr() as *const c_char,
            value.as_mut_ptr() as *mut c_char,
        )
    };
    if len <= 0 {
        return None;
    }
    std::str::from_utf8(&value[..len as usize])
        .ok()
        .map(|s| s.to_string())
}

/// Face HAL logging is opt-in: stock ships quiet, and a chatty vendor HAL that
/// writes every frame is a battery complaint. `persist.sys.facehal.verbose=1`
/// turns it on, mirroring how the stock HAL gates its own traces.
pub fn verbose() -> bool {
    matches!(
        get_prop(b"persist.sys.facehal.verbose\0").as_deref(),
        None | Some("1") | Some("true") | Some("y") | Some("on")
    )
}

pub fn info(msg: &str) {
    if !verbose() {
        return;
    }
    if let Ok(c) = std::ffi::CString::new(msg) {
        unsafe {
            __android_log_print(
                ANDROID_LOG_INFO,
                TAG.as_ptr() as *const c_char,
                b"%s\0".as_ptr() as *const c_char,
                c.as_ptr(),
            );
        }
    }
}

pub fn error(msg: &str) {
    if let Ok(c) = std::ffi::CString::new(msg) {
        unsafe {
            __android_log_print(
                ANDROID_LOG_ERROR,
                TAG.as_ptr() as *const c_char,
                b"%s\0".as_ptr() as *const c_char,
                c.as_ptr(),
            );
        }
    }
}

/// rsbinder routes parcel/dispatch failures through the `log` facade. Without a
/// logger installed those messages are dropped, so a transaction that fails
/// during unmarshal looks identical to one that never arrived. Forward them to
/// logcat under a distinct tag so they are distinguishable from our own lines.
struct RsbinderLogger;

impl log::Log for RsbinderLogger {
    fn enabled(&self, _metadata: &log::Metadata<'_>) -> bool {
        true
    }

    fn log(&self, record: &log::Record<'_>) {
        rsbinder_log(&format!("[{}] {}", record.level(), record.args()));
    }

    fn flush(&self) {}
}

static RSBINDER_LOGGER: RsbinderLogger = RsbinderLogger;

fn rsbinder_log(msg: &str) {
    if let Ok(c) = std::ffi::CString::new(msg) {
        unsafe {
            __android_log_print(
                ANDROID_LOG_INFO,
                RSBINDER_TAG.as_ptr() as *const c_char,
                b"%s\0".as_ptr() as *const c_char,
                c.as_ptr(),
            );
        }
    }
}

/// Install the `log` logger rsbinder writes to. Idempotent: a second call is
/// ignored rather than panicking on the global logger slot.
pub fn init_rsbinder_logger() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = log::set_logger(&RSBINDER_LOGGER);
        log::set_max_level(log::LevelFilter::Debug);
    });
}

/// Keep the Once alive so `verbose()` stays a one-shot read of the property.
pub fn init() {
    INIT.call_once(|| {
        let _ = verbose();
    });
}

#[allow(dead_code)]
pub type RawPtr = *mut c_void;
