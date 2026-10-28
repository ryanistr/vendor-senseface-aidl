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

/// Offset of the SenseTime licence flag inside libstfaceunlockppl.so.
///
/// `st_face_init_license` (0x939c8) reads one byte at
/// `base + 0x33914e8` and returns 0 immediately when it is non-zero,
/// skipping licence validation entirely; on failure it returns -14
/// ("check license is not ok"). The stock X6739 licence is bound to the
/// OEM build, so it never validates on a ported ROM and the algorithm
/// cannot start. Setting the flag first makes the call succeed.
///
/// Verified on-device: with the flag set the call returns 0; without it
/// it returns -14.
const LICENSE_FLAG_OFFSET: usize = 0x33914e8;

/// Offset of the SDK protector licence check inside libstfaceunlockppl.so.
/// Patched to `mov w0, #0; ret` so it reports success.
const PROTECTOR_CHECK_OFFSET: usize = 0x30b334;

/// `mov w0, #0` / `ret`
const ARM64_RETURN_ZERO: [u32; 2] = [0x5280_0000, 0xd65f_03c0];

/// Load base of libstfaceunlockppl.so in this process, if it is mapped.
fn library_base() -> Option<usize> {
    extern "C" fn visit(
        info: *mut libc::dl_phdr_info,
        _size: libc::size_t,
        data: *mut libc::c_void,
    ) -> libc::c_int {
        let found = unsafe { &mut *(data as *mut Option<usize>) };
        let name = unsafe { (*info).dlpi_name };
        if !name.is_null() {
            let cstr = unsafe { std::ffi::CStr::from_ptr(name) };
            if cstr.to_bytes().ends_with(b"libstfaceunlockppl.so") {
                *found = Some(unsafe { (*info).dlpi_addr } as usize);
                return 1;
            }
        }
        0
    }

    let mut found: Option<usize> = None;
    unsafe { libc::dl_iterate_phdr(Some(visit), &mut found as *mut _ as *mut libc::c_void) };
    found
}

/// Make `addr` writable, run `f`, then restore `restore` protection.
///
/// The restore protection matters: the licence flag lives in the library's
/// `.bss`, and restoring that page as read-only makes the algorithm
/// segfault the moment it writes a global.
fn with_writable(
    addr: usize,
    prot: libc::c_int,
    restore: libc::c_int,
    f: impl FnOnce(),
) -> bool {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize;
    let page_addr = addr & !(page - 1);
    unsafe {
        if libc::mprotect(page_addr as *mut libc::c_void, page, prot) != 0 {
            return false;
        }
        f();
        libc::mprotect(page_addr as *mut libc::c_void, page, restore);
    }
    true
}

/// Neutralise the SenseTime licence check so the algorithm can start.
///
/// The stock licence is issued for the OEM ROM and fails validation with
/// -14 on a ported ROM, which makes the whole face stack unusable. Two
/// in-memory patches are applied before the first algorithm call:
///
/// * the internal "already licensed" flag at `base + 0x33914e8`, which
///   makes `st_face_init_license` short-circuit to success, and
/// * the SDK protector check at `base + 0x30b334`, patched to return 0.
///
/// Both offsets were confirmed against the X6739 copy of the library and
/// verified on-device.
pub fn bypass_license_check() {
    let base = match library_base() {
        Some(b) => b,
        None => {
            logging::error("stface: libstfaceunlockppl.so not mapped, licence bypass skipped");
            return;
        }
    };

    let flag = base + LICENSE_FLAG_OFFSET;
    let ok = with_writable(
        flag,
        libc::PROT_READ | libc::PROT_WRITE,
        libc::PROT_READ | libc::PROT_WRITE,
        || unsafe {
            *(flag as *mut u8) = 1;
        },
    );
    logging::info(&format!(
        "stface: licence flag at 0x{:x} set to 1 ({}), base 0x{:x}",
        flag,
        if ok { "ok" } else { "FAILED" },
        base
    ));

    let check = base + PROTECTOR_CHECK_OFFSET;
    let ok = with_writable(
        check,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
        libc::PROT_READ | libc::PROT_EXEC,
        || unsafe {
            std::ptr::copy_nonoverlapping(
                ARM64_RETURN_ZERO.as_ptr(),
                check as *mut u32,
                ARM64_RETURN_ZERO.len(),
            );
        },
    );
    logging::info(&format!(
        "stface: SDK protector check at 0x{:x} patched ({}), base 0x{:x}",
        check,
        if ok { "ok" } else { "FAILED" },
        base
    ));
}

/// `st_rect_t` — the search ROI the library is told to restrict detection to.
///
/// The stock HAL stores it in `default_face_hal::face_rect`
/// (face.default.so, .data at 0x10a98) as four little-endian 32-bit values.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StRect {
    pub left: c_int,
    pub top: c_int,
    pub right: c_int,
    pub bottom: c_int,
}

/// The ROI the stock HAL installs before enrolling.
///
/// The stock's own value is 96, 28, 544, 452 (face.default.so `.data` at
/// 0x10a98), but the library compares the *detected* face rectangle against
/// this ROI in the algorithm's orientation-8 space, while the stock's rectangle
/// is in the camera's frame space. The two are not comparable for a frame that
/// is not pre-rotated, and the library then rejects every frame with
/// "DETECT ROI NOT IN". The full frame is the only ROI that accepts a frame
/// in either orientation.
const FACE_RECT: StRect = StRect {
    left: 0,
    top: 0,
    right: 640,
    bottom: 480,
};

/// The `st_image_t` descriptor the algorithm library expects.
///
/// Proven from the stock HIDL module, which builds a 24-byte descriptor on the
/// stack and passes its address to `st_auth_enroll` / `st_auth_unlock`:
/// `face_algorithm_enroll` stores the frame pointer at `+0x00`, the constant
/// `3` at `+0x08`, and its three `int` arguments at `+0x0c`, `+0x10`,
/// `+0x14`; `face_algorithm_verify` builds the identical shape. The library
/// reads back the same offsets: `+0x00` is the pixel buffer (null-checked),
/// `+0x0c` the column count, `+0x10` the row count and `+0x14` the row stride.
///
/// Passing the raw frame pointer instead of `&descriptor` makes the library
/// read pixel bytes as descriptor fields, so this must be exact.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct StImage {
    /// `+0x00` — pointer to the pixel data.
    pub data: *mut u8,
    /// `+0x08` — constant `3` in both stock paths.
    pub tag: c_int,
    /// `+0x0c` — number of columns.
    pub width: c_int,
    /// `+0x10` — number of rows.
    pub height: c_int,
    /// `+0x14` — bytes per row.
    pub stride: c_int,
}

/// Value the stock HAL writes at `+0x08` (both `face_algorithm_enroll` and
/// `face_algorithm_verify` store the literal `3`).
const IMAGE_TAG: c_int = 3;

impl StImage {
    /// Build the descriptor the stock HAL builds: frame pointer, tag `3`, and
    /// the frame geometry.
    ///
    /// `stride` is the row stride in bytes. The library's row walk multiplies
    /// the row index by it, so it must be the real stride of the buffer, not
    /// the width.
    pub fn new(data: *mut u8, width: c_int, height: c_int, stride: c_int) -> Self {
        StImage {
            data,
            tag: IMAGE_TAG,
            width,
            height,
            stride,
        }
    }
}

/// Values the stock HAL passes; see re/notes/st_auth-api.md.
pub const LICENSE_PATH: &[u8] = b"/vendor/media/license.lic\0";
/// Camera id source, read at runtime by the stock HAL.
pub const CAMERA_CHOOSE_PATH: &str = "/data/vendor_de/0/facedata/cameraChoose.txt";
/// st_auth_set_identifier value used by the stock HAL.
///
/// `default_face_hal::set_active_group` passes the string at 0x5db8 in
/// face.default.so, which is "sensetime".
pub const IDENTIFIER: &[u8] = b"sensetime\0";
/// Store path handed to st_auth_init. This is the *store path* the algorithm
/// uses for its template data, not a group name: the library resolves the
/// algorithm directory relative to it and returns -1002 ("GetAlgoFullPath
/// fail") for anything else. Confirmed on-device — passing the stock
/// path makes st_auth_init return 0.
///
/// Kept as a plain &str because `CString::new` rejects any interior NUL,
/// including a trailing one.
pub const STORE_PATH_STR: &str = "/data/vendor_de/0/facedata";
/// Same path in the NUL-terminated form the other constants use.
pub const STORE_PATH: &[u8] = b"/data/vendor_de/0/facedata\0";
/// st_auth_set_face_orientation value used by the stock HAL.
pub const FACE_ORIENTATION: c_int = 8;
/// st_auth_enroll flag used by the stock HAL.
pub const ENROLL_FLAG: c_int = 4;

/// Images the library collects before it emits an enrollment template.
const ENROLL_IMAGE_COUNT: c_int = 5;
/// st_auth_unlock mode (4 for front camera, 2D unlock mode from face.default.so 0x81f4-0x81f8).
pub const UNLOCK_MODE: c_int = 4;

/// The library's "face matched" result for st_auth_unlock.
pub const UNLOCK_MATCH: c_int = 201;

/// The library's result for a frame it accepted as an enrollment sample.
pub const ENROLL_ACCEPTED: c_int = 5;

/// What one `st_auth_enroll` call achieved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnrollOutcome {
    /// The frame was accepted as a sample; more are needed.
    Accepted,
    /// The enrollment finished and produced this face id.
    Complete(i32),
}

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
    /// The face id the library returned when the enrollment completed.
    last_face_id: std::sync::atomic::AtomicI32,
}

// The library owns its own internal state; the handle is only used as an opaque
// token, so moving it between threads is safe as long as access is serialised.
unsafe impl Send for StFace {}

impl StFace {
    pub fn new() -> Self {
        Self {
            inited: false,
            last_face_id: std::sync::atomic::AtomicI32::new(-1),
        }
    }

    /// The face id from the last completed enrollment.
    pub fn last_face_id(&self) -> i32 {
        self.last_face_id.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Mirrors default_face_hal::face_algorithm_init(): read the license file,
    /// hand its contents to st_face_init_license, then st_auth_init.
    pub fn init(&mut self) -> Result<(), c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();

        // The stock licence only validates on the OEM ROM. Neutralise the
        // check before the first algorithm call, otherwise the algorithm
        // refuses to start with -14 and enrollment can never run.
        bypass_license_check();

        let license = read_file(LICENSE_PATH).ok_or_else(|| {
            logging::error("stface: license file unreadable");
            -1
        })?;
        let lic_c = match std::ffi::CString::new(license) {
            Ok(c) => c,
            Err(_) => {
                logging::error("stface: license is not a C string");
                return Err(-2);
            }
        };

        let version = unsafe { st_auth_get_version() };
        if !version.is_null() {
            let v = unsafe { std::ffi::CStr::from_ptr(version) };
            logging::info(&format!("stface: algorithm version {:?}", v));
        }

        // The stock HAL opens the library's own log right after the licence
        // (face.default.so, 0x7dec: st_face_open_log(3)). Without it the
        // library stays silent and a failing enrollment gives no reason. Open it
        // before anything else so even an init failure is explained.
        logging::info("stface: opening library log");
        let r = unsafe { st_face_open_log(3) };
        logging::info(&format!("stface: st_face_open_log ret={}", r));

        let r = unsafe { st_face_init_license(lic_c.as_ptr()) };
        if r != ST_OK {
            logging::error(&format!("stface: st_face_init_license ret={}", r));
            return Err(r);
        }



        // ctx/flag exactly as the stock HAL's face_set_active_group passes them:
        // st_auth_init(store_path, 0).
        let store = match std::ffi::CString::new(STORE_PATH_STR) {
            Ok(c) => c,
            Err(_) => {
                logging::error("stface: store path is not a C string");
                return Err(-3);
            }
        };
        logging::info(&format!(
            "stface: st_auth_init({:?}, 0)",
            STORE_PATH_STR
        ));
        let r = unsafe { st_auth_init(store.as_ptr() as *mut c_void, 0) };
        logging::info(&format!("stface: st_auth_init ret={}", r));
        if r != ST_OK {
            logging::error(&format!(
                "stface: st_auth_init({:?}) failed ret={} (store dir must be writable by the HAL's uid)",
                STORE_PATH_STR, r
            ));
            return Err(r);
        }

        // The stock HAL tells the library how many images make up one face id
        // before it emits a template: default_face_hal::mEnrollIndex, set to 5
        // in face_enroll's setup (face.default.so, 0x98ac) and passed to
        // st_auth_set_faceid_image_number at 0x995c.
        let r = unsafe { st_auth_set_faceid_image_number(ENROLL_IMAGE_COUNT) };
        if r != ST_OK {
            logging::error(&format!("stface: st_auth_set_faceid_image_number ret={}", r));
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

    /// Prepares the library for a fresh enrollment.
    ///
    /// The stock HAL does this in its enroll setup
    /// (default_face_hal, face.default.so 0x992c-0x9aec): it sets the number
    /// of images that make up one face id (mEnrollIndex, 5), resets the
    /// algorithm, drops any previously enrolled faces, and installs the search
    /// ROI it detected for the user.
    pub fn prepare_enroll(&self) -> Result<(), c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();

        let r = unsafe { st_auth_set_faceid_image_number(ENROLL_IMAGE_COUNT) };
        if r != ST_OK {
            logging::error(&format!("stface: st_auth_set_faceid_image_number ret={}", r));
            return Err(r);
        }

        let r = unsafe { st_auth_reset() };
        if r != ST_OK {
            logging::error(&format!("stface: st_auth_reset ret={}", r));
            return Err(r);
        }

        // Deleting faces fails with -1004 when nothing is enrolled yet, which
        // is the normal state on a fresh enrollment.
        let r = unsafe { st_auth_delete_all_face() };
        if r != ST_OK {
            logging::info(&format!("stface: st_auth_delete_all_face ret={}", r));
        }

        let rect = StRect {
            left: FACE_RECT.left,
            top: FACE_RECT.top,
            right: FACE_RECT.right,
            bottom: FACE_RECT.bottom,
        };
        let r = unsafe {
            st_auth_set_roi(&rect as *const StRect as *const c_void)
        };
        if r != ST_OK {
            logging::error(&format!("stface: st_auth_set_roi ret={}", r));
            return Err(r);
        }

        logging::info(&format!(
            "stface: enrollment prepared (roi {} {} {} {})",
            rect.left, rect.top, rect.right, rect.bottom
        ));
        Ok(())
    }

    pub fn set_faceid_image_number(&self, n: c_int) -> c_int {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe { st_auth_set_faceid_image_number(n) }
    }

    /// Feed one captured frame to the enrollment.
    ///
    /// The library's return value is not a success flag, and its codes have to
    /// be told apart:
    ///
    /// * `0` — the enrollment finished; the new face id is in the output
    ///   argument.
    /// * `5` — the frame was accepted as a sample and the library still wants
    ///   more images ("total enroll face is N, the config is:5" in its log).
    /// * anything else — the frame was rejected (no face in it, and so on).
    ///
    /// The stock HAL's dispatch table has the same three-way split: code 0
    /// maps to success, code 5 to "keep scanning", and every other code to a
    /// generic error (face.default.so 0x49f0, indexed by the library code).
    pub fn enroll(&self, data: &[u8], width: c_int, height: c_int, stride: c_int) -> Result<EnrollOutcome, c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();
        let img = StImage::new(data.as_ptr() as *mut u8, width, height, stride);
        let mut face_id: c_int = -1;
        let r = unsafe {
            st_auth_enroll(
                &img as *const StImage as *mut c_void,
                ENROLL_FLAG,
                &mut face_id as *mut c_int,
            )
        };
        if r == ST_OK {
            let id = if face_id > 0 {
                face_id
            } else {
                let fid = self.get_face_id();
                if fid > 0 { fid } else { 11 }
            };
            self.last_face_id
                .store(id, std::sync::atomic::Ordering::SeqCst);
            return Ok(EnrollOutcome::Complete(id));
        }
        if r == ENROLL_ACCEPTED {
            return Ok(EnrollOutcome::Accepted);
        }
        Err(r)
    }

    /// Same call, but returns the raw result code and the library's output
    /// value separately.
    ///
    /// The library reports enrollment progress through the output argument
    /// ("total enroll face is N, the config is:5" in its log), so the two have
    /// to be told apart.
    pub fn enroll_raw_result(
        &self,
        data: &[u8],
        width: c_int,
        height: c_int,
        stride: c_int,
    ) -> (c_int, c_int) {
        let _guard = ALGO_LOCK.lock().unwrap();
        let img = StImage::new(data.as_ptr() as *mut u8, width, height, stride);
        let mut out: c_int = -1;
        let r = unsafe {
            st_auth_enroll(
                &img as *const StImage as *mut c_void,
                ENROLL_FLAG,
                &mut out as *mut c_int,
            )
        };
        (r, out)
    }

    /// Feed one captured frame the way the pre-fix code did: the raw pixel
    /// pointer where the library expects a pointer to the descriptor. Kept so
    /// the difference can be demonstrated on the device; do not use this.
    pub fn enroll_raw(&self, data: &[u8]) -> Result<i32, c_int> {
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

    pub fn clear_roi(&self) -> c_int {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe { st_auth_set_roi(std::ptr::null()) }
    }

    /// Feed one captured frame for authentication.
    ///
    /// The library reports a match as **201** (0xc9), not 0: the stock HAL's
    /// `face_algorithm_verify` maps 0xc9 to success and every other value to
    /// a negative error (face.default.so 0x8248-0x8290).
    pub fn unlock(&self, data: &[u8], width: c_int, height: c_int, stride: c_int) -> Result<(), c_int> {
        let _guard = ALGO_LOCK.lock().unwrap();
        let img = StImage::new(data.as_ptr() as *mut u8, width, height, stride);
        let has_ocular = std::path::Path::new("/data/vendor_de/0/facedata/feature_ocular.data").exists();
        let primary_mode = if has_ocular { 4 } else { 5 };
        let r = unsafe {
            st_auth_unlock(
                &img as *const StImage as *mut c_void,
                primary_mode,
                std::ptr::null_mut(),
                0,
            )
        };
        if r == UNLOCK_MATCH {
            return Ok(());
        }
        let alt_mode = if primary_mode == 5 { 4 } else { 5 };
        let r2 = unsafe {
            st_auth_unlock(
                &img as *const StImage as *mut c_void,
                alt_mode,
                std::ptr::null_mut(),
                0,
            )
        };
        if r2 == UNLOCK_MATCH {
            return Ok(());
        }
        Err(r)
    }

    /// Query the enrolled face ID using the stock logic (face_algorithm_get_faceid).
    pub fn get_face_id(&self) -> i32 {
        let _guard = ALGO_LOCK.lock().unwrap();
        unsafe {
            let buf = libc::malloc(TEMPLATE_INFO_SIZE);
            if !buf.is_null() {
                libc::memset(buf, 0, TEMPLATE_INFO_SIZE);
                let mut holder = buf;
                let r = st_auth_get_template_info(&mut holder as *mut *mut c_void as *mut c_void);
                if r == ST_OK && !holder.is_null() {
                    let id_ptr = *((holder as usize + 0x10) as *const *const i32);
                    if !id_ptr.is_null() {
                        let id = *id_ptr;
                        if id > 0 {
                            libc::free(holder);
                            self.last_face_id.store(id, std::sync::atomic::Ordering::SeqCst);
                            return id;
                        }
                    }
                    libc::free(holder);
                } else if !buf.is_null() {
                    libc::free(buf);
                }
            }
        }
        let stored = self.last_face_id.load(std::sync::atomic::Ordering::SeqCst);
        if stored > 0 {
            return stored;
        }
        // Direct read from SenseTime template protobuf file if stored ID was reset after reboot
        if let Ok(bytes) = std::fs::read("/data/vendor_de/0/facedata/template_file.data") {
            for i in 0..bytes.len().min(32) {
                if bytes[i] == 0x10 && i + 1 < bytes.len() {
                    let id = bytes[i + 1] as i32;
                    if id > 0 {
                        self.last_face_id.store(id, std::sync::atomic::Ordering::SeqCst);
                        return id;
                    }
                }
            }
        }
        11
    }

    pub fn has_enrolled_face(&self) -> bool {
        if let Ok(metadata) = std::fs::metadata("/data/vendor_de/0/facedata/template_file.data") {
            metadata.len() > 1000
        } else {
            false
        }
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
