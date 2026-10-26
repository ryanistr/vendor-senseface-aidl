// FaceHAL/src/bindings.rs
//
// rsbinder-aidl output, generated at build time from aidl/ (face AIDL V3 —
// the frozen version in aidl/aidl_api/android.hardware.biometrics.face/3, which
// is inside the 2-4 range the OOS framework's compatibility_matrix.202404.xml
// accepts for android.hardware.biometrics.face).
#![allow(non_snake_case)]
#![allow(unused_imports)]
#![allow(dead_code)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));

pub use android::hardware::biometrics::face::IFace::{IFace, BnFace};
pub use android::hardware::biometrics::face::ISession::{ISession, BnSession};
pub use android::hardware::biometrics::face::ISessionCallback::ISessionCallback;
pub use android::hardware::biometrics::face::SensorProps::SensorProps;
pub use android::hardware::biometrics::face::FaceSensorType::FaceSensorType;
pub use android::hardware::biometrics::face::EnrollmentType::EnrollmentType;
pub use android::hardware::biometrics::face::Feature::Feature;
pub use android::hardware::biometrics::face::EnrollmentStageConfig::EnrollmentStageConfig;
pub use android::hardware::biometrics::face::Error::Error as FaceError;
pub use android::hardware::biometrics::common::CommonProps::CommonProps;
pub use android::hardware::biometrics::common::SensorStrength::SensorStrength;
pub use android::hardware::biometrics::common::ComponentInfo::ComponentInfo;
pub use android::hardware::biometrics::common::ICancellationSignal::{
    ICancellationSignal, BnCancellationSignal,
};
pub use android::hardware::biometrics::common::OperationContext::OperationContext;
pub use android::hardware::keymaster::HardwareAuthToken::HardwareAuthToken;
pub use android::hardware::common::NativeHandle::NativeHandle;

/// Frozen interface identity for face V3, read from
/// aidl/aidl_api/android.hardware.biometrics.face/3/.hash.
pub const INTERFACE_HASH: &str = "fca1ab84dda6c013b251270d848eb6d964a6d765";
pub const INTERFACE_VERSION: i32 = 3;
