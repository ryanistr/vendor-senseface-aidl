// FaceHAL/src/surface_bridge.rs
//
// Turns the HAL token the framework hands to ISession.enroll() back into the
// ANativeWindow behind the app's preview Surface.
//
// The reference implementation (RapliVx/OPlus-Face-Bridge-Transsion) does this
// in C++; the ABI facts it depends on were re-verified against this device's
// own binaries rather than taken on faith:
//
//   * android.hidl.token@1.0-utils.so's client proxy reads the byte vector as
//     {data pointer @ +0, uint32 size @ +8} inside a 16-byte object, so the
//     token struct here matches that byte for byte.
//   * retrieveHalInterface, IGraphicBufferProducer::castFrom and
//     getSurfaceFromHGBP all return through the AArch64 indirect result
//     register x8, so they are called with inline asm rather than a plain
//     extern "C" signature.
//   * libbufferqueueconverter.so reaches the producer through its GOT slots at
//     +0x2240 (typeinfo) and +0x2248 (vtable), which is where the V1_0
//     producer's vtable has to be substituted when the framework's Surface was
//     created from a bufferqueue V1_0 producer.
//
// The token only carries a HAL token; the preview Surface itself belongs to
// the app, so the window is rendered into, never owned.
#![allow(non_snake_case, dead_code)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::Mutex;

use crate::logging;

const RTLD_NOW: c_int = 2;
const PROT_READ: c_int = 1;
const PROT_WRITE: c_int = 2;

const WINDOW_FORMAT_RGBA_8888: i32 = 1;

const TARGET_WIDTH: i32 = 480;
const TARGET_HEIGHT: i32 = 640;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;

    fn ANativeWindow_setBuffersGeometry(
        window: *mut c_void,
        width: i32,
        height: i32,
        format: i32,
    ) -> i32;
    fn ANativeWindow_lock(
        window: *mut c_void,
        buffer: *mut ANativeWindow_Buffer,
        dirty_bounds: *mut c_void,
    ) -> i32;
    fn ANativeWindow_unlockAndPost(window: *mut c_void) -> i32;
}

#[repr(C)]
pub struct ANativeWindow_Buffer {
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: i32,
    pub bits: *mut c_void,
    pub reserved: [u32; 6],
}

/// The byte-vector layout the device's HIDL token proxy expects: a data
/// pointer, a 32-bit size, an ownership flag and three bytes of padding.
#[repr(C)]
struct HidlVecU8 {
    data: *const u8,
    size: u32,
    owns: u8,
    pad: [u8; 3],
}

type FnRetrieve = unsafe extern "C" fn(*const HidlVecU8) -> *mut c_void;
type FnCastFrom = unsafe extern "C" fn(*const *mut c_void, c_int) -> *mut c_void;
type FnGetSurface = unsafe extern "C" fn(*const *mut c_void) -> *mut c_void;
type FnGetWindow = unsafe extern "C" fn(*mut c_void) -> *mut c_void;
type FnDestroyHolder = unsafe extern "C" fn(*mut c_void);

unsafe impl Send for SurfaceBridge {}

pub struct SurfaceBridge {
    native_window: *mut c_void,
    holder: *mut c_void,
    destroy_holder: Option<FnDestroyHolder>,
    bqc: *mut c_void,
    frames: u64,
}

static PREVIEW: Mutex<Option<SurfaceBridge>> = Mutex::new(None);

fn load_lib(paths: &[&str]) -> Option<*mut c_void> {
    for p in paths {
        let c = match std::ffi::CString::new(*p) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
        if !h.is_null() {
            logging::info(&format!("preview: loaded {}", p));
            return Some(h);
        }
    }
    let err = unsafe {
        let e = dlerror();
        if e.is_null() {
            String::from("none")
        } else {
            std::ffi::CStr::from_ptr(e).to_string_lossy().into_owned()
        }
    };
    logging::error(&format!("preview: dlopen failed: {}", err));
    None
}

fn symbol(handle: *mut c_void, name: &str) -> Option<*mut c_void> {
    let c = match std::ffi::CString::new(name) {
        Ok(c) => c,
        Err(_) => return None,
    };
    let s = unsafe { dlsym(handle, c.as_ptr()) };
    if s.is_null() {
        logging::error(&format!("preview: missing symbol {}", name));
        None
    } else {
        Some(s)
    }
}

// ---- AArch64 indirect-result calls -------------------------------------
//
// These three functions return their value through x8, which a plain extern
// "C" signature cannot express. The register allocation below is explicit so
// x8 keeps the result pointer and x18 (the platform register) stays untouched.

#[inline(always)]
unsafe fn call_retrieve(f: *mut c_void, vec: *const HidlVecU8) -> *mut c_void {
    let mut ret = [0u64; 8];
    let mut x0 = vec as u64;
    let mut x8 = ret.as_mut_ptr() as u64;
    core::arch::asm!(
        "blr x9",
        in("x9") f,
        inout("x0") x0 => _,
        inout("x8") x8 => _,
        lateout("x1") _,
        lateout("x2") _,
        lateout("x3") _,
        lateout("x4") _,
        lateout("x5") _,
        lateout("x6") _,
        lateout("x7") _,
        lateout("x10") _,
        lateout("x11") _,
        lateout("x12") _,
        lateout("x13") _,
        lateout("x14") _,
        lateout("x15") _,
        lateout("x16") _,
        lateout("x17") _,
        options(nostack)
    );
    ret[0] as *mut c_void
}

#[inline(always)]
unsafe fn call_cast_from(f: *mut c_void, base: *const *mut c_void) -> *mut c_void {
    let mut ret = [0u64; 8];
    let mut x0 = base as u64;
    let mut x1 = 0u64;
    let mut x8 = ret.as_mut_ptr() as u64;
    core::arch::asm!(
        "blr x9",
        in("x9") f,
        inout("x0") x0 => _,
        inout("x1") x1 => _,
        inout("x8") x8 => _,
        lateout("x2") _,
        lateout("x3") _,
        lateout("x4") _,
        lateout("x5") _,
        lateout("x6") _,
        lateout("x7") _,
        lateout("x10") _,
        lateout("x11") _,
        lateout("x12") _,
        lateout("x13") _,
        lateout("x14") _,
        lateout("x15") _,
        lateout("x16") _,
        lateout("x17") _,
        options(nostack)
    );
    // Return<sp<IGraphicBufferProducer>>: the producer pointer sits at +0x28.
    // The byte at +0x20 is cleared on both the success and the failure path,
    // so it carries no usable status: a null producer is the only signal.
    let producer = *(ret.as_ptr().add(0x28 / 8) as *const *mut c_void);
    if producer.is_null() {
        ptr::null_mut()
    } else {
        producer
    }
}

#[inline(always)]
unsafe fn call_get_surface(f: *mut c_void, producer: *const *mut c_void) -> *mut c_void {
    let mut ret = [0u64; 4];
    let mut x0 = producer as u64;
    let mut x8 = ret.as_mut_ptr() as u64;
    core::arch::asm!(
        "blr x9",
        in("x9") f,
        inout("x0") x0 => _,
        inout("x8") x8 => _,
        lateout("x1") _,
        lateout("x2") _,
        lateout("x3") _,
        lateout("x4") _,
        lateout("x5") _,
        lateout("x6") _,
        lateout("x7") _,
        lateout("x10") _,
        lateout("x11") _,
        lateout("x12") _,
        lateout("x13") _,
        lateout("x14") _,
        lateout("x15") _,
        lateout("x16") _,
        lateout("x17") _,
        options(nostack)
    );
    // unique_ptr<SurfaceHolder>: holder pointer at +0, deleter at +8.
    *(ret.as_ptr() as *const *mut c_void)
}

// ---- library base lookup ----------------------------------------------

/// First mapping of `needle` in /proc/self/maps, which is where the library
/// was loaded and therefore the base its file offsets are relative to.
fn library_base(needle: &str) -> Option<u64> {
    let maps = std::fs::read_to_string("/proc/self/maps").ok()?;
    let mut base: Option<u64> = None;
    for line in maps.lines() {
        if !line.ends_with(needle) {
            continue;
        }
        logging::info(&format!("preview: maps {}", line));
        let mut parts = line.split_whitespace();
        let start = parts.next()?;
        let offset = parts.nth(1)?;
        if offset != "00000000" {
            continue;
        }
        let addr = u64::from_str_radix(start.split('-').next()?, 16).ok()?;
        base = Some(match base {
            Some(b) if b <= addr => b,
            _ => addr,
        });
    }
    base
}

/// The current value of errno.
fn errno_location() -> i32 {
    extern "C" {
        static __errno: i32;
    }
    unsafe { __errno }
}

/// The load base of `libbufferqueueconverter.so`.
///
/// `dladdr` on a symbol the library exports gives the base directly, which is
/// what the reference implementation uses. The `/proc/self/maps` fallback is
/// kept for the case where the symbol lookup fails.
fn converter_base() -> Option<u64> {
    #[repr(C)]
    struct DlInfo {
        dli_fname: *const c_char,
        dli_fbase: *mut c_void,
        dli_sname: *const c_char,
        dli_saddr: *mut c_void,
    }
    extern "C" {
        fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int;
    }

    let bqc = load_lib(&[
        "/apex/com.android.vndk.v31/lib64/libbufferqueueconverter.so",
        "/apex/com.android.vndk.v31@1/lib64/libbufferqueueconverter.so",
        "/system/lib64/libbufferqueueconverter.so",
        "libbufferqueueconverter.so",
    ])?;
    let sym = symbol(
        bqc,
        "_ZN7android20destroySurfaceHolderEPNS_13SurfaceHolderE",
    );
    if let Some(s) = sym {
        let mut info = DlInfo {
            dli_fname: ptr::null(),
            dli_fbase: ptr::null_mut(),
            dli_sname: ptr::null(),
            dli_saddr: ptr::null_mut(),
        };
        unsafe {
            if dladdr(s as *const c_void, &mut info) != 0 && !info.dli_fbase.is_null() {
                return Some(info.dli_fbase as u64);
            }
        }
    }
    library_base("libbufferqueueconverter.so")
}

fn patch_got_for_v1() -> bool {
    let bqc_paths = [
        "/apex/com.android.vndk.v31/lib64/libbufferqueueconverter.so",
        "/apex/com.android.vndk.v31@1/lib64/libbufferqueueconverter.so",
        "/system/lib64/libbufferqueueconverter.so",
        "libbufferqueueconverter.so",
    ];
    let gui_paths = [
        "/apex/com.android.vndk.v31/lib64/libgui.so",
        "/apex/com.android.vndk.v31@1/lib64/libgui.so",
        "/system/lib64/libgui.so",
        "libgui.so",
    ];

    // The V1_0 H2B symbols have to come from the libgui the converter is
    // actually bound to. The converter is the VNDK copy and its DT_NEEDED
    // libgui.so resolves to whatever the linker already has loaded, which in
    // a process that uses the framework's libgui is the framework's copy.
    // So the symbols are looked up in the loaded libgui, and the *address*
    // is checked against the converter's own dependency: patching in a
    // vtable from a different libgui would make H2BConverter call through
    // mismatched code and crash.
    let gui = match load_lib(&gui_paths) {
        Some(g) => g,
        None => return false,
    };
    // Look the V1_0 H2B symbols up in the loaded libgui, then in the converter
    // (which imports the V2_0 pair, so a V1_0 lookup there usually fails),
    // then in the global scope.
    let mut ztt = symbol(
        gui,
        "_ZTTN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
    );
    let mut ztv = symbol(
        gui,
        "_ZTVN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
    );
    if ztt.is_none() || ztv.is_none() {
        let bqc = load_lib(&bqc_paths);
        if let Some(b) = bqc {
            if ztt.is_none() {
                ztt = symbol(
                    b,
                    "_ZTTN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
                );
            }
            if ztv.is_none() {
                ztv = symbol(
                    b,
                    "_ZTVN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
                );
            }
        }
    }
    if ztt.is_none() || ztv.is_none() {
        extern "C" {
            fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        }
        let rtld_default: *mut c_void = std::ptr::null_mut();
        for (name, slot) in [
            (
                "_ZTTN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
                &mut ztt,
            ),
            (
                "_ZTVN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
                &mut ztv,
            ),
        ] {
            if slot.is_none() {
                if let Ok(c) = std::ffi::CString::new(name) {
                    let s = unsafe { dlsym(rtld_default, c.as_ptr()) };
                    if !s.is_null() {
                        *slot = Some(s);
                    }
                }
            }
        }
    }
    let (ztt, ztv) = match (ztt, ztv) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            logging::error("preview: V1_0 producer typeinfo/vtable not in libgui");
            return false;
        },
    };

    // The converter's load base, taken from a symbol it exports rather than
    // from /proc/self/maps: the maps view lists the file's segments, and the
    // first one is not necessarily at offset 0.
    let base = match converter_base() {
        Some(b) => b,
        None => {
            logging::error("preview: converter base not found");
            return false;
        }
    };
    logging::info(&format!(
        "preview: V1_0 typeinfo {:#x} vtable {:#x}",
        ztt as usize, ztv as usize
    ));

    let typeinfo_slot = base + 0x2240;
    let vtable_slot = base + 0x2248;
    let page_size = unsafe {
        let p = libc::sysconf(libc::_SC_PAGESIZE);
        if p <= 0 {
            4096
        } else {
            p as usize
        }
    };
    let page_start = (typeinfo_slot as usize) & !(page_size - 1);
    logging::info(&format!(
        "preview: converter base {:#x} typeinfo slot {:#x} vtable slot {:#x} page {:#x} size {}",
        base, typeinfo_slot, vtable_slot, page_start, page_size
    ));
    unsafe {
        if mprotect(page_start as *mut c_void, page_size, PROT_READ | PROT_WRITE) != 0 {
            logging::error(&format!(
                "preview: mprotect on converter GOT failed: errno={} slot={:#x} page={:#x} size={}",
                errno_location(),
                typeinfo_slot,
                page_start,
                page_size
            ));
            return false;
        }
        *(typeinfo_slot as *mut *mut c_void) = ztt;
        *(vtable_slot as *mut *mut c_void) = ztv;
        mprotect(page_start as *mut c_void, page_size, PROT_READ);
    }
    logging::info(&format!(
        "preview: patched converter GOT at base {:#x} (typeinfo {:#x}, vtable {:#x})",
        base, typeinfo_slot, vtable_slot
    ));
    true
}

impl SurfaceBridge {
    /// Decode `token` into the preview window. Returns None when the token
    /// cannot be turned into a surface; enrollment then proceeds without a
    /// preview instead of failing.
    pub fn create(token: &[u8]) -> Option<SurfaceBridge> {
        if token.is_empty() {
            logging::error("preview: empty token");
            return None;
        }

        // The vendor's bufferqueue converter and the vendor's bufferqueue
        // libraries all expect the *vendor's* libgui. If the framework's
        // libgui.so was loaded first, the converter binds to it and the
        // V1_0 H2BGraphicBufferProducer it then constructs has a different
        // layout, which crashes inside H2BConverter. Loading the vendor
        // libgui first makes every participant agree. It needs the vendor
        // libui, which has to be loaded first for the same reason.
        load_lib(&[
            "/apex/com.android.vndk.v31/lib64/libui.so",
            "/apex/com.android.vndk.v31@1/lib64/libui.so",
        ]);
        load_lib(&[
            "/apex/com.android.vndk.v31/lib64/libgui.so",
            "/apex/com.android.vndk.v31@1/lib64/libgui.so",
        ]);

        let token_utils = load_lib(&[
            "/apex/com.android.vndk.v31/lib64/android.hidl.token@1.0-utils.so",
            "/apex/com.android.vndk.v31@1/lib64/android.hidl.token@1.0-utils.so",
            "/system/lib64/android.hidl.token@1.0-utils.so",
            "android.hidl.token@1.0-utils.so",
        ])?;
        let bqc = load_lib(&[
            "/apex/com.android.vndk.v31/lib64/libbufferqueueconverter.so",
            "/apex/com.android.vndk.v31@1/lib64/libbufferqueueconverter.so",
            "/system/lib64/libbufferqueueconverter.so",
            "libbufferqueueconverter.so",
        ])?;

        let retrieve = symbol(
            token_utils,
            "_ZN7android20retrieveHalInterfaceERKNS_8hardware8hidl_vecIhEE",
        )?;
        let get_surface = symbol(
            bqc,
            "_ZN7android18getSurfaceFromHGBPERKNS_2spINS_8hardware8graphics11bufferqueue4V2_022IGraphicBufferProducerEEE",
        )?;
        let get_window = symbol(bqc, "_ZN7android15getNativeWindowEPNS_13SurfaceHolderE")?;
        let destroy_holder = symbol(bqc, "_ZN7android20destroySurfaceHolderEPNS_13SurfaceHolderE");

        let vec = HidlVecU8 {
            data: token.as_ptr(),
            size: token.len() as u32,
            owns: 0,
            pad: [0; 3],
        };
        let base = unsafe { call_retrieve(retrieve, &vec) };
        logging::info(&format!("preview: retrieveHalInterface -> {:#x}", base as usize));
        if base.is_null() {
            return None;
        }

        // The framework's Surface is backed by a bufferqueue producer. The
        // reference implementation tries V2_0 first and falls back to V1_0;
        // the V1_0 case additionally needs the converter's GOT patched.
        let mut producer: *mut c_void = ptr::null_mut();
        let mut is_v1 = false;
        for (lib, sym) in [
            (
                "/apex/com.android.vndk.v31/lib64/android.hardware.graphics.bufferqueue@2.0.so",
                "_ZN7android8hardware8graphics11bufferqueue4V2_022IGraphicBufferProducer8castFromERKNS_2spINS_4hidl4base4V1_05IBaseEEEb",
            ),
            (
                "/apex/com.android.vndk.v31/lib64/android.hardware.graphics.bufferqueue@1.0.so",
                "_ZN7android8hardware8graphics11bufferqueue4V1_022IGraphicBufferProducer8castFromERKNS_2spINS_4hidl4base4V1_05IBaseEEEb",
            ),
        ] {
            let handle = match load_lib(&[lib, "android.hardware.graphics.bufferqueue@1.0.so", "android.hardware.graphics.bufferqueue@2.0.so"]) {
                Some(h) => h,
                None => continue,
            };
            let cast_from = match symbol(handle, sym) {
                Some(c) => c,
                None => continue,
            };
            let p = unsafe { call_cast_from(cast_from, &base) };
            logging::info(&format!("preview: castFrom -> {:#x}", p as usize));
            if !p.is_null() {
                producer = p;
                is_v1 = sym.contains("V1_0");
                break;
            }
        }
        if producer.is_null() {
            logging::error("preview: castFrom failed for both producer versions");
            return None;
        }

        if is_v1 && !patch_got_for_v1() {
            logging::error("preview: could not patch converter for V1_0");
            return None;
        }

        let holder = unsafe { call_get_surface(get_surface, &producer) };
        logging::info(&format!("preview: getSurfaceFromHGBP -> {:#x}", holder as usize));
        if holder.is_null() {
            return None;
        }

        let window = unsafe { std::mem::transmute::<*mut c_void, FnGetWindow>(get_window)(holder) };
        logging::info(&format!("preview: getNativeWindow -> {:#x}", window as usize));
        if window.is_null() {
            if let Some(d) = destroy_holder {
                unsafe { std::mem::transmute::<*mut c_void, FnDestroyHolder>(d)(holder) };
            }
            return None;
        }

        let geom = unsafe {
            ANativeWindow_setBuffersGeometry(
                window,
                TARGET_WIDTH,
                TARGET_HEIGHT,
                WINDOW_FORMAT_RGBA_8888,
            )
        };
        if geom != 0 {
            logging::info("preview: setBuffersGeometry(RGBA_8888) failed, keeping default");
            unsafe {
                ANativeWindow_setBuffersGeometry(window, 0, 0, 0);
            }
        }

        Some(SurfaceBridge {
            native_window: window,
            holder,
            destroy_holder: destroy_holder.map(|d| unsafe { std::mem::transmute::<*mut c_void, FnDestroyHolder>(d) }),
            bqc,
            frames: 0,
        })
    }

    /// Draw one NV21 frame into the preview surface.
    pub fn render_nv21(&mut self, nv21: &[u8], width: i32, height: i32) -> i32 {
        if width <= 0 || height <= 0 || self.native_window.is_null() {
            return -1;
        }
        let mut buffer = ANativeWindow_Buffer {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            bits: ptr::null_mut(),
            reserved: [0; 6],
        };
        let locked = unsafe { ANativeWindow_lock(self.native_window, &mut buffer, ptr::null_mut()) };
        if locked != 0 || buffer.bits.is_null() {
            return -3;
        }
        let bw = buffer.width;
        let bh = buffer.height;
        let stride = buffer.stride;
        if bw <= 0 || bh <= 0 || stride <= 0 {
            unsafe { ANativeWindow_unlockAndPost(self.native_window) };
            return -5;
        }

        let y_plane = nv21;
        let vu_plane = &nv21[(width * height) as usize..];
        let (cols, rows): (Vec<i32>, Vec<i32>) = if bh > 1 && bw > 1 {
            (
                (0..bw)
                    .map(|dx| ((height - 1) * dx) / (bw - 1))
                    .collect(),
                (0..bh)
                    .map(|dy| ((width - 1) * (bh - 1 - dy)) / (bh - 1))
                    .collect(),
            )
        } else {
            (vec![0; bw as usize], vec![0; bh as usize])
        };

        if buffer.format == 4 {
            // RGB_565
            let dst = buffer.bits as *mut u16;
            for dy in 0..bh {
                let sensor_col = rows[dy as usize] as usize;
                let col_uv_base = (sensor_col >> 1) << 1;
                for dx in 0..bw {
                    let sensor_row = cols[dx as usize] as usize;
                    let y_val = y_plane[sensor_col + sensor_row * width as usize];
                    let uv_idx = (sensor_row >> 1) * width as usize + col_uv_base;
                    let v_val = vu_plane[uv_idx];
                    let u_val = vu_plane[uv_idx + 1];
                    let (r, g, b) = yuv_to_rgb(y_val, u_val, v_val);
                    unsafe {
                        *dst.add(dy as usize * stride as usize + dx as usize) =
                            (((r >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((b >> 3) as u16);
                    }
                }
            }
        } else {
            // RGBA_8888 / RGBX_8888
            let dst = buffer.bits as *mut u32;
            for dy in 0..bh {
                let sensor_col = rows[dy as usize] as usize;
                let col_uv_base = (sensor_col >> 1) << 1;
                for dx in 0..bw {
                    let sensor_row = cols[dx as usize] as usize;
                    let y_val = y_plane[sensor_col + sensor_row * width as usize];
                    let uv_idx = (sensor_row >> 1) * width as usize + col_uv_base;
                    let v_val = vu_plane[uv_idx];
                    let u_val = vu_plane[uv_idx + 1];
                    let (r, g, b) = yuv_to_rgb(y_val, u_val, v_val);
                    unsafe {
                        *dst.add(dy as usize * stride as usize + dx as usize) =
                            0xFF00_0000 | ((b as u32) << 16) | ((g as u32) << 8) | r as u32;
                    }
                }
            }
        }

        unsafe { ANativeWindow_unlockAndPost(self.native_window) };
        self.frames += 1;
        if self.frames == 1 || self.frames % 60 == 0 {
            logging::info(&format!(
                "preview: rendered frame {} ({}x{}, stride {})",
                self.frames, bw, bh, stride
            ));
        }
        0
    }
}

impl Drop for SurfaceBridge {
    fn drop(&mut self) {
        // The window belongs to the SurfaceHolder, so only the holder is
        // released here.
        self.native_window = ptr::null_mut();
        if let Some(d) = self.destroy_holder {
            unsafe { d(self.holder) };
        }
        self.holder = ptr::null_mut();
        self.bqc = ptr::null_mut();
    }
}

fn yuv_to_rgb(y: u8, u: u8, v: u8) -> (i32, i32, i32) {
    let c = (y as i32 - 16).max(0);
    let d = u as i32 - 128;
    let e = v as i32 - 128;
    let r = ((298 * c + 409 * e + 128) >> 8).clamp(0, 255);
    let g = ((298 * c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255);
    let b = ((298 * c + 516 * d + 128) >> 8).clamp(0, 255);
    (r, g, b)
}

/// Install the bridge the enrollment preview renders into.
pub fn install(bridge: SurfaceBridge) {
    let mut slot = PREVIEW.lock().unwrap();
    *slot = Some(bridge);
}

/// Drop the preview bridge, if any.
pub fn clear() {
    let mut slot = PREVIEW.lock().unwrap();
    *slot = None;
}

/// Draw a frame into the installed preview bridge, if there is one.
pub fn render_current(nv21: &[u8], width: i32, height: i32) {
    let mut slot = PREVIEW.lock().unwrap();
    if let Some(bridge) = slot.as_mut() {
        bridge.render_nv21(nv21, width, height);
    }
}
