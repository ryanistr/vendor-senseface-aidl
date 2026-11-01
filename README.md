# FaceHAL — AIDL face HAL for the X6739 OOS port

Rust (rsbinder) implementation of `android.hardware.biometrics.face.IFace/default`
— the AIDL face HAL the OOS (Android 15) framework binds to. It drives the front
camera and the same SenseTime algorithm library the stock HIDL HAL used
(`libstfaceunlockppl.so`), so match results come from the proven stack rather
than a reimplementation.

Reverse engineering notes, including the `st_auth_*` signatures and the
services.jar verification that shaped the interface: `re/notes/face-hal-re.md`.

This directory is its own git repository. Its history was split out of the
`oos` ROM worktree repo (`git subtree split --prefix=ReconstructedHALSource/FaceHAL`),
so every commit below is reachable from the ROM repo's history too.

## Build

```bash
scripts/build.sh            # NDK r27, aarch64-linux-android, API 30
```

Produces `target/aarch64-linux-android/release/face-hal-service` and verifies
every `DT_NEEDED` resolves in the ROM tree before reporting success. The crate
is named `face-hal-service` because a Rust bin name cannot contain `.`; the
staging script installs it under the real name.

`target/` is build output and is not tracked.

## Stage into the vendor tree

```bash
scripts/stage_vendor.sh     # writes erofs/vendor only — never repacks, never flashes
```

Installs:

- `vendor/bin/hw/android.hardware.biometrics.face-service.example` — named to
  match the vendor sepolicy's existing `hal_face_default_exec` rule
- `vendor/etc/init/android.hardware.biometrics.face-service.rc` — `class
  late_start` (mirrors the stock HIDL rc's /data race workaround) +
  `interface aidl android.hardware.biometrics.face.IFace/default`
- an AIDL face V3 entry in `vendor/etc/vintf/manifest.xml`

The script fails if the rc path/interface or the manifest entry disagree with
what the binary registers.

## Run on a device without repacking

The HAL can be A/B'd straight from `/data/local/tmp`, because init cannot exec a
vendor HAL whose backing store is `/data` (SELinux denies the transition):

```bash
adb shell su -c 'stop vendor.face-hal-aidl-default'
adb push target/aarch64-linux-android/release/face-hal-service /data/local/tmp/facework/facehal_new
adb shell su -c 'chmod 755 /data/local/tmp/facework/facehal_new'
adb shell su -c 'su system -c "/data/local/tmp/facework/facehal_new" >/dev/null 2>&1 &'
```

Check exactly one instance is alive afterwards (`pidof`, not `ps | grep` — the
process name is truncated to 15 chars), and that `md5sum` on the device matches
the local binary. Restart the stock service when the test is over.

Logs go to logcat under the tag **`FaceHalHW`**; `persist.sys.facehal.verbose=1`
enables them (it is on by default). `logcat -G 32M` before a run — the default
buffer wraps away the SenseTime progress lines.

## Layout

```
src/main.rs      service registration
src/face.rs      IFace — getSensorProps, createSession
src/session.rs   ISession — enroll / authenticate / remove / lockout
src/camera.rs    NDK Camera2 front-camera capture
src/surface_bridge.rs  preview surface bridge (HGBP token -> ANativeWindow)
src/stface.rs    FFI to libstfaceunlockppl.so (recovered st_auth_* API)
src/logging.rs   logcat under the stock tag "FaceHalHW", gated on
                 persist.sys.facehal.verbose=1
src/bindings.rs  rsbinder-aidl output + interface identity constants
aidl/            face AIDL V3 sources and frozen aidl_api snapshots
re/notes/        durable RE findings
re/disasm/       disassembly of the stock algorithm library
re/runs/         before/after snapshots from specific debugging sessions
```

`libgui_sys.so` and `bqc.so` in this directory are **pulled copies of the
device's own `libgui.so` and `libbufferqueueconverter.so`**, kept as the
disassembly input for `re/`. They are ROM blobs, not source.

## Design decisions worth knowing

- **Sensor strength is CONVENIENCE, not STRONG.** RGB camera, no TEE-backed
  template storage, so the HAL must not hand the framework a real
  HardwareAuthToken; `getAuthenticatorId` returns 0.
- **`getInterfaceHash` / `getInterfaceVersion` are injected by `build.rs`.** The
  framework answers them at reserved codes `0xfffffe` / `0xffffff`, which the
  frozen `.aidl` does not declare, so rsbinder-aidl never generates them. Without
  them `FaceStartUserClient` swallows an `UNKNOWN_TRANSACTION`, logs "Remote
  exception" and finishes the client — face auth would never start.
- **The algorithm library is process-global and not thread-safe**, so every
  entry point in `stface.rs` serialises on one mutex.
- Only `libstfaceunlockppl.so` and `libcamera2ndk_vendor.so` (both in
  `/vendor/lib64`) plus `libmediandk`/`libnativewindow`/`liblog` (llndk) are
  linked. No platform library outside the vendor namespace is referenced.
- **The enrollment preview is rendered by the HAL itself.** The framework hands
  the HAL a `GraphicBufferProducer` token for the app's preview surface, and the
  HAL fills it from the camera frames. The preview is mirrored (selfie-style);
  only the display blit is flipped, the algorithm still sees the un-mirrored
  sensor frame.

## The preview freeze (fixed)

The enrollment preview used to show a live camera image for ~2 s and then freeze
on a stale frame.

Cause: the `-38` recovery path in `render_nv21`. `-38` is `INVALID_OPERATION`,
and libgui returns it from **two** different places — `Surface::lock`'s
"already locked" (`Surface.cpp:2717`) and `BufferQueueProducer`'s "attempting to
exceed the max dequeued buffer count" (`BufferQueueProducer.cpp:378`). The old
code called `ANativeWindow_unlockAndPost` on every `-38`, which on the second
flavour queues a buffer that was never written and desynchronises the producer's
slot accounting, so every later dequeue fails the same way.

Fix: the HAL tracks whether it actually holds a lock and only posts in that
case, and it logs **every** lock failure (code, elapsed ms, held-lock state,
post result) rather than only the first five, so a stall and an error are
distinguishable in the log.

Verified on device: continuous frames with zero lock failures, and the camera
is fully released when enrollment ends (`dumpsys media.camera` shows no active
clients, no `/dev/video*` fd is held, and the HAL does not reopen the camera on
its own).

Note for anyone re-deriving this: `ANativeWindow_setDequeueTimeout` is **not**
a consumer-only call — `Surface::setDequeueTimeout` forwards straight to
`mGraphicBufferProducer` (`Surface.cpp:218-220`), so the 50 ms timeout does
bound the producer's `dequeueBuffer`. AOSP sources for this tree live at
`/mnt/project/Misc/aosp_headers/api36/frameworks/native/libs/gui/`; use them
instead of re-disassembling libgui.

## Not verified

The AIDL HAL plus its RRO overlay have not been repacked and flashed — the
device runs the build pushed to `/data/local/tmp`. `TODO.md` in the ROM worktree
lists the repack/flash verification as a separate, explicitly ordered step.
