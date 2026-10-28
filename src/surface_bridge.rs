#![allow(non_snake_case, dead_code)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::Mutex;

use crate::logging;

const RTLD_NOW: c_int = 2;
const WINDOW_FORMAT_RGBA_8888: i32 = 1;
const TARGET_WIDTH: i32 = 480;
const TARGET_HEIGHT: i32 = 640;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;

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

#[repr(C)]
struct HidlVecU8 {
    data: *const u8,
    size: u32,
    owns: u8,
    pad: [u8; 3],
}

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

type FnOperatorNew = unsafe extern "C" fn(usize) -> *mut c_void;
type FnOperatorDelete = unsafe extern "C" fn(*mut c_void);
type FnH2bCtor = unsafe extern "C" fn(this: *mut c_void, base_sp: *const *mut c_void);
type FnSurfaceCtor = unsafe extern "C" fn(
    this: *mut c_void,
    producer_sp: *const *mut c_void,
    controlled_by_app: bool,
    surface_control_sp: *const *mut c_void,
);
type FnRefBaseIncStrong = unsafe extern "C" fn(this: *const c_void, id: *const c_void);
type FnRefBaseDecStrong = unsafe extern "C" fn(this: *const c_void, id: *const c_void);

unsafe impl Send for SurfaceBridge {}

pub struct SurfaceBridge {
    native_window: *mut c_void,
    surface: *mut c_void,
    fn_dec_strong: Option<FnRefBaseDecStrong>,
    frames: u64,
    fail_count: u64,
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

// ABI requires indirect return pointer in x8
#[inline(always)]
unsafe fn call_retrieve(f: *mut c_void, vec: *const HidlVecU8) -> *mut c_void {
    let mut ret = [0u64; 8];
    core::arch::asm!(
        "blr {fn_ptr}",
        fn_ptr = in(reg) f,
        in("x0") vec,
        in("x8") ret.as_mut_ptr(),
        lateout("x1") _,
        lateout("x2") _,
        lateout("x3") _,
        lateout("x4") _,
        lateout("x5") _,
        lateout("x6") _,
        lateout("x7") _,
        lateout("x9") _,
        lateout("x10") _,
        lateout("x11") _,
        lateout("x12") _,
        lateout("x13") _,
        lateout("x14") _,
        lateout("x15") _,
        lateout("x16") _,
        lateout("x17") _,
        lateout("x30") _,
        options(nostack)
    );
    ret[0] as *mut c_void
}

// Return<sp<IGraphicBufferProducer>> places the producer pointer at offset +0x28
#[inline(always)]
unsafe fn call_cast_from(f: *mut c_void, base: *const *mut c_void) -> *mut c_void {
    let mut ret = [0u64; 8];
    core::arch::asm!(
        "blr {fn_ptr}",
        fn_ptr = in(reg) f,
        in("x0") base,
        in("x1") 0u64,
        in("x8") ret.as_mut_ptr(),
        lateout("x2") _,
        lateout("x3") _,
        lateout("x4") _,
        lateout("x5") _,
        lateout("x6") _,
        lateout("x7") _,
        lateout("x9") _,
        lateout("x10") _,
        lateout("x11") _,
        lateout("x12") _,
        lateout("x13") _,
        lateout("x14") _,
        lateout("x15") _,
        lateout("x16") _,
        lateout("x17") _,
        lateout("x30") _,
        options(nostack)
    );
    *(ret.as_ptr().add(0x28 / 8) as *const *mut c_void)
}

fn is_valid_native_window(anw: *mut c_void) -> bool {
    if anw.is_null() {
        return false;
    }
    let addr = anw as usize;
    if (addr & 0x7) != 0 || addr < 0x1000 {
        return false;
    }
    unsafe {
        // ANativeWindow perform function pointer resides at +0x98
        let perform_ptr = *((addr + 0x98) as *const *mut c_void);
        !perform_ptr.is_null()
    }
}

// Scans libgui .text for the H2BGraphicBufferProducer constructor prologue
fn find_h2b_ctor(libgui_base: usize, is_v1: bool) -> Option<FnH2bCtor> {
    const PATTERN: [u8; 24] = [
        0xfd, 0x7b, 0xbe, 0xa9,
        0xf4, 0x4f, 0x01, 0xa9,
        0xfd, 0x03, 0x00, 0x91,
        0xf4, 0x03, 0x00, 0xaa,
        0x00, 0xa0, 0x01, 0x91,
        0xf3, 0x03, 0x01, 0xaa,
    ];

    // Scan within .text segment boundaries (0xa0000..0x160000)
    let start_offset = 0x0a0000usize;
    let end_offset = 0x160000usize;
    let slice = unsafe { std::slice::from_raw_parts((libgui_base + start_offset) as *const u8, end_offset - start_offset) };

    let mut matches = Vec::new();
    let mut offset = 0usize;
    while offset + PATTERN.len() <= slice.len() {
        if &slice[offset..offset + PATTERN.len()] == &PATTERN {
            matches.push(libgui_base + start_offset + offset);
            offset += PATTERN.len();
            if matches.len() >= 2 {
                break;
            }
        } else {
            offset += 4;
        }
    }

    logging::info(&format!(
        "preview: found {} H2B constructor pattern matches: {:?}",
        matches.len(),
        matches.iter().map(|m| format!("{:#x}", m)).collect::<Vec<_>>()
    ));

    let chosen_addr = if matches.len() >= 2 {
        if is_v1 { matches[1] } else { matches[0] }
    } else if matches.len() == 1 {
        matches[0]
    } else {
        // Hardcoded offsets for Android 15 libgui if scan fails
        let fallback_rel = if is_v1 { 0x115c50usize } else { 0x115b80usize };
        libgui_base + fallback_rel
    };

    logging::info(&format!(
        "preview: selected H2B constructor at {:#x} (mode: {})",
        chosen_addr,
        if is_v1 { "V1_0" } else { "V2_0" }
    ));

    Some(unsafe { std::mem::transmute::<usize, FnH2bCtor>(chosen_addr) })
}

impl SurfaceBridge {
    pub fn create(token: &[u8]) -> Option<SurfaceBridge> {
        if token.is_empty() {
            logging::error("preview: empty token");
            return None;
        }

        let token_utils = load_lib(&[
            "/apex/com.android.vndk.v31/lib64/android.hidl.token@1.0-utils.so",
            "/apex/com.android.vndk.v31@1/lib64/android.hidl.token@1.0-utils.so",
            "/system/lib64/android.hidl.token@1.0-utils.so",
            "android.hidl.token@1.0-utils.so",
        ])?;

        let retrieve = symbol(
            token_utils,
            "_ZN7android20retrieveHalInterfaceERKNS_8hardware8hidl_vecIhEE",
        )?;

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

        let mut producer: *mut c_void = ptr::null_mut();
        let mut is_v1 = false;
        for (libs, sym, v1) in [
            (
                &[
                    "/system/lib64/android.hardware.graphics.bufferqueue@2.0.so",
                    "android.hardware.graphics.bufferqueue@2.0.so",
                ][..],
                "_ZN7android8hardware8graphics11bufferqueue4V2_022IGraphicBufferProducer8castFromERKNS_2spINS_4hidl4base4V1_05IBaseEEEb",
                false,
            ),
            (
                &[
                    "/system/lib64/android.hardware.graphics.bufferqueue@1.0.so",
                    "android.hardware.graphics.bufferqueue@1.0.so",
                ][..],
                "_ZN7android8hardware8graphics11bufferqueue4V1_022IGraphicBufferProducer8castFromERKNS_2spINS_4hidl4base4V1_05IBaseEEEb",
                true,
            ),
        ] {
            let handle = match load_lib(libs) {
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
                is_v1 = v1;
                break;
            }
        }
        if producer.is_null() {
            logging::error("preview: castFrom failed for both producer versions");
            return None;
        }

        let libgui = load_lib(&[
            "/system/lib64/libgui.so",
            "libgui.so",
        ])?;

        let surface_ctor_sym = symbol(
            libgui,
            "_ZN7android7SurfaceC1ERKNS_2spINS_22IGraphicBufferProducerEEEbRKNS1_INS_7IBinderEEE",
        )?;
        let fn_surface_ctor: FnSurfaceCtor = unsafe {
            std::mem::transmute(surface_ctor_sym)
        };

        let mut dlinfo = DlInfo {
            dli_fname: ptr::null(),
            dli_fbase: ptr::null_mut(),
            dli_sname: ptr::null(),
            dli_saddr: ptr::null_mut(),
        };
        let dladdr_res = unsafe { dladdr(surface_ctor_sym, &mut dlinfo) };
        if dladdr_res == 0 || dlinfo.dli_fbase.is_null() {
            logging::error("preview: dladdr failed for libgui.so base");
            return None;
        }
        let libgui_base = dlinfo.dli_fbase as usize;
        logging::info(&format!("preview: libgui base address: {:#x}", libgui_base));

        let h2b_ctor = find_h2b_ctor(libgui_base, is_v1)?;

        let libutils = load_lib(&["libutils.so", "/system/lib64/libutils.so"])?;
        let fn_inc_strong: FnRefBaseIncStrong = unsafe {
            std::mem::transmute(symbol(libutils, "_ZNK7android7RefBase9incStrongEPKv")?)
        };
        let fn_dec_strong: FnRefBaseDecStrong = unsafe {
            std::mem::transmute(symbol(libutils, "_ZNK7android7RefBase9decStrongEPKv")?)
        };

        let libcxx = load_lib(&["libc++.so"])?;
        let fn_new: FnOperatorNew = unsafe {
            std::mem::transmute(symbol(libcxx, "_Znwm")?)
        };

        // Allocate 120 bytes for H2BGraphicBufferProducer
        let h2b_mem = unsafe { fn_new(120) };
        if h2b_mem.is_null() {
            logging::error("preview: failed to allocate H2BGraphicBufferProducer");
            return None;
        }

        // Construct H2BGraphicBufferProducer directly
        let producer_ptr = producer;
        unsafe { h2b_ctor(h2b_mem, &producer_ptr) };
        logging::info(&format!("preview: constructed H2BGraphicBufferProducer at {:#x}", h2b_mem as usize));

        // Allocate 8352 bytes for Surface (Android 15 sizeof(Surface))
        let surface_mem = unsafe { fn_new(8352) };
        if surface_mem.is_null() {
            logging::error("preview: failed to allocate Surface");
            return None;
        }

        // Construct Surface
        let null_binder: *mut c_void = ptr::null_mut();
        let h2b_sp = h2b_mem;
        unsafe {
            fn_surface_ctor(surface_mem, &h2b_sp, true, &null_binder);
            fn_inc_strong(surface_mem, surface_mem);
        }
        logging::info(&format!("preview: constructed Surface at {:#x}", surface_mem as usize));

        // ANativeWindow interface is located at surface + 0x10
        let anw = unsafe { (surface_mem as *mut u8).add(0x10) as *mut c_void };
        if !is_valid_native_window(anw) {
            logging::error(&format!("preview: invalid ANativeWindow pointer at {:#x}", anw as usize));
            unsafe { fn_dec_strong(surface_mem, surface_mem) };
            return None;
        }
        logging::info(&format!("preview: validated ANativeWindow at {:#x}", anw as usize));

        type FnAnwPerform = unsafe extern "C" fn(*mut c_void, i32, ...) -> i32;
        let fn_perform: FnAnwPerform = unsafe {
            std::mem::transmute(*((anw as usize + 0x98) as *const *mut c_void))
        };
        for api in 1..=4 {
            let res = unsafe { fn_perform(anw, 14, api) };
            if res == 0 {
                logging::info(&format!("preview: disconnected stale api {}", api));
            }
        }

        logging::info("preview: surface bridge successfully established");

        Some(SurfaceBridge {
            native_window: anw,
            surface: surface_mem,
            fn_dec_strong: Some(fn_dec_strong),
            frames: 0,
            fail_count: 0,
        })
    }

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
            let fails = self.fail_count;
            self.fail_count += 1;
            if fails < 5 || fails % 30 == 0 {
                logging::info(&format!(
                    "preview: ANativeWindow_lock failed: {} (bits={:?}, fails={})",
                    locked, buffer.bits, fails + 1
                ));
            }
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
        let max_w = width - 1;
        let max_h = height - 1;
        let denom_w = (bw - 1).max(1);
        let denom_h = (bh - 1).max(1);

        if buffer.format == 4 {
            // WINDOW_FORMAT_RGB_565
            let dst = buffer.bits as *mut u16;
            for dy in 0..bh {
                let sensor_col = ((max_w * (bh - 1 - dy)) / denom_h) as usize;
                let col_uv_base = (sensor_col >> 1) << 1;
                let dst_row = unsafe { dst.add(dy as usize * stride as usize) };
                for dx in 0..bw {
                    let sensor_row = ((max_h * dx) / denom_w) as usize;
                    let y_val = y_plane[sensor_col + sensor_row * width as usize];
                    let uv_idx = (sensor_row >> 1) * width as usize + col_uv_base;
                    let v_val = vu_plane[uv_idx];
                    let u_val = vu_plane[uv_idx + 1];
                    let (r, g, b) = yuv_to_rgb(y_val, u_val, v_val);
                    unsafe {
                        *dst_row.add(dx as usize) =
                            (((r >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((b >> 3) as u16);
                    }
                }
            }
        } else {
            // WINDOW_FORMAT_RGBA_8888 or RGBX_8888
            let dst = buffer.bits as *mut u32;
            for dy in 0..bh {
                let sensor_col = ((max_w * (bh - 1 - dy)) / denom_h) as usize;
                let col_uv_base = (sensor_col >> 1) << 1;
                let dst_row = unsafe { dst.add(dy as usize * stride as usize) };
                for dx in 0..bw {
                    let sensor_row = ((max_h * dx) / denom_w) as usize;
                    let y_val = y_plane[sensor_col + sensor_row * width as usize];
                    let uv_idx = (sensor_row >> 1) * width as usize + col_uv_base;
                    let v_val = vu_plane[uv_idx];
                    let u_val = vu_plane[uv_idx + 1];
                    let (r, g, b) = yuv_to_rgb(y_val, u_val, v_val);
                    unsafe {
                        *dst_row.add(dx as usize) =
                            0xFF00_0000 | ((b as u32) << 16) | ((g as u32) << 8) | r as u32;
                    }
                }
            }
        }

        unsafe { ANativeWindow_unlockAndPost(self.native_window) };
        self.frames += 1;
        if self.frames == 1 || self.frames % 5 == 0 {
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
        if !self.surface.is_null() {
            if let Some(dec) = self.fn_dec_strong {
                unsafe { dec(self.surface, self.surface) };
            }
            self.surface = ptr::null_mut();
            self.native_window = ptr::null_mut();
        }
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

pub fn install(bridge: SurfaceBridge) {
    let mut slot = PREVIEW.lock().unwrap();
    *slot = Some(bridge);
}

pub fn clear() {
    let mut slot = PREVIEW.lock().unwrap();
    *slot = None;
}

pub fn render_current(nv21: &[u8], width: i32, height: i32) {
    let mut slot = PREVIEW.lock().unwrap();
    if let Some(bridge) = slot.as_mut() {
        bridge.render_nv21(nv21, width, height);
    }
}
