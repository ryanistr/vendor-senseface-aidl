// FaceHAL/src/main.rs
//
// Entry point for the AIDL face HAL. Registers
// android.hardware.biometrics.face.IFace/default on the vndbinder, which is
// what the OOS (Android 15) framework's FaceService binds to.
//
// The stock X6739 stack only ever exposed the HIDL
// android.hardware.biometrics.face@1.0::IBiometricsFace/default (a stub named
// "...-service.example" plus face.default.so); this binary is the AIDL
// replacement that closes that gap.
mod bindings;
mod camera;
mod face;
mod logging;
mod session;
mod stface;
mod surface_bridge;

use rsbinder::{hub, ProcessState};

use bindings::android::hardware::biometrics::face::IFace::BnFace;
use face::FaceHal;

const SERVICE_NAME: &str = "android.hardware.biometrics.face.IFace/default";

fn main() {
    logging::init();
    logging::init_rsbinder_logger();
    logging::info("Face HAL (AIDL, rsbinder) starting");

    // Default driver + a thread pool sized for the camera callback path.
    ProcessState::init_default();
    ProcessState::start_thread_pool();

    let service = FaceHal::new();
    let binder = BnFace::new_binder(service);

    match hub::add_service(SERVICE_NAME, binder.as_binder()) {
        Ok(()) => logging::info(&format!("registered {}", SERVICE_NAME)),
        Err(e) => {
            logging::error(&format!("failed to register {}: {:?}", SERVICE_NAME, e));
            // Nothing useful can happen without the service, so exit and let
            // init's restart policy retry rather than idling silently.
            std::process::exit(1);
        }
    }

    let _ = ProcessState::join_thread_pool();
}
