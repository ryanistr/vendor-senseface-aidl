# Face HAL — reverse engineering notes (X6739 / OOS port)

Everything below was read out of the artifacts in this repo. Nothing is inferred
from documentation or memory. File offsets are virtual addresses in the named
binary.

## 1. What the stock stack actually ships

The X6739 vendor tree has **two** face HALs, and neither is what an Android 15
framework wants:

| Path | What it is |
|---|---|
| `vendor/bin/hw/android.hardware.biometrics.face@1.0-service.example` | 30 KB AOSP-default **HIDL** service wrapper. `DT_NEEDED` = libcutils, libhardware, libstagefright_foundation, libhidlbase, libutils, liblog, `android.hardware.biometrics.face@1.0.so`, libc++, libc, libm, libdl. Registers `IBiometricsFace/default` via `registerAsService`. |
| `vendor/lib64/hw/face.default.so` | 68 KB HIDL **module** — the real logic. `DT_NEEDED` = libcamera2ndk_vendor, libcamera_metadata, libmediandk, libnativewindow, libcutils, libutils, libui, power HALs (mtkpower 1.0/1.1/1.2, android.hardware.power 1.0/1.1/1.2, `-V1-ndk_platform`), liblog, libm, **libstfaceunlockppl.so**, libc++, libdl, libdl. |
| `vendor/lib64/libstfaceunlockppl.so` | 54 MB **SenseTime** algorithm library. Exports the `st_auth_*` / `st_face_*` C API (936 defined dynsyms). |
| `vendor/bin/hw/vendor.transsion.hardware.tranfacaidl-service` | Transsion's **own AIDL** HAL (`vendor.transsion.hardware.tranfacaidl` V1, `ITranfacAidl/default`) — a different, vendor-private interface, not `android.hardware.biometrics.face`. |
| `vendor/etc/vintf/manifest/manifest_face_default.xml` | Declares the HIDL face@1.0 HAL. |
| `vendor/media/license.lic` | 2156-byte license file. **Present** — not the missing piece. |

`face.default.so` is namespace `default_face_hal` and exports exactly these
algorithm entry points (from `.dynsym`):

```
_ZN16default_face_hal13threadSchedueEi            threadSchedue(int)
_ZN16default_face_hal19face_algorithm_initEv      face_algorithm_init()
_ZN16default_face_hal21face_algorithm_enrollEPKhiii  face_algorithm_enroll(uchar const*, int, int, int)
_ZN16default_face_hal21face_algorithm_verifyEPKhiii  face_algorithm_verify(uchar const*, int, int, int)
_ZN16default_face_hal25face_algorithm_get_faceidEv   face_algorithm_get_faceid()
_ZN16default_face_hal26face_feature_ocular_statusEv  face_feature_ocular_status()
_ZN16default_face_hal27camera_first_frame_callbackEv camera_first_frame_callback()
```

It `dlopen`s `libstfaceunlockppl.so` and resolves the `st_*` API by `dlsym`
(no `DT_NEEDED` on that library), so the algorithm is swappable at runtime.

## 2. Recovered `st_*` API and call sites

Every signature below was read from the call site in `face.default.so` and
cross-checked against the callee's own prologue in `libstfaceunlockppl.so`
(e.g. `st_auth_unlock` saves x0/w1/x2/w3 → four args; `st_face_init_license`
stores x0 → one arg).

| Symbol | Recovered signature | Evidence |
|---|---|---|
| `st_face_init_license` | `int(const char* license_data)` | 0x7d84, x0 = buffer read from `/vendor/media/license.lic` |
| `st_auth_get_version` | `const char*(void)` | 0x7d60 / 0x9a70, result used as a log `%s` |
| `st_face_open_log` | `(int level)` — called with 3 | 0x7dec, `mov w0, 3` |
| `st_auth_init` | `int(const char* group, int flag)` — called with `("mainuser", 0)` | 0xa2c4; x20 is `strlen`'d and `snprintf`'d, so it is a string. The only `mainuser` bytes in the binary are the suffix of the log string `"face_set_active_group mainuser"` at 0x4db0. |
| `st_auth_set_identifier` | `int(const char*)` — `"sensetime"` | 0xa2ec, x0 = 0x5db8 |
| `st_auth_set_face_orientation` | `int(int)` — called with 8 | 0xa310, `mov w0, 8` |
| `st_auth_set_roi` | `int(const void*)` — called with NULL and with a global | 0x9aec (global), 0xa574 (`mov x0, xzr`) |
| `st_auth_set_faceid_image_number` | `int(int)` | 0x995c |
| `st_auth_enroll` | `int(void* image, int flag=4, int* out_face_id)` | 0x7ed8, x0 = frame buffer, w1 = 4, x2 = &out |
| `st_auth_unlock` | `int(void* image, int mode, void* arg3=NULL, int arg4=0)` | 0x8228, x2 = xzr, w3 = wzr |
| `st_auth_get_template_info` | `int(void* out)` — out is `malloc(0x18)` and zeroed | 0x82f8 |
| `st_auth_delete_face` | `int(int face_id)` | 0xa16c |
| `st_auth_delete_all_face` | `int(void)` | 0x9abc |
| `st_auth_reset` | `int(void)` | 0x9a8c |
| `st_auth_deinit` | `int(void)` | 0x9820 |

Path constants (all read from `.data.rel.ro` pointers at 0x10a78–0x10a98):

| Symbol | Value |
|---|---|
| `LICENSE_PATH` | `/vendor/media/license.lic` |
| `FILE_PATH_CAMERA_ID` | `/data/vendor_de/0/facedata/cameraChoose.txt` |
| `FILE_NAME_ENROLL_YUV` | `/faceIdEnroll.yuv` |
| `FILE_NAME_FEATURE_OCULAR` | `/feature_ocular.txt` |

Log tag is `FaceHalHW`. Frame geometry is whatever the camera service returns;
the algorithm consumes the **Y plane** of a `YUV_420_888` `AImageReader` image.

Not recovered (open): the literal `default_face_hal::mMaxEnrollCount` value, and
the layout of the 24-byte `st_template_info_t`. Both are documented as
constants/opaque in `src/stface.rs`.

## 3. How the OS actually talks to the face HAL — verified in services.jar

This was checked after the fact and it changed the implementation. Method:
extract `classes*.dex` from `system/system/framework/services.jar` and
`framework.jar`, `baksmali` them, read the smali.

**`services.jar`** (`com.android.server.biometrics.sensors.face.FaceService`):

- Contains the literal string `android.hardware.biometrics.face.IFace/` — the
  service-name prefix used to look up **`android.hardware.biometrics.face.IFace/default`**.
  This is the AIDL HAL, not the HIDL one.
- `FaceService` calls `IFace.getSensorProps()` for sensor discovery.
- `FaceStartUserClient` calls `IFace.getInterfaceVersion()` inside a `try`.
- **The catch block logs `Slog.e("FaceStartUserClient", "Remote exception", e)`
  and calls `callback.onClientFinished(this, false)`** — i.e. if
  `getInterfaceVersion()` fails, the client is finished and authentication never
  starts. Silent failure, no crash.
- Also still references the legacy HIDL path
  (`android.hardware.biometrics.face@1.0::IBiometricsFace`,
  `...ClientCallback`, and `android.hardware.face.HidlFaceSensorConfig`).

**`framework.jar`** provides the Java AIDL class
`android.hardware.biometrics.face.IFace` (in `classes2.dex`), so it is loadable.
Its `IFace$Stub` transaction constants, read from the smali:
```
TRANSACTION_getSensorProps      = 0x1
TRANSACTION_createSession       = 0x2
TRANSACTION_getInterfaceHash    = 0xfffffe
TRANSACTION_getInterfaceVersion = 0xffffff
```

Two consequences that shaped this HAL:

1. **The framework expects face AIDL V3.** `framework.jar` declares the full V3
   method set (`getEnrollmentConfig`, `authenticateWithContext`,
   `enrollWithContext`, `detectInteractionWithContext`, `onContextChanged`, …),
   and `system/system/etc/vintf/compatibility_matrix.202404.xml` accepts
   `android.hardware.biometrics.face` AIDL `2-4`. V3 is what is implemented.
2. **`getInterfaceHash`/`getInterfaceVersion` live at reserved codes 0xfffffe /
   0xffffff, not as sequential methods.** The frozen V3 `.aidl` snapshot
   (`aidl/aidl_api/.../3/IFace.aidl`) omits both, so rsbinder-aidl never
   generates them and both calls would answer `UNKNOWN_TRANSACTION` — which per
   the catch block above silently kills every authentication.
   `build.rs` therefore injects handlers for those two codes into the generated
   `on_transact` dispatch. The hash returned is the frozen V3 hash
   `fca1ab84dda6c013b251270d848eb6d964a6d765` (`aidl_api/.../3/.hash`).

   ### The HAL must be DECLARED in the device manifest, not just registered

   `FaceService` does not use `ServiceManager.getService`. It builds
   `fqName = IFace.DESCRIPTOR + "/" + instance` (i.e.
   `android.hardware.biometrics.face.IFace/default`) and calls
   `ServiceManager.waitForDeclaredService(fqName)` (`FaceService.smali:390`, inside
   `lambda$new$2`). `waitForDeclaredService` only returns services **declared in
   the device's VINTF manifest**, so an undeclared HAL is never found no matter that
   it registered successfully.

   In this tree the effective device manifest is `/vendor/etc/vintf/manifest.xml` —
   the ~30 fragment files under `etc/vintf/manifest/` are **not** referenced by it
   (zero `<include>` elements; spot-checked: the `gnss@2.1-service.xml` fragment's
   HAL is absent from `manifest.xml`). So the AIDL face HAL entry has to be inlined
   into `manifest.xml` itself. `scripts/stage_vendor.sh` does that; converting only
   `manifest_face_default.xml` to AIDL leaves the HAL undeclared and undiscoverable.

## 4. Why the binary is named `...face-service.example`

The vendor's own sepolicy already contains, in
`vendor/etc/selinux/vendor_file_contexts`:

```
/(vendor|system/vendor)/bin/hw/android\.hardware\.biometrics\.face-service\.example u:object_r:hal_face_default_exec:s0
```

i.e. the stock ROM anticipated an AIDL face service binary under exactly that
name. Shipping under it needs no sepolicy change.

**Caveat found by testing:** `gen_file-contexts` (what the repack runs) is a
gap-filler — it preserves entries already present in
`erofs/config/vendor_file_contexts` and assigns every *new* file a blanket
`u:object_r:hal_allocator_default_exec:s0`. `config/vendor_file_contexts` only
has the HIDL name (`face@1.0-service.example`), not the AIDL one, so a repack
would label the new binary `hal_allocator_default_exec` and the service would be
denied. Verified by running the tool against a scratch copy. See the open
question in the report.

## 5. The `st_image_t` descriptor — layout PROVEN from the stock binary

`st_auth_enroll` / `st_auth_unlock` do **not** take a bare frame pointer. The
stock `face.default.so` builds a 24-byte descriptor on the stack and passes its
address. Two independent stock functions build it identically, which pins the
layout:

`default_face_hal::face_algorithm_enroll` (0x7e30):

```
0x7e4c  mov  w20, w3          ; arg3
0x7e50  mov  w21, w2          ; arg2
0x7e54  mov  w22, w1          ; arg1
0x7e60  mov  x23, x0          ; arg0 = frame pointer
0x7e84  mov  w19, 3           ; constant
0x7e88  str  x23, [sp, 0x10] ; desc+0x00 = frame pointer
0x7e94  stp  w19, w22, [sp, 0x18] ; desc+0x08 = 3, desc+0x0c = arg1
0x7e9c  stp  w21, w20, [sp, 0x20] ; desc+0x10 = arg2, desc+0x14 = arg3
0x7ecc  add  x0, sp, 0x10     ; x0 = &desc  <- what st_auth_enroll receives
0x7ed8  bl   st_auth_enroll
```

`default_face_hal::face_algorithm_verify` (0x8014) builds the same 24-byte
shape at `sp+0x00` (base `sp`, not `sp+0x10`):

```
0x8030  mov  w19, w3
0x8034  mov  w20, w2
0x8038  mov  w21, w1
0x8044  mov  x22, x0
0x8060  mov  w25, 3
0x806c  str  x22, [sp]        ; desc+0x00 = frame pointer
0x8074  stp  w25, w21, [sp, 8]   ; desc+0x08 = 3, desc+0x0c = arg1
0x807c  stp  w20, w19, [sp, 0x10] ; desc+0x10 = arg2, desc+0x14 = arg3
```

So the descriptor is:

| offset | size | value |
|---|---|---|
| `+0x00` | 8 | frame/data pointer (the image buffer) |
| `+0x08` | 4 | constant `3` in both stock paths |
| `+0x0c` | 4 | stock arg1 |
| `+0x10` | 4 | stock arg2 |
| `+0x14` | 4 | stock arg3 |

The library agrees on the shape. `libstfaceunlockppl.so` reads the same offsets:
`+0x00` is loaded and null-checked (`ldr x0,[x21]; cbz`), and `+0x0c`, `+0x10`,
`+0x14` are read as three separate `int`s (`ldr w1,[x20,#0xc]`,
`ldr w2,[x20,#0x10]`, `ldr w3,[x20,#0x14]`) at 0xb1878-0xb1880, where `x20` is
the descriptor. The consumer at 0x158e34 reads `+0x0c` as a column count
(`cmp w3,w5` in the inner loop), `+0x10` as a row count (`cmp w20,w2` outer
loop) and `+0x14` as a per-row byte stride (`mul w5,w20,w4`), i.e.
`+0x0c`=width, `+0x10`=height, `+0x14`=stride. The same `+0x00` pointer is
the pixel buffer.

`+0x08` is never read by any consumer found in `libstfaceunlockppl.so` — it is
written as the constant `3` by both stock paths and appears to be a
channel/format tag the library ignores on these paths.

**Consequence for the port:** `src/stface.rs` must pass `&descriptor`, not the
raw Y-plane pointer. Passing the plane pointer directly makes the library read
pixel bytes as if they were the descriptor — the crash this port has to fix.

## 6. The `st_image_t` descriptor — layout PROVEN, and the frame is NV21

Section 5 pinned the 24-byte descriptor. The **values** are pinned here too, and
they contradict what the port originally assumed: the algorithm is fed an
**NV21** buffer at **640x480**, not the camera's Y plane.

The stock caller of `face_algorithm_enroll` is in `face.default.so` at 0xadc4:

```
0xadb4  ldr  x0, [sp, #0x8]     ; the frame buffer
0xadb8  mov  w1, #0x280          ; 640
0xadbc  mov  w2, #0x1e0          ; 480
0xadc0  mov  w3, #0x280          ; 640
0xadc4  bl   face_algorithm_enroll
```

`face_algorithm_enroll` is `default_face_hal::face_algorithm_enroll(uchar
const*, int, int, int)` — the three ints are **width, height, stride**. The
stock passes them as 640, 480, 640, and inside the function they land in the
descriptor at `+0x0c`, `+0x10`, `+0x14`.

So the descriptor is:

| offset | type | value in the stock |
|---|---|---|
| `+0x00` | pointer | frame buffer (NV21) |
| `+0x08` | int | `3` |
| `+0x0c` | int | width (640) |
| `+0x10` | int | height (480) |
| `+0x14` | int | stride (640) |

**`+0x08` is the pixel format, and `3` means NV21.** The library's own
format-name table sits at 0x595840 in `libstfaceunlockppl.so`
(`yuv420p`, `nv12`, `nv21`, `bgra`, `rgb`, `rgba`) and the lookup function at
0x1d4568 maps them to integers with the per-name branch targets reading
`2` (yuv420p), `3` (nv12), `5` (gray), `4` (bgra), `7` (rgb). The stock writes
the literal `3`, so the format it declares is **NV21** — the same format the
library's `nv21 -> rgba` converter consumes.

The stock builds that NV21 buffer itself, in
`default_face_hal::camera_first_frame_callback` (0x8634-0x86bc): it reads all
three planes of the camera's `YUV_420_888` image, copies the Y plane, and then
interleaves the V and U planes into one contiguous buffer. The camera is opened
with `AImageReader_newWithUsage(640, 480, 0x23 /* YUV_420_888 */, 3, 1)`
(0xbc74-0xbc8c) and the callback rejects any frame that is not 640x480
(0x85c8-0x85d4).

### What this changes in the port

1. `src/camera.rs` must convert `YUV_420_888` to NV21 (Y plane, then
   interleaved V,U) and capture at **640x480**, not 480x640.
2. `src/stface.rs` must hand the library `&descriptor`, where the descriptor is
   `{ data, 3, width, height, stride }`.

### Proof on the device

`src/selftest.rs` (a device-side binary, not shipped) calls the real
`st_auth_enroll` with both variants:

```
selftest: descriptor data=0x... tag=3 width=640 height=480 stride=640
selftest: enroll returned ret=1 (expected: no face)   <- descriptor
selftest: RAW variant — passing the pixel pointer where the descriptor belongs
selftest: RAW enroll returned ret=-1                  <- raw pointer
```

The same call returns `1` with the descriptor and `-1` with the raw pointer:
the library only reads the frame when it is handed a descriptor.


## 7. The camera's chroma planes are already interleaved — the NV21 build is a pixel-stride walk

The stock's interleave loop (0x8674-0x86bc) is **not** a per-row planar copy.
It walks the chroma planes as a flat array with the plane's *pixel* stride:

```
0x8680  mov  x8, xzr            ; i = 0
0x8684  ldp  x9, x11, [sp, #0x18] ; x9 = V plane, x11 = U plane
0x868c  lsr  w12, w10, #1       ; sample count = u_len / 2
0x8690  ldrsw x10, [sp, #0x38]  ; x10 = U plane *pixel* stride
0x869c  add  x13, x19, x13      ; chroma destination = frame + y_len
0x86a0  ldrb w14, [x9, x8]      ; V byte at byte offset i*pixel_stride
0x86a8  strb w14, [x15, #0x120] ; store V
0x86ac  ldrb w14, [x11, x8]     ; U byte at the same offset
0x86b0  add  x8, x8, x10        ; i += pixel_stride
0x86b8  strb w14, [x15, #0x121] ; store U
```

Measured on the device, the camera's planes are:

```
y_stride=640 y_pixel=1   u_stride=640 u_pixel=2   v_stride=640 v_pixel=2
u=0x7083be1001  v=0x7083be1000
```

The U plane pointer is **one byte above** the V plane pointer and both have
pixel stride 2: the camera's buffer is already interleaved V,U,V,U... The
stock's loop therefore reproduces the source chroma verbatim.

The port's original per-row planar copy read `u_stride` bytes per row at
`cw = width/2` columns, i.e. it read the wrong bytes entirely.

## 8. `st_face_open_log(3)` — the library's own log

The stock calls `st_face_open_log(3)` right after `st_face_init_license`
(0x7dec). Without it the library is completely silent and a failing
enrollment gives no reason at all. With it, the library logs under the tag
`STFACEUNLOCK` and the port can see the exact rejection reason.

## 9. Enrollment setup calls the port was missing

`default_face_hal::set_active_group` (0xa1dc-0xa338) does, in order:

1. `st_auth_init(store_path, 0)`
2. `st_auth_set_identifier("mainuser")`
3. `st_auth_set_face_orientation(8)`

and the enroll setup (`face.default.so` 0x98ac-0x995c) does:

4. `mEnrollIndex = 5`, then `st_auth_set_faceid_image_number(5)`
5. `st_auth_reset()`
6. `st_auth_delete_all_face()`
7. `st_auth_set_roi(&face_rect)`

`face_rect` lives in `face.default.so`'s `.data` at 0x10a98 and is four
little-endian `int32`: **96, 28, 544, 452**.

`st_auth_delete_all_face` returns **-1004** when nothing is enrolled yet; that
is the normal state on a fresh enrollment and must not abort the setup.

## 10. The ROI gate: "DETECT ROI NOT IN"

With the library log enabled, the rejection is explicit:

```
FaceDetectionClass.cpp(121): image face orientation is 8
FaceUnlock2DInterFace.cpp(474): current enroll face width*height: 142*132
FaceUnlock2DInterFace.cpp(497): detect face orientation is:1
FaceUnlock2DInterFace.cpp: DETECT ROI NOT IN [403 196 545 327] VS [96 28 544 452]
```

The library compares the *detected* face rectangle against the configured ROI
and rejects the frame when the face falls outside it. The stock's ROI of
(96, 28, 544, 452) is in the camera's own frame orientation, and the detected
rect is reported in the algorithm's orientation-8 space, so the two are not
directly comparable for a portrait image that is not pre-rotated.

Setting the ROI to the full frame (0, 0, 640, 480) removes the rejection.

## 11. Proof that the descriptor and the NV21 buffer are correct

With a real face image fed through the port's own conversion, the library
detects, aligns and poses it:

```
FaceDetection.cpp(243): detect face count before filter:1
FaceDetection.cpp(289): detect rect:[18 145 118 252]
FaceAlign.cpp(295): rectCount is 1 ,realRectCount is 1 align_threhold is 1.000000
FaceCalcPose.cpp(197): [pitch:-2.546045 yaw:0.160323 roll:0.287024]
FaceoOcular.cpp(213): left_valid_score:9.976554 right_valid_score:9.973040
```

That exercises the descriptor layout, the format tag, the width/height/stride
fields and the chroma interleave in one shot: a wrong descriptor or a wrong
NV21 build produces "NO FACE" and nothing else.

## 12. The stock enrolls from a *file*, not from the camera

The stock's per-frame enroll driver (0xacd4-0xadc4) does **not** pass the
camera frame to the algorithm:

```
0xacd4  adrp x20, 0xf000
0xacd8  ldr  x20, [x20, #0x540]   ; mEnrollPath
0xacdc  ldr  x1, [x20]            ; the path string
0xacf8  bl   0x7c48               ; read_file(path, &buf, &size)
0xacfc  ldr  w8, [sp, #0x4]
0xad00  cmp  w8, #0x63             ; size must exceed 99
0xad04  b.gt  0xadb4
0xadb4  ldr  x0, [sp, #0x8]        ; the file contents are the frame
0xadb8  mov  w1, #0x280            ; 640
0xadbc  mov  w2, #0x1e0            ; 480
0xadc0  mov  w3, #0x280            ; 640
0xadc4  bl   face_algorithm_enroll
```

`mEnrollPath` is built in `set_active_group` (0xa348-0xa378) as
`store_path + "/faceIdEnroll.yuv"`. So the stock HAL's own model is: the
camera writes each frame to `<store>/faceIdEnroll.yuv`, and the enroll
algorithm reads that file back.

The reference bridge (RapliVx/OPlus-Face-Bridge-Transsion, `src/hal.rs`
`yuv_feeder_worker`) does exactly this: it writes every camera frame to
`/data/vendor_de/0/facedata/faceIdEnroll.yuv` through a `.tmp` file and an
atomic rename, and the vendor HAL picks the frame up from there.

The port feeds the frame directly to `st_auth_enroll` instead, which is
equivalent for the algorithm and avoids the file round-trip.


## 13. The library's result codes, and the enrollment contract

The stock HAL's `face_algorithm_enroll` (0x7e30-0x8010) calls `st_auth_enroll`
and then maps the library's return code through a 129-entry table at
`face.default.so` 0x49f0:

```
0x7f04  mov  w8, w20              ; the library's return code
0x7f14  ldrb w11, [x9, x8]        ; table[code]
0x7f18  add  x10, x10, x11, lsl #2
0x7f1c  br   x10                  ; jump to the handler
```

Decoded from the table:

| library code | stock returns | meaning |
|---|---|---|
| 0 | 1008 (0x3f0) | **success**, the new face id is written out |
| 5 | 0 | still collecting samples, keep scanning |
| 1, 3, 4, 6-81, 102 | 1 | the frame was rejected |
| 2 | 22 | |
| 103, 104 | 19 | |
| 105 | 1002 | |
| 113-117 | 6, 7, 9, 8, 21 | |
| 120, 121 | 5, 4 | |
| 124, 125 | 2, 10 | |
| 129 | 23 | |

So **5 is the "sample accepted, more needed" code and 0 is the completion
code**. The port's `stface::enroll` therefore returns
`EnrollOutcome::Accepted` for 5 and `EnrollOutcome::Complete(face_id)` for 0;
everything else is an error.

The library's own log confirms the counting:

```
FaceUnlock2DInterFace.cpp(894): total enroll face is 1, the config is:5
FaceUnlock2DInterFace.cpp(894): total enroll face is 2, the config is:5
...
FaceUnlock2DInterFace.cpp(676): enroll person compare score:0.999869
```

The "config is:5" is `st_auth_set_faceid_image_number(5)`, which the stock
sets in its enroll setup (`face.default.so` 0x98ac-0x995c).

## 14. The library's store path and permissions

`st_auth_init(store_path, 0)` resolves `<store_path>/.algo` for the model
files and writes `template_file.data` next to it. The stock's store path is
`/data/vendor_de/0/facedata` (the reference bridge passes the same). The
directory is `drwx------ system:system` with the SELinux type
`face_vendor_data_file`, so **the HAL must run as the `system` user** — the
stock's init rc does `user system` / `group system`. A process running as root
cannot write there (SELinux denies it), which is why the enrollment template
only appeared once the probe was run as `system`.

## 15. The preview token, end to end

The framework mints the preview token in `system_server`:
`FaceEnrollClient.obtainSurfaceHandlesIfNeeded` calls
`FaceService.acquireSurfaceHandle(Surface)` (a native method in
`libandroid_servers.so`) and then `AidlNativeHandleUtils.dup()`s the result
into the handle that goes to the HAL. The token bytes are the HGBP producer
token: `ints[0]` is the byte length and `ints[1..]` the bytes.

Measured on the device, the framework hands the HAL a **40-byte token in 11
ints**:

```
FaceHalHW: session: enroll type=0 features=1 preview=fd=0
          ints=[40, 9, 0, 906153097, -1660070750, ...]
```

The port decodes it with `retrieveHalInterface` (from
`android.hidl.token@1.0-utils.so`) and then `castFrom` on the bufferqueue
producer. Both succeed on this ROM:

```
FaceHalHW: preview: retrieveHalInterface -> 0xb400007e131ca620
FaceHalHW: preview: castFrom -> 0xb400007e131ca7e0
```

so the token format and the HIDL vector layout are right.

The reference implementation (RapliVx/OPlus-Face-Bridge-Transsion)
`src/surface_bridge.cpp` tries **V2_0 first** and falls back to V1_0, and for
the V1_0 case patches `libbufferqueueconverter.so`'s GOT at
`base + 0x2240` (the V2_0 producer's typeinfo) and `base + 0x2248` (its
vtable) with the V1_0 producer's, so that the converter's
`getSurfaceFromHGBP` understands the token's producer version. The port does
the same, and takes the converter's load base from `dladdr` on a symbol the
converter exports, as the reference does.

## 16. The AIDL enrolment callback contract

`ISessionCallback` has no `onEnrolled`; the completion signal is
`onEnrollmentProgress(enrollmentId, remaining)` with `remaining == 0`. The
port calls it with the new face id and 0 when the library returns 0, and with
the library's remaining count while it is still collecting.

## 17. `ANativeWindow_lock` returning -38 means "already locked", not "timed out"

The HAL's preview path does `ANativeWindow_lock` -> write pixels ->
`ANativeWindow_unlockAndPost` on an `ANativeWindow` obtained from a
`GraphicBufferProducer` token, i.e. the HAL is the **producer**.

In the device's `/system/lib64/libgui.so` (host copy
`RE/faceinv/libgui_sys.so`), `android::Surface::lock(ANativeWindow_Buffer*, ARect*)`
is at **0xbdd40**. Its failure paths:

- **0xbe224** — logs `"Surface::lock failed, already locked"` (string @0x5cdde) and
  returns **-38** (`mov w22, #-0x26`). This is the *"a buffer from a previous frame is
  still locked"* path. libgui does **not** post anything here.
- **0xbe280** — the producer/dequeue failure: logs `"dequeueBuffer failed (%s)"`
  (string @0x66684), stores the producer's errno in `w22` and returns it.
- **0xbe2cc** — another failure path returning -38 with its own log.

`android::Surface::unlockAndPost()` is at **0x12bf80**:

- **0x12c060** — logs `"Surface::unlockAndPost failed, no locked buffer"`
  (string @0x6ab2b) and returns **-38**.
- **0x12c0bc** — the queueBuffer failure path (string @0x6ab3f).

Consequence: `src/surface_bridge.rs`'s recovery of calling `unlockAndPost` when
`ANativeWindow_lock` returns -38 is **not** libgui's behaviour, and posting a buffer
the HAL did not lock can desynchronise the producer/consumer slot state so that
every later `lock` keeps returning -38 — a permanent freeze.

## 18. `ANativeWindow_setDequeueTimeout` is a consumer-side call

`NATIVE_WINDOW_SET_DEQUEUE_TIMEOUT = 37` (`frameworks/native/libs/nativewindow/
include/system/window.h:249`), dispatched by `ANativeWindow_setDequeueTimeout`
(`libnativewindow.so` @0xa7c0, `mov w1, #0x25`). It is handled by the consumer
side, `android::Surface::hook_perform` (libgui @0x125c60), whose dispatch is a
bitmask test against opcode 0..0x2f.

The producer-side timeout lives in `BufferQueueProducer::setDequeueTimeout`
(libgui **@0xf1140**), reached from `BufferQueueProducer::dequeueBuffer`
(@0xcd0c0, which holds the strings `"waitForFreeSlotThenRelock TIMED_OUT %ld,
custom timeout %ld ..."` and `"dequeueTime%s%ld"`).

So the HAL calling `ANativeWindow_setDequeueTimeout(50ms)` does **not** bound the
producer's blocking `dequeueBuffer`. The producer idiom is `Surface::dequeueBuffer`
(@0xbfaf0) / `Surface::queueBuffer` (@0xbe630), not the CPU
`ANativeWindow_lock`/`unlockAndPost` pair.

## 19. `acquireSurfaceHandle` is not an exported symbol

`nm -D RE/faceinv/libandroid_servers.so` lists 644 dynamic symbols and **no**
`acquireSurfaceHandle`. The strings are present, so the native method is
registered by name (JNI `RegisterNatives`). It is declared
`public static native NativeHandle acquireSurfaceHandle(Surface surface)` at
`FaceService.java:80` and is **not callable** from an app or from the HAL.

## 20. Window format constants

`android/hardware_buffer.h`: `AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM = 1`,
`AHARDWAREBUFFER_FORMAT_R8G8B8X8_UNORM = 2`,
`AHARDWAREBUFFER_FORMAT_R5G6B5_UNORM = 4`. Hence
`WINDOW_FORMAT_RGBA_8888 = 1`, `WINDOW_FORMAT_RGBX_8888 = 2`,
`WINDOW_FORMAT_RGB_565 = 4`. The HAL requests format 1 and branches only on
`buffer.format == 4`, so a consumer that grants BGRA/RGBX gets the wrong byte
order (cosmetic, not the freeze).
