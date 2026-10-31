// FaceHAL/src/camera.rs
//
// NDK Camera2 front-camera capture. Ported from the proven X6739 Rust camera
// driver used by the earlier face HAL on this same device (MT6893), with the
// image path narrowed to the Y plane that the algorithm library consumes.
//
// Only libcamera2ndk_vendor (/vendor/lib64), libmediandk and libnativewindow
// (both llndk) are linked, so every DT_NEEDED resolves inside the vendor
// linker namespace.
#![allow(non_snake_case, dead_code)]

use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

use crate::logging;

pub enum ACameraManager {}
pub enum ACameraDevice {}
pub enum AImageReader {}
pub enum ACameraCaptureSession {}
pub enum ACaptureRequest {}
pub enum ACaptureSessionOutputContainer {}
pub enum ACaptureSessionOutput {}
pub enum ACameraOutputTarget {}
pub enum ANativeWindow {}
pub enum AImage {}

#[repr(C)]
pub struct ACameraDevice_StateCallbacks {
    pub context: *mut c_void,
    pub onDisconnected: Option<extern "C" fn(*mut c_void, *mut ACameraDevice)>,
    pub onError: Option<extern "C" fn(*mut c_void, *mut ACameraDevice, c_int)>,
}

#[repr(C)]
pub struct ACameraCaptureSession_stateCallbacks {
    pub context: *mut c_void,
    pub onClosed: Option<extern "C" fn(*mut c_void, *mut ACameraCaptureSession)>,
    pub onReady: Option<extern "C" fn(*mut c_void, *mut ACameraCaptureSession)>,
    pub onActive: Option<extern "C" fn(*mut c_void, *mut ACameraCaptureSession)>,
}

#[repr(C)]
pub struct AImageReader_ImageListener {
    pub context: *mut c_void,
    pub onImageAvailable: Option<extern "C" fn(*mut c_void, *mut AImageReader)>,
}

const ACAMERA_OK: c_int = 0;
const AMEDIA_OK: c_int = 0;
const AIMAGE_FORMAT_YUV_420_888: i32 = 35;
/// AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN — the stock HAL's reader usage flag.
const AIMAGE_USAGE_CPU_READ_OFTEN: i64 = 0x1;
const TEMPLATE_PREVIEW: c_int = 1;

/// Capture geometry. The stock HAL opens its AImageReader at exactly 640x480
/// (`AImageReader_newWithUsage(640, 480, YUV_420_888, 3, 1)`) and rejects any
/// frame that is not that size, so the port uses the same geometry.
pub const CAPTURE_WIDTH: i32 = 640;
pub const CAPTURE_HEIGHT: i32 = 480;

/// Pixel format code for NV21 in the algorithm library's own format enum.
/// The library maps its format names to integers in the order
/// yuv420p=1, nv12=2, nv21=3, bgra=4, rgb=5, rgba=6 (the name table is at
/// 0x595840 in libstfaceunlockppl.so, and the lookup at 0x1d4568 returns those
/// values). The stock HAL writes the literal `3` into the descriptor's `+0x08`
/// field, i.e. NV21.
pub const FORMAT_NV21: i32 = 3;

extern "C" {
    fn ACameraManager_create() -> *mut ACameraManager;
    fn ACameraManager_delete(manager: *mut ACameraManager);
    fn ACameraManager_openCamera(
        manager: *mut ACameraManager,
        id: *const c_char,
        callbacks: *mut ACameraDevice_StateCallbacks,
        device: *mut *mut ACameraDevice,
    ) -> c_int;
    fn ACameraDevice_close(device: *mut ACameraDevice) -> c_int;
    fn ACameraDevice_createCaptureRequest(
        device: *mut ACameraDevice,
        template: c_int,
        request: *mut *mut ACaptureRequest,
    ) -> c_int;
    fn ACameraDevice_createCaptureSession(
        device: *mut ACameraDevice,
        outputs: *mut ACaptureSessionOutputContainer,
        callbacks: *const ACameraCaptureSession_stateCallbacks,
        session: *mut *mut ACameraCaptureSession,
    ) -> c_int;
    fn ACameraCaptureSession_close(session: *mut ACameraCaptureSession);
    fn ACameraCaptureSession_setRepeatingRequest(
        session: *mut ACameraCaptureSession,
        callbacks: *mut c_void,
        numRequests: c_int,
        requests: *mut *mut ACaptureRequest,
        captureSequenceId: *mut c_int,
    ) -> c_int;
    fn ACaptureSessionOutputContainer_create(
        container: *mut *mut ACaptureSessionOutputContainer,
    ) -> c_int;
    fn ACaptureSessionOutputContainer_free(container: *mut ACaptureSessionOutputContainer);
    fn ACaptureSessionOutputContainer_add(
        container: *mut ACaptureSessionOutputContainer,
        output: *mut ACaptureSessionOutput,
    ) -> c_int;
    /// The vendor's camera2ndk takes the output's raw `native_handle*`, not an
    /// `ANativeWindow*`: it converts it to a `hidl_handle` itself, and the
    /// library imports no `ANativeWindow` symbol at all. The stock HAL passes
    /// the reader's handle from `AImageReader_getWindowNativeHandle`.
    fn ACaptureSessionOutput_create(
        handle: *mut c_void,
        output: *mut *mut ACaptureSessionOutput,
    ) -> c_int;
    fn ACaptureSessionOutput_free(output: *mut ACaptureSessionOutput);
    fn ACameraOutputTarget_create(
        handle: *mut c_void,
        target: *mut *mut ACameraOutputTarget,
    ) -> c_int;
    fn ACameraOutputTarget_free(target: *mut ACameraOutputTarget);
    fn ACaptureRequest_addTarget(
        request: *mut ACaptureRequest,
        target: *mut ACameraOutputTarget,
    ) -> c_int;
    fn ACaptureRequest_free(request: *mut ACaptureRequest);
    fn AImageReader_new(
        width: i32,
        height: i32,
        format: i32,
        maxImages: i32,
        reader: *mut *mut AImageReader,
    ) -> c_int;
    fn AImageReader_newWithUsage(
        width: i32,
        height: i32,
        format: i32,
        maxImages: i32,
        usage: i64,
        reader: *mut *mut AImageReader,
    ) -> c_int;
    fn AImageReader_delete(reader: *mut AImageReader);
    fn AImageReader_getWindow(reader: *mut AImageReader, window: *mut *mut ANativeWindow) -> c_int;
    /// Deprecated, but the vendor camera2ndk still speaks it: it takes the
    /// reader's raw `native_handle*`, not an `ANativeWindow*`.
    fn AImageReader_getWindowNativeHandle(
        reader: *mut AImageReader,
        handle: *mut *mut c_void,
    ) -> c_int;
    fn AImageReader_setImageListener(
        reader: *mut AImageReader,
        listener: *mut AImageReader_ImageListener,
    ) -> c_int;
    fn AImageReader_acquireNextImage(reader: *mut AImageReader, image: *mut *mut AImage) -> c_int;
    fn AImage_delete(image: *mut AImage);
    fn AImage_getWidth(image: *mut AImage, width: *mut i32) -> c_int;
    fn AImage_getHeight(image: *mut AImage, height: *mut i32) -> c_int;
    fn AImage_getPlaneData(
        image: *mut AImage,
        planeIdx: c_int,
        data: *mut *mut u8,
        dataLen: *mut c_int,
    ) -> c_int;
    fn AImage_getPlanePixelStride(
        image: *mut AImage,
        plane: c_int,
        pixel_stride: *mut c_int,
    ) -> c_int;
    fn AImage_getPlaneRowStride(
        image: *mut AImage,
        planeIdx: c_int,
        rowStride: *mut c_int,
    ) -> c_int;
    fn ANativeWindow_acquire(window: *mut ANativeWindow);
    fn ANativeWindow_release(window: *mut ANativeWindow);
}

/// A captured frame: the Y-plane pointer, its geometry, and the plane's row
/// stride in bytes. The stride is passed separately because the algorithm
/// descriptor needs the real stride, which is not the width for a padded
/// camera buffer.

/// Convert a YUV_420_888 image into the NV21 buffer the algorithm expects.
///
/// The Y plane is copied row by row (dropping any row padding) and the V and U
/// planes are then interleaved as VU pairs, which is the NV21 layout the stock
/// HAL produces at 0x86a0-0x86bc.
#[allow(clippy::too_many_arguments)]
/// Packs the camera's YUV_420_888 planes into the NV21 buffer the SenseTime
/// library expects: Y plane, then interleaved V,U.
///
/// The stock HAL (face.default.so, camera_first_frame_callback at
/// 0x8634-0x86bc) walks the chroma planes as a flat array indexed by the
/// plane's *pixel* stride, not per row by the row stride, because the camera
/// exposes a semi-planar buffer through the planar planes.
/// Writes the first captured frame to /data/local/tmp/frame.nv21 so the
/// pixels the algorithm actually sees can be inspected off-device.
fn dump_frame(nv21: &[u8]) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static DONE: AtomicBool = AtomicBool::new(false);
    if DONE.swap(true, Ordering::SeqCst) {
        return;
    }
    if std::fs::write("/data/local/tmp/frame.nv21", nv21).is_ok() {
        logging::info(&format!("camera: dumped {} byte frame", nv21.len()));
    }
}

fn to_nv21(
    y: *const u8,
    _y_stride: i32,
    y_len: usize,
    u: *const u8,
    u_pixel_stride: i32,
    _u_len: usize,
    v: *const u8,
    width: i32,
    height: i32,
) -> Option<Vec<u8>> {
    if width <= 0 || height <= 0 {
        return None;
    }
    let w = width as usize;
    let h = height as usize;
    let ps = u_pixel_stride.max(1) as usize;
    let samples = (w * h) / 4;
    let total_len = (w * h * 3) / 2;
    let mut out = vec![0u8; total_len];

    unsafe {
        let copy_y = y_len.min(w * h);
        std::ptr::copy_nonoverlapping(y, out.as_mut_ptr(), copy_y);
        let chroma = out.as_mut_ptr().add(w * h);
        for i in 0..samples {
            let src_off = i * ps;
            let dst_off = i * 2;
            *chroma.add(dst_off) = *v.add(src_off);
            *chroma.add(dst_off + 1) = *u.add(src_off);
        }
    }
    Some(out)
}

pub type FrameFn = Box<dyn FnMut(*const u8, i32, i32, i32) + Send>;

struct CallbackCtx {
    active: Mutex<bool>,
    cb: Mutex<Option<FrameFn>>,
}

pub struct CameraEngine {
    manager: *mut ACameraManager,
    device: *mut ACameraDevice,
    reader: *mut AImageReader,
    session: *mut ACameraCaptureSession,
    request: *mut ACaptureRequest,
    outputs: *mut ACaptureSessionOutputContainer,
    img_output: *mut ACaptureSessionOutput,
    target: *mut ACameraOutputTarget,
    window: *mut ANativeWindow,
    /// The reader's raw output handle, which the vendor camera2ndk expects.
    window_handle: *mut c_void,

    device_callbacks: Box<ACameraDevice_StateCallbacks>,
    session_callbacks: Box<ACameraCaptureSession_stateCallbacks>,
    reader_listener: Box<AImageReader_ImageListener>,

    image_callback_ctx: *mut c_void,
    output_added: bool,
}

unsafe impl Send for CameraEngine {}
unsafe impl Sync for CameraEngine {}

impl CameraEngine {
    pub fn new() -> Self {
        Self {
            manager: unsafe { ACameraManager_create() },
            device: ptr::null_mut(),
            reader: ptr::null_mut(),
            session: ptr::null_mut(),
            request: ptr::null_mut(),
            outputs: ptr::null_mut(),
            img_output: ptr::null_mut(),
            target: ptr::null_mut(),
            window: ptr::null_mut(),
            window_handle: ptr::null_mut(),
            device_callbacks: Box::new(ACameraDevice_StateCallbacks {
                context: ptr::null_mut(),
                onDisconnected: Some(Self::on_disconnected),
                onError: Some(Self::on_error),
            }),
            session_callbacks: Box::new(ACameraCaptureSession_stateCallbacks {
                context: ptr::null_mut(),
                onClosed: Some(Self::on_session_closed),
                onReady: Some(Self::on_session_ready),
                onActive: Some(Self::on_session_active),
            }),
            reader_listener: Box::new(AImageReader_ImageListener {
                context: ptr::null_mut(),
                onImageAvailable: Some(Self::on_image_available),
            }),
            image_callback_ctx: ptr::null_mut(),
            output_added: false,
        }
    }

    pub fn open(&mut self, camera_id: &str) -> bool {
        if self.manager.is_null() {
            logging::error("camera: manager is null");
            return false;
        }
        let id_c = match CString::new(camera_id) {
            Ok(c) => c,
            Err(_) => return false,
        };
        let res = unsafe {
            ACameraManager_openCamera(self.manager, id_c.as_ptr(), &mut *self.device_callbacks, &mut self.device)
        };
        if res != ACAMERA_OK || self.device.is_null() {
            logging::error(&format!("camera: open {} failed ret={}", camera_id, res));
            return false;
        }
        logging::info(&format!("camera: opened {}", camera_id));
        true
    }

    pub fn start_capture<F>(&mut self, width: i32, height: i32, callback: F) -> bool
    where
        F: FnMut(*const u8, i32, i32, i32) + Send + 'static,
    {
        logging::info(&format!("camera: start_capture {}x{}", width, height));
        unsafe {
            self.teardown_session();

            if self.reader.is_null() {
                // The stock HAL opens the reader with
                // AImageReader_newWithUsage(640, 480, YUV_420_888, 3, 1) —
                // usage 1 is CPU_READ_OFTEN, without which the buffers cannot
                // be read back on the CPU, and maxImages is 3.
                let res = AImageReader_newWithUsage(
                    width,
                    height,
                    AIMAGE_FORMAT_YUV_420_888,
                    3,
                    AIMAGE_USAGE_CPU_READ_OFTEN,
                    &mut self.reader,
                );
                if res != AMEDIA_OK || self.reader.is_null() {
                    logging::error(&format!("camera: AImageReader_newWithUsage ret={}", res));
                    return false;
                }
                // The output's raw native handle is what the vendor camera2ndk
                // wants (it has no ANativeWindow support of its own).
                let mut handle: *mut c_void = ptr::null_mut();
                let handle_status =
                    AImageReader_getWindowNativeHandle(self.reader, &mut handle);
                if handle_status != AMEDIA_OK || handle.is_null() {
                    logging::error(&format!(
                        "camera: AImageReader_getWindowNativeHandle ret={}",
                        handle_status
                    ));
                    return false;
                }
                self.window_handle = handle;
            }

            if !self.image_callback_ctx.is_null() {
                drop(Arc::from_raw(
                    self.image_callback_ctx as *const CallbackCtx,
                ));
                self.image_callback_ctx = ptr::null_mut();
            }

            let cb: FrameFn = Box::new(callback);
            let ctx = Arc::into_raw(Arc::new(CallbackCtx {
                active: Mutex::new(true),
                cb: Mutex::new(Some(cb)),
            })) as *mut c_void;
            self.image_callback_ctx = ctx;
            self.reader_listener.context = ctx;
            AImageReader_setImageListener(self.reader, &mut *self.reader_listener);

            let res = ACaptureSessionOutputContainer_create(&mut self.outputs);
            if res != ACAMERA_OK {
                logging::error(&format!("camera: output container create ret={}", res));
                return false;
            }
            let res = ACaptureSessionOutput_create(self.window_handle, &mut self.img_output);
            if res != ACAMERA_OK {
                logging::error(&format!("camera: session output create ret={}", res));
                return false;
            }
            let res = ACaptureSessionOutputContainer_add(self.outputs, self.img_output);
            if res != ACAMERA_OK {
                logging::error(&format!("camera: output container add ret={}", res));
                return false;
            }
            self.output_added = true;

            let res = ACameraOutputTarget_create(self.window_handle, &mut self.target);
            if res != ACAMERA_OK {
                logging::error(&format!("camera: output target create ret={}", res));
                return false;
            }

            let res = ACameraDevice_createCaptureRequest(self.device, TEMPLATE_PREVIEW, &mut self.request);
            if res != ACAMERA_OK || self.request.is_null() {
                logging::error(&format!("camera: createCaptureRequest ret={}", res));
                return false;
            }
            ACaptureRequest_addTarget(self.request, self.target);

            let sess_status = ACameraDevice_createCaptureSession(
                self.device,
                self.outputs,
                &*self.session_callbacks,
                &mut self.session,
            );
            if sess_status != ACAMERA_OK || self.session.is_null() {
                logging::error(&format!("camera: createCaptureSession ret={}", sess_status));
                return false;
            }

            let req_status = ACameraCaptureSession_setRepeatingRequest(
                self.session,
                ptr::null_mut(),
                1,
                &mut self.request,
                ptr::null_mut(),
            );
            if req_status != ACAMERA_OK {
                logging::error(&format!("camera: setRepeatingRequest ret={}", req_status));
                return false;
            }
            logging::info("camera: capture loop started");
        }
        true
    }

    pub fn stop_capture(&mut self) {
        unsafe { self.teardown_session(); }
    }

    fn deactivate_callback(&mut self) {
        if !self.image_callback_ctx.is_null() {
            unsafe {
                let ctx = &*(self.image_callback_ctx as *const CallbackCtx);
                if let Ok(mut active) = ctx.active.lock() {
                    *active = false;
                }
                if let Ok(mut cb) = ctx.cb.lock() {
                    *cb = None;
                }
            }
        }
    }

    unsafe fn teardown_session(&mut self) {
        self.deactivate_callback();
        if !self.session.is_null() {
            ACameraCaptureSession_close(self.session);
            self.session = ptr::null_mut();
        }
        if !self.request.is_null() {
            if !self.target.is_null() {
                ACaptureRequest_addTarget(self.request, ptr::null_mut());
            }
            ACaptureRequest_free(self.request);
            self.request = ptr::null_mut();
        }
        if !self.target.is_null() {
            ACameraOutputTarget_free(self.target);
            self.target = ptr::null_mut();
        }
        if !self.outputs.is_null() {
            ACaptureSessionOutputContainer_free(self.outputs);
            self.outputs = ptr::null_mut();
        }
        if !self.img_output.is_null() {
            if !self.output_added {
                ACaptureSessionOutput_free(self.img_output);
            }
            self.img_output = ptr::null_mut();
        }
        self.output_added = false;
    }

    extern "C" fn on_disconnected(_context: *mut c_void, _device: *mut ACameraDevice) {
        logging::error("camera: disconnected");
    }

    extern "C" fn on_error(_context: *mut c_void, _device: *mut ACameraDevice, error: c_int) {
        logging::error(&format!("camera: device error {}", error));
    }

    extern "C" fn on_session_closed(_context: *mut c_void, _session: *mut ACameraCaptureSession) {}

    extern "C" fn on_session_ready(_context: *mut c_void, _session: *mut ACameraCaptureSession) {}

    extern "C" fn on_session_active(_context: *mut c_void, _session: *mut ACameraCaptureSession) {}

    extern "C" fn on_image_available(context: *mut c_void, reader: *mut AImageReader) {
        if context.is_null() {
            return;
        }
        unsafe {
            let ctx = &*(context as *const CallbackCtx);
            let (nv21, width, height) = {
                let active = match ctx.active.lock() {
                    Ok(g) => g,
                    Err(_) => return,
                };
                if !*active {
                    return;
                }
                let mut image = ptr::null_mut();
                let res = AImageReader_acquireNextImage(reader, &mut image);
                if res != AMEDIA_OK || image.is_null() {
                    return;
                }
                let mut width = 0;
                let mut height = 0;
                AImage_getWidth(image, &mut width);
                AImage_getHeight(image, &mut height);

                // The algorithm library consumes NV21, not the Y plane. The stock
                // HAL copies the Y plane and then interleaves the V and U planes
                // into one buffer (face.default.so, camera_first_frame_callback at
                // 0x8634-0x86bc), so this does the same.
                let mut y_data = ptr::null_mut();
                let mut y_len = 0;
                AImage_getPlaneData(image, 0, &mut y_data, &mut y_len);
                let mut u_data = ptr::null_mut();
                let mut u_len = 0;
                AImage_getPlaneData(image, 1, &mut u_data, &mut u_len);
                let mut v_data = ptr::null_mut();
                let mut v_len = 0;
                AImage_getPlaneData(image, 2, &mut v_data, &mut v_len);

                let mut y_stride = 0;
                AImage_getPlaneRowStride(image, 0, &mut y_stride);
                let mut u_pixel = 0;
                AImage_getPlanePixelStride(image, 1, &mut u_pixel);

                let nv21 = if !y_data.is_null() && !u_data.is_null() && !v_data.is_null() {
                    to_nv21(
                        y_data,
                        y_stride,
                        y_len as usize,
                        u_data,
                        u_pixel,
                        u_len as usize,
                        v_data,
                        width,
                        height,
                    )
                } else {
                    None
                };
                AImage_delete(image);
                (nv21, width, height)
            };

            if let Some(nv21) = nv21 {
                dump_frame(&nv21);
                if let Ok(mut lock) = ctx.cb.try_lock() {
                    if let Some(cb) = lock.as_mut() {
                        // The NV21 buffer is tightly packed, so its row
                        // stride is the width.
                        cb(nv21.as_ptr(), width, height, width);
                    }
                }
            }
        }
    }
}

impl Drop for CameraEngine {
    fn drop(&mut self) {
        unsafe {
            logging::info("camera: dropping engine");
            self.deactivate_callback();
            self.teardown_session();
            if !self.device.is_null() {
                ACameraDevice_close(self.device);
                self.device = ptr::null_mut();
            }
            if !self.window.is_null() {
                ANativeWindow_release(self.window);
                self.window = ptr::null_mut();
            }
            if !self.reader.is_null() {
                AImageReader_setImageListener(self.reader, ptr::null_mut());
                AImageReader_delete(self.reader);
                self.reader = ptr::null_mut();
            }
            if !self.manager.is_null() {
                ACameraManager_delete(self.manager);
                self.manager = ptr::null_mut();
            }
            if !self.image_callback_ctx.is_null() {
                drop(Arc::from_raw(self.image_callback_ctx as *const CallbackCtx));
                self.image_callback_ctx = ptr::null_mut();
            }
        }
    }
}
