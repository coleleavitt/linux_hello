# Capture, liveness and matching

How `linux_hello` turns camera frames into a verdict, and how this follows Windows Hello Face.
F- and L-ids refer to [windows-hello-re.md](windows-hello-re.md); the gaps that remain are listed
in [feature-gap.md](feature-gap.md). Only open models are used (SCRFD, ArcFace, FaceNox,
MiniFASNetV2); the Microsoft models extracted for analysis are not.

## Pipeline per frame pair

```
/dev/video3 (UVCM metadata) ─ illumination flag per frame ─┐
/dev/video2 (IR 640x360 @15) ───────────────────────────────┴─▶ lit/ambient pair (< 300 ms apart)
  lit frame ─▶ SCRFD faces (3 largest) ─▶ Windows IR detector faces (box, 5 points); per face:
     size/edge gates ─▶ ambient warp ─▶ face brightness + lighting status ─▶ occlusion, frontal pose
     good ─▶ embed lit − min(lit, warped ambient) ─▶ live vote unless the RGB models veto
     weak ─▶ embed the lit frame ───────────▶ no vote
     too dark / too bright / too small / edge / occluded / not frontal / no face ─▶ no print, no vote (reason counted)
/dev/video0 (RGB 640x360) ─▶ whole, not dark ─▶ SCRFD faces ─▶ the one overlapping the registered IR box (none: colour net skipped) ─▶ colour net veto
                                        (without the Windows models: FaceNox + MiniFASNetV2 mean P(real) ─▶ veto if < 0.5)
  track: every analysed face followed by box overlap and print similarity; a new face starts a new attempt
  attempt: ≥ 4 prints and estimated FAR of the best print < 1e-5 (similarity > 0.50), no other face
           of the pair with a lower FAR → recognised; from then on 2 live votes → MATCH, 3 spoof votes → NOTLIVE
  session: 16 pairs over all attempts → give up; once more if the face moved or the exposure changed
  after MATCH: add the best live print if it was a borderline match
```

### 1. Lit/ambient pairs (F10, F12, F13, F15, L1–L3)

**Which nodes (F4).** Windows finds the IR sensor through its INF (`30C9:00F3 MI_02` is a
`KSCATEGORY_SENSOR_CAMERA`, F4) and the WinBio sensor allow-list. linux_hello takes the IR camera's
USB id (`--camera vid:pid`, default the first of `camera::KNOWN_IR_CAMERAS` present: `30c9:00f3`)
and, at every request (`CameraSetup::nodes` → `camera::find_cameras`), reads
`/sys/class/video4linux/video*` and asks each node `VIDIOC_QUERYCAP` and `VIDIOC_ENUM_FMT`: the IR
node is the video-capture node of that USB device offering the 8-bit greyscale format `GREY`, its
metadata node the `META_CAPTURE` node on the same USB interface (sysfs `device` link), and the RGB
camera the colour-only capture node of the same device. On this ThinkPad that is `/dev/video2`
(interface 1.2, `GREY` only), `/dev/video3` (interface 1.2, metadata) and `/dev/video0` (interface
1.0, `MJPG`/`YUYV`). Looking up at every request follows a renumbering or a re-plug between
requests; it costs four `open`+`ioctl` pairs. An id with no such device falls back to the card-name
probe (`find_ir_camera`, "ir"/"infrared" in the name) with a warning; on this machine the names are
truncated (`Integrated Camera: Integrated I`), so the probe finds nothing and the request fails
with `no IR camera found`.

The IR emitter lights every other frame by itself (L1). The camera's uvcvideo metadata node
(`/dev/video3` here) carries Microsoft's `MetadataId_FrameIllumination` item for every
frame (L2, L3); this is the same signal Windows reads as `InfraredMediaFrame.IsIlluminated` (F10).

- `linux_hello` sets the node's format to `UVCM` (`V4L2_META_FMT_UVC_MSXU_1_5`) before streaming;
  after boot it defaults to `UVCH`, which lacks the Microsoft items.
- Each metadata buffer is a list of `uvc_meta_buf` blocks (`u64 ns; u16 sof; u8 length; u8 flags;
  buf[length-2]`). Inside `buf`, PTS (4 bytes, flag 0x04) and SCR (6 bytes, flag 0x08) are skipped,
  then items `{u32 id; u32 size; payload}` follow. Item 6 is the illumination item; bit 0 of its
  first `u32` means "emitter on". The parser is tested on a recording from this camera
  (`tests/data/ir_meta_uvcm.bin`).
- Video and metadata buffers are paired by their V4L2 `sequence` number. A frame whose metadata
  was dropped is not used.
- A pair is two consecutive frames, one lit and one ambient, less than 300 ms apart by their
  buffer timestamps (F13). At 15 fps they are 67 ms apart.
- Without a metadata node (`--no-ir-metadata`), the brighter of two consecutive frames is taken
  as the lit one (sum of every 5th pixel of every 5th row, as in Windows' `FrameStrobe`, F15).
  Windows uses its brightness guess only for file replay.

**Exposure on the face (L6).** The IR node exposes the UVC 1.5 region-of-interest control
(`region_of_interest_rectangle`, `region_of_interest_auto_ctrls` bit 0 = auto-exposure). After
every pair with a face, `IrCamera::expose_on` sets the rectangle to the SCRFD box of the lit
frame (clamped to the frame, ≥ 2×2), as Windows' `SensorController` meters on the face (Q1).
Auto-exposure then targets the face rather than the whole frame: with the owner at ≈ 60 cm in a
dark room the face brightness (F43) went from 139 with whole-frame metering to 110 with the face
rectangle; a change shows after ≈ 10 frames and settles after ≈ 20–28 frames (0.67 s / 1.5 s),
so it mostly helps the second half of a session and the next one (the camera keeps its exposure
between streams, L7). Both controls are written in one `VIDIOC_S_EXT_CTRLS` (they are one UVC
control; the v4l crate cannot read rect or bitmask controls, so `camera.rs` uses its raw
bindings). Re-setting the rectangle every pair as the box jitters costs 0.5 ms and does not
disturb the exposure (measured over 40 re-sets). The driver default (`(0,0)/640x360`, auto on) is
written when the camera opens, in case an earlier run died, and when it closes. A camera without
the controls logs once (debug) and meters the whole frame; a camera that rejects a rectangle
logs once (warn) and keeps whatever it has.

**RGB capture.** The RGB camera streams YUYV 640×360, the IR frame's size and a centre crop of its
4:3 sensor frame (the 640×480 frame less 60 rows at the top and at the bottom: the two modes
differ by 1.4 grey levels per pixel at that offset, 4.7 if the 480-row frame is scaled instead).
640×480 does not work next to the IR stream on this laptop's USB 2.0 link (both cameras hang off
one 480 Mb/s port): alone it delivers whole frames, but with the IR stream running every frame
arrives truncated at 155–173 KB of 614 400 and flagged `V4L2_BUF_FLAG_ERROR`, at 30 fps as at
8 fps (`v4l2-ctl --stream-mmap --verbose`, 2026-10-16; 640×360 YUYV and MJPEG 640×480 arrive
whole). The mapped buffer comes back whole whatever `bytesused` says, so such a frame showed as
the top 133 rows of the picture over a green block (YUYV zeros), and SCRFD found no face in it:
74 of 74 frames in two `watch` runs that morning, when `rgb_real=-` hid that the colour net never
ran. `Camera::capture_rgb` now drops a frame the driver flags as broken or that is shorter than
the format; `OpenCameras::capture_liveness` reads past at most `MAX_RGB_SKIPS` = 4 broken or
dark frames per pair, then the pair has no RGB frame and the colour net is skipped (`color=no_rgb`,
no vote; without the Windows Hello models the open RGB models veto, as for a dropped camera).
The IR-to-RGB registration (§5) is the 640×480 fit moved up by the 60 cropped rows.

**RGB exposure.** The RGB node offers `auto_exposure` (default aperture priority),
`exposure_time_absolute`, `exposure_dynamic_framerate`, `power_line_frequency`,
`backlight_compensation` and `white_balance_automatic`; linux_hello sets none of them. The
"2–3 underexposed frames every ≈ 5 s" of earlier runs are not the camera's: they are the owner's
`irlux` daemon (`~/.local/share/irlux`, niri's ambient-light sensor, sampling every 60 s). It
keeps the RGB camera locked in manual exposure between samples (so the camera is found at
`auto_exposure=1`, e.g. `exposure_time_absolute=1250` → 8 fps in a dim room, and auto white
balance off), and when its sample falls while linux_hello holds the camera, its `ffmpeg` fails
with `EBUSY` and it retries every 5 s; each retry writes `auto_exposure=1`,
`exposure_time_absolute=156`, then `auto_exposure=3` back, which restarts the auto-exposure:
the frame drops to ≈ 0.15 of its level and climbs back over ≈ 25 frames (0.8 s), 2–3 dark pairs
and 2–3 dim ones at 7.5 pairs/s. Measured 2026-10-16 by sampling the controls at 5 Hz while
streaming: the writes recur every 5.17–5.20 s (the 5 s retry plus its work) from the moment
irlux's 60 s sample comes due — 17, 21 and 30 s into three runs, never in a run under 15 s —
and the `watch` output of the night before has the same dips 5.1–5.2 s apart. Alone, with the
controls left alone, auto exposure held the level within ±2 % over 40 s with the IR stream
running and 30 s without; no control setting of linux_hello can stop another process writing
the controls, so none is applied, and the dark-frame skip above covers the first half of a
restart. Dim frames are no risk to the colour net: on the restart frames of that output
p(spoof) was 0.002–0.26 (§5). The fix belongs in irlux (open the device before touching the
controls, and back off longer than 5 s when it is busy).

**Start-up (L7).** `OpenCameras::open` opens the IR camera first (its first frame comes after
≈ 90 ms alone), drops `WARMUP_FRAMES = 4` frames, then opens the RGB camera (first frame after
≈ 50 ms; started first it takes ≈ 245 ms, and the earlier note that it stalls when started after
the IR stream did not reproduce in 20 runs). The metadata node is read along with the warm-up
frames so that it is in step with the video by the first pair. The first pair with its RGB frame
is ready ≈ 0.54 s after opening (before: ≈ 1.6 s with RGB first and 16 warm-up frames; measured
on 2026-10-16 with a scratch example, 8 runs each). The wait for an RGB frame is bounded by
`RGB_FRAME_TIMEOUT = 1 s` (4× the slowest first frame measured): the stall seen once early on
would otherwise hang the unlock. After a timeout, at the first frame or later, the session goes on
without the RGB camera (`warn` log): the colour net is skipped, as Windows skips a classifier
whose input is missing (F25 status 6/7); the IR nets still vote. Without the Windows Hello models
the open RGB models veto a frame with no RGB face, so there every frame then votes spoof
(`NOTLIVE`, fail closed). The timeout is a `poll` timeout on the V4L2 stream (`Camera::set_timeout`);
no stalled device was at hand, so only the error path is unit-tested. 4 warm-up frames because the camera resumes
near its last exposure and converges by 1–2 % per lit frame: the first lit frame was 17–45 %
off after 30–300 s idle, frame 4–5 is within ≈ 30 %, and after that it only drifts; a ±35 %
brightness change moves a print's cosine similarity by < 0.015 and the cross-sitting match
score by < 0.02 (`examples/embed_dir` on brightness-scaled validation frames). Frames still
saturated after an abnormal start (L7) are rejected by the F44 gates until exposure comes down.
`examples/ir_exposure [--frames N] [x1,y1,x2,y2 | full ...]` prints the per-frame brightness
from stream start and after each region change.

**Rotation (F10, F63).** `--rotate 0|90|180|270` (global option, so also
`daemon … --rotate 180` through `LINUX_HELLO_ARGS`) turns every IR and RGB frame counter-clockwise
by that angle right after capture (`Camera::rotate`), before the lit/ambient pairing and detection
see it; the pairing is by sequence number and timestamp and does not change. The exposure region is
mapped back to sensor coordinates (`Rotation::to_sensor`). Windows rotates by the camera's mount
angle from its ACPI `_PLD` (or the `OrientationCW` registry override), not by the live display
orientation (F63); linux_hello has no automatic source either: this ThinkPad has no IIO
accelerometer (`/sys/bus/iio` is absent) and no iio-sensor-proxy, so the angle is the option only.
Default 0; the built-in camera of this laptop needs none.

**Presence wait (F55, F57).** Windows runs face as a WinBio presence monitor with no service
timeout; the UI ends the session. `Authenticator::verify` is one session (up to 16 pairs, plus a
re-attempt). `Authenticator::verify_within(user, deadline)`, used by the daemon for
`AUTH <user> <seconds>` (lock screen, console login; [pam.md](pam.md)), keeps the cameras open
and runs sessions back to back while they end in `NOFACE` (any reason, `not_frontal` included)
and the deadline has not passed; the session running at the deadline finishes. `NOMATCH` and
`NOTLIVE` end the wait at once, so a wrong face gets exactly the single session plus its one
re-attempt (F49). Between sessions the tracker starts over; the camera keeps its exposure, so the
second session needs no warm-up (L7). The daemon caps the wait at 120 s.

### 2. Frame gates (F45, F46)

The faces are found on the lit frame (SCRFD); the `MAX_FACES` = 3 largest go through the gates, each
on its own (§7). With the Windows Hello models, the Windows IR detector (`MsRetinaDetector`,
MODEL_FILES_110) runs once on the lit frame and its face overlapping a SCRFD one (IoU ≥ 0.5,
`Analysis::windows_face`) is the face Windows would track: its
five points give the brightness block (§3), the ambient-warp anchors (§4) and the liveness crops (§5),
its box the occlusion crop, its 6-DoF output the pose. Windows' 31-point landmark set is, on this
path, those five points plus copies of them (`RetinaEngine::ComputeAlignment`, lbf.md "Which aligner
runs"), so nothing else is aligned; the legacy LBF aligner does not run. SCRFD's own box and points
keep two jobs: the size/edge gates and the ArcFace recognition crop, whose alignment template is
SCRFD's. Without the Windows Hello models SCRFD's points stand in everywhere.

A face gives no print and no vote if:

| Reason (`NOFACE` word) | Check | Windows |
|---|---|---|
| `no_face` | no detection | — |
| `too_far` | box width < 72 px or < 0.12 × shorter frame side (43 px at 640×360) | F46, F54 |
| `edge` | box closer to a frame edge than inter-eye distance / 5 | F46 |
| `too_bright` | lit face brightness > 205 | F44 status 2 |
| `too_dark` | lit face brightness < 35 | F44 status 3 |
| `clipped` | more than 1 % of the face ellipse (inscribed in the box) at pixel value 255: too close, or a lamp on the face | F44 status 6, F45 (counted as too close) |
| `occluded` | Windows Hello occlusion model: 1 − p(none) > 0.9 (> 0.5 while enrolling); only with the converted models | F46 |
| `not_frontal` | Windows Hello IR detector's head pose: \|yaw\| > 20° or \|pitch\| > 25°, or that detector has no face overlapping the SCRFD face (IoU ≥ 0.5; checked right after the size/edge gates, before the lighting status); only with the converted models | F46, F59 |

The occlusion model is the converted Windows Hello one (`models/windows_hello/converted/occlusion`,
docs/winhello-models/cefa.md). Its input is the face box resampled to 128×128, as in Windows; the box
is the Windows IR detector's (on the owner's frontal frames p(none) is 0.991–0.999 with it, 0.961–0.998
with SCRFD's box). It is the only occlusion detector of Windows' stock path: the eye and mouth/nose
detectors run only with `IsFaceOcclusionDetectorEnabled` off, the legacy path (F65), and are not ported.

The frontal check uses the pose Windows uses: the 6-DoF output of its IR RetinaFace-style detector
(`models/windows_hello/converted/ldmf/ort/MODEL_FILES_110.ort`, `RetinaEngine::ComputeOrientation`,
docs/winhello-models/ldmf.md §7, §11). The detector runs on the whole lit frame (about 3 ms at 640×360) after the
other gates. If it reports no face where SCRFD found one, the frame is rejected too: Windows tracks faces with
this detector, so such a face would get no orientation there. The owner's frontal frames read |yaw| ≤ 5° and
pitch −11…−15°. The legacy Windows estimator (LBF aligner + `IrFaceOrientation`) is ported bit-exact
(`LbfAligner`, `IrFaceOrientation`, docs/winhello-models/lbf.md) but is not a gate: it reads about +25° of yaw on
frontal faces from this camera, so it would reject most real frames.

### 3. Face brightness and lighting status (F14, F43, F44)

Face brightness (F43) is the mean of every 2nd pixel in every 2nd row of the block
x ∈ [max(p3.x, p9.x), min(p4.x, p15.x)], y ∈ [max(p9.y, p15.y), min(p3.y, p4.y)] of Windows' 31-point
set: p3/p4 are the mouth corners, p9/p15 the eye-contour bottoms. On Windows' RetinaEngine path p9 and
p15 are the eye centres (`RetinaEngine::ComputeAlignment` copies the detector's five points into the
contour slots), so the block spans x from the inner of (left eye, left mouth corner) to the inner of
(right eye, right mouth corner), and y from the lower eye to the higher mouth corner, from the Windows
IR detector's five points (SCRFD's without the Windows models). It is measured on the lit frame and
on the ambient frame warped onto it (§4): `lit`, `ambient`, `d = lit − ambient`.

On the owner's frontal frames the lit brightness from the Retina points is within 1.5 of the value
from SCRFD's points (86.4–91.9 vs 87.0–91.1), and within 1.6 of the block from the legacy LBF
aligner's real eye contour (`examples/ms_check`, `brightness`).

| Status | Rule | Effect |
|---|---|---|
| too bright (2) | lit > 205 | rejected |
| too dark (3) | lit < 35 | rejected |
| clipped (6) | > 1 % of the face ellipse clipped | rejected (`NOFACE clipped`) |
| clipped around (7) | > 1 % of the band around the face clipped | print from the lit frame, used for matching; no vote; not used for augmentation |
| weak (4) | d < 45, or ambient > 25 (strong ambient IR: sunlight, incandescent and halogen lamps) | print from the lit frame, used for matching; no vote; not used for augmentation |
| good (1) | otherwise | print from `lit − min(lit, ambient)`; votes |

The ambient limit is Windows' constant 25 (`sub_1800380CC`, F44); it has no higher tier. Measured
2026-10-16 in a room with an incandescent lamp: the owner's face read ambient 30–40 with emitter
gain 52–63 and the liveness nets voting live on the difference image, so Windows and linux_hello both
fall back to the password there. `daemon --max-ambient <n>` raises the limit (logged at warn):
above 25 the subtraction is a weaker liveness signal, because a photo lit by the same ambient IR
also passes the emitter-gain rule, and only the anti-spoofing nets then tell them apart.

Windows also turns `d > 205` into status 2; with lit ≤ 205 that cannot happen. It smooths the
ambient brightness over frames; linux_hello does not.

**Clipped pixels (F44 status 6/7, `CheckIfTooMuchSaturatedPixelOnFace`).** On the lit frame,
`lighting::clipped` counts the pixels at 255 inside the ellipse inscribed in the tracked face's box
(the Windows IR detector's, SCRFD's without the Windows models) and in the band around it, the box
grown by 25 % of its width and height on every side, outside the ellipse (`CLIPPED_BAND`; Windows'
band size is not recovered). More than 1 % of a region (`MAX_CLIPPED`, Windows' area · 1 / 100)
means status 6 for the ellipse — the frame is rejected and reported as `clipped` (Windows counts
it as "too close", F45) — and status 7 for the band: Windows keeps the print tagged (flag 0x20),
and the status not being 1, does not subtract the ambient frame, so here the frame is handled like
status 4. Windows defines a clipped pixel as one equal to its neighbours 2 px away in both
directions; linux_hello takes the sensor's maximum, 255. The check sits between the exposure
status and the ambient comparison, as in Windows, so a bright band is reported before weak
lighting. On the owner's frontal frames no pixel reaches 255 (one on `watch_0435`); a 12×12
clipped patch on the face (1.2 % of the ellipse) gives `clipped`, a 25 px clipped column beside
the face `clippedaround` with the print kept and no vote (`examples/check_frames` on edited copies
of `watch_0014_ir_lit.png`). `watch`'s `log.jsonl` carries the two fractions (`clipped_face`,
`clipped_around`).

### 4. Ambient subtraction (F12, F13, F40)

For good frames, recognition uses `diff = lit − min(lit, ambient)` per pixel, the light the
emitter added. This removes sunlight and other ambient IR, so enrollment and unlock see the same
lighting. Enrollment uses only good frames, so every enrolled print is a difference image.

**This camera gives no ambient image.** Its emitter-off frames are blank (every pixel ≤ 6 of 255 in
every recording, by day and by night), so on the P16 Gen 3 the subtraction is a no-op, the "ambient"
brightness is always 0, and the sunlight rule (ambient > 25) has never fired. The frames are real
sensor frames of a room without infrared light, not blanked ones, and no camera control changes
that ([camera.md](camera.md)); what direct sunlight does to them is still to be measured. `IrPair::ambient_image` reports such a frame as absent (mean ≤ `BLANK_AMBIENT`), and
the anti-spoofing nets then get the general-IR treatment of §5 instead of the difference-image one
(F42). Measured 2026-10-16 in daylight, lying down: the diffusion net (slot 12) read p(real)
0.38–0.47 on 74 frames of the owner (0 % live, every attempt `NOTLIVE`), the general net (slot 11)
read 0.61 (100 % live); after the change 14 of 14 sessions matched.

The ambient frame is first moved onto the lit one, as Windows does (`AdjustFaceForAmbientLight`,
`RemoveAmbientLightFromFace`, F13, F40):

- Three anchors per lit face (`ComputeAnchorPoints`): the means of the left-eye, right-eye and mouth
  points of the 31-point set, which on the Retina path are the two eye centres and the midpoint of the
  mouth corners (`lighting::anchors`).
- Where the anchors were in the ambient frame is taken by linear motion between the current lit
  frame and the previous one: `cur + (prev − cur)·t`, `t` = (t_ambient − t_cur) / (t_prev − t_cur) from
  the V4L2 buffer timestamps (`lighting::interpolate`). Windows weighs the two lit frames the other way
  round (`AdjustFaceForAmbientLight` 0x180036c10: `cur·(1 − t') + prev·t'` with
  `t' = (t_ambient − t_prev) / (t_cur − t_prev)`), which is the same at the only point it sees,
  t = 0.5 (alternate frames at a fixed rate). Here the ambient frame may also follow the lit one
  (t < 0), which the linear motion covers.
- The affine through the three anchor pairs (`lighting::affine_from_anchors`, Windows'
  `Affine2DMatrix::DeriveTransform`) maps lit positions to ambient ones; the warped ambient frame
  reads `ambient(M·p)` at every pixel, bilinear with zero padding (`lighting::warp`,
  `ApplyAffineTransformToImage`). It is whole-frame, 1.5 ms at 640×360; Windows warps only the ambient
  crop (face crop + 100 px, F39).
- The previous lit face must be less than 300 ms old (Windows' pairing constant, F13) and the same
  face (the face of the previous lit frame with the best box IoU, ≥ 0.3, the tracker's rule, for
  Windows' same-tracking-id condition). Otherwise, and for the first pair of a face, the ambient
  frame is subtracted as captured. With several faces each gets its own warp (whole frame, 1.5 ms). Windows also requires
  both lit frames frontal; here the previous frame's anchors are kept even when that frame was
  rejected later.

The lighting status (§3) reads the ambient brightness off the warped frame, as Windows does; the
liveness nets get the unwarped difference (F40 (b), §5).

Checked on the saved frames (`examples/ms_check`, `warp`): between consecutive lit frames of a still
sitter (`ir_11…19`, anchors move 0.1–1.3 px by SCRFD's own jitter) the matrix is within 0.01 of the
identity plus a 0.5–5 px translation; the dark frames there are black (ambient 0), so the subtracted
image and its ArcFace print are unchanged (cosine 1.000). With the previous lit frame standing in
for the ambient frame (t = 1), the warped frame differs from the raw one by 1–3.6 gray levels per
pixel in the face box and the F43 brightness by ≤ 0.8, and the residual `|lit − ambient|` over the box
stays at 1.5–3.3 (1.8 → 2.3 and 2.0 → 3.3 on two frames: the landmark jitter becomes a sub-pixel
shift). When the face has moved (owner frames from different moments, Retina anchors 3–9 px apart)
the warp halves the residual: 12.4 → 4.1 and 6.4 → 3.9.

### 5. Liveness votes (F22–F25)

Windows' live votes come from two IR anti-spoofing CNNs; the colour CNN can only veto (F24).
For a good frame:

| Evidence | Vote |
|---|---|
| IR: without the Windows Hello models, the good lighting status (reflects the emitter like a nearby face, little ambient IR) | +1 |
| IR: with the Windows Hello models, their frame total (F25), see below | −3…+1 |
| RGB, only without the Windows Hello models: FaceNox + MiniFASNetV2 mean P(real) < 0.5, or no RGB face corresponding to the IR face (below) | −2 |

**Colour face of an IR face (F66).** Windows pairs the IR face with the colour face that overlaps
the IR box mapped into the colour frame by a per-registration affine, learnt at run time from
observed pairs; the mapped box is grown by 7.5 % per side and the overlap must be at least 0.55 of
the smaller box, else the pair is "low overlapping" and, with exactly one face in each frame, the
two are paired anyway (the rule Windows applies before it has a registration); otherwise the face
has no colour face and the colour net is skipped with status 7, no vote, as any classifier with
missing input (F25). linux_hello does the same with a fixed registration
(`pipeline::ColorRegistration::P16_GEN3`, `corresponding`): the cameras are built in side by side,
so the IR SCRFD box maps to the RGB SCRFD box by a scale and offset per axis, x' = 0.91·x + 23,
y' = 0.90·y + 10. That is the least-squares fit of the box corners over 148 saved pairs of the
owner at RGB 640×480 (y' = 0.90·y + 70; `examples/color_pairs` on `validation_frames/watch_*` and
a `watch` output; residual σ 3.3 px in x, 4.4 px in y, max 16.5 px; the face's IR box spanned
x 277–361, y 98–166, so the fit is only checked in that area), less the 60 rows the 640×360
capture crops at the top (§1). On 26 live pairs at 640×360 (owner lying down, IR box x 325–425,
y 122–254) the mapped box is within 4 px (x) and 9 px (y) of the RGB box and every pair overlaps
fully. The scale includes SCRFD drawing its RGB boxes ≈ 0.9× its IR boxes of the
same face; the mapping is box to box, which is what the overlap needs. With it every saved pair
overlaps fully (the grown box contains the RGB box); the RGB face has to sit ≈ 50 px (x) or
≈ 70 px (y) away from where the IR face says, half a face, before the overlap falls under 0.55.
Several faces in the RGB frame: the one overlapping most, as Windows (before: the nearest box
centre). The fallback keeps the colour net running on a camera pair the constant does not fit;
on this laptop it only matters when a single RGB face is far from the single IR face.

Windows also computes four colour quality flags on the paired face (`EvaluateColorFaceQuality`,
F66: exposure status of the colour face, low alignment confidence, both opposite edges cut = too
close, too far) but only counts them in telemetry; they stop nothing, so linux_hello does not
compute them.

Frame total > 0 is a live vote, < 0 a spoof vote. Weak and rejected frames do not vote (and the
liveness models are not run for them), so with the Windows Hello models a frame is live only when
the lighting status and the Windows nets agree.

The physics checks of [liveness-physics.md](liveness-physics.md) (eye glint, emitter falloff,
flicker, IR/RGB material, micro-motion, parallax, blink) are scored on every face with a print
after the vote and logged; a check with a threshold (none today without `--trained-gate`) turns a
live vote into an abstention, −1 as Windows' colour net.

With the Windows Hello models the open RGB models do not run: Windows' colour net (slot 13) is the
RGB veto, as in Windows, and counting both would weigh the same RGB evidence twice. This matters
in backlight: on 147 live frames of the owner with a lamp behind the head, FaceNox gave P(real)
≈ 0.002 and MiniFASNetV2 ≈ 0.998 on almost every frame. Their mean sat on the 0.5 veto line, so
20 % of good frames became spoof votes and 8 of 15 sessions ended NOTLIVE.

**Windows Hello frame total** (when `models/windows_hello/converted` exists; `--no-windows-hello`
turns it off). Good frames are the frames Windows sends to its diffusion net (status 1, ambient
≤ 25, F40, F42), so:

| Net (slot) | Input | Rule (flag-on thresholds, mode 0) |
|---|---|---|
| diffusion IR (12) | `lit − min(lit, ambient)` (unwarped, F40), 128×128 crop aligned on the Windows IR detector's five points (F23) | p(real) > 0.5037 → +1 |
| colour (13) | face detected in the RGB frame, aligned crop, BGR | p(spoof) > 0.5 → −1 |

The `watch` line and `log.jsonl` carry the colour net's say as `color=`: its p(spoof) when it
ran (`color=0.013`), else why not: `no_rgb` (no RGB frame came with the pair: camera dropped or
only broken frames, §1), `no_face` (no SCRFD face in the RGB frame), `no_match` (no RGB face
overlaps the registered IR box, Windows' status 7), `skipped` (not a good-lighting frame, or no
Windows Hello models; the open models' P(real) is `rgb_real=`). Live on the owner (2026-10-16,
dim room, 10 s `watch` on a scratch template): p(spoof) 0.005–0.103, median 0.025, over 24
good frames, `skipped` on the 2 clipped-band frames, two `MATCH`es. On the saved frames of the
night before, which include three exposure restarts (§1): 0.002–0.26 on the dim frames
(RGB mean 59–95 of 117), one 0.58 on a frame at normal brightness, no RGB face on two dim
frames.
| border (14) | whole IR frame as 128×128 | p(spoof) > 0.7 → −1 |
| HH (15) | whole IR frame as 128×128 | p(spoof) > 0.5 → −1 |

In mode 0 the IR nets never vote spoof (spoof threshold 1.0). Windows runs slots 14 and 15 only
after the first live vote; linux_hello runs them on every good frame, which can only add vetoes.
`examples/ms_check` runs the nets on saved frames; their outputs match the Python reference
implementations (`tools/winhello_models`) to within 1e-3.

The veto weighs −2, not −1 as Windows' colour veto: with a single IR vote, −1 would only cancel the
live vote, and the "3 spoof frames" rule could never fire. −2 is the weight Windows gives its
multi-task IR vetoes in strict mode (F25).

If the liveness models are configured but fail to load, `auth` and `daemon` exit with an error
(fail closed, as Windows starts in "spoof" when anti-spoofing is on but no classifier is enabled).
`--no-liveness` turns off only the RGB veto; the lighting status still votes.

### 6. Matching (F28, F50)

A user's template is a list of prints (512-float ArcFace embeddings), one per enrollment frame,
not an average. The similarity of a probe is the maximum cosine similarity over the user's prints,
i.e. the minimum distance `1 − cos` (F28, F50). Faces are compared only with the template of the
user being authenticated (1:1).

As Windows (F27, F51, F60), the similarity is not thresholded directly. A calibration table for the
IR camera (`models/calibration.json`, keyed by the camera's USB `vid:pid`, with a default; this
laptop's `30c9:00f3`) maps the best similarity to the estimated false-accept rate of one comparison
by linear interpolation between its (FPR, TAR, similarity) rows; times the comparisons one attempt
can make, the prints in the template × 16 pairs, that is the attempt's FAR (`PrintMatch::far`,
`CalibrationTable::far`). The face is recognised when it is below `TARGET_FAR` = 1e-5. The shipped
table comes from grayscale LFW ([calibration.md](calibration.md), "Calibration table"); its 0.50 row
is the 1e-5 point for 15 prints (240 comparisons), the former `MATCH_THRESHOLD` 0.50 (one comparison
FAR ≈ 4e-8, simulated per-attempt FAR ≈ 1.8e-6, FRR 0.23% per probe frame against 15 prints). With
the 16 prints of one enrollment the point is 0.503, with 31 (16 enrolled + 10 augmented + 5
on-failure) 0.525, so the bound stays honest as the template grows. Every per-frame log line and
`watch`'s `log.jsonl` carry the FAR next to the similarity (`similarity=0.641 far=2.3e-7`); the
daemon reply keeps the similarity only.

**Verification condition (F67).** Windows reads the FAR of a comparison from the calibration
table of its *verification condition*, derived from the quality flags of the probe frame and of
the closest stored print: clean (0), weak lighting or a clipped band (1, statuses 4/5/7), a
non-frontal pose (2), an edge cut (5), and the coupled mean/pair prints (4). The five tables of
the v5 model are one per condition (cefa.md §2.2), all identical in the shipped build, and the
best GFP is kept per condition with the lowest deciding, so the condition changes no number
there; the print's source (enrolled, on-failure, on-success) does not enter it. Windows' × 1…4
is the number of enrolled users, capped (F52): × 1 for one user, and linux_hello matches 1:1.
Here every stored print comes from a good-lighting frame (enrollment, augmentation and
enroll-on-failure take live frames only), so the condition is the probe frame's:
`Condition::Clean` for a good frame, `Condition::Weak` for a weak or clipped-band frame
(`Condition::of`, recorded in `PrintMatch::condition`); non-frontal and edge-cut frames give no
print (§2). The FAR comes from the one table per camera, as Windows' identical tables; a
per-condition table would need impostor data from weak-lighting frames, which there is none of.
Two Windows details are not followed: on-failure prints are compared only while Windows'
`AutoEnrollmentMode` is on (here they are matched whenever they are in the template, and only
the daemon with `--enroll-on-failure` adds them), and Windows counts the 4 prints a track needs
among the prints of the current frame's quality category (here any 4 prints of the track).

**Second opinion (F28, F50, F51).** With the Windows Hello models, every print also carries Microsoft's
own recognition feature: the v5 net (`MsRecognizer`, [winhello-models/cefa.md](winhello-models/cefa.md)
§7) on the same recognition image, cropped at the Windows IR detector's five points, 256 floats
L2-normalised, stored as `windows` next to `features`. A probe with such a feature is compared with the
prints that have one by Windows' engine distance `1 − cos` (F50), the minimum over them (F28), and
Microsoft's calibration table from the model container (FAR 1e-5 at distance 0.350,
[calibration.md](calibration.md) "Microsoft's table") gives its FAR. The face is recognised only when
ArcFace's FAR is below 1e-5 **and**, if there is a second opinion, Microsoft's FAR is too
(`PrintMatch::recognised`; Windows' `targetFAR > GFP`, F27). Prints enrolled without the models, and
probes taken without them, have no second opinion and are decided by ArcFace alone, so older stores keep
working. The duplicate-enrollment check and enroll-on-failure's relaxed threshold use ArcFace only. The log
line adds `ms=0.013 ms_far=7.8e-7` (`-` without a second opinion). On the validation frames (cefa.md §7) the
two agree on the owner within one sitting and on the strangers; the chin-cut `ir_*` frames against a
full-face template pass ArcFace (0.61–0.64) but not Microsoft's net (distance 0.42–0.45), the frames
Windows itself would drop at the edge gate (§2). Cost: 4–6 ms per frame.

### 7. Decision (F25, F47, F48)

The order follows Windows' `MakeDecisions`: recognise first, then liveness on the same face.
"The same face" is the face track (F37, F47): `FaceTracker` follows every analysed face (the 3
largest of the lit frame, `MAX_FACES`) from pair to pair. A face continues the track whose last
box it overlaps best (IoU ≥ 0.3) among the tracks not already continued in this pair, provided its
print, when the frame gives one, matches the track's last print (cosine ≥ 0.50,
`SAME_FACE_SIMILARITY`); otherwise it starts a new track. A track whose face is not seen for 7
pairs (≈ 1 s, Windows' 1000 ms purge, F37) is dropped; a face back within that time continues its
track. Each track has its own attempt (`Session`, `Attempt`): its prints, best similarity and
liveness votes start from zero, so the owner's prints never recognise a photo held up next to the
owner or in the owner's place, and the owner's live votes never count for it. Rejected faces (no
print) continue their track by place. The 16-pair limit counts the whole session (twice, after a
re-attempt, below). Analysing a second face costs ≈ 31–37 ms per pair on top of ≈ 60 ms for one
(per face: ArcFace 20 ms, Microsoft's recogniser ≈ 5 ms, the Windows nets 4 ms, occlusion 1 ms,
warp and subtraction 2 ms; SCRFD and the Windows IR detector run once per frame, 28 ms); a third
face that fails the size gate costs ≈ 5 ms (`examples/check_frames` on `watch_0014_ir_lit.png`
with stranger faces pasted in, release build, machine under load; 55 ms for one face before the
recogniser was added).

The print check is an addition: Windows tracks by geometry alone (`FaceTrackerInternal`). 0.50
is the calibration table's 1e-5-per-attempt point (per-comparison FAR 4e-8 on grayscale LFW,
where 2 % of genuine pairs — photos from different occasions — fall below it). On the saved
frames prints of one sitter 67 ms apart score 0.957–0.986, seconds apart 0.96–0.98, the same
sitter in another sitting with the chin cut off 0.60–0.64, and different people ≤ 0.19 (the six
faces of the stranger photo among themselves; −0.07…0.04 against the owner;
`examples/embed_dir`). A false split only restarts the attempt (4 more prints); a missed switch
would hand the track's recognition to the new face.

1. **Recognise:** once the track has ≥ 4 prints (good or weak frames) and the estimated FAR of
   the best similarity over them is < 1e-5 (similarity > 0.50 with the shipped table and 15
   stored prints, 0.503 with 16), and no other face of the pair matches the user with a lower
   FAR (F48 `MakeDecisions`: "no other tracked face matches the same user better"). With the
   owner and a photo of the owner both in view, only the better-matching one is recognised;
   once it leaves the frame the other can be.
2. **Liveness:** votes count only from the recognising frame on. ≥ 2 live votes → `MATCH <best
   similarity>`; else ≥ 3 spoof votes → `NOTLIVE`.
3. **Give up** after 16 frame pairs (Windows: 48 frames or a counter > 16; one pair is two frames).
   The verdict comes from the user's track: the recognised one, else the best-matching track with
   ≥ 4 prints over the session. That track recognised → `NOTLIVE`; not recognised → `NOMATCH
   <best>`; no track with 4 prints → `NOFACE <most frequent rejection reason of the largest face
   over the session>`. Earlier, while no track is recognised, 4 consecutive occluded frames (§2) of
   every face in view end the session with `NOFACE occluded` (F46, `IdentifyAsGuest(3)`): a mask
   or a hand will not go away within the budget, so the password prompt comes ≈ 1.6 s sooner. A
   clear frame in between starts a face's count over; a new track starts it over too.

The earliest possible `MATCH` is therefore at the 5th pair (4 prints, the 4th frame votes live,
the 5th too), where voting from the first pair allowed a match at the 2nd pair.

**Re-attempt (F49).** Before a session ends in `NOMATCH` or `NOTLIVE` (at the pair limit or by three
spoof votes), it checks what the deciding track's face did over the attempt's frames, as Windows'
`sub_1800575E0` does for a track about to become a guest. If the face moved — the range of its box
centre, normalised by the frame width and height, has ((Δx)² + (Δy)²) / 2 > 0.01 (about 0.14 of
the frame), or its box width changed by more than 20 % — or the exposure changed — the standard
deviation of the last 10 lit face brightness values (§3, including rejected too-dark/too-bright
frames) is above 12 — the session gets another 16 pairs with fresh counters for the track (prints,
best similarity, votes), once per track, as Windows (`ReAttemptRecognitionByMovement`,
`ReAttemptRecognitionByExposure`). A still face in steady light is rejected at the 16th pair as
before; `NOFACE` is never re-attempted (nothing was recognised). Each track keeps its own motion
record, so a photo replacing the owner does not inherit the owner's movement; it can earn a
re-attempt of its own if it reaches a verdict, so a session can run past 32 pairs only when
another track decides it. As Windows, a track with more than one clipped-face frame (§3 status
6) gets no re-attempt: a face too close or under a lamp will not fix itself. A re-attempted session reads up to 32 pairs, ≈ 4.3 s at 15 fps plus the ≈ 0.5 s
start-up, within the PAM module's 10 s timeout.

Checked on saved frames (`validation_frames`): five consecutive lit frames of one sitting keep one
track (boxes move ≤ 2 px); the owner's frames from another sitting, 110 px higher, and the stranger
photo each start a new track. With a stranger's face pasted next to the owner's
(`examples/check_frames --store`), the owner's track is recognised (similarity 0.998) and live
while the stranger's reads −0.07 and stays unrecognised. Limits: a photo of the owner held
exactly where the face was continues the track (same place, same identity: the liveness votes
have to catch it); faces beyond the 3 largest are not analysed. Not implemented: the "closer to
another enrolled user" check (1:1 by design).

### 8. Template augmentation (F31, F53)

After a `MATCH`, the best-matching print of a live-voting frame is added to the template, once
per session, if:

- it is a borderline match, as in Windows: estimated FAR in [5e-6, 1e-5) (`PrintMatch::is_borderline`,
  F53); with the shipped table that is similarity 0.50–0.52;
- generation of the stored print it matched + 1 ≤ 3 (enrolled prints are 0).

New prints get `source: "augment"`; at most 10 augmented prints per user, the oldest augmented
print is dropped first; enrolled prints are never dropped. The store is saved right away (atomic
rename, mode 0600); a TPM-sealed store stays sealed.

### 9. Enroll-on-failure (F31, opt-in)

Off unless the daemon runs with `--enroll-on-failure` (OpenRC: `LINUX_HELLO_ARGS="--enroll-on-failure"`
in `/etc/conf.d/linux_hello`). Windows' `EnrollOnFailure` / `AugmentUserProfileOnFailure` add a
print from a failed face attempt after the user signs in another way. Here:

1. A session ends in `NOMATCH`. Its candidate is the best-matching print of a live-voting (good
   lighting) frame, if the face was "deemed real" by its own votes (≥ 2 live votes counted over all
   its frames, `FaceNotDeemedAsARealFace`) and it was the only face track of the session
   (`AbortingSinceOtherFacesWereSeenDuringTrackedSession`; a second face in view, or the same face
   back after a second, is another track). The daemon keeps it in memory with a
   random 128-bit token and replies `NOMATCH 0.430 <token>`. Any later `AUTH` drops it.
2. The PAM module keeps `<user> <token>` in the PAM handle. When the password (or another module)
   then authenticates the user, the application calls `pam_setcred`; the module sends
   `ENROLL_ON_FAILURE <user> <token>`. A face match never leaves a token behind, and a face check
   later in the same transaction replaces it.
3. The daemon accepts the command only from uid 0 (`SO_PEERCRED`), only for the same user and
   token, and only ≤ 60 s after the face check. It adds the print if:
   - its similarity to the user's enrolled and augmented prints is ≥ 0.40
     (`ENROLL_ON_FAILURE_THRESHOLD`). Windows reads `EnrollOnFailureThresholdToMatch`,
     `…RelaxedThresholdToMatch` and `…ThresholdToNotMatchOtherUsers` from a policy block that its
     caller copies into the engine at runtime (`sub_180041800`, engine config floats 730–732); the
     values are not in the binaries. 0.40 is the per-attempt FAR ≈ 1e-5 point of
     [calibration.md](calibration.md) (simulated 9.1e-6 for 15 prints × 16 frames), the FAR Windows
     targets for unlocking (F27);
   - no other enrolled user matches it (estimated FAR < 1e-5, as a recognition).

   It replies `ADDED 0.432` or `REFUSED no_pending | not_enrolled | too_far <s> | other_user <user> <s>`
   and logs the result at info (`enroll-on-failure user=cole result=ADDED 0.432`); the PAM module logs
   the reply and, after `ADDED`, tells the user from the same `pam_sm_setcred`
   (`PAM_TEXT_INFO` "Face print added after password login"), so a print never appears in the
   template without the user hearing of it. A refusal is only logged.

New prints get `source: "on_failure"` and generation 3, so a later match against them never adds
an augmented print. At most 5 per user, the oldest dropped first. They are matched like any other
print, but the 0.40 check ignores them, so a chain of on-failure prints cannot drift away from the
enrolled face.

**Risk.** Someone who knows the user's password and sits at the camera can get their own face
added, and then unlock with the face. The 0.40 check limits this to faces that already look like
the user's: for a stranger that happens about once in 100,000 attempts on LFW (the same rate
Windows accepts for an unlock), while family members and look-alikes are the realistic case. A
stranger who knows the password can already run sudo, but enroll-on-failure turns that into
lasting face access, which the owner may not notice. Hence it is off by default. Prints from a
photo or screen are not candidates when the liveness votes reject them.

## Daemon reply and PAM messages

The first word is unchanged from earlier versions, so older PAM modules keep working:

| Reply | PAM | Message |
|---|---|---|
| `MATCH 0.812` | success | — |
| `NOMATCH 0.310`, `NOMATCH 0.430 <token>` (enroll-on-failure candidate) | auth error | Face not recognized |
| `NOTLIVE` | auth error | Face not accepted as live |
| `NOFACE too_dark` / `too_bright` / `clipped` / `too_far` / `edge` / `occluded` / `not_frontal` / `no_face` | auth error | Face too dark: move closer… / Face too bright: move back… / Face too bright: move back or turn away from the lamp / Face too small: move closer… / Face at the edge… / Face covered… / Face turned away… / No face seen… |
| `UNAVAILABLE lid_closed` | ignore | Face login not available: the lid is closed |
| `NOTENROLLED`, `ERROR …` | ignore | — |

`UNAVAILABLE lid_closed` (`IsLidClosedScenario`): the daemon answers it before opening a camera
when `/proc/acpi/button/lid/*/state` says `closed` and the IR camera is built in. Built in means the
USB device above `/sys/class/video4linux/<videoN>/device` has `removable` = `fixed` (this
laptop's `/dev/video2`, USB 3-4, reports `fixed`); `removable` = `removable` is an external camera,
which is still used with the lid closed, and anything else counts as built in. A PAM module from
before this reply treats the unknown first word as "not available" too.

## Store format

```json
{ "cole": { "prints": [ { "features": [512 floats], "generation": 0, "source": "enroll" }, ... ] } }
```

`source` is `enroll`, `augment` or `on_failure`; stores written before enroll-on-failure read unchanged.

Stores from before this format (one averaged `features` vector per user) are refused with an
error that asks to re-enroll: `linux_hello enroll <user>` replaces the template. A sealed old store
must be re-enrolled the same way.

## Enrollment (F54, F28)

`linux_hello enroll <user> [--frames 16] [--add] [--force]` tells the user once to look at the
camera and move the head a little, as when talking to someone, then reads frame pairs from the IR
camera and keeps one print per pair that passes every frame gate (§2, with the enrollment
occlusion threshold) with good lighting (§3), until it has `--frames` prints — 16 by default, the
per-enrolment count of Windows' v5 parameter block (F54) — or 10 s have passed
(`ENROLL_PRINTS`, `ENROLL_TIMEOUT`). The terminal shows `7/16` as prints come in. A print whose
cosine similarity to a print already collected is above 0.98 (`DUPLICATE_PRINT`) is the same view
of the face and does not count, so 16 frames of a face held still do not make 16 prints; on this
camera consecutive lit frames of a still sitter score 0.96–0.99 to each other and frames seconds
apart 0.96–0.98 (`validation_frames`), so the rule drops only near-identical frames and small
natural movement gives new prints. At the end the command reports what it collected and why
frames were skipped (`near_duplicate 12, too_dark 3, weak_lighting 2`). A new template is stored
when at least 4 prints were collected (`MIN_PRINTS`, the count a session needs to recognise); with
fewer the command fails and says so. There is no pose guidance beyond the prompt: Windows' multi-pose
enrolment is approximated by the natural movement and the near-duplicate rule.

`--add` is Windows' "improve recognition": the new prints are appended to the user's existing
template (e.g. with glasses on, or in different light) instead of replacing it, as `enroll`
prints of generation 0. A template holds at most 100 prints of all kinds (`MAX_PRINTS`, F54);
`--add` collects at most the room left and refuses a full template. Near-duplicates of the prints
already in the template do not count either, so adding in the same conditions adds little.
`--add` needs at least one new print.

Before storing, both modes compare every new print with the templates of the other enrolled users,
as Windows' `CheckForDuplicateEnrollment`, with the same FAR estimate as a recognition (§6, so
against the other user's print count). If one matches (estimated FAR < 1e-5), enrollment fails and
names the closest such user: `this face matches enrolled user alice (similarity 0.641, FAR
2.3e-7); use --force to enroll it anyway`. `--force` stores the prints anyway (twins, or one
person with two accounts). Re-enrolling a user compares only with the others. Checked on saved
frames (library API): a template from two frontal owner frames matches the other two at 0.965 and
0.983 (FAR 1.4e-8, 1.3e-8); enrolling those two as another user is refused naming the owner;
`--add` appends them (4 prints, the FAR of a perfect match then reads 2.5e-8, from 1.3e-8 with 2
prints); the six faces of the stranger photo score −0.05…0.03 against the owner and are not refused.

`contrib/install.sh` runs `enroll` the same way (16 prints, 10 s), with the prompt and progress on
the terminal of the `sudo` call.

## Inspecting

`linux_hello watch [--seconds 30]` prints one line per pair with one part per analysed face: its
track id, face brightness lit/ambient, `d`, status or rejection reason, RGB P(real), vote,
similarity and its estimated FAR, whether the track is recognised and its liveness state, then
the physics checks' scores (`glint=0.53 shape=0.69 …`, [liveness-physics.md](liveness-physics.md))
(`log.jsonl` has the same under `faces`, the scores under `physics`). It saves `NNNN_{lit,ambient,diff,rgb}.png` and `log.jsonl` to `--out` (default
`/tmp/linux_hello_watch`). When a session reaches a verdict it prints it and starts a new one.
`watch` does not augment the template.
