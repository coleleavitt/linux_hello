# Capture, liveness and matching

How `linux_hello` turns camera frames into a verdict, and how this follows Windows Hello Face.
F- and L-ids refer to [windows-hello-re.md](windows-hello-re.md); the gaps that remain are listed
in [feature-gap.md](feature-gap.md). Only open models are used (SCRFD, ArcFace, FaceNox,
MiniFASNetV2); the Microsoft models extracted for analysis are not.

## Pipeline per frame pair

```
/dev/video3 (UVCM metadata) ─ illumination flag per frame ─┐
/dev/video2 (IR 640x360 @15) ───────────────────────────────┴─▶ lit/ambient pair (< 300 ms apart)
  lit frame ─▶ SCRFD face ─▶ size/edge gates ─▶ Windows IR detector face (box, 5 points) ─▶ ambient warp
     ─▶ face brightness + lighting status ─▶ occlusion, frontal pose
     good ─▶ embed lit − min(lit, warped ambient) ─▶ live vote unless the RGB models veto
     weak ─▶ embed the lit frame ───────────▶ no vote
     too dark / too bright / too small / edge / occluded / not frontal / no face ─▶ no print, no vote (reason counted)
/dev/video0 (RGB) ─▶ SCRFD face ─▶ FaceNox + MiniFASNetV2 mean P(real) ─▶ veto if < 0.5
  track: largest face followed by box overlap; a new face starts a new attempt
  attempt: ≥ 4 prints and estimated FAR of the best print < 1e-5 (similarity > 0.50) → recognised;
           from then on 2 live votes → MATCH, 3 spoof votes → NOTLIVE
  session: 16 pairs over all attempts → give up; once more if the face moved or the exposure changed
  after MATCH: add the best live print if it was a borderline match
```

### 1. Lit/ambient pairs (F10, F12, F13, F15, L1–L3)

The IR emitter lights every other frame by itself (L1). The camera's uvcvideo metadata node
(`/dev/video3`, `--ir-metadata`) carries Microsoft's `MetadataId_FrameIllumination` item for every
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

### 2. Frame gates (F45, F46)

The face (largest SCRFD detection) is found on the lit frame. With the Windows Hello models, the
Windows IR detector (`MsRetinaDetector`, MODEL_FILES_110) then runs on the lit frame and its face
overlapping the SCRFD one (IoU ≥ 0.5, `Analysis::windows_face`) is the face Windows would track: its
five points give the brightness block (§3), the ambient-warp anchors (§4) and the liveness crops (§5),
its box the occlusion crop, its 6-DoF output the pose. Windows' 31-point landmark set is, on this
path, those five points plus copies of them (`RetinaEngine::ComputeAlignment`, lbf.md "Which aligner
runs"), so nothing else is aligned; the legacy LBF aligner does not run. SCRFD's own box and points
keep two jobs: the size/edge gates and the ArcFace recognition crop, whose alignment template is
SCRFD's. Without the Windows Hello models SCRFD's points stand in everywhere.

A pair gives no print and no vote if:

| Reason (`NOFACE` word) | Check | Windows |
|---|---|---|
| `no_face` | no detection | — |
| `too_far` | box width < 72 px or < 0.12 × shorter frame side (43 px at 640×360) | F46, F54 |
| `edge` | box closer to a frame edge than inter-eye distance / 5 | F46 |
| `too_bright` | lit face brightness > 205 | F44 status 2 |
| `too_dark` | lit face brightness < 35 | F44 status 3 |
| `occluded` | Windows Hello occlusion model: 1 − p(none) > 0.9 (> 0.5 while enrolling); only with the converted models | F46 |
| `not_frontal` | Windows Hello IR detector's head pose: \|yaw\| > 20° or \|pitch\| > 25°, or that detector has no face overlapping the SCRFD face (IoU ≥ 0.5; checked right after the size/edge gates, before the lighting status); only with the converted models | F46, F59 |

The occlusion model is the converted Windows Hello one (`models/windows_hello/converted/occlusion`,
docs/winhello-models/cefa.md). Its input is the face box resampled to 128×128, as in Windows; the box
is the Windows IR detector's (on the owner's frontal frames p(none) is 0.991–0.999 with it, 0.961–0.998
with SCRFD's box).

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
| weak (4) | d < 45, or ambient > 25 (strong ambient IR, e.g. sunlight) | print from the lit frame, used for matching; no vote; not used for augmentation |
| good (1) | otherwise | print from `lit − min(lit, ambient)`; votes |

Windows also turns `d > 205` into status 2; with lit ≤ 205 that cannot happen. It smooths the
ambient brightness over frames and counts clipped pixels (status 6/7); linux_hello does neither.

### 4. Ambient subtraction (F12, F13, F40)

For good frames, recognition uses `diff = lit − min(lit, ambient)` per pixel, the light the
emitter added. This removes sunlight and other ambient IR, so enrollment and unlock see the same
lighting. Enrollment uses only good frames, so every enrolled print is a difference image.

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
  face (box IoU ≥ 0.3, the tracker's rule, for Windows' same-tracking-id condition). Otherwise, and
  for the first pair of a face, the ambient frame is subtracted as captured. Windows also requires
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
| RGB, only without the Windows Hello models: FaceNox + MiniFASNetV2 mean P(real) < 0.5, or no face in the RGB frame | −2 |

Frame total > 0 is a live vote, < 0 a spoof vote. Weak and rejected frames do not vote (and the
liveness models are not run for them), so with the Windows Hello models a frame is live only when
the lighting status and the Windows nets agree.

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

### 7. Decision (F25, F47, F48)

The order follows Windows' `MakeDecisions`: recognise first, then liveness on the same face.
"The same face" is the face track (F37, F47): `FaceTracker` follows the largest face of each pair
from pair to pair while its box overlaps the last one (IoU ≥ 0.3). A face that does not overlap,
or that comes back after 7 pairs (≈ 1 s, Windows' 1000 ms purge, F37) without a face, starts a new
track, and a new track starts a new attempt: its prints, best similarity and liveness votes start
from zero. So the owner's prints never recognise a photo held up next, and the owner's live votes
never count for it. Pairs without a face keep the current track. The 16-pair limit counts the
whole session (twice, after a re-attempt, below).

1. **Recognise:** once the track has ≥ 4 prints (good or weak frames) and the estimated FAR of
   the best similarity over them is < 1e-5 (similarity > 0.50 with the shipped table and 15
   stored prints, 0.503 with 16).
2. **Liveness:** votes count only from the recognising frame on. ≥ 2 live votes → `MATCH <best
   similarity>`; else ≥ 3 spoof votes → `NOTLIVE`.
3. **Give up** after 16 frame pairs (Windows: 48 frames or a counter > 16; one pair is two frames):
   the current track recognised → `NOTLIVE`; ≥ 4 prints but no match → `NOMATCH <best>`; fewer
   than 4 prints → `NOFACE <most frequent rejection reason over the session>`. Earlier, while the
   track is not yet recognised, 4 consecutive occluded frames (§2) end the session with `NOFACE
   occluded` (F46, `IdentifyAsGuest(3)`): a mask or a hand will not go away within the budget, so
   the password prompt comes ≈ 1.6 s sooner. A clear frame in between starts the count over; a new
   track starts it over too.

The earliest possible `MATCH` is therefore at the 5th pair (4 prints, the 4th frame votes live,
the 5th too), where voting from the first pair allowed a match at the 2nd pair.

**Re-attempt (F49).** Before a session ends in `NOMATCH` or `NOTLIVE` (at the pair limit or by three
spoof votes), it checks what the tracked face did over the attempt's frames, as Windows'
`sub_1800575E0` does for a track about to become a guest. If the face moved — the range of its box
centre, normalised by the frame width and height, has ((Δx)² + (Δy)²) / 2 > 0.01 (about 0.14 of
the frame), or its box width changed by more than 20 % — or the exposure changed — the standard
deviation of the last 10 lit face brightness values (§3, including rejected too-dark/too-bright
frames) is above 12 — the session gets another 16 pairs with fresh counters for the track (prints,
best similarity, votes), once per session (`ReAttemptRecognitionByMovement`,
`ReAttemptRecognitionByExposure`). A still face in steady light is rejected at the 16th pair as
before; `NOFACE` is never re-attempted (nothing was recognised). The motion record starts over
with a new track, so a photo replacing the owner does not inherit the owner's movement. Windows
also refuses the re-attempt after more than one "too many saturated pixels" frame; linux_hello
has no saturated-pixel count (F44 status 6/7), so that part is left out. A re-attempted session
reads up to 32 pairs, ≈ 4.3 s at 15 fps plus the ≈ 0.5 s start-up, within the PAM module's 10 s
timeout.

Checked on saved frames (`validation_frames`): five consecutive lit frames of one sitting keep one
track (boxes move ≤ 2 px); the owner's frames from another sitting, 110 px higher, and the stranger
photo each start a new track. Limits: only the largest face is tracked; a photo held exactly where
the face was (IoU ≥ 0.3) continues the track. Not implemented: the "closer to another user" check
(1:1 by design).

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
   (`AbortingSinceOtherFacesWereSeenDuringTrackedSession`). The daemon keeps it in memory with a
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
| `NOFACE too_dark` / `too_bright` / `too_far` / `edge` / `no_face` | auth error | Face too dark: move closer… / Face too bright: move back… / Face too small: move closer… / Face at the edge… / No face seen… |
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

`linux_hello watch [--seconds 30]` prints one line per pair: face brightness lit/ambient, `d`,
status or rejection reason, RGB P(real), vote, similarity and its estimated FAR per attempt, the
face track, whether the track's face is recognised and its liveness state. It saves `NNNN_{lit,ambient,diff,rgb}.png` and `log.jsonl` to `--out` (default
`/tmp/linux_hello_watch`). When a session reaches a verdict it prints it and starts a new one.
`watch` does not augment the template.
