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
const TEMPLATE_PREVIEW: c_int = 1;

/// Default capture geometry. The stock HAL lets the camera service pick, but a
/// fixed size keeps the enrollment/auth frame pipeline deterministic.
pub const CAPTURE_WIDTH: i32 = 480;
pub const CAPTURE_HEIGHT: i32 = 640;

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
    fn ACaptureSessionOutput_create(
        window: *mut ANativeWindow,
        output: *mut *mut ACaptureSessionOutput,
    ) -> c_int;
    fn ACaptureSessionOutput_free(output: *mut ACaptureSessionOutput);
    fn ACameraOutputTarget_create(
        window: *mut ANativeWindow,
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
    fn AImageReader_delete(reader: *mut AImageReader);
    fn AImageReader_getWindow(reader: *mut AImageReader, window: *mut *mut ANativeWindow) -> c_int;
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
    fn ANativeWindow_acquire(window: *mut ANativeWindow);
    fn ANativeWindow_release(window: *mut ANativeWindow);
}

pub type FrameFn = Box<dyn FnMut(*const u8, i32, i32) + Send>;

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
        F: FnMut(*const u8, i32, i32) + Send + 'static,
    {
        logging::info(&format!("camera: start_capture {}x{}", width, height));
        unsafe {
            self.teardown_session();

            if self.reader.is_null() {
                let res = AImageReader_new(width, height, AIMAGE_FORMAT_YUV_420_888, 5, &mut self.reader);
                if res != AMEDIA_OK || self.reader.is_null() {
                    logging::error(&format!("camera: AImageReader_new ret={}", res));
                    return false;
                }
                let mut native_window = ptr::null_mut();
                let win_status = AImageReader_getWindow(self.reader, &mut native_window);
                if win_status != AMEDIA_OK || native_window.is_null() {
                    logging::error(&format!("camera: AImageReader_getWindow ret={}", win_status));
                    return false;
                }
                ANativeWindow_acquire(native_window);
                self.window = native_window;
            }

            if !self.image_callback_ctx.is_null() {
                drop(Arc::from_raw(
                    self.image_callback_ctx as *const Mutex<Option<FrameFn>>,
                ));
                self.image_callback_ctx = ptr::null_mut();
            }

            let cb: FrameFn = Box::new(callback);
            let ctx = Arc::into_raw(Arc::new(Mutex::new(Some(cb)))) as *mut c_void;
            self.image_callback_ctx = ctx;
            self.reader_listener.context = ctx;
            AImageReader_setImageListener(self.reader, &mut *self.reader_listener);

            let res = ACaptureSessionOutputContainer_create(&mut self.outputs);
            if res != ACAMERA_OK {
                logging::error(&format!("camera: output container create ret={}", res));
                return false;
            }
            let res = ACaptureSessionOutput_create(self.window, &mut self.img_output);
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

            let res = ACameraOutputTarget_create(self.window, &mut self.target);
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

    unsafe fn teardown_session(&mut self) {
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
        let mut image = ptr::null_mut();
        unsafe {
            let res = AImageReader_acquireNextImage(reader, &mut image);
            if res != AMEDIA_OK || image.is_null() {
                return;
            }
            let mut width = 0;
            let mut height = 0;
            AImage_getWidth(image, &mut width);
            AImage_getHeight(image, &mut height);

            let mut y_data = ptr::null_mut();
            let mut y_len = 0;
            AImage_getPlaneData(image, 0, &mut y_data, &mut y_len);

            if !context.is_null() && !y_data.is_null() {
                let m = &*(context as *const Mutex<Option<FrameFn>>);
                if let Ok(mut lock) = m.try_lock() {
                    if let Some(cb) = lock.as_mut() {
                        cb(y_data, width, height);
                    }
                }
            }
            AImage_delete(image);
        }
    }
}

impl Drop for CameraEngine {
    fn drop(&mut self) {
        unsafe {
            logging::info("camera: dropping engine");
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
                let arc = Arc::from_raw(self.image_callback_ctx as *const Mutex<Option<FrameFn>>);
                *arc.lock().unwrap() = None;
                self.image_callback_ctx = ptr::null_mut();
            }
        }
    }
}
