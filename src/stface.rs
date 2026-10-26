// FaceHAL/src/stface.rs
//
// FFI to libstfaceunlockppl.so — the SenseTime face-unlock algorithm library
// that the stock X6739 HIDL face HAL (erofs/vendor/lib64/hw/face.default.so)
// loads by dlopen. Every signature below was recovered from that HAL's call
// sites and cross-checked against the callee's own prologue; see
// re/notes/st_auth-api.md for the derivation and the residual uncertainty.
//
// This is the same library the stock stack uses, so the AIDL HAL produces the
// same match results as the HIDL HAL it replaces.
#![allow(dead_code)]

use std::os::raw::{c_char, c_int, c_void};
use std::sync::Mutex;

use crate::logging;

/// Values the stock HAL passes; see re/notes/st_auth-api.md.
pub const LICENSE_PATH: &[u8] = b"/vendor/media/license.lic\0";
/// Camera id source, read at runtime by the stock HAL.
pub const CAMERA_CHOOSE_PATH: &str = "/data/vendor_de/0/facedata/cameraChoose.txt";
/// st_auth_set_identifier value used by the stock HAL.
pub const IDENTIFIER: &[u8] = b"sensetime\0";
/// st_auth_set_face_orientation value used by the stock HAL.
pub const FACE_ORIENTATION: c_int = 8;
/// st_auth_enroll flag used by the stock HAL.
pub const ENROLL_FLAG: c_int = 4;
/// st_auth_unlock arg3/arg4 as passed by the stock HAL.
pub const UNLOCK_MODE_NULL: c_int = 0;

pub const ST_OK: c_int = 0;

extern "C" {
    fn st_auth_init(ctx: *mut c_void, flag: c_int) -> c_int;
    fn st_auth_deinit() -> c_int;
    fn st_auth_reset() -> c_int;
    fn st_auth_get_version() -> *const c_char;
    fn st_face_init_license(license_data: *const c_char) -> c_int;
    fn st_face_open_log(level: c_int) -> c_int;
    fn st_auth_set_identifier(id: *const c_char) -> c_int;
    fn st_auth_set_face_orientation(orientation: c_int) -> c_int;
    fn st_auth_set_roi(roi: *const c_void) -> c_int;
    fn st_auth_set_faceid_image_number(count: c_int) -> c_int;
    fn st_auth_enroll(image: *mut c_void, flag: c_int, out_face_id: *mut c_int) -> c_int;
    fn st_auth_unlock(image: *mut c_void, mode: c_int, arg3: *mut c_void, arg4: c_int) -> c_int;
    fn st_auth_delete_face(face_id: c_int) -> c_int;
    fn st_auth_delete_all_face() -> c_int;
    fn st_auth_get_template_info(out: *mut c_void) -> c_int;
}

/// Size of the struct st_auth_get_template_info fills, taken from the stock
/// HAL's malloc(0x18) immediately before the call.
pub const TEMPLATE_INFO_SIZE: usize = 0x18;

/// The algorithm library is process-global and not thread-safe for concurrent
/// enroll/auth, so every entry point serialises on this mutex.
static ALGO_LOCK: Mutex<()> = Mutex::new(());

pub struct StFace {
    inited: bool,
}

// The library owns its own internal state; the handle is only used as an opaque
// token, so moving it between threads is safe as long as access is serialised.
unsafe impl Send for StFace {}

impl StFace {
    pub fn new() -> Self {
        Self { inited: false }
    }

    /// Mirrors default_face_hal::face_algorithm_init(): read the license file,
    /// hand its contents to st_face_init_license, then st_auth_init.
    pub fn init(&mut self) -> Result<(), c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();

        let license = read_file(LICENSE_PATH).ok_or_else(|| {
            logging::error("stface: license file unreadable");
            -1
        })?;
        let lic_c = match std::ffi::CString::new(license) {
            Ok(c) => c,
            Err(_) => return Err(-2),
        };

        let version = unsafe { st_auth_get_version() };
        if !version.is_null() {
            let v = unsafe { std::ffi::CStr::from_ptr(version) };
            logging::info(&format!("stface: algorithm version {:?}", v));
        }

        let r = unsafe { st_face_init_license(lic_c.as_ptr()) };
        if r != ST_OK {
            logging::error(&format!("stface: st_face_init_license ret={}", r));
            return Err(r);
        }

        // ctx/flag exactly as the stock HAL passes them.
        let r = unsafe { st_auth_init(std::ptr::null_mut(), 0) };
        if r != ST_OK {
            logging::error(&format!("stface: st_auth_init ret={}", r));
            return Err(r);
        }

        unsafe {
            let r = st_auth_set_identifier(IDENTIFIER.as_ptr() as *const c_char);
            if r != ST_OK {
                logging::error(&format!("stface: st_auth_set_identifier ret={}", r));
                return Err(r);
            }
            let r = st_auth_set_face_orientation(FACE_ORIENTATION);
            if r != ST_OK {
                logging::error(&format!("stface: st_auth_set_face_orientation ret={}", r));
                return Err(r);
            }
        }

        self.inited = true;
        logging::info("stface: initialised");
        Ok(())
    }

    pub fn set_faceid_image_number(&self, n: c_int) -> c_int {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe { st_auth_set_faceid_image_number(n) }
    }

    /// Feed one captured frame. `data` is the Y plane of a YUV_420_888 image.
    pub fn enroll(&self, data: &[u8]) -> Result<i32, c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();
        let mut face_id: c_int = -1;
        let r = unsafe {
            st_auth_enroll(
                data.as_ptr() as *mut c_void,
                ENROLL_FLAG,
                &mut face_id as *mut c_int,
            )
        };
        if r != ST_OK {
            return Err(r);
        }
        Ok(face_id)
    }

    /// Feed one captured frame for authentication.
    pub fn unlock(&self, data: &[u8]) -> Result<(), c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();
        let r = unsafe {
            st_auth_unlock(
                data.as_ptr() as *mut c_void,
                UNLOCK_MODE_NULL,
                std::ptr::null_mut(),
                0,
            )
        };
        if r != ST_OK {
            return Err(r);
        }
        Ok(())
    }

    pub fn delete_face(&self, face_id: i32) -> c_int {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe { st_auth_delete_face(face_id) }
    }

    pub fn delete_all(&self) -> c_int {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe { st_auth_delete_all_face() }
    }

    pub fn reset(&self) -> c_int {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe { st_auth_reset() }
    }

    pub fn template_info(&self) -> Result<[u8; TEMPLATE_INFO_SIZE], c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();
        let mut buf = [0u8; TEMPLATE_INFO_SIZE];
        let r = unsafe { st_auth_get_template_info(buf.as_mut_ptr() as *mut c_void) };
        if r != ST_OK {
            return Err(r);
        }
        Ok(buf)
    }

    pub fn deinit(&mut self) {
        if !self.inited {
            return;
        }
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe {
            st_auth_deinit();
        }
        self.inited = false;
    }
}

impl Drop for StFace {
    fn drop(&mut self) {
        self.deinit();
    }
}

/// Read a whole file the way the stock HAL does (fopen/fread/ftell), so a
/// missing or unreadable file is a plain None rather than a crash.
pub fn read_file(path: &[u8]) -> Option<Vec<u8>> {
    let path_str = String::from_utf8_lossy(&path[..path.len().saturating_sub(1)]).into_owned();
    let mut f = match std::fs::File::open(std::ffi::OsStr::new(&path_str)) {
        Ok(f) => f,
        Err(_) => return None,
    };
    use std::io::Read;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}
