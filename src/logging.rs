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
    static V: Once = Once::new();
    static mut ENABLED: bool = false;
    unsafe {
        V.call_once(|| {
            ENABLED = matches!(
                get_prop(b"persist.sys.facehal.verbose\0").as_deref(),
                Some("1") | Some("true") | Some("y") | Some("on")
            );
        });
        ENABLED
    }
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

/// Keep the Once alive so `verbose()` stays a one-shot read of the property.
pub fn init() {
    INIT.call_once(|| {
        let _ = verbose();
    });
}

#[allow(dead_code)]
pub type RawPtr = *mut c_void;
