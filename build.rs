// FaceHAL build.rs
//
// Generates the face AIDL bindings with rsbinder-aidl and points the linker at
// the ROM's own libraries so DT_NEEDED only ever names something the device can
// resolve. Every path here comes from the oos worktree; nothing is invented.
use std::path::PathBuf;
use std::env;

fn main() {
    let aidl_dir = PathBuf::from("aidl");

    // The face interface pulls in biometrics.common, android.hardware.common and
    // android.hardware.keymaster, so all four packages must be searchable.
    let iface = aidl_dir.join("android/hardware/biometrics/face/IFace.aidl");
    let isession = aidl_dir.join("android/hardware/biometrics/face/ISession.aidl");
    let icb = aidl_dir.join("android/hardware/biometrics/face/ISessionCallback.aidl");
    let icancel = aidl_dir.join("android/hardware/biometrics/common/ICancellationSignal.aidl");

    for p in [&iface, &isession, &icb, &icancel] {
        println!("cargo:rerun-if-changed={}", p.display());
        assert!(p.exists(), "missing AIDL source: {}", p.display());
    }

    let out_dir = env::var("OUT_DIR").unwrap();
    let out_file = PathBuf::from(&out_dir).join("bindings.rs");

    rsbinder_aidl::Builder::new()
        .source(iface)
        .source(isession)
        .source(icb)
        .source(icancel)
        .include_dir(&aidl_dir)
        .output(&out_file)
        .generate()
        .expect("failed to generate face AIDL bindings");

    // rsbinder's generated traits are async in 0.6; the service impls use
    // #[async_trait], so the generated traits need the attribute too.
    let mut content = std::fs::read_to_string(&out_file).unwrap();
    for t in [
        "IFaceAsyncService",
        "ISessionAsyncService",
        "ISessionCallbackAsyncService",
        "ICancellationSignalAsyncService",
    ] {
        let target = format!("pub trait {}: rsbinder::Interface", t);
        let repl = format!("#[async_trait::async_trait]\npub trait {}: rsbinder::Interface", t);
        content = content.replace(&target, &repl);
    }
    std::fs::write(&out_file, content).unwrap();

    // ---- link search paths (ROM-provided libraries only) ----
    let root = env::var("ROM_TREE")
        .unwrap_or_else(|_| "/mnt/project/RomTools/ROM_WORKDIR/oos/erofs".to_string());
    let vendor64 = format!("{}/vendor/lib64", root);
    let system64 = format!("{}/system/system/lib64", root);

    for dir in [&vendor64, &system64] {
        println!("cargo:rustc-link-search=native={}", dir);
    }

    // /vendor/lib64: shipped with the ROM, resolvable from a vendor process.
    println!("cargo:rustc-link-lib=dylib=stfaceunlockppl");
    println!("cargo:rustc-link-lib=dylib=camera2ndk_vendor");
    // llndk members (system/etc/llndk.libraries.txt): reachable from vendor.
    println!("cargo:rustc-link-lib=dylib=mediandk");
    println!("cargo:rustc-link-lib=dylib=nativewindow");
    println!("cargo:rustc-link-lib=dylib=log");
}
