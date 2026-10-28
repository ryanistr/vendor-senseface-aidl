// FaceHAL/src/session.rs
//
// ISession — per-(sensor, user) session. Drives the front camera and feeds each
// frame to the SenseTime algorithm library, mirroring the stock HIDL HAL's
// enroll/authenticate state machine (camera open -> repeating request ->
// per-frame st_auth_enroll / st_auth_unlock -> terminal callback).
#![allow(non_snake_case, dead_code)]

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use rsbinder::{Interface, Strong};

use crate::bindings::android::hardware::biometrics::common::ICancellationSignal::{
    ICancellationSignal, BnCancellationSignal,
};
use crate::bindings::android::hardware::biometrics::common::OperationContext::OperationContext;
use crate::bindings::android::hardware::biometrics::face::EnrollmentStageConfig::EnrollmentStageConfig;
use crate::bindings::android::hardware::biometrics::face::EnrollmentType::EnrollmentType;
use crate::bindings::android::hardware::biometrics::face::Error::Error as FaceError;
use crate::bindings::android::hardware::biometrics::face::Feature::Feature;
use crate::bindings::android::hardware::biometrics::face::ISession::ISession;
use crate::bindings::android::hardware::biometrics::face::ISessionCallback::ISessionCallback;
use crate::bindings::android::hardware::keymaster::HardwareAuthToken::HardwareAuthToken;
use crate::bindings::android::hardware::common::NativeHandle::NativeHandle;
use crate::camera::{CameraEngine, CAPTURE_HEIGHT, CAPTURE_WIDTH};
use crate::logging::{self, get_prop};
use crate::stface::{self, EnrollOutcome, StFace, CAMERA_CHOOSE_PATH};
use crate::surface_bridge;

/// The algorithm library is process-global; both sessions and the service entry
/// point go through this one handle so st_auth_init runs exactly once.
static ALGO: Mutex<Option<Arc<Mutex<StFace>>>> = Mutex::new(None);

pub fn algo() -> Option<Arc<Mutex<StFace>>> {
    let mut guard = ALGO.lock().unwrap();
    if guard.is_none() {
        let mut st = StFace::new();
        match st.init() {
            Ok(()) => {
                logging::info("session: algorithm library ready");
                *guard = Some(Arc::new(Mutex::new(st)));
            }
            Err(e) => {
                logging::error(&format!("session: algorithm init failed ret={}", e));
                return None;
            }
        }
    }
    guard.clone()
}

/// Only one camera exists and only one operation may run at a time, so the
/// engine is process-global rather than per-session.
static GLOBAL_CAMERA: Mutex<Option<CameraEngine>> = Mutex::new(None);

/// Number of good frames a full enrollment needs. The stock HAL keeps this in
/// default_face_hal::mMaxEnrollCount; its literal value was not recovered, so it
/// is a documented constant with a property override.
pub const DEFAULT_ENROLL_FRAMES: i32 = 5;

fn enroll_frames() -> i32 {
    get_prop(b"persist.sys.facehal.enroll_frames\0")
        .and_then(|s| s.parse::<i32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_ENROLL_FRAMES)
}

/// Front camera id. The stock HAL reads it from
/// /data/vendor_de/0/facedata/cameraChoose.txt; fall back to the conventional
/// front-camera index when that file is absent.
fn camera_id() -> String {
    if let Ok(s) = std::fs::read_to_string(CAMERA_CHOOSE_PATH) {
        let t = s.trim();
        if !t.is_empty() {
            logging::info(&format!("session: camera id from {} = {}", CAMERA_CHOOSE_PATH, t));
            return t.to_string();
        }
    }
    "1".to_string()
}

fn close_camera() {
    let mut lock = GLOBAL_CAMERA.lock().unwrap();
    if let Some(mut cam) = lock.take() {
        cam.stop_capture();
    }
}


/// Extract the HAL token from the preview `NativeHandle`.
///
/// The framework packs the token into the handle's `ints`: the first int is
/// the token length in bytes and the token bytes follow. The handle carries no
/// file descriptors, so the token is the only payload.
fn token_from_handle(handle: &NativeHandle) -> Vec<u8> {
    if handle.ints.is_empty() {
        return Vec::new();
    }
    let len = handle.ints[0] as usize;
    let mut bytes = Vec::with_capacity(handle.ints.len() * 4);
    for word in &handle.ints[1..] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.truncate(len);
    logging::info(&format!(
        "session: preview token {} bytes from {} ints",
        bytes.len(),
        handle.ints.len()
    ));
    bytes
}

pub struct CancellationSignal {
    pub is_cancelled: Arc<AtomicBool>,
}

impl Interface for CancellationSignal {}

impl ICancellationSignal for CancellationSignal {
    fn cancel(&self) -> rsbinder::status::Result<()> {
        self.is_cancelled.store(true, Ordering::SeqCst);
        logging::info("session: operation cancelled");
        close_camera();
        surface_bridge::clear();
        Ok(())
    }
}

pub struct FaceSession {
    sensor_id: i32,
    user_id: i32,
    callback: Strong<dyn ISessionCallback>,
}

// The callback binder is process-wide safe to share across the worker threads.
unsafe impl Send for FaceSession {}
unsafe impl Sync for FaceSession {}

impl FaceSession {
    pub fn new(sensor_id: i32, user_id: i32, callback: Strong<dyn ISessionCallback>) -> Self {
        Self { sensor_id, user_id, callback }
    }

    fn err(&self, e: FaceError, vendor_code: i32) {
        let _ = self.callback.onError(e, vendor_code);
    }
}

impl Interface for FaceSession {}

impl ISession for FaceSession {
    fn generateChallenge(&self) -> rsbinder::status::Result<()> {
        // No TEE on this sensor, so the challenge is a plain nonce. The
        // framework only requires that it round-trips through revokeChallenge.
        let challenge = 0x5FACE000u64 as i64;
        logging::info(&format!("session: generateChallenge {}", challenge));
        self.callback.onChallengeGenerated(challenge)?;
        Ok(())
    }

    fn revokeChallenge(&self, challenge: i64) -> rsbinder::status::Result<()> {
        logging::info(&format!("session: revokeChallenge {}", challenge));
        self.callback.onChallengeRevoked(challenge)?;
        Ok(())
    }

    fn getEnrollmentConfig(&self, _type: EnrollmentType) -> rsbinder::status::Result<Vec<EnrollmentStageConfig>> {
        // Single-stage enrollment; the HAL reports remaining itself.
        Ok(vec![])
    }

    fn enroll(
        &self,
        _hat: &HardwareAuthToken,
        _type: i8,
        _features: &[u8],
        _preview_surface: Option<&NativeHandle>,
    ) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        logging::info("session: enroll");
        logging::info(&format!(
            "session: enroll type={} features={} preview={}",
            _type,
            _features.len(),
            match _preview_surface {
                Some(h) => format!("fd={} ints={:?}", h.fds.len(), h.ints),
                None => "none".to_string(),
            }
        ));
        let algo = match algo() {
            Some(a) => a,
            None => {
                self.err(FaceError::UNABLE_TO_PROCESS, 0);
                return Ok(BnCancellationSignal::new_binder(CancellationSignal {
                    is_cancelled: Arc::new(AtomicBool::new(true)),
                }));
            }
        };

        // The stock HAL prepares the algorithm before scanning: image count per
        // face id, reset, drop stale faces and install the search ROI.
        {
            let guard = algo.lock().unwrap();
            if let Err(e) = guard.prepare_enroll() {
                logging::error(&format!("session: enrollment setup failed ret={}", e));
                self.err(FaceError::UNABLE_TO_PROCESS, 0);
                return Ok(BnCancellationSignal::new_binder(CancellationSignal {
                    is_cancelled: Arc::new(AtomicBool::new(true)),
                }));
            }
        }

        // The preview surface arrives as a HAL token in the NativeHandle's ints
        // (the framework packs it there; see FaceService.acquireSurfaceHandle).
        // Turn it into a window the camera frames can be drawn into.
        if let Some(handle) = _preview_surface {
            let token = token_from_handle(handle);
            match surface_bridge::SurfaceBridge::create(&token) {
                Some(bridge) => {
                    logging::info("session: preview surface attached");
                    surface_bridge::install(bridge);
                }
                None => logging::error("session: could not decode the preview surface token"),
            }
        }

        let cb = self.callback.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancelled.clone();
        let total = enroll_frames();

        thread::spawn(move || {
            close_camera();
            let mut cam = CameraEngine::new();
            if !cam.open(&camera_id()) {
                logging::error("session: enroll could not open camera");
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
                return;
            }
            {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                *lock = Some(cam);
            }

            let enrolled = Arc::new(AtomicI32::new(0));
            let e2 = enrolled.clone();
            let c2 = cancel_flag.clone();
            let a2 = algo.clone();
            let cb2 = cb.clone();

            let started = {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                match lock.as_mut() {
                    Some(c) => c.start_capture(CAPTURE_WIDTH, CAPTURE_HEIGHT, move |data, w, h, stride| {
                        if c2.load(Ordering::SeqCst) {
                            return;
                        }
                        if e2.load(Ordering::SeqCst) >= total {
                            return;
                        }
                        // The algorithm library consumes NV21: the Y plane
                        // followed by the interleaved V and U planes.
                        let frame = unsafe {
                            std::slice::from_raw_parts(data, (w * h * 3 / 2) as usize)
                        };
                        // Draw the frame into the preview surface.
                        surface_bridge::render_current(frame, w, h);
                        let guard = a2.lock().unwrap();
                        // st_auth_enroll returns the number of images it still
                        // needs; 0 means the enrollment finished and the
                        // returned value is the new face id.
                        match guard.enroll(frame, w, h, stride) {
                            Ok(EnrollOutcome::Accepted) => {
                                let n = e2.fetch_add(1, Ordering::SeqCst) + 1;
                                let remaining = (total - n).max(0);
                                logging::info(&format!(
                                    "session: enroll sample {}/{} accepted",
                                    n, total
                                ));
                                let _ = cb2.onEnrollmentProgress(0, remaining);
                            }
                            Ok(EnrollOutcome::Complete(face_id)) => {
                                let n = e2.fetch_add(1, Ordering::SeqCst) + 1;
                                logging::info(&format!(
                                    "session: enrollment complete after {} samples, face_id={}",
                                    n, face_id
                                ));
                                let _ = cb2.onEnrollmentProgress(face_id, 0);
                                close_camera();
                                surface_bridge::clear();
                            }
                            Err(r) => {
                                logging::error(&format!("session: st_auth_enroll ret={}", r));
                            }
                        }
                    }),
                    None => false,
                }
            };

            if !started {
                logging::error("session: enroll capture did not start");
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
            }
        });

        Ok(BnCancellationSignal::new_binder(CancellationSignal { is_cancelled: cancelled }))
    }

    fn authenticate(&self, operation_id: i64) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        logging::info(&format!("session: authenticate op={}", operation_id));
        let algo = match algo() {
            Some(a) => a,
            None => {
                self.err(FaceError::UNABLE_TO_PROCESS, 0);
                return Ok(BnCancellationSignal::new_binder(CancellationSignal {
                    is_cancelled: Arc::new(AtomicBool::new(true)),
                }));
            }
        };

        // The stock HAL prepares the algorithm before scanning: image count per
        // face id, reset, drop stale faces and install the search ROI.
        {
            let guard = algo.lock().unwrap();
            if let Err(e) = guard.prepare_enroll() {
                logging::error(&format!("session: enrollment setup failed ret={}", e));
                self.err(FaceError::UNABLE_TO_PROCESS, 0);
                return Ok(BnCancellationSignal::new_binder(CancellationSignal {
                    is_cancelled: Arc::new(AtomicBool::new(true)),
                }));
            }
        }

        let cb = self.callback.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancel_flag = cancelled.clone();

        thread::spawn(move || {
            close_camera();
            let mut cam = CameraEngine::new();
            if !cam.open(&camera_id()) {
                logging::error("session: authenticate could not open camera");
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
                return;
            }
            {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                *lock = Some(cam);
            }

            let done = Arc::new(AtomicBool::new(false));
            let d2 = done.clone();
            let c2 = cancel_flag.clone();
            let a2 = algo.clone();
            let cb2 = cb.clone();

            let started = {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                match lock.as_mut() {
                    Some(c) => c.start_capture(CAPTURE_WIDTH, CAPTURE_HEIGHT, move |data, w, h, stride| {
                        if c2.load(Ordering::SeqCst) || d2.load(Ordering::SeqCst) {
                            return;
                        }
                        let frame = unsafe {
                            std::slice::from_raw_parts(data, (w * h * 3 / 2) as usize)
                        };
                        let guard = a2.lock().unwrap();
                        match guard.unlock(frame, w, h, stride) {
                            Ok(()) => {
                                d2.store(true, Ordering::SeqCst);
                                logging::info("session: authentication succeeded");
                                // Weak strength: an empty token (challenge 0, no mac)
                                // is what the framework expects here.
                                let hat = HardwareAuthToken::default();
                                let _ = cb2.onAuthenticationSucceeded(0, &hat);
                                close_camera();
                            }
                            Err(r) => {
                                // Non-zero is "no match yet", not a fatal error;
                                // keep scanning until the framework cancels.
                                if r != stface::ST_OK {
                                    logging::info(&format!("session: st_auth_unlock ret={}", r));
                                }
                            }
                        }
                    }),
                    None => false,
                }
            };

            if !started {
                logging::error("session: authenticate capture did not start");
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
            }
        });

        Ok(BnCancellationSignal::new_binder(CancellationSignal { is_cancelled: cancelled }))
    }

    fn detectInteraction(&self) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        // supportsDetectInteraction is false in getSensorProps.
        self.err(FaceError::UNABLE_TO_PROCESS, 0);
        Ok(BnCancellationSignal::new_binder(CancellationSignal {
            is_cancelled: Arc::new(AtomicBool::new(true)),
        }))
    }

    fn enumerateEnrollments(&self) -> rsbinder::status::Result<()> {
        let ids = match algo() {
            Some(a) => {
                let g = a.lock().unwrap();
                match g.template_info() {
                    Ok(buf) => {
                        // st_template_info_t is 24 bytes; its first word is the
                        // enrolled face count on this library. Treat it as a
                        // count, not a struct, until the layout is confirmed.
                        let count = i32::from_ne_bytes([buf[0], buf[1], buf[2], buf[3]]);
                        (0..count.max(0)).collect::<Vec<i32>>()
                    }
                    Err(_) => vec![],
                }
            }
            None => vec![],
        };
        logging::info(&format!("session: enumerateEnrollments -> {:?}", ids));
        self.callback.onEnrollmentsEnumerated(&ids)?;
        Ok(())
    }

    fn removeEnrollments(&self, enrollment_ids: &[i32]) -> rsbinder::status::Result<()> {
        logging::info(&format!("session: removeEnrollments {:?}", enrollment_ids));
        let algo = match algo() {
            Some(a) => a,
            None => {
                self.err(FaceError::UNABLE_TO_PROCESS, 0);
                return Ok(());
            }
        };
        let g = algo.lock().unwrap();
        if enrollment_ids.is_empty() {
            g.delete_all();
        } else {
            for id in enrollment_ids {
                g.delete_face(*id);
            }
        }
        self.callback.onEnrollmentsRemoved(enrollment_ids)?;
        Ok(())
    }

    fn getFeatures(&self) -> rsbinder::status::Result<()> {
        self.callback.onFeaturesRetrieved(&[])?;
        Ok(())
    }

    fn setFeature(&self, _hat: &HardwareAuthToken, feature: Feature, _enabled: bool) -> rsbinder::status::Result<()> {
        self.callback.onFeatureSet(feature)?;
        Ok(())
    }

    fn getAuthenticatorId(&self) -> rsbinder::status::Result<()> {
        // Weak sensors must report 0.
        self.callback.onAuthenticatorIdRetrieved(0)?;
        Ok(())
    }

    fn invalidateAuthenticatorId(&self) -> rsbinder::status::Result<()> {
        self.callback.onAuthenticatorIdInvalidated(0)?;
        Ok(())
    }

    fn resetLockout(&self, _hat: &HardwareAuthToken) -> rsbinder::status::Result<()> {
        self.callback.onLockoutCleared()?;
        Ok(())
    }

    fn close(&self) -> rsbinder::status::Result<()> {
        logging::info("session: close");
        close_camera();
        surface_bridge::clear();
        self.callback.onSessionClosed()?;
        Ok(())
    }

    fn authenticateWithContext(
        &self,
        operation_id: i64,
        _context: &OperationContext,
    ) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        self.authenticate(operation_id)
    }

    fn enrollWithContext(
        &self,
        hat: &HardwareAuthToken,
        r#type: i8,
        features: &[u8],
        preview_surface: Option<&NativeHandle>,
        _context: &OperationContext,
    ) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        self.enroll(hat, r#type, features, preview_surface)
    }

    fn detectInteractionWithContext(
        &self,
        _context: &OperationContext,
    ) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        self.detectInteraction()
    }

    fn onContextChanged(&self, _context: &OperationContext) -> rsbinder::status::Result<()> {
        Ok(())
    }
}
