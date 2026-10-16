# The camera: USB `30c9:00f3` (Lenovo "Integrated Camera", Realtek, ThinkPad P16 Gen 3)

What the device offers at the USB level, what the Windows driver does with it, and what was tried
to get an ambient (emitter-off) image out of the IR function. Measured 2026-10-16 on the laptop
(`uvcvideo` 7.1.0, kernel 7.1.0-13554); descriptors from `lsusb -v -d 30c9:00f3`; Windows side from
the Lenovo camera driver package `n4mer09w` (Realtek 10.0.22000.20406: `RtLeJFIR.inf`, `RtLeJF.inf`,
`RsDMFTIR64.dll`) and the sensor adapter notes in [windows-hello-re.md](windows-hello-re.md).
Evidence tags as there: **OBSERVED** / **INFERRED** / **UNKNOWN**.

## The question, and the answer

Can this camera deliver a real ambient frame? Today every emitter-off frame is black (mean ≤ 0.04,
no pixel above 6 in any recording), so linux_hello has no ambient image: no sunlight gate (F14)
and no difference image for the diffusion net (F42, [capture-and-liveness.md](capture-and-liveness.md) §4).

- The emitter-off frames are **real sensor readouts, not frames blanked by the firmware or by a
  driver**: they carry 2–15 noise pixels of value 1–5 per frame (a zeroed buffer would carry none),
  more (1300–1900) when the auto-exposure sat higher in an earlier recording, and with the strobe
  switched off (experiment E1 below) the noise grows frame by frame as the auto-exposure raises the
  gain on the dark scene, in the lens-shading pattern (dark centre, noisy border) of a sensor
  looking at nothing. Nothing on the Windows path blanks them either (§4): the Realtek DMFT
  passes the IR samples through untouched and `usbvideo.sys` does not touch pixels.
- They are black because the scene has no light in the sensor's band. **INFERRED**: the sensor is
  filtered to its own near-infrared emitter (presumably 940 nm; the wavelength is not in the
  descriptors) and the room during these recordings gave nothing in that band (the RGB function
  reported a 50 ms exposure at 20 fps in its `CaptureStats` minutes earlier, so the room was dim in
  visible light too). Whether sunlight or an incandescent lamp shows up in the emitter-off frame is
  **UNKNOWN** until someone measures it (§6, "Still to measure").
- No control changes this. The IR function has exactly one stream format (640×360 Y8 at 15 fps),
  no exposure or gain control, and its Microsoft face-authentication control knows two states:
  alternate-frame strobe (the power-on default) and emitter off (§5).

So the code stays as it is: `IrPair::ambient_image` keeps treating a blank frame as absent, and the
sunlight gate stays dormant until an ambient frame with content is seen.

## 1. Device, functions, nodes

| | |
|---|---|
| USB id | `30c9:00f3`, bcdDevice 10.16, USB 2.01 high speed, bus powered 500 mA, `removable` = `fixed` (built in) |
| Strings | manufacturer `8SSC21K64646V1SR58L317H` (a Lenovo FRU), product `Integrated Camera`, serial `0001` |
| Interfaces | 0+1 RGB camera (UVC 1.5 VideoControl + VideoStreaming), 2+3 IR camera (`Integrated IR Camera`), 4 HID (`Camera HID Device`), 5 DFU (`Camera DFU Device`) |
| Linux nodes | `/dev/video0` RGB capture, `/dev/video1` RGB metadata, `/dev/video2` IR capture, `/dev/video3` IR metadata; `/dev/media0`, `/dev/media1` |
| BOS | one platform capability, MS OS 2.0 descriptor set: Windows version 0x06030000, 1559 bytes, vendor code 0x06, alternate-enumeration code 0x15 (`lsusb` prints an unparsed second capability `08 10 11 01 03 00 00 00`). Reading the set from Linux (vendor request 0x06, wIndex 7) returns one byte `00`, so what it carries is **UNKNOWN**; **INFERRED** it holds the camera-profile registry values (`UVC-CPV2FaceAuth`…) that the INF does not install (§4) |
| uvcvideo quirks | `/sys/module/uvcvideo/parameters/quirks` = 4294967295 = the module's own default `uvc_quirks_param = -1`, meaning "use the device table" (`uvc_driver.c`: `dev->quirks = uvc_quirks_param == -1 ? dev->info->quirks : uvc_quirks_param`); nothing in `/etc/modprobe.d` or the kernel command line sets it, and `30c9` is not in the device table, so the effective quirks are 0 plus `UVC_QUIRK_MSXU_META` (set at probe, §3) |

### RGB function (interfaces 0/1)

- Input terminal 1: auto-exposure mode, auto-exposure priority, exposure time (absolute), privacy, region of interest.
- Processing unit 2: brightness, contrast, hue, saturation, sharpness, gamma, white balance temperature (+ auto), backlight compensation, power line frequency.
- Extension units: 4 `{1229a78c-47b4-4094-b0ce-db07386fb938}` (2 controls), 7 `{26b8105a-0713-4870-979d-da79444bb68e}` "Realtek Extended Controls Unit" (8 controls), 8 `{0f3f95dc-2632-4c4e-92c9-a04782f43bc8}` "Microsoft Extended Controls Unit" (4 controls).
- Formats: MJPG 640×360 … 2592×1944 at 30 fps, YUYV 640×480 and 640×360 at 30 fps.

### IR function (interfaces 2/3)

- Input terminal 9: privacy and region of interest only (`bmControls 0x00240000`). **No exposure, gain or auto-exposure control.**
- No processing unit. Output terminal 12 ← unit 14 ← 13 ← 10 ← 11 ← terminal 9.
- Extension units: 11 `{45b5da73-23c1-4a3d-a368-610f078c4397}` (0 controls), 10 `{1229a78c-…}` (2 controls), 13 `{26b8105a-…}` "Realtek Extended Controls Unit" (1 control), **14 `{0f3f95dc-…}` "IR Microsoft Extended Controls Unit" (2 controls: `bmControls 20 01` = D5 `FACE_AUTHENTICATION`, D8 `METADATA`)**. Unit 14 was missed in the first descriptor reading (windows-hello-re.md L4, now corrected).
- Formats: exactly one, `FORMAT_UNCOMPRESSED` GUID `{00000032-…}` = Y8 (`GREY`), 640×360, one frame interval 666666 (15 fps), still-image method 1. `VIDIOC_S_PARM` 30 or 7 fps → 15; `TRY_FMT` 1280×720 → 640×360; YUYV refused.
- V4L2 controls: `region_of_interest_rectangle` (default `(0,0)/640x360`), `region_of_interest_auto_ctrls` (bitmask 1 = auto-exposure, default 1), `privacy` (read-only, 0).

## 2. Extension-unit controls (read with `UVCIOC_CTRL_QUERY`)

Every selector of every unit was probed with GET_LEN; the ones below answered (the others fail with
`EINVAL`/`ENOENT`, matching `bmControls`). Values are the bytes returned, little endian. Names come
from `RsDMFTIR64.dll`: its Realtek control name table at `off_1800F11B0` (selector 1 = `RTK_EXT_ISP_SPECIAL_EFFECT_CTL`,
2 `EVCOM`, 3 `CTE`, 4 `AE_LOCK`, 5 `AWB_LOCK`, 6 `AF_LOCK`, 7 `LED_TORCH`, 8 `LED_FLASH`, 9 `ISO`,
10 `PHOTO_SCENEMODE`, 11 `MANUAL_FLASH_CAPTURE`, 12 `COLOR_ENABLE`, 13 `COVER_MODE`,
14 `MIRAMATRIX_IR_LED`, 15–18 `not_used`, 19 `ROI_MODE`, 20 `ROI`, 21 `3ASTS`, 22 `BURSTMODE`,
23 `STILLMODE`, 24 `STILLSETTING`, 25 `IDEAEYE_SENSITIVITY`, 26 `IDEAEYE_STATUS`, 27 `IDEAEYE_MODE`,
28 `CTL_SEL_SLAVE_SENSOR_PREVIEW`, 29 `IQ_MODE`, 30 `IQ_PARAMETER`) and from the code that uses the
units (§4); the Microsoft selectors from the "UVC extensions 1.5" spec.

| Function | Unit | Selector | Len | GET_INFO | CUR | MIN | MAX | RES | DEF | Meaning |
|---|---|---|---|---|---|---|---|---|---|---|
| RGB | 4 | 10 | 8 | GET+SET | `030800bbbbbbbbbb` | `0000000000000000` | `ffffffffffffffff` | `0100000000000000` | `0000000000000000` | Realtek vendor command (`cmdXMemRead/Write`, I2C, `IsSupportIR`…; `CRtsIRCam` + `ExtUnitBase` in RsDMFTIR64) |
| RGB | 4 | 11 | 8 | GET+SET | `EEACCES` | `0000000000000000` | `ffffffffffffffff` | `0100000000000000` | `0000000000000000` | Realtek vendor data (GET_CUR `EACCES` without a preceding command) |
| RGB | 7 | 3 | 2 | GET | `a00f` | `0000` | `ffff` | `0100` | `8813` | `RTK_EXT_CTE_CTL` (colour temperature estimate, K; read-only) |
| RGB | 7 | 12 | 1 | GET+SET | `01` | `00` | `01` | `00` | `01` | `RTK_EXT_COLOR_ENABLE_CTL` |
| RGB | 7 | 16 | 1 | GET+SET | `00` | `00` | `01` | `01` | `00` | `RTK_EXT_not_used_0x10` in the name table; the DMFT calls it `PrivacyEnableCtl` (registry `CameraPrivacy`) |
| RGB | 7 | 18 | 27 | GET+SET | `00000064001400000300001f640000000000000001000000000000` | `00ff0100000000000000000000000000000000000000000120c007` | `000000000000000000000000000000000000000120c00704490a68` | `0000000000000000000000000000000120c00704490a6802430a60` | `00000064001400000300001f640000000000000001000000000000` | `RTK_EXT_not_used_0x12`; the same 27 bytes appear in RGB metadata item `0x80000003` |
| RGB | 7 | 19 | 1 | GET+SET | `00` | `00` | `01` | `01` | `00` | `RTK_EXT_ROI_MODE_CTL` |
| RGB | 7 | 20 | 10 | GET+SET | `00000000df017f020100` | `00000000df017f020100` | `00000000df017f020100` | `00000000df017f020100` | `00000000df017f020100` | `RTK_EXT_ROI_CTL` (rect 0,0–639,479 + flags) |
| RGB | 7 | 25 | 1 | GET+SET | `03` | `01` | `04` | `01` | `04` | `RTK_EXT_IDEAEYE_SENSITIVITY_CTL`; written by `CRsDMFTIR::SetLenovoTSPMode` |
| RGB | 7 | 26 | 1 | GET+SET | `00` | `00` | `01` | `01` | `00` | `RTK_EXT_IDEAEYE_STATUS_CTL` |
| RGB | 8 | 2 | 15 | GET+SET+ASYNC | `010000000000006061020000000000` | `00000000000000d007000000000000` | `07000000000000d012130000000000` | `00000000000000e803000000000000` | `010000000000006061020000000000` | MSXU `EXPOSURE` (0x02) |
| RGB | 8 | 3 | 11 | GET+SET+ASYNC | `0100000000000000000000` | `10000000000000feffffff` | `1000000000000002000000` | `0100000000000000000000` | `0100000000000000000000` | MSXU `EVCOMPENSATION` (0x03) |
| RGB | 8 | 9 | 4 | GET+SET | `01000000` | `00000000` | `01000000` | `01000000` | `00000000` | MSXU `METADATA` (0x09); uvcvideo sets it to GET_MAX at probe |
| RGB | 8 | 14 | 20 | GET+SET | `000000006400000032000000640000000a000000` | `0000000000000000000000000000000000000000` | `010000006400000032000000640000000a000000` | `0000000000000000000000000000000000000000` | `000000006400000032000000640000000a000000` | MSXU `FRAMERATE_THROTTLE` (0x0E) |
| IR | 10 | 10 | 8 | GET+SET | `030800bbbbbbbbbb` | `0000000000000000` | `ffffffffffffffff` | `0100000000000000` | `0000000000000000` | Realtek vendor command (as unit 4) |
| IR | 10 | 11 | 8 | GET+SET | `EEACCES` | `0000000000000000` | `ffffffffffffffff` | `0100000000000000` | `0000000000000000` | Realtek vendor data (as unit 4) |
| IR | 13 | 16 | 1 | GET+SET | `00` | `00` | `01` | `01` | `00` | `PrivacyEnableCtl` (as unit 7 selector 16) |
| IR | 14 | 6 | 9 | GET+SET | `010302000000000000` | `000000000000000000` | `010302000000000000` | `000000000000000000` | `010302000000000000` | MSXU `FACE_AUTHENTICATION` (0x06): `bNumEntries 1`, `bInterfaceNumber 3`, `bmControlFlags 02` = D1 alternative frame illumination |
| IR | 14 | 9 | 4 | GET+SET | `01000000` | `00000000` | `01000000` | `01000000` | `00000000` | MSXU `METADATA` (0x09); uvcvideo sets it to 1 at probe |

Notes:

- `{1229a78c-…}` (units 4/10) is Realtek's vendor command channel: selector 10 takes an 8-byte command
  (`03 08 00 bb…` is whatever was last written), selector 11 moves the data; the DMFT's `cmdXMemRead/Write`,
  `VendorCmd_ReadI2CReg8/WriteI2CReg16`, `IsSupportIR`, `EnableIRMode`, `GetIRResolution`, `GetHwVer`
  go through it (`CRtsIRCam`, `ExtUnitBase::ExtUnitBase` 0x180002830). It reaches sensor registers and
  firmware memory and was **not written** here.
- `{45b5da73-…}` (unit 11) is the unit `CRtsIRCam` looks up first (`sub_1800026F0`); with 0 controls
  it gets the `ExtUnitDummy` implementation and the vendor commands fall back to units 4/10.
- Unit 13 selector 16 is written by the DMFT with the registry value `CameraPrivacy` ("`<DMFT> Support
  PrivacyEnableCtl`", `CRsDMFTIR::RestoreIRPrivacyControl`, 0x1800302C0): a software privacy switch
  (**OBSERVED** for the IR unit; the RGB unit's selector 16 is the same selector of the same GUID,
  **INFERRED** same meaning). Not written here: its effect on the stream is unknown and it has nothing to do with the emitter.
- Selector 17 of the same unit is set to 1 only for "Acer IR cam and HLK" test runs; selector 25 by
  `CRsDMFTIR::SetLenovoTSPMode` (RGB only here).
- GET_INFO 0x13 on the MSXU exposure/EV controls = GET+SET+asynchronous.

## 3. Metadata (`/dev/video3`, `/dev/video1`)

Both metadata nodes offer `UVCH` (payload header only) and `UVCM` (`V4L2_META_FMT_UVC_MSXU_1_5`,
upstream since 2025-07, commit 2ab4019aa34d). uvcvideo enables it at probe (`uvc_meta_detect_msxu`,
`uvc_metadata.c`): GET_CUR of the MSXU `METADATA` control, and if 0, SET_CUR with GET_MAX (= 1 here),
then `UVC_QUIRK_MSXU_META`. That is why selector 9 reads `cur 1, def 0` on both functions; the value
is uvcvideo's, not a leftover. Experiment E3 (§5) shows the camera emits the items regardless.

Each `UVCM` record is `struct uvc_meta_buf` (`ns`, `sof`, `length`, `flags`) followed by the payload
header minus its first two bytes: PTS (4), SCR (6), then `KSCAMERA_METADATA_ITEMHEADER` items
(`MetadataId` u32, `Size` u32, data). The first record of a stream has no items (`length 12`).

IR function (one record per frame, `length 40`, header flags `0x8c`/`0x8d` alternating FID):

| Item id | Size | Content | Seen |
|---|---|---|---|
| `0x80000004` (vendor) | 12 | 4 bytes, always `00000000` | every record of every capture |
| 6 `MetadataId_FrameIllumination` | 16 | `Flags` u32 (bit 0 = on), `Reserved` u32 | every record; alternates 1/0 in strobe mode, 0 in emitter-off mode |

No exposure, gain or timestamp item beyond the UVC PTS/SCR; the lighting status cannot be read from
IR metadata. PTS advances 1 015 942 ticks of the 15 MHz clock (67.7 ms) per frame in strobe mode and
508 000 (33.9 ms) with the emitter off.

RGB function (`length 176`, from `/tmp/camre_meta_rgb.raw`, previous run):

| Item id | Size | Content |
|---|---|---|
| 3 `MetadataId_CaptureStats` | 80 | `Flags 0x423` (exposure time, EV compensation, white balance, sensor frame rate), `ExposureTime` 500000 × 100 ns = 50 ms, `WhiteBalance` 4000 K, `SensorFramerate` 20 (fps; the sensor dropped from 30 to 20 fps for the long exposure); first record 0 |
| `0x80000001` | 16 | two floats 1.0, 1.0 |
| `0x80000002` | 20 | three floats 1.5, 1.0, 2.109375 |
| `0x80000003` | 36 | the 27 bytes of Realtek unit 7 selector 18 (`00000064001400000300001f64…`) |
| `0x80000004` | 12 | `00000000` |

## 4. What the Windows driver does

INF (`RtLeJFIR.inf`, model `RS_SE_IR_CAMERA` for `USB\VID_30C9&PID_00F3&MI_02`; `RtLeJF.inf`
`RS_RGB_DMFT_CAMERA` for `MI_00`), all **OBSERVED**:

- Both functions use the inbox `usbvideo.inf` (`Needs = USBVideo.NT`); Realtek adds device MFTs:
  IR `CameraDeviceMftCLSIDChain = {09D98F23-EB50-4984-808F-DA19DCF0CA6E}` = `RsDMFTIR64.dll`, RGB
  `{9DF42396-2789-44F7-803B-10299788AF04}` = `RsDMFTNB.dll` (with `RsDMFTNB_Assets.dll`,
  `RsEyeContactCorrectionNB_Assets.dll`).
- IR device parameters (`RS_IR_CAMERA.NT.HW.AddReg`): `SensorCameraMode = 1`, `SkipCameraEnumeration = 1`,
  `EnableDshowRedirection = 1`, `PreferDeviceInfo = 1`; interface `KSCATEGORY_SENSOR_CAMERA` + `KSCATEGORY_CAPTURE`,
  `RTCFlags = 0x10`. **No FaceAuth profile key**: `UVC-CPV2FaceAuth = 0xFFFF0000` exists only in the
  `RS_IR_CAMERA_PROFILE0` model, which no hardware id here uses; `RS_SensorGroup.AddReg` is empty. The
  profile Windows Hello selects (F6) therefore comes from the device's MS OS 2.0 descriptor set (§1) — **INFERRED**.
- RGB: `UVC-EnableDependentStillPinCapture = 0`, `MJPGTranslationSubType = NV12`, `EnableDshowRedirection = 1`.

`RsDMFTIR64.dll` on the IR stream (IDA on a copy under `re/conv/camera-re/`, dumps in `re/RsDMFTIR64/`):

- It does not read or write pixels. No media-buffer lock appears in the code; the only functions that
  read frame bytes, `CRtsIRCam::GetIRLEDFlag_Ytag` (a 16-bit flag in the LSBs of the first 16 pixels,
  `sub_180005100`) and `GetIRLEDFlag_UVtag` (bytes 1/3/5/7 of a YUY2 frame, `sub_180004F40`), have no
  caller in this build. They are the lit-frame tag of older Realtek IR cameras; this camera's frames
  carry no such tag (first 16 pixels of lit frames have random LSBs; dark frames are 0).
- `CIROutPin::AddSample` (0x18000B570, F17): on the first sample of a 640×360 stream at 15 fps it sends
  `KSPROPERTY_CAMERACONTROL_EXTENDED_FACEAUTH_MODE` with `Flags = 2`, `Capability = 2` to the driver
  ("`IR sample work around`") and otherwise forwards the sample.
- `IRController::KsProperty` (0x18000E420, F16): for this VID/PID (not in the emulation list 04F2/0408/0BDA)
  the FACEAUTH_MODE property is **passed through** to `usbvideo.sys` ("`Driver support … we just passthrough`");
  the chosen value is mirrored to the registry `EXT_FACEAUTHMODE` (2 after a successful query, 1 at
  end of streaming). `usbvideo.sys` maps the property to the MSXU face-authentication control of unit 14
  (Microsoft's documented mapping; `usbvideo.sys` not analysed).
- So Windows writes `bmControlFlags = 02` into a control whose current and default value already is 02.
  It sets nothing else on the IR function: no exposure (there is no control), no vendor command path is
  used for the emitter on this model (`IsMirametrix` false: VID/PID not 04CA:706C / 04F2:B5EA and no
  control 14 on the IR Realtek unit).

Consequence: Windows sees the same emitter-off frames Linux sees. Its pipeline copes (F14: ambient
brightness 0 → `d = lit` ≥ 45 → status 1), and the sunlight rule simply never fires in a room like this one.

## 5. Experiments (2026-10-16, owner lying in bed ≈ 1 m away, dim room)

Procedure for each: read the control, set it, stream 60 frames of `/dev/video2` (GREY 640×360) with
`/dev/video3` (`UVCM`) in parallel under `timeout`, restore the control, read it back. Frame means are
over the whole frame (0–255). Every control was verified restored afterwards (face auth `01 03 02 00…`,
metadata `01 00 00 00`, ROI `(0,0)/640x360` auto 1, unit 13 selector 16 `00`).

| # | Setting | Result |
|---|---|---|
| E0 | nothing changed (baseline) | 14.76 fps; frames alternate dark / lit: means `0.00, 54.6, 0.00, 53.6, …` settling at 41.4; dark frames have 0.01 % non-zero pixels (max 1); illumination flag alternates 0/1 and matches |
| E1 | unit 14 sel 6 `bmControlFlags = 01` (authentication disabled) | **29.5 fps**, PTS step 33.9 ms; emitter off: every frame dark, means ≤ 0.04, non-zero pixels rise from 0.01 % to 1.5 % over ≈ 12 frames (auto-exposure gain ramp), max 17; illumination flag 0 on every frame. The value persists after the stream stops (read back `01`) until restored. This reproduces `/tmp/camre_ir_d0.raw` of the previous run |
| E2 | `bmControlFlags = 04` (background subtraction) | accepted by the firmware although GET_MAX does not advertise it; behaviour identical to 02: 14.76 fps, alternating strobe, flags alternate. Also tried `00` and `03`: identical to 02. Only the exact value `01` switches the emitter off |
| E3 | unit 14 sel 9 `METADATA = 0` | the camera still sends the `UVCM` items (61 records, flags alternate) and strobes as before; restored to 1 (uvcvideo would otherwise re-enable only at the next probe) |
| E4 | ROI `(0,0)/80x60` auto 1 (meter a dark corner, L6) for 90 frames | lit mean 52 → 41 (no rise this time: the auto-exposure was already at its ceiling for this scene; L6 saw 66 → 120 with the owner nearer), dark frames unchanged (12–16 non-zero pixels, max 5) |
| E5 | other frame intervals / sizes | none exist (§1); `S_PARM` and `TRY_FMT` return 15 fps / 640×360 |
| — | unit 13 sel 16 (`PrivacyEnableCtl`) | not toggled: meaning known (software privacy), unrelated to the emitter, effect on the stream unknown |
| — | RGB MSXU (unit 8: exposure, EV) | not tried: they act on the RGB function's own exposure, and the RGB controls belong to the parallel RGB work (its `auto_exposure`/`white_balance_automatic` were changed by it during these runs) |
| — | Realtek vendor command unit (4/10 sel 10/11) | not written: register/firmware memory access, not restorable |
| — | `uvcvideo quirks=0` | not needed: the running value is the default (§1) |

Previous run's files in `/tmp` (restart notes): `camre_ir.raw`/`camre_ir2.raw`/`camre_ir30.raw`
baselines (40/60/40 frames), `camre_ir_k0..k3.raw` four unrecorded settings that all left the strobe
unchanged (lit means 45–51, dark 0.0; k3 merely started on a lit frame), `camre_ir_d0.raw` = E1,
`camre_meta*.raw` the metadata of the baseline, of E1 and of the RGB function, `camre_d0_avg_x400.png`
the 60-frame average of E1 × 400 (the lens-shading noise pattern). This run's files: `/tmp/camre2_*.raw`.

## 6. Conclusion

- **No reversible setting gives an ambient frame with content**, because nothing is wrong with the
  frames: the emitter-off frame is the sensor's view of the scene at the lit frame's exposure, and the
  scene is black in the sensor's band. **OBSERVED** in this room, day and night.
- The strobe is the firmware's power-on default (`FACE_AUTHENTICATION` cur = def = 02); the only
  alternative is emitter off at 30 fps (E1). Windows sets the same default (§4). F8/F9/F16/F17 are
  therefore implemented by the firmware for this camera; linux_hello needs to write nothing.
- The illumination flag (metadata item 6) is the only per-frame information; no exposure or gain item
  exists on the IR function, so a lighting status from metadata is not available.
- `src/camera.rs` stays unchanged. If an ambient frame with content is ever seen, the existing
  subtraction and the sunlight gate (`lighting.rs`, F14) take over without code changes, since
  `IrPair::ambient_image` only treats a *blank* frame as absent.

Still to measure (needs a near-infrared source in front of the camera; none was available in this session):
run `cargo run --example ir_exposure -- --frames 30` (or `linux_hello watch`) with direct sunlight on
the face, an incandescent/halogen lamp, or a phone's face-unlock flood illuminator pointed at the
camera, and read the `lit=false` lines. A mean above a few gray levels proves the sensor sees
ambient IR and that the F14 gate works here; a mean that stays at 0 under direct sunlight would mean
the firmware exposes the emitter-off frame far shorter than the lit one, which no control can change.
