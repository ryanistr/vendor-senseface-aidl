#![allow(non_snake_case, dead_code)]

use std::ffi::c_void;
use std::os::raw::{c_char, c_int};
use std::ptr;
use std::sync::Mutex;

use crate::logging;

const RTLD_NOW: c_int = 2;
const RTLD_GLOBAL: c_int = 0x100;

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

type FnGetNativeWindow = unsafe extern "C" fn(holder: *mut c_void) -> *mut c_void;
type FnDestroySurfaceHolder = unsafe extern "C" fn(holder: *mut c_void);

unsafe impl Send for SurfaceBridge {}

pub struct SurfaceBridge {
    holder: *mut c_void,
    native_window: *mut c_void,
    fn_destroy: Option<FnDestroySurfaceHolder>,
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
        let h = unsafe { dlopen(c.as_ptr(), RTLD_NOW | RTLD_GLOBAL) };
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

// Calls getSurfaceFromHGBP which returns sp<SurfaceHolder> with indirect result pointer in x8
#[inline(always)]
unsafe fn call_get_surface(f: *mut c_void, producer_sp: *const *mut c_void) -> *mut c_void {
    let mut ret = [0u64; 2];
    core::arch::asm!(
        "blr {fn_ptr}",
        fn_ptr = in(reg) f,
        in("x0") producer_sp,
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

impl SurfaceBridge {
    pub fn create(token: &[u8]) -> Option<SurfaceBridge> {
        if token.is_empty() {
            logging::error("preview: empty token");
            return None;
        }

        let token_utils = load_lib(&[
            "/apex/com.android.vndk.v31/lib64/android.hidl.token@1.0-utils.so",
            "/apex/com.android.vndk.v31@1/lib64/android.hidl.token@1.0-utils.so",
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
                    "/apex/com.android.vndk.v31/lib64/android.hardware.graphics.bufferqueue@2.0.so",
                    "android.hardware.graphics.bufferqueue@2.0.so",
                ][..],
                "_ZN7android8hardware8graphics11bufferqueue4V2_022IGraphicBufferProducer8castFromERKNS_2spINS_4hidl4base4V1_05IBaseEEEb",
                false,
            ),
            (
                &[
                    "/apex/com.android.vndk.v31/lib64/android.hardware.graphics.bufferqueue@1.0.so",
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

        let conv = load_lib(&[
            "/apex/com.android.vndk.v31/lib64/libbufferqueueconverter.so",
            "libbufferqueueconverter.so",
        ])?;

        let fn_get_surface = symbol(
            conv,
            "_ZN7android18getSurfaceFromHGBPERKNS_2spINS_8hardware8graphics11bufferqueue4V2_022IGraphicBufferProducerEEE",
        )?;
        let fn_get_native: FnGetNativeWindow = unsafe {
            std::mem::transmute(symbol(conv, "_ZN7android15getNativeWindowEPNS_13SurfaceHolderE")?)
        };
        let fn_destroy: FnDestroySurfaceHolder = unsafe {
            std::mem::transmute(symbol(conv, "_ZN7android20destroySurfaceHolderEPNS_13SurfaceHolderE")?)
        };

        let mut dlinfo = DlInfo {
            dli_fname: ptr::null(),
            dli_fbase: ptr::null_mut(),
            dli_sname: ptr::null(),
            dli_saddr: ptr::null_mut(),
        };
        let dladdr_res = unsafe { dladdr(fn_get_surface, &mut dlinfo) };
        if dladdr_res == 0 || dlinfo.dli_fbase.is_null() {
            logging::error("preview: dladdr failed for libbufferqueueconverter.so base");
            return None;
        }
        let conv_base = dlinfo.dli_fbase as usize;
        logging::info(&format!("preview: libbufferqueueconverter base address: {:#x}", conv_base));

        // When producer is V1_0, patch libbufferqueueconverter's GOT to use V1_0 H2BGraphicBufferProducer
        // GOT slots 0x2240 (ZTT) and 0x2248 (ZTV).
        let mut orig_ztt: usize = 0;
        let mut orig_ztv: usize = 0;
        let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
        let got_page = (conv_base + 0x2000) & !(page_size - 1);

        if is_v1 {
            let got_ztt_ptr = (conv_base + 0x2240) as *mut usize;
            let got_ztv_ptr = (conv_base + 0x2248) as *mut usize;
            unsafe {
                orig_ztt = *got_ztt_ptr;
                orig_ztv = *got_ztv_ptr;
            }

            // Derive libgui.so base address from original GOT[0x2240] (which points to libgui V2_0 ZTT at offset 0x119880)
            let libgui_base = orig_ztt.wrapping_sub(0x119880);
            let ztt_v1 = libgui_base + 0x1197b0;
            let ztv_v1 = libgui_base + 0x112188;

            logging::info(&format!(
                "preview: patching GOT (libgui_base={:#x}, ztt={:#x}, ztv={:#x})",
                libgui_base, ztt_v1, ztv_v1
            ));

            let mprot_rw = unsafe {
                libc::mprotect(
                    got_page as *mut c_void,
                    page_size * 2,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            };
            if mprot_rw == 0 {
                unsafe {
                    *got_ztt_ptr = ztt_v1;
                    *got_ztv_ptr = ztv_v1;
                    libc::mprotect(got_page as *mut c_void, page_size * 2, libc::PROT_READ);
                }
                logging::info("preview: patched GOT for V1_0 producer");
            } else {
                logging::error("preview: failed to make GOT writable");
            }
        }

        let producer_ptr = producer;
        let holder = unsafe { call_get_surface(fn_get_surface, &producer_ptr) };

        // Restore GOT if patched
        if is_v1 && orig_ztt != 0 {
            let got_ztt_ptr = (conv_base + 0x2240) as *mut usize;
            let got_ztv_ptr = (conv_base + 0x2248) as *mut usize;
            if unsafe {
                libc::mprotect(
                    got_page as *mut c_void,
                    page_size * 2,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            } == 0 {
                unsafe {
                    *got_ztt_ptr = orig_ztt;
                    *got_ztv_ptr = orig_ztv;
                    libc::mprotect(got_page as *mut c_void, page_size * 2, libc::PROT_READ);
                }
            }
        }

        if holder.is_null() {
            logging::error("preview: getSurfaceFromHGBP returned null");
            return None;
        }
        logging::info(&format!("preview: obtained SurfaceHolder at {:#x}", holder as usize));

        let anw = unsafe { fn_get_native(holder) };
        if !is_valid_native_window(anw) {
            logging::error(&format!("preview: invalid ANativeWindow pointer at {:#x}", anw as usize));
            unsafe { fn_destroy(holder) };
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

        logging::info("preview: surface bridge successfully established via libbufferqueueconverter");

        Some(SurfaceBridge {
            holder,
            native_window: anw,
            fn_destroy: Some(fn_destroy),
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
        if !self.holder.is_null() {
            if let Some(destroy) = self.fn_destroy {
                unsafe { destroy(self.holder) };
            }
            self.holder = ptr::null_mut();
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
