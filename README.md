# FaceHAL

## What it is

A face recognition HAL for Android. It implements `android.hardware.biometrics.face.IFace/default`
over AIDL, the interface the Android framework binds to. The HAL drives the phone's front
camera and hands the frames to a SenseTime algorithm library (`libstfaceunlockppl.so`) that
ships in the phone's vendor partition.

## What it is used for

Two operations, both driven by the framework:

- Enroll: the user registers a face. The app shows a live camera preview, and the HAL
  sends sampled frames to the algorithm until it accepts enough of them.
- Authenticate: the phone checks a face against the enrolled one to unlock.

The HAL also renders the enrollment preview. The framework passes it a
`GraphicBufferProducer` token for the app's preview surface, and the HAL fills that
surface with the camera frames itself.

## Language

Rust, edition 2021. 3,470 lines across 9 files in `src/`.

| Crate | Version | License |
|---|---|---|
| `rsbinder` | 0.6.0 | Apache-2.0 |
| `rsbinder-aidl` (build dependency) | 0.6.0 | Apache-2.0 |
| `async-trait` | 0.1 | MIT OR Apache-2.0 |
| `libc` | 0.2 | MIT OR Apache-2.0 |
| `log` | 0.4 | MIT OR Apache-2.0 |

`rsbinder` provides the Binder IPC and the AIDL code generation. `Cargo.lock` pins the
full transitive dependency set.

## How it works

The framework talks to the HAL over Binder. The HAL talks to the camera over the NDK
Camera2 API and to the algorithm over a plain C ABI.

```
  Settings app
      |  FaceManager.enroll(surface, ...)
      v
  system_server  (FaceService)
      |  FaceService.acquireSurfaceHandle(surface)  ->  NativeHandle
      |  ISession.enroll(handle)
      v
  FaceHAL  (this repo)
      |
      +--> surface_bridge.rs
      |      NativeHandle -> HIDL token (11 ints, 40 bytes)
      |      -> IGraphicBufferProducer -> ANativeWindow
      |      -> lock / write pixels / unlockAndPost
      |      -> the app's preview surface
      |
      +--> camera.rs
      |      AImageReader, 640x480 YUV_420_888
      |      -> NV21
      |         |
      |         +--> surface_bridge.rs   (preview pixels)
      |         |
      |         +--> stface.rs
      |                st_auth_enroll / st_auth_unlock
      |
      +--> ISessionCallback
             onEnrollmentProgress / onEnrollmentError
```

Enrollment samples five frames by default, settable with the
`persist.sys.facehal.enroll_frames` property. For each one the algorithm returns a result
code: 0 means the face is enrolled, 5 means the sample was accepted, any other positive
value means the sample was rejected and the HAL asks for another.

The preview is mirrored, selfie style. Only the display blit is flipped, so the algorithm
still receives the un-mirrored sensor frame.

## Build

```bash
scripts/build.sh
```

Needs the Android NDK r27. Produces `target/aarch64-linux-android/release/face-hal-service`.
The script checks that every library the binary needs resolves in the ROM tree, and fails
if one does not.

`target/` is build output and is not tracked by git.

## Use

### Stage into the ROM tree

```bash
scripts/stage_vendor.sh
```

Writes into `erofs/vendor/` only. It never repacks an image and never flashes anything.
The script fails if the init rc, the interface name or the VINTF entry disagree with what
the binary registers.

### Test on a device without repacking

init cannot run a HAL whose binary lives in `/data`, so the replacement has to be started
by hand with the stock service stopped:

```bash
adb shell su -c 'stop vendor.face-hal-aidl-default'
adb push target/aarch64-linux-android/release/face-hal-service /data/local/tmp/facework/facehal_new
adb shell su -c 'chmod 755 /data/local/tmp/facework/facehal_new'
adb shell su -c 'su system -c "/data/local/tmp/facework/facehal_new" >/dev/null 2>&1 &'
```

Check that exactly one instance is alive with `pidof`, because the process name is
truncated to 15 characters, and compare the md5 on the device with the local file.
Restart the stock service when the test is over.

Logs go to logcat under the tag `FaceHalHW`. The property `persist.sys.facehal.verbose=1`
turns them on. Run `logcat -G 32M` first, because the default buffer wraps them away.

### Drive an enrollment

```bash
adb shell am start -a android.settings.FACE_ENROLL
```

The framework puts a credential prompt in front of the enrollment page, so the phone has
to be unlocked first.

## Credits

SenseTime wrote the face algorithm, `libstfaceunlockppl.so`, and its license file sits in
`vendor/media/license.lic`. The HAL calls that library rather than reimplementing
matching, which is why the results match the stock behaviour.

The AOSP framework provided the AIDL face interface and the `IFace`/`ISession` contract.
The `.aidl` sources and their frozen snapshots are in `aidl/`.

rsbinder, by hiking90, provides Binder IPC and AIDL code generation in Rust.

MediaTek provided the camera NDK library `libcamera2ndk_vendor.so` in the vendor partition.

The AOSP framework's `libgui` provided `Surface`, `BufferQueue` and the bufferqueue
converter. The HAL reaches the preview surface through `libbufferqueueconverter.so` and
patches it at runtime for the Android 15 vtable layout. The disassembly that made this
possible is in `re/`.

The `libgui_sys.so` and `bqc.so` files in this directory are copies of the device's own
libraries, kept as the input for that disassembly.

## Legal

This repository's own source, meaning `src/`, `scripts/`, `build.rs`, `Cargo.toml` and
`re/`, is licensed under the Apache License 2.0. The full text is in `LICENSE`.

`aidl/` contains AOSP interface definitions, also Apache-2.0.

`libgui_sys.so` and `bqc.so` are copies of binaries from the phone's own system and vendor
partitions. They are not part of this project's source, they are there so the disassembly
in `re/` can be reproduced. They carry whatever license their original vendor shipped them
under.

The algorithm library `libstfaceunlockppl.so` is not in this repository. The HAL loads it
from the phone's vendor partition at runtime, and its license is in
`vendor/media/license.lic` in the ROM tree.

`re/disasm/` holds disassembly of the vendor's own HIDL module, `face.default.so`, taken
from the ROM's vendor partition. It is analysis material for the API recovery, not
source, and it is not covered by this repository's Apache-2.0 grant.

Enrolling a face stores biometric data in the phone's protected storage. The HAL reports
itself as CONVENIENCE strength, not STRONG, because it has no TEE-backed storage for
templates and hands the framework no hardware auth token.

## Not verified

The AIDL HAL and its overlay have not been repacked and flashed. What ran on the device was
the binary pushed to `/data/local/tmp`. The repack and flash are a separate, explicitly
ordered step.
