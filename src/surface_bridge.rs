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

#[repr(C)]
struct Elf64_Ehdr {
    e_ident: [u8; 16],
    e_type: u16,
    e_machine: u16,
    e_version: u32,
    e_entry: u64,
    e_phoff: u64,
    e_shoff: u64,
    e_flags: u32,
    e_ehsize: u16,
    e_phentsize: u16,
    e_phnum: u16,
    e_shentsize: u16,
    e_shnum: u16,
    e_shstrndx: u16,
}

#[repr(C)]
struct Elf64_Phdr {
    p_type: u32,
    p_flags: u32,
    p_offset: u64,
    p_vaddr: u64,
    p_paddr: u64,
    p_filesz: u64,
    p_memsz: u64,
    p_align: u64,
}

#[repr(C)]
struct Elf64_Dyn {
    d_tag: i64,
    d_val_ptr: u64,
}

#[repr(C)]
struct Elf64_Sym {
    st_name: u32,
    st_info: u8,
    st_other: u8,
    st_shndx: u16,
    st_value: u64,
    st_size: u64,
}

const PT_DYNAMIC: u32 = 2;
const DT_NULL: i64 = 0;
const DT_STRTAB: i64 = 5;
const DT_SYMTAB: i64 = 6;

unsafe fn find_symbol_in_elf_memory(base: usize, target_name: &str) -> Option<*mut c_void> {
    if base == 0 {
        return None;
    }
    let ehdr = &*(base as *const Elf64_Ehdr);
    if &ehdr.e_ident[0..4] != b"\x7fELF" {
        return None;
    }

    let phdr_ptr = (base + ehdr.e_phoff as usize) as *const Elf64_Phdr;
    let mut dyn_ptr: *const Elf64_Dyn = ptr::null();
    for i in 0..ehdr.e_phnum as usize {
        let ph = &*phdr_ptr.add(i);
        if ph.p_type == PT_DYNAMIC {
            dyn_ptr = (base + ph.p_vaddr as usize) as *const Elf64_Dyn;
            break;
        }
    }
    if dyn_ptr.is_null() {
        return None;
    }

    let mut symtab_addr: usize = 0;
    let mut strtab_addr: usize = 0;
    let mut d = dyn_ptr;
    loop {
        let tag = (*d).d_tag;
        if tag == DT_NULL {
            break;
        }
        if tag == DT_SYMTAB {
            symtab_addr = (*d).d_val_ptr as usize;
        } else if tag == DT_STRTAB {
            strtab_addr = (*d).d_val_ptr as usize;
        }
        d = d.add(1);
    }

    if symtab_addr == 0 || strtab_addr == 0 {
        return None;
    }
    if symtab_addr < base {
        symtab_addr += base;
    }
    if strtab_addr < base {
        strtab_addr += base;
    }

    let target_bytes = target_name.as_bytes();
    let symtab = symtab_addr as *const Elf64_Sym;
    let strtab = strtab_addr as *const u8;

    for i in 1..20000 {
        let sym = &*symtab.add(i);
        if sym.st_name != 0 {
            let mut name_ptr = strtab.add(sym.st_name as usize);
            let mut matched = true;
            for &b in target_bytes {
                if *name_ptr != b {
                    matched = false;
                    break;
                }
                name_ptr = name_ptr.add(1);
            }
            if matched && *name_ptr == 0 {
                return Some((base + sym.st_value as usize) as *mut c_void);
            }
        }
        if symtab_addr < strtab_addr && (symtab.add(i) as usize) >= strtab_addr {
            break;
        }
    }

    None
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
    holds_lock: bool,
    last_post: std::time::Instant,
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

static TEXT_PATCHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

unsafe fn patch_bqc_text(conv_base: usize) {
    if TEXT_PATCHED.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }

    let page_size = libc::sysconf(libc::_SC_PAGESIZE) as usize;
    let text_page = (conv_base + 0x1000) & !(page_size - 1);

    if libc::mprotect(
        text_page as *mut libc::c_void,
        page_size,
        libc::PROT_READ | libc::PROT_WRITE | libc::PROT_EXEC,
    ) != 0 {
        logging::error("preview: failed to make bqc .text writable");
        return;
    }

    let p_alloc = (conv_base + 0x10c0) as *mut u32;
    let p_vtab1 = (conv_base + 0x12b8) as *mut u32;
    let p_vtab2 = (conv_base + 0x12bc) as *mut u32;

    logging::info(&format!(
        "preview: original bqc opcodes: alloc={:#x}, vtab1={:#x}, vtab2={:#x}",
        *p_alloc, *p_vtab1, *p_vtab2
    ));

    // mov w0, #0x20e0 (8416 bytes for Android 15 Surface)
    *p_alloc = 0x5284_1c00;
    // add x10, x8, #0x258 (Android 15 RefBase vtable)
    *p_vtab1 = 0x9109_610a;
    // add x8, x8, #0x198 (Android 15 BBinder vtable)
    *p_vtab2 = 0x9106_6108;

    libc::mprotect(
        text_page as *mut libc::c_void,
        page_size,
        libc::PROT_READ | libc::PROT_EXEC,
    );

    core::arch::asm!(
        "dc cvau, {p1}",
        "dc cvau, {p2}",
        "dsb ish",
        "ic ivau, {p1}",
        "ic ivau, {p2}",
        "dsb ish",
        "isb",
        p1 = in(reg) (conv_base + 0x10c0),
        p2 = in(reg) (conv_base + 0x12b8),
    );

    logging::info("preview: patched bqc .text for Android 15 Surface and vtables");
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

        unsafe { patch_bqc_text(conv_base); }

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

            let mut gui_info = DlInfo {
                dli_fname: ptr::null(),
                dli_fbase: ptr::null_mut(),
                dli_sname: ptr::null(),
                dli_saddr: ptr::null_mut(),
            };
            let dladdr_gui = unsafe { dladdr(orig_ztt as *const c_void, &mut gui_info) };
            let libgui_base = if dladdr_gui != 0 && !gui_info.dli_fbase.is_null() {
                gui_info.dli_fbase as usize
            } else {
                0
            };
            logging::info(&format!(
                "preview: libgui base from dladdr: {:#x}",
                libgui_base
            ));

            let ztt_v1 = unsafe {
                find_symbol_in_elf_memory(
                    libgui_base,
                    "_ZTTN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
                )
            };
            let ztv_v1 = unsafe {
                find_symbol_in_elf_memory(
                    libgui_base,
                    "_ZTVN7android8hardware8graphics11bufferqueue4V1_05utils24H2BGraphicBufferProducerE",
                )
            };

            if let (Some(ztt), Some(ztv)) = (ztt_v1, ztv_v1) {
                logging::info(&format!(
                    "preview: patching GOT (orig_ztt={:#x}, ztt_v1={:#x}, orig_ztv={:#x}, ztv_v1={:#x})",
                    orig_ztt, ztt as usize, orig_ztv, ztv as usize
                ));

                let mprot_rw = unsafe {
                    libc::mprotect(
                        got_page as *mut c_void,
                        page_size,
                        libc::PROT_READ | libc::PROT_WRITE,
                    )
                };
                if mprot_rw == 0 {
                    unsafe {
                        *got_ztt_ptr = ztt as usize;
                        *got_ztv_ptr = ztv as usize;
                        libc::mprotect(got_page as *mut c_void, page_size, libc::PROT_READ);
                    }
                    logging::info("preview: patched GOT for V1_0 producer");
                } else {
                    logging::error("preview: failed to make GOT writable");
                }
            } else {
                logging::error("preview: could not resolve V1_0 ZTT/ZTV symbols from libgui memory");
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
                    page_size,
                    libc::PROT_READ | libc::PROT_WRITE,
                )
            } == 0 {
                unsafe {
                    *got_ztt_ptr = orig_ztt;
                    *got_ztv_ptr = orig_ztv;
                    libc::mprotect(got_page as *mut c_void, page_size, libc::PROT_READ);
                }
                logging::info("preview: restored GOT");
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

        let libnative = load_lib(&["libnativewindow.so", "/system/lib64/libnativewindow.so"]);
        let mut target_w = 480;
        let mut target_h = 480;
        if let Some(h) = libnative {
            type FnGetDim = unsafe extern "C" fn(*mut c_void) -> i32;

            if let Some(sym) = symbol(h, "ANativeWindow_getWidth") {
                let fn_w: FnGetDim = unsafe { std::mem::transmute(sym) };
                let w = unsafe { fn_w(anw) };
                if w > 0 {
                    target_w = w;
                }
            }
            if let Some(sym) = symbol(h, "ANativeWindow_getHeight") {
                let fn_h: FnGetDim = unsafe { std::mem::transmute(sym) };
                let h = unsafe { fn_h(anw) };
                if h > 0 {
                    target_h = h;
                }
            }
            logging::info(&format!("preview: detected window dimensions {}x{}", target_w, target_h));
        }

        let geom = unsafe {
            ANativeWindow_setBuffersGeometry(anw, target_w, target_h, 1)
        };
        logging::info(&format!(
            "preview: ANativeWindow_setBuffersGeometry({}, {}, RGBA_8888) ret={}",
            target_w, target_h, geom
        ));

        logging::info("preview: surface bridge successfully established via libbufferqueueconverter");

        Some(SurfaceBridge {
            holder,
            native_window: anw,
            fn_destroy: Some(fn_destroy),
            frames: 0,
            fail_count: 0,
            holds_lock: false,
            last_post: std::time::Instant::now() - std::time::Duration::from_millis(50),
        })
    }

    pub fn render_nv21(&mut self, nv21: &[u8], width: i32, height: i32) -> i32 {
        if width <= 0 || height <= 0 || self.native_window.is_null() {
            return -1;
        }

        if self.last_post.elapsed() < std::time::Duration::from_millis(10) {
            return 0;
        }

        let mut buffer = ANativeWindow_Buffer {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            bits: ptr::null_mut(),
            reserved: [0; 6],
        };
        let t_lock = std::time::Instant::now();
        let locked = unsafe { ANativeWindow_lock(self.native_window, &mut buffer, ptr::null_mut()) };
        let lock_ms = t_lock.elapsed().as_millis();
        if locked != 0 || buffer.bits.is_null() {
            let fails = self.fail_count;
            self.fail_count += 1;
            if fails < 5 || fails % 30 == 0 {
                logging::info(&format!(
                    "preview: lock failed: code={} bits={:?} fails={} lock_ms={} frames={}",
                    locked, buffer.bits, fails + 1, lock_ms, self.frames
                ));
            }
            return -3;
        }
        self.fail_count = 0;
        self.holds_lock = true;
        if self.frames == 0 {
            logging::info(&format!(
                "preview: first lock ok: {}x{} stride {} format {}",
                buffer.width, buffer.height, buffer.stride, buffer.format
            ));
        }

        let bw = buffer.width;
        let bh = buffer.height;
        let stride = buffer.stride;
        if bw <= 0 || bh <= 0 || stride <= 0 {
            let res = unsafe { ANativeWindow_unlockAndPost(self.native_window) };
            self.holds_lock = false;
            if res != 0 {
                logging::error(&format!("preview: unlockAndPost err: {}", res));
            }
            return -5;
        }

        let y_plane = nv21;
        let vu_plane = &nv21[(width * height) as usize..];

        // Aspect-ratio preserving center-crop (cover mode).
        // Sensor portrait orientation: width is sensor height, height is sensor width.
        let src_pw = height;
        let src_ph = width;

        let crop_w: i32;
        let crop_h: i32;
        let crop_x0: i32;
        let crop_y0: i32;

        if bw * src_ph > bh * src_pw {
            crop_w = src_pw;
            crop_h = (src_pw * bh) / bw;
            crop_x0 = 0;
            crop_y0 = (src_ph - crop_h) / 2;
        } else {
            crop_h = src_ph;
            crop_w = (src_ph * bw) / bh;
            crop_x0 = (src_pw - crop_w) / 2;
            crop_y0 = 0;
        }

        let denom_w = (bw - 1).max(1);
        let denom_h = (bh - 1).max(1);

        let bw_usize = (bw as usize).min(1080);
        let mut row_y_offs = [0usize; 1080];
        let mut row_uv_offs = [0usize; 1080];
        for dx in 0..bw_usize {
            let px = crop_x0 + (dx as i32 * (crop_w - 1)) / denom_w;
            let s_row = px.clamp(0, height - 1) as usize;
            row_y_offs[dx] = s_row * width as usize;
            row_uv_offs[dx] = (s_row >> 1) * width as usize;
        }

        // The preview is mirrored (selfie-style): the front camera's raw frame is
        // a direct view of the user, but a mirror reads naturally. Only the
        // display is flipped; the algorithm keeps the un-mirrored sensor frame.
        if buffer.format == 4 {
            // WINDOW_FORMAT_RGB_565
            let dst = buffer.bits as *mut u16;
            for dy in 0..bh {
                let py = crop_y0 + (dy * (crop_h - 1)) / denom_h;
                let sensor_col = ((width - 1) - py).clamp(0, width - 1) as usize;
                let col_uv_base = (sensor_col >> 1) << 1;
                let dst_row = unsafe { dst.add(dy as usize * stride as usize) };
                for dx in 0..bw_usize {
                    let y_val = y_plane[sensor_col + row_y_offs[dx]];
                    let uv_idx = row_uv_offs[dx] + col_uv_base;
                    let v_val = vu_plane[uv_idx];
                    let u_val = vu_plane[uv_idx + 1];
                    let (r, g, b) = yuv_to_rgb(y_val, u_val, v_val);
                    unsafe {
                        *dst_row.add((bw as usize - 1) - dx) =
                            (((r >> 3) as u16) << 11) | (((g >> 2) as u16) << 5) | ((b >> 3) as u16);
                    }
                }
            }
        } else {
            // WINDOW_FORMAT_RGBA_8888 or RGBX_8888
            let dst = buffer.bits as *mut u32;
            for dy in 0..bh {
                let py = crop_y0 + (dy * (crop_h - 1)) / denom_h;
                let sensor_col = ((width - 1) - py).clamp(0, width - 1) as usize;
                let col_uv_base = (sensor_col >> 1) << 1;
                let dst_row = unsafe { dst.add(dy as usize * stride as usize) };
                for dx in 0..bw_usize {
                    let y_val = y_plane[sensor_col + row_y_offs[dx]];
                    let uv_idx = row_uv_offs[dx] + col_uv_base;
                    let v_val = vu_plane[uv_idx];
                    let u_val = vu_plane[uv_idx + 1];
                    let (r, g, b) = yuv_to_rgb(y_val, u_val, v_val);
                    unsafe {
                        *dst_row.add((bw as usize - 1) - dx) =
                            0xFF00_0000 | ((b as u32) << 16) | ((g as u32) << 8) | r as u32;
                    }
                }
            }
        }

        let post_res = unsafe { ANativeWindow_unlockAndPost(self.native_window) };
        self.holds_lock = false;
        if post_res != 0 {
            logging::error(&format!("preview: ANativeWindow_unlockAndPost failed: {}", post_res));
        }
        self.last_post = std::time::Instant::now();
        self.frames += 1;
        if self.frames == 1 || self.frames % 15 == 0 {
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
        if self.holds_lock && !self.native_window.is_null() {
            unsafe { ANativeWindow_unlockAndPost(self.native_window) };
            self.holds_lock = false;
        }
        if !self.holder.is_null() {
            if let Some(destroy) = self.fn_destroy {
                unsafe { destroy(self.holder) };
            }
            self.holder = ptr::null_mut();
            self.native_window = ptr::null_mut();
        }
    }
}

#[inline(always)]
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

pub fn init_hidl_thread_pool() {
    let lib = load_lib(&["/system/lib64/libhidlbase.so", "libhidlbase.so"]);
    let handle = match lib {
        Some(h) => h,
        None => {
            logging::error("hidl: could not dlopen libhidlbase.so");
            return;
        }
    };

    type FnConfig = unsafe extern "C" fn(usize, bool);
    if let Some(sym) = symbol(handle, "_ZN7android8hardware22configureRpcThreadpoolEmb") {
        let fn_config: FnConfig = unsafe { std::mem::transmute(sym) };
        unsafe { fn_config(4, false) };
        logging::info("hidl: configureRpcThreadpool(4, false) ok");
    }

    type FnJoin = unsafe extern "C" fn();
    if let Some(sym) = symbol(handle, "_ZN7android8hardware17joinRpcThreadpoolEv") {
        let fn_join: FnJoin = unsafe { std::mem::transmute(sym) };
        std::thread::Builder::new()
            .name("hwbinder".into())
            .spawn(move || {
                logging::info("hidl: hwbinder worker thread started");
                unsafe { fn_join() };
            })
            .ok();
    }
}
