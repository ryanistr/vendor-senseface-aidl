// FaceHAL/src/face.rs
//
// IFace — the top-level face HAL interface the framework binds to.
//
// Sensor strength is CONVENIENCE, not STRONG: this is an RGB camera sensor with
// no TEE-backed template storage, so it must not hand the framework a
// HardwareAuthToken. That is also why getAuthenticatorId returns 0.
#![allow(non_snake_case, dead_code)]

use rsbinder::{status::Result as BinderResult, Interface, Strong};

use crate::bindings::android::hardware::biometrics::common::CommonProps::CommonProps;
use crate::bindings::android::hardware::biometrics::common::ComponentInfo::ComponentInfo;
use crate::bindings::android::hardware::biometrics::common::SensorStrength::SensorStrength;
use crate::bindings::android::hardware::biometrics::face::FaceSensorType::FaceSensorType;
use crate::bindings::android::hardware::biometrics::face::IFace::IFace;
use crate::bindings::android::hardware::biometrics::face::ISession::BnSession;
use crate::bindings::android::hardware::biometrics::face::ISession::ISession;
use crate::bindings::android::hardware::biometrics::face::ISessionCallback::ISessionCallback;
use crate::bindings::android::hardware::biometrics::face::SensorProps::SensorProps;
use crate::logging;
use crate::session::FaceSession;

/// Sensor id used by the stock HIDL HAL (default_face_hal::SENSOR_ID).
pub const SENSOR_ID: i32 = 1;

pub struct FaceHal {}

impl FaceHal {
    pub fn new() -> Self {
        Self {}
    }
}

impl Interface for FaceHal {}

impl IFace for FaceHal {
    fn getSensorProps(&self) -> BinderResult<Vec<SensorProps>> {
        logging::info("face: getSensorProps");
        let props = SensorProps {
            commonProps: CommonProps {
                sensorId: SENSOR_ID,
                sensorStrength: SensorStrength::CONVENIENCE,
                maxEnrollmentsPerUser: 3,
                componentInfo: vec![ComponentInfo {
                    componentId: "faceSensor".to_string(),
                    hardwareVersion: "sensetime".to_string(),
                    firmwareVersion: "1.0".to_string(),
                    serialNumber: "0".to_string(),
                    softwareVersion: "1.0".to_string(),
                }],
            },
            sensorType: FaceSensorType::RGB,
            halControlsPreview: true,
            previewDisplayId: 0,
            enrollPreviewWidth: 480,
            enrollPreviewHeight: 640,
            enrollTranslationX: 0.0,
            enrollTranslationY: 0.0,
            enrollPreviewScale: 1.0,
            supportsDetectInteraction: false,
        };
        Ok(vec![props])
    }

    fn createSession(
        &self,
        sensor_id: i32,
        user_id: i32,
        callback: &Strong<dyn ISessionCallback>,
    ) -> BinderResult<Strong<dyn ISession>> {
        logging::info(&format!(
            "face: createSession sensor={} user={}",
            sensor_id, user_id
        ));
        let session = FaceSession::new(sensor_id, user_id, callback.clone());
        Ok(BnSession::new_binder(session))
    }
}
