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
// The ROM's framework asks the HAL for its interface version and uses it to pick
// the session call shapes: version >= 2 selects enrollWithContext, version >= 4
// selects enrollWithOptions. Both of those need the ROM's extended
// OperationContext / FaceEnrollOptions parcelables, which carry OPlus-specific
// nested types that are not in this tree's AIDL snapshot. Version 1 keeps the
// framework on the plain enroll(hat, byte, byte[], preview) / authenticate(long)
// calls, whose layouts do match.
pub const INTERFACE_VERSION: i32 = 1;

/// AIDL reserved transaction codes for `@VintfStability` interfaces.
///
/// These are NOT sequential method codes. The framework's own
/// `android.hardware.biometrics.face.IFace$Stub` declares
/// `TRANSACTION_getInterfaceHash = 0xfffffe` and
/// `TRANSACTION_getInterfaceVersion = 0xffffff` (read from the smali of
/// framework.jar in this ROM), while the frozen V3 `.aidl` snapshot omits both
/// methods entirely. rsbinder-aidl therefore never generates them, and the calls
/// would answer `UNKNOWN_TRANSACTION` — which `FaceStartUserClient` treats as a
/// remote exception and finishes the client without ever authenticating.
/// `build.rs` injects handlers for these two codes into the generated dispatch.
pub const GET_INTERFACE_HASH_TRANSACTION: u32 = 0xfffffe;
pub const GET_INTERFACE_VERSION_TRANSACTION: u32 = 0xffffff;
