// FaceHAL/src/session.rs
//
// ISession — per-(sensor, user) session. Drives the front camera and feeds each
// frame to the SenseTime algorithm library, mirroring the stock HIDL HAL's
// enroll/authenticate state machine (camera open -> repeating request ->
// per-frame st_auth_enroll / st_auth_unlock -> terminal callback).
#![allow(non_snake_case, dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

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
use crate::stface::{EnrollOutcome, StFace, CAMERA_CHOOSE_PATH};
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

/// Serializes camera open -> capture loop -> close across session worker threads.
static SESSION_LOCK: Mutex<()> = Mutex::new(());

/// Tracks the active operation's cancellation flag and wakeup condvar so starting
/// a new operation or closing the session immediately wakes and stops the prior one.
static ACTIVE_OP: Mutex<Option<(Arc<AtomicBool>, Arc<Condvar>)>> = Mutex::new(None);

fn cancel_active_operation() {
    let mut guard = ACTIVE_OP.lock().unwrap();
    if let Some((ref cancel, ref notify)) = *guard {
        cancel.store(true, Ordering::SeqCst);
        notify.notify_all();
    }
    *guard = None;
}

fn register_active_operation(cancel: &Arc<AtomicBool>, notify: &Arc<Condvar>) {
    let mut guard = ACTIVE_OP.lock().unwrap();
    if let Some((ref prev_cancel, ref prev_notify)) = *guard {
        prev_cancel.store(true, Ordering::SeqCst);
        prev_notify.notify_all();
    }
    *guard = Some((cancel.clone(), notify.clone()));
}

type FrameData = (Vec<u8>, i32, i32, i32);

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
    pub is_completed: Arc<AtomicBool>,
    pub notify: Arc<Condvar>,
    pub callback: Option<Strong<dyn ISessionCallback>>,
}

impl Interface for CancellationSignal {}

impl ICancellationSignal for CancellationSignal {
    fn cancel(&self) -> rsbinder::status::Result<()> {
        if !self.is_cancelled.swap(true, Ordering::SeqCst) {
            logging::info("session: operation cancelled");
            self.notify.notify_all();
            if !self.is_completed.load(Ordering::SeqCst) {
                if let Some(ref cb) = self.callback {
                    let _ = cb.onError(FaceError::CANCELED, 0);
                }
            }
        }
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
                    is_completed: Arc::new(AtomicBool::new(false)),
                    notify: Arc::new(Condvar::new()),
                    callback: None,
                }));
            }
        };

        let preview_token = _preview_surface.map(token_from_handle);
        let cb = self.callback.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let completed = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Condvar::new());
        register_active_operation(&cancelled, &notify);

        let cancel_flag = cancelled.clone();
        let complete_flag = completed.clone();
        let notify_worker = notify.clone();
        let total = enroll_frames();

        thread::spawn(move || {
            let _session_guard = SESSION_LOCK.lock().unwrap();
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }

            close_camera();
            surface_bridge::clear();

            {
                let guard = algo.lock().unwrap();
                if let Err(e) = guard.prepare_enroll() {
                    logging::error(&format!("session: enrollment setup failed ret={}", e));
                    let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
                    return;
                }
                let r = guard.set_faceid_image_number(total);
                logging::info(&format!("session: enroll set_faceid_image_number({}) ret={}", total, r));
            }

            if let Some(ref token) = preview_token {
                match surface_bridge::SurfaceBridge::create(token) {
                    Some(bridge) => {
                        logging::info("session: preview surface attached");
                        surface_bridge::install(bridge);
                    }
                    None => logging::error("session: could not decode the preview surface token"),
                }
            }

            let mut cam = CameraEngine::new();
            if !cam.open(&camera_id()) {
                logging::error("session: enroll could not open camera");
                surface_bridge::clear();
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
                return;
            }
            {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                *lock = Some(cam);
            }

            let latest_frame: Arc<Mutex<Option<FrameData>>> = Arc::new(Mutex::new(None));
            let lf_cb = latest_frame.clone();
            let c_cb = cancel_flag.clone();
            let n_cb = notify_worker.clone();

            let started = {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                match lock.as_mut() {
                    Some(c) => c.start_capture(CAPTURE_WIDTH, CAPTURE_HEIGHT, move |data, w, h, stride| {
                        if c_cb.load(Ordering::SeqCst) {
                            return;
                        }
                        let frame = unsafe {
                            std::slice::from_raw_parts(data, (w * h * 3 / 2) as usize)
                        };
                        // Render every camera frame immediately without blocking on algorithm calls.
                        surface_bridge::render_current(frame, w, h);

                        if let Ok(mut slot) = lf_cb.lock() {
                            *slot = Some((frame.to_vec(), w, h, stride));
                        }
                        n_cb.notify_all();
                    }),
                    None => false,
                }
            };

            if !started {
                logging::error("session: enroll capture did not start");
                close_camera();
                surface_bridge::clear();
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
                return;
            }

            let start_time = Instant::now();
            let mut next_sample_at = start_time + Duration::from_millis(700);
            let mut enrolled_count = 0i32;

            loop {
                if cancel_flag.load(Ordering::SeqCst) {
                    break;
                }
                if start_time.elapsed() > Duration::from_secs(30) {
                    logging::info("session: enroll timed out after 30s");
                    if !cancel_flag.swap(true, Ordering::SeqCst) {
                        let _ = cb.onError(FaceError::TIMEOUT, 0);
                    }
                    break;
                }

                let now = Instant::now();
                if now < next_sample_at {
                    let wait_dur = (next_sample_at - now).min(Duration::from_millis(100));
                    let slot = latest_frame.lock().unwrap();
                    let _ = notify_worker.wait_timeout(slot, wait_dur).unwrap();
                    continue;
                }

                let maybe_frame = {
                    let mut slot = latest_frame.lock().unwrap();
                    if slot.is_none() {
                        let (mut slot, _) = notify_worker
                            .wait_timeout(slot, Duration::from_millis(100))
                            .unwrap();
                        slot.take()
                    } else {
                        slot.take()
                    }
                };

                let (frame_bytes, w, h, stride) = match maybe_frame {
                    Some(f) => f,
                    None => continue,
                };

                if cancel_flag.load(Ordering::SeqCst) {
                    break;
                }

                let outcome = {
                    let guard = algo.lock().unwrap();
                    guard.enroll(&frame_bytes, w, h, stride)
                };

                match outcome {
                    Ok(EnrollOutcome::Accepted) => {
                        enrolled_count += 1;
                        let remaining = (total - enrolled_count).max(1);
                        logging::info(&format!(
                            "session: enroll sample {}/{} accepted (remaining={})",
                            enrolled_count, total, remaining
                        ));
                        let _ = cb.onEnrollmentProgress(1, remaining);
                        // Sample 1 triggers FaceEnrollNewActivity.onEnrollProgressStart (600ms delay + 450ms fade-in + 560ms 0->20% ring).
                        let delay_ms = if enrolled_count == 1 { 1600 } else { 650 };
                        next_sample_at = Instant::now() + Duration::from_millis(delay_ms);
                    }
                    Ok(EnrollOutcome::Complete(face_id)) => {
                        let final_id = if face_id > 0 { face_id } else { 1 };
                        enrolled_count = total;
                        complete_flag.store(true, Ordering::SeqCst);
                        logging::info(&format!(
                            "session: enrollment complete after {} samples, face_id={}",
                             enrolled_count, final_id
                        ));
                        let _ = cb.onEnrollmentProgress(final_id, 0);
                        // Allow the final 80%->100% UI ring transition (560ms) to finish before teardown.
                        let slot = latest_frame.lock().unwrap();
                        let _ = notify_worker
                            .wait_timeout(slot, Duration::from_millis(600))
                            .unwrap();
                        break;
                    }
                    Err(r) => {
                        if r != 1 {
                            logging::info(&format!("session: st_auth_enroll ret={}", r));
                        }
                        next_sample_at = Instant::now() + Duration::from_millis(80);
                    }
                }
            }

            cancel_flag.store(true, Ordering::SeqCst);
            close_camera();
            surface_bridge::clear();
        });

        Ok(BnCancellationSignal::new_binder(CancellationSignal {
            is_cancelled: cancelled,
            is_completed: completed,
            notify,
            callback: Some(self.callback.clone()),
        }))
    }

    fn authenticate(&self, operation_id: i64) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        logging::info(&format!("session: authenticate op={}", operation_id));
        let algo = match algo() {
            Some(a) => a,
            None => {
                self.err(FaceError::UNABLE_TO_PROCESS, 0);
                return Ok(BnCancellationSignal::new_binder(CancellationSignal {
                    is_cancelled: Arc::new(AtomicBool::new(true)),
                    is_completed: Arc::new(AtomicBool::new(false)),
                    notify: Arc::new(Condvar::new()),
                    callback: None,
                }));
            }
        };

        let cb = self.callback.clone();
        let cancelled = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Condvar::new());
        register_active_operation(&cancelled, &notify);

        let cancel_flag = cancelled.clone();
        let done_flag = done.clone();
        let notify_worker = notify.clone();
        let user_id = self.user_id as i64;

        thread::spawn(move || {
            let _session_guard = SESSION_LOCK.lock().unwrap();
            if cancel_flag.load(Ordering::SeqCst) {
                return;
            }

            close_camera();

            // Reset algorithm ROI to full frame (matching stock face_authenticate at 0xa574).
            {
                let guard = algo.lock().unwrap();
                let r = guard.clear_roi();
                logging::info(&format!("session: authenticate cleared roi ret={}", r));
            }

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

            let latest_frame: Arc<Mutex<Option<FrameData>>> = Arc::new(Mutex::new(None));
            let lf_cb = latest_frame.clone();
            let c_cb = cancel_flag.clone();
            let n_cb = notify_worker.clone();

            let started = {
                let mut lock = GLOBAL_CAMERA.lock().unwrap();
                match lock.as_mut() {
                    Some(c) => c.start_capture(CAPTURE_WIDTH, CAPTURE_HEIGHT, move |data, w, h, stride| {
                        if c_cb.load(Ordering::SeqCst) {
                            return;
                        }
                        let frame = unsafe {
                            std::slice::from_raw_parts(data, (w * h * 3 / 2) as usize)
                        };
                        if let Ok(mut slot) = lf_cb.lock() {
                            *slot = Some((frame.to_vec(), w, h, stride));
                        }
                        n_cb.notify_all();
                    }),
                    None => false,
                }
            };

            if !started {
                logging::error("session: authenticate capture did not start");
                close_camera();
                let _ = cb.onError(FaceError::UNABLE_TO_PROCESS, 0);
                return;
            }

            let start_time = Instant::now();
            let mut frame_counter = 0i32;

            loop {
                if cancel_flag.load(Ordering::SeqCst) || done_flag.load(Ordering::SeqCst) {
                    break;
                }
                if start_time.elapsed() > Duration::from_secs(8) {
                    done_flag.store(true, Ordering::SeqCst);
                    if !cancel_flag.swap(true, Ordering::SeqCst) {
                        logging::info("session: authenticate timed out after 8s");
                        let _ = cb.onError(FaceError::TIMEOUT, 0);
                    }
                    break;
                }

                let maybe_frame = {
                    let mut slot = latest_frame.lock().unwrap();
                    if slot.is_none() {
                        let (mut slot, _) = notify_worker
                            .wait_timeout(slot, Duration::from_millis(100))
                            .unwrap();
                        slot.take()
                    } else {
                        slot.take()
                    }
                };

                let (frame_bytes, w, h, stride) = match maybe_frame {
                    Some(f) => f,
                    None => continue,
                };

                if cancel_flag.load(Ordering::SeqCst) || done_flag.load(Ordering::SeqCst) {
                    break;
                }

                let result = {
                    let guard = algo.lock().unwrap();
                    match guard.unlock(&frame_bytes, w, h, stride) {
                        Ok(()) => {
                            let fid = guard.get_face_id();
                            Ok(if fid > 0 { fid } else { 1 })
                        }
                        Err(r) => Err(r),
                    }
                };

                match result {
                    Ok(face_id) => {
                        done_flag.store(true, Ordering::SeqCst);
                        cancel_flag.store(true, Ordering::SeqCst);
                        logging::info(&format!("session: authentication succeeded for face_id={}", face_id));
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as i64;
                        let hat = HardwareAuthToken {
                            challenge: operation_id,
                            userId: user_id,
                            authenticatorId: face_id as i64,
                            authenticatorType: crate::bindings::android::hardware::keymaster::HardwareAuthenticatorType::HardwareAuthenticatorType(8),
                            timestamp: crate::bindings::android::hardware::keymaster::Timestamp::Timestamp {
                                milliSeconds: now_ms,
                            },
                            mac: vec![0u8; 32],
                        };
                        let _ = cb.onAuthenticationSucceeded(face_id, &hat);
                        break;
                    }
                    Err(r) => {
                        frame_counter += 1;
                        if r != 1 || frame_counter % 15 == 0 {
                            logging::info(&format!(
                                "session: st_auth_unlock ret={} (frame {})",
                                r, frame_counter
                            ));
                        }
                    }
                }
            }

            cancel_flag.store(true, Ordering::SeqCst);
            close_camera();
        });

        Ok(BnCancellationSignal::new_binder(CancellationSignal {
            is_cancelled: cancelled,
            is_completed: done,
            notify,
            callback: Some(self.callback.clone()),
        }))
    }

    fn detectInteraction(&self) -> rsbinder::status::Result<Strong<dyn ICancellationSignal>> {
        // supportsDetectInteraction is false in getSensorProps.
        self.err(FaceError::UNABLE_TO_PROCESS, 0);
        Ok(BnCancellationSignal::new_binder(CancellationSignal {
            is_cancelled: Arc::new(AtomicBool::new(true)),
            is_completed: Arc::new(AtomicBool::new(false)),
            notify: Arc::new(Condvar::new()),
            callback: None,
        }))
    }

    fn enumerateEnrollments(&self) -> rsbinder::status::Result<()> {
        let ids = match algo() {
            Some(a) => {
                let g = a.lock().unwrap();
                if g.has_enrolled_face() {
                    vec![g.get_face_id()]
                } else {
                    vec![]
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
        cancel_active_operation();
        let _session_guard = SESSION_LOCK.lock().unwrap();
        close_camera();
        surface_bridge::clear();
        drop(_session_guard);
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
