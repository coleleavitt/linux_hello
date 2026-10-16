# Capture, liveness and matching

How `linux_hello` turns camera frames into a verdict, and how this follows Windows Hello Face.
F- and L-ids refer to [windows-hello-re.md](windows-hello-re.md); the gaps that remain are listed
in [feature-gap.md](feature-gap.md). Only open models are used (SCRFD, ArcFace, FaceNox,
MiniFASNetV2); the Microsoft models extracted for analysis are not.

## Pipeline per frame pair

```
/dev/video3 (UVCM metadata) ─ illumination flag per frame ─┐
/dev/video2 (IR 640x360 @15) ───────────────────────────────┴─▶ lit/ambient pair (< 300 ms apart)
  lit frame ─▶ SCRFD face ─▶ size/edge gates ─▶ face brightness + lighting status
     good ─▶ embed lit − min(lit, ambient) ─▶ live vote unless the RGB models veto
     weak ─▶ embed the lit frame ───────────▶ no vote
     too dark / too bright / too small / edge / no face ─▶ no print, no vote (reason counted)
/dev/video0 (RGB) ─▶ SCRFD face ─▶ FaceNox + MiniFASNetV2 mean P(real) ─▶ veto if < 0.5
  track: largest face followed by box overlap; a new face starts a new attempt
  attempt: ≥ 4 prints and best similarity > threshold → recognised;
           from then on 2 live votes → MATCH, 3 spoof votes → NOTLIVE
  session: 16 pairs over all attempts → give up
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

### 2. Frame gates (F45, F46)

The face (largest SCRFD detection) is found on the lit frame. A pair gives no print and no vote if:

| Reason (`NOFACE` word) | Check | Windows |
|---|---|---|
| `no_face` | no detection | — |
| `too_far` | box width < 72 px or < 0.12 × shorter frame side (43 px at 640×360) | F46, F54 |
| `edge` | box closer to a frame edge than inter-eye distance / 5 | F46 |
| `too_bright` | lit face brightness > 205 | F44 status 2 |
| `too_dark` | lit face brightness < 35 | F44 status 3 |
| `occluded` | Windows Hello occlusion model: 1 − p(none) > 0.9 (> 0.5 while enrolling); only with the converted models | F46 |

The occlusion model is the converted Windows Hello one (`models/windows_hello/converted/occlusion`,
docs/winhello-models/cefa.md). Its input is the face box resampled to 128×128, as in Windows. Not
implemented: the frontal-pose check (|yaw| ≤ 20°, |pitch| ≤ 25°, F59). The converted head-pose
model reads about +25° of yaw on frontal faces from this camera (docs/winhello-models/lbf.md), so
a pose gate built on it would reject most real frames.

### 3. Face brightness and lighting status (F14, F43, F44)

Face brightness (F43) is the mean of every 2nd pixel in every 2nd row of the block between the
eyes and the mouth corners. Windows bounds it with eye-contour points of its 31-point aligner;
with SCRFD's 5 points the block spans x from the inner of (left eye, left mouth corner) to the
inner of (right eye, right mouth corner), and y from the lower eye to the higher mouth corner. It
is measured on the lit and on the ambient frame: `lit`, `ambient`, `d = lit − ambient`.

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

Difference from Windows: Windows warps the ambient face crop onto the lit one with an affine
transform from eye and mouth anchors before subtracting (F13, F40). `linux_hello` subtracts the
whole frames without motion compensation: the two frames are 67 ms apart, and a face moves little
in that time.

### 5. Liveness votes (F22–F25)

Windows' live votes come from two IR anti-spoofing CNNs; the colour CNN can only veto (F24).
For a good frame:

| Evidence | Vote |
|---|---|
| IR: without the Windows Hello models, the good lighting status (reflects the emitter like a nearby face, little ambient IR) | +1 |
| IR: with the Windows Hello models, their frame total (F25), see below | −3…+1 |
| RGB models (FaceNox + MiniFASNetV2) mean P(real) < 0.5, or no face in the RGB frame | −2 |

Frame total > 0 is a live vote, < 0 a spoof vote. Weak and rejected frames do not vote (and the
liveness models are not run for them), so with the Windows Hello models a frame is live only when
the lighting status and the Windows nets agree.

**Windows Hello frame total** (when `models/windows_hello/converted` exists; `--no-windows-hello`
turns it off). Good frames are the frames Windows sends to its diffusion net (status 1, ambient
≤ 25, F40, F42), so:

| Net (slot) | Input | Rule (flag-on thresholds, mode 0) |
|---|---|---|
| diffusion IR (12) | `lit − min(lit, ambient)`, 5-point aligned 128×128 crop | p(real) > 0.5037 → +1 |
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

`MATCH_THRESHOLD` is 0.50, calibrated on grayscale LFW ([calibration.md](calibration.md)): one
comparison FAR ≈ 4e-8 (tail fit), simulated per-attempt FAR ≈ 1.8e-6 for 15 prints × 16 frames,
FRR 0.23% per probe frame against 15 prints. Windows instead converts the distance into an
estimated FAR with a calibration table and accepts below 1e-5 (F27, F51); `linux_hello` uses one
fixed cosine threshold chosen for the same 1e-5 target per attempt.

### 7. Decision (F25, F47, F48)

The order follows Windows' `MakeDecisions`: recognise first, then liveness on the same face.
"The same face" is the face track (F37, F47): `FaceTracker` follows the largest face of each pair
from pair to pair while its box overlaps the last one (IoU ≥ 0.3). A face that does not overlap,
or that comes back after 7 pairs (≈ 1 s, Windows' 1000 ms purge, F37) without a face, starts a new
track, and a new track starts a new attempt: its prints, best similarity and liveness votes start
from zero. So the owner's prints never recognise a photo held up next, and the owner's live votes
never count for it. Pairs without a face keep the current track. The 16-pair limit counts the
whole session.

1. **Recognise:** once the track has ≥ 4 prints (good or weak frames) and the best similarity
   over them is > 0.50.
2. **Liveness:** votes count only from the recognising frame on. ≥ 2 live votes → `MATCH <best
   similarity>`; else ≥ 3 spoof votes → `NOTLIVE`.
3. **Give up** after 16 frame pairs (Windows: 48 frames or a counter > 16; one pair is two frames):
   the current track recognised → `NOTLIVE`; ≥ 4 prints but no match → `NOMATCH <best>`; fewer
   than 4 prints → `NOFACE <most frequent rejection reason over the session>`.

The earliest possible `MATCH` is therefore at the 5th pair (4 prints, the 4th frame votes live,
the 5th too), where voting from the first pair allowed a match at the 2nd pair.

Checked on saved frames (`validation_frames`): five consecutive lit frames of one sitting keep one
track (boxes move ≤ 2 px); the owner's frames from another sitting, 110 px higher, and the stranger
photo each start a new track. Limits: only the largest face is tracked; a photo held exactly where
the face was (IoU ≥ 0.3) continues the track. Not implemented: the re-attempt on movement or
exposure change (F49) and the "closer to another user" check (1:1 by design).

### 8. Template augmentation (F31, F53)

After a `MATCH`, the best-matching print of a live-voting frame is added to the template, once
per session, if:

- it is a borderline match: similarity in (0.50, 0.55] (`AUGMENT_MARGIN` 0.05). Windows augments
  only when the estimated FAR is in [FAR/2, FAR]; a fixed band above the cosine threshold stands
  in for that;
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
   - no other enrolled user matches it above `MATCH_THRESHOLD`.

   It replies `ADDED 0.432` or `REFUSED no_pending | not_enrolled | too_far <s> | other_user <user> <s>`;
   the PAM module only logs the reply.

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

## Enrollment

`linux_hello enroll <user> [--frames 5]` reads frame pairs from the IR camera until it has
`--frames` pairs that pass every frame gate with good lighting (at most 4 pairs per print), and
stores one print per pair.

Before storing, it compares every new print with the templates of the other enrolled users, as
Windows' `CheckForDuplicateEnrollment`. If one matches above `MATCH_THRESHOLD` (0.50), enrollment
fails and names the closest such user: `this face matches enrolled user alice (similarity 0.641);
use --force to enroll it anyway`. `--force` stores the prints anyway (twins, or one person with two
accounts). Re-enrolling a user compares only with the others. Checked on saved frames: the owner's
lit frames from one sitting against a template from another sitting match at 0.641; the six
faces of the stranger photo score −0.05…0.03 against the owner and are not refused.

## Inspecting

`linux_hello watch [--seconds 30]` prints one line per pair: face brightness lit/ambient, `d`,
status or rejection reason, RGB P(real), vote, similarity, the face track, whether the track's
face is recognised and its liveness state. It saves `NNNN_{lit,ambient,diff,rgb}.png` and `log.jsonl` to `--out` (default
`/tmp/linux_hello_watch`). When a session reaches a verdict it prints it and starts a new one.
`watch` does not augment the template.
