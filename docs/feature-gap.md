# Windows Hello Face vs linux_hello: feature gap

Source of the Windows side: [windows-hello-re.md](windows-hello-re.md) (F-ids) and the converter notes in
[winhello-models/](winhello-models/). Source of the linux_hello side: the code in `src/`, `pam/`, `contrib/`,
`scripts/` and `docs/` as of 2026-10-15 (read, not the README alone).

Status values: **Done**, **Partial**, **Missing**, **N/A** (with the reason). Rows marked Done on 2026-10-15 were
implemented on branch `parity` (see [capture-and-liveness.md](capture-and-liveness.md)).

## Capture & emitter

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| IR camera found by sensor-group / FaceAuth profile, hidden from apps (F4, F6) | Done (different mechanism): CLI `--device` (default `/dev/video2`) and `--ir-metadata` (default `/dev/video3`); `find_ir_camera` (unused) probes by card name | Optional: match by USB VID:PID / `V4L2_CAP_META_CAPTURE` sibling instead of name |
| IR format = first FaceAuth format; warn < 7.5 fps or < 340 px (F7) | Partial: `camera.rs` asks 640×480 GREY then YUYV; driver gives 640×360 | Read back the actual format and refuse/ warn below 340 px or 7.5 fps |
| Emitter alternate-frame mode via `FACEAUTH_MODE` = 2 (F8, F9, F16, F17) | N/A on this camera: the IR function strobes by itself and has no MSXU face-auth control (L1, L4, L5) | Only needed for other cameras: UVC XU `{0F3F95DC…}` selector 6 via `UVCIOC_CTRL_QUERY` |
| Per-frame lit/unlit flag from `MF_CAPTURE_METADATA_FRAME_ILLUMINATION` (F10, F19) | **Done**: `IrCamera` reads `UVCM` metadata from `/dev/video3` (item 6, `Flags & 1`), paired with video by sequence; frames without a flag are dropped (L2, L3) | — |
| Brightness-based strobe guess only for replay (F15) | **Done**: fallback only with `--no-ir-metadata` (brighter of two consecutive frames, F15 sampling) | — |
| Warm-up: exposure settles ≈ 11 frames (L1) | **Done**: `WARMUP_FRAMES = 4`; the camera resumes near its last exposure (L7) and a ±35 % brightness offset does not move match scores; first pair ≈ 0.54 s after opening (was ≈ 1.6 s) | — |
| RGB camera in parallel for colour liveness (F22) | Done: `OpenCameras` opens IR first, then RGB (≈ 50 ms); RGB dark-frame skip (`DARK_RATIO 0.5`, 4 skips); an RGB frame that does not come within 1 s (`RGB_FRAME_TIMEOUT`) drops the RGB camera for the session, the colour net is skipped like a Windows classifier with missing input (F25 status 6/7) | — |
| Secure camera path (`SECURE_MODE`, SecureUSBVideo trustlet, VTL1 frame buffers) (F8, F36) | N/A: needs VBS/trustlets; no Linux equivalent for UVC | Closest: run capture + matching only in the root daemon (already so) |
| Driver face-detection property (`FACEDETECTION`, id 29) (F8) | N/A: not used for auth; camera does not expose it | — |
| ROI / exposure control on the face (`SensorController`, `ToggleFaceDetectionOnDriverIfCapable`) (Q1) | **Done**: `IrCamera::expose_on` sets the UVC region-of-interest rectangle to the last face box with auto-exposure on, after every pair (L6); defaults restored at open and close; no-op without the controls | — |

## Frame pairing & ambient light

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Pair unlit + next lit frame, ambient < 300 ms older (F12, F13) | **Done**: consecutive lit/ambient frames < 300 ms apart by buffer timestamps (either order) | — |
| Ambient crop = previous face crop + 100 px per side (F39) | Different: the whole ambient frame is warped and subtracted (1.5 ms at 640×360) | Crop around last known box to save work (optimisation only) |
| Recognition image = lit − min(lit, warped ambient), affine from eye/eye/mouth anchors, only when lighting status = 1 (F13, F40) | **Done**: the ambient frame is warped onto the lit one by the affine through the three anchors (eyes, mouth midpoint), their ambient-frame positions interpolated from the previous lit frame (< 300 ms, same face) — capture-and-liveness.md §4; the first pair of a face is subtracted as captured | — |
| Liveness image = lit − min(lit, ambient) unwarped, only if status 1 and ambient ≤ 25 (F40) | **Done**: the diffusion net gets the unwarped difference (`MsIrLiveness::frame`), the recognition print the warped one | — |
| Single-slot hand-off, newest frame wins (F11) | Done (sequential capture loop) | — |
| Driver background-subtraction mode (FACEAUTH_MODE 4) + background brightness check (F9, F44) | N/A (camera does alternate frames) | — |

## Face detection, crops & quality gates

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Detection on the lit frame, tracker across frames (FaceTrackerInternal, RetinaEngine ONNX or LBF aligner) (Q5, F58) | **Done**: SCRFD per pair on the lit frame, the 3 largest faces analysed (`MAX_FACES`); `FaceTracker` follows each by box IoU ≥ 0.3 and print cosine ≥ 0.50 (the latter an addition to Windows), dropped after 7 pairs (≈ 1 s, F37) without the face. The Windows IR Retina detector (MODEL_FILES_110, `MsRetinaDetector`) runs once on the lit frame; its face overlapping a SCRFD face (`Analysis::windows_face`) gives the frontal check, the brightness block, the ambient-warp anchors, the occlusion box and the liveness crops | — |
| 31-point landmark set (5 RetinaFace + eye contours + mouth) (F58) | **Done** as on Windows' RetinaEngine path: points 0–4 are the Windows IR detector's five, and points 5–30 are copies of them (`RetinaEngine::ComputeAlignment`, lbf.md "Which aligner runs"), so the F43 block and the F13 anchors follow from the five Retina points (`lighting::face_brightness`, `lighting::anchors`). The LBF aligner only runs on Windows' legacy path; `LbfAligner` stays a diagnostic (`examples/ms_check`) | — |
| Recognition crop 3× box, ≤ 400 px, 8-bit lit luminance (F38) | Different for ArcFace: 112×112 aligned crop from the RGB-converted IR frame. **Done** for the second opinion: Microsoft's net gets its own 281-canvas/128×128 five-point crop (`MsRecognizer::crop`, cefa.md §4) from the full frame; the 3× box is only Windows' intermediate buffer | Keep ArcFace alignment; do subtraction on a 3× crop before aligning |
| Face brightness = mean of 2×2-subsampled cheek/nose box (F43) | **Done**: block between the eye centres and the mouth corners of the Windows IR detector's five points (on Windows' Retina path the eye-contour points p9/p15 are the eye centres); ambient brightness on the warped ambient frame; SCRFD's points without the Windows models | — |
| Exposure status: > 205 too bright, < 35 too dark, clipped pixels > 1 % → reject (F44, F45) | **Done**: 205/35 rejected; pixels at 255 counted in the face ellipse (> 1 % → status 6, rejected as `NOFACE clipped`, PAM "move back or turn away from the lamp") and in the band around the face, the box grown by 25 % per side (> 1 % → status 7: print kept from the lit frame, no vote, no augmentation, like status 4) (`lighting::clipped`, `MAX_CLIPPED`, `CLIPPED_BAND`). Windows' clipped-pixel definition (equal to ±2 px neighbours) and band size are not reproduced | — |
| Lit − ambient ≥ 45, ambient ≤ 25 (F14, F44) | **Done**: otherwise status 4 (print kept for matching, no vote, no augmentation) | — |
| Min face width 72 px and ≥ 0.12 × min(frame w,h) (F46, F54) | **Done** (`too_far`) | — |
| Edge cut: box ≥ inter-ocular/5 from border (F46) | **Done** (`edge`, frame rejected) | — |
| Frontal pose: \|yaw\| ≤ 20°, \|pitch\| ≤ 25° (F46, F59) | **Done** (with the converted models): `FacePipeline::analyze` takes the pose of the Windows IR Retina detector (MODEL_FILES_110, `RetinaEngine::ComputeOrientation` angle order, F59) for the face overlapping SCRFD's; not frontal or no such face → `NOFACE not_frontal`. All 4 frontal owner frames pass (ldmf.md §11) | — |
| Occlusion detectors (eyes, mouth/nose, face; ONNX); 4 consecutive occluded frames → give up; occlusion mode after 30 frames ≥ 50 % (F46, F61) | **Partial**: face-level occlusion gate in `FacePipeline::analyze` (converted Windows Hello model, used when `models/windows_hello/converted` exists; 1 − p_none > 0.9, 0.5 while enrolling; face-box input as in `sub_18004B5B8`, the Windows IR detector's box); occluded frames give no print and the reply `NOFACE occluded`. **4 consecutive occluded frames while the track is not yet recognised end the session with `NOFACE occluded`** (`OCCLUDED_GIVE_UP`, `IdentifyAsGuest(3)`). The "occlusion mode" (30 frames ≥ 50 %, off after 15 clean frames) is an enrolment behaviour (F65): a mostly-occluded enrolment stops taking prints until the face has been clear for 15 frames; in an unlock the flag is only the give-up marker. Missing for `enroll` (which skips occluded frames and stops at 10 s anyway) | Optional: in `enroll`, after > 30 gated frames of which ≥ half were occluded, take no print until 15 clean frames in a row |
| Re-attempt by movement (centre range > 0.01, size ±20 %) or exposure (σ > 12) before rejecting (F49) | **Done**: a session that would end in `NOMATCH` or `NOTLIVE` gets another 16 pairs with a fresh `Attempt` for the deciding track, once per track as Windows, when that face's normalised centre ranged over ((Δx)² + (Δy)²)/2 > 0.01 or its box width changed > 20 %, or the σ of its last 10 lit brightness values (F43) is > 12, and not after more than one clipped-face frame (F44 status 6) (`Session`, `FaceMotion`, `MAX_CLIPPED_FRAMES`) | — |
| Colour-face correspondence + colour quality (edge cut, too close/far, low-confidence alignment) (`EvaluateColorFaceQuality`) | Partial: the RGB face whose normalised box centre is nearest to the IR face's; no registration, no colour quality gates | Map IR box to RGB with a fixed extrinsic offset; reject if none |
||||||| ab9fe12
| Occlusion detectors (eyes, mouth/nose, face; ONNX); 4 consecutive occluded frames → give up; occlusion mode after 30 frames ≥ 50 % (F46, F61) | **Partial**: face-level occlusion gate in `FacePipeline::analyze` (converted Windows Hello model, used when `models/windows_hello/converted` exists; 1 − p_none > 0.9, 0.5 while enrolling; face-box input as in `sub_18004B5B8`, the Windows IR detector's box); occluded frames give no print and the reply `NOFACE occluded`. **4 consecutive occluded frames while the track is not yet recognised end the session with `NOFACE occluded`** (`OCCLUDED_GIVE_UP`, `IdentifyAsGuest(3)`). The "occlusion mode" (30 frames ≥ 50 %, off after 15 clean frames) is an enrolment behaviour (F65): a mostly-occluded enrolment stops taking prints until the face has been clear for 15 frames; in an unlock the flag is only the give-up marker. Missing for `enroll` (which skips occluded frames and stops at 10 s anyway) | Optional: in `enroll`, after > 30 gated frames of which ≥ half were occluded, take no print until 15 clean frames in a row |
| Re-attempt by movement (centre range > 0.01, size ±20 %) or exposure (σ > 12) before rejecting (F49) | **Done**: a session that would end in `NOMATCH` or `NOTLIVE` gets another 16 pairs with a fresh `Attempt`, once, when the tracked face's normalised centre ranged over ((Δx)² + (Δy)²)/2 > 0.01 or its box width changed > 20 %, or the σ of its last 10 lit brightness values (F43) is > 12 (`Session`, `FaceMotion`). Windows' "not after too many saturated frames" is left out: there is no saturated-pixel count (F44 status 6/7 Missing) | — |
| Colour-face correspondence + colour quality (edge cut, too close/far, low-confidence alignment) (`EvaluateColorFaceQuality`) | Partial: largest face on RGB, no geometric correspondence to IR face | Map IR box to RGB with a fixed extrinsic offset; reject if none |

## Liveness

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| IR CNNs (general on lit crop, diffusion on difference crop), 128×128×1, SqueezeNet, cosine 2×512 head (F22, F23, F42, F62) | **Done** (converted Windows Hello model, used when `models/windows_hello/converted` exists): good-lighting frames run the diffusion net (slot 12) on the difference image, crop aligned on the Windows IR detector's five points (F23), flag-on mode-0 thresholds; the frame total (F25) must be > 0 together with the lighting gate for a live vote (`Vote::from_evidence`). The general net (slot 11) runs only in `examples/ms_check` | — |
| Colour CNN veto (BGR 128×128) (F22, F62) | **Done**: with the Windows Hello models, their colour net (slot 13) on the face found in the RGB frame; without them, FaceNox + MiniFASNetV2 mean < 0.5 vetoes a frame (−2, so a vetoed frame is a spoof vote) | — |
| Multi-task IR border / HH vetoes, enabled after first live vote (F22, F42) | **Done** (converted Windows Hello model, used when `models/windows_hello/converted` exists); they run on every good-lighting frame, not only after the first live vote, which can only add vetoes. The colour net (slot 13) runs on the face detected in the RGB frame | — |
| Thresholds: IR live > 0.5 (strict 0.9), spoof > 0.5 strict only; colour 0.5; border 0.7 (F24) | Partial: 0.5 on RGB mean | Use per-model thresholds once IR model exists |
| Frame voting: ≥ 2 live → accept, ≥ 3 spoof → reject, 48 frames → reject; counted from the recognition frame (F25, F48) | **Done**: live vote = good lighting status; counted from recognition; give up after 16 pairs | — |
| Fail closed if liveness enabled but no model loaded (F25) | Done-ish: `--no-liveness` disables explicitly; missing model fails at load | — |
| Lit/ambient physics (screens emit no IR; lighting gate) as implicit liveness (F14) | **Done**: the lighting status is the only live vote | — |
| Enhanced anti-spoofing policy (`FacialFeaturesUseEnhancedAntiSpoofing`) (F22, F55) | N/A (no policy layer) | A config flag selecting the strict thresholds |

## Recognition & thresholds

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Embedding CNN on 128×128×1 IR crop, ~256-d (F30) | **Done** as a second opinion (converted Windows Hello model, used when `models/windows_hello/converted` exists): `MsRecognizer` runs the v5 net (`MODEL_FILES_123`, `FaceRecognitionCnnImpl2` crop at the Windows IR detector's points, cefa.md §7) on the recognition image next to ArcFace R50 (112×112 RGB-replicated IR, 512-d), which stays the first opinion; prints store both features (`windows`, 256-d L2-normalised). Rust port exact to 4.5e-8 on identical crops; 4–6 ms per frame | — |
| Distance 1 − cos, clamped ≤ 1 (F50) | Done (cosine similarity for ArcFace; `recognizer::distance` for Microsoft's feature) | — |
| Min distance over all stored prints (+coupled mean/pair prints) (F28) | **Done** (max cosine over prints for ArcFace, min engine distance over the prints with a Windows feature for Microsoft's net); coupled prints Missing | Coupled prints optional |
| Distance → FAR via per-hardware calibration table, × 1…4 correction; accept FAR < 1e-5 (F27, F51) | **Done** (same structure, LFW data): `models/calibration.json` holds (FPR, TAR, similarity) rows per camera (USB `vid:pid`) with a default; `CalibrationTable::pair_far` interpolates as `sub_1800397B4` (linear, exponential past the best row), × the template's prints × 16 pairs (the comparisons one attempt can make; 240 for 15 prints) stands in for Windows' × 1…4; recognised when `far < 1e-5` (`TARGET_FAR`), the former 0.50 for 15 prints, 0.503 for 16. Logs print `far=` next to `similarity=`. **Microsoft's own table** (the v5 container header, FAR 1e-5 at distance 0.350) is read from `converted/cefa/MODEL_FILES_123.json` (`calibration::load_windows`, git-ignored, not shipped) and gives the second opinion's FAR as Windows uses it (× 1: one verification condition); recognised only if ArcFace's FAR < 1e-5 and, with a second opinion, Microsoft's too (`PrintMatch::recognised`). Logs add `ms=` `ms_far=` (calibration.md "Microsoft's table"). Tooling for the IR check: `examples/impostor_capture` records gated pairs of a consenting volunteer, `examples/calibrate_ir` scores them with the pipeline's own crop against the template and both tables (`docs/calibration.md`, "IR impostor check") | Capture real IR impostors (none yet) and, if they sit above the table, build an IR-calibrated ArcFace table |
| ≥ 4 prints on the track before deciding (F48, F54) | **Done** per track: every track has its own `Attempt` (prints, best similarity, votes) within the 16-pair session | — |
| 1:N with "closer to another tracked/enrolled user" check (F48) | **Partial**: "no other tracked face matches the same user better" is Done (a track is recognised only if no other face of the pair has a lower estimated FAR for the user, `Session::add`); other enrolled users are N/A by design (1:1 for the PAM user) | Could add: reject if another enrolled user scores higher |
||||||| ab9fe12
| Distance → FAR via per-hardware calibration table, × 1…4 correction; accept FAR < 1e-5 (F27, F51) | **Done** (same structure, LFW data): `models/calibration.json` holds (FPR, TAR, similarity) rows per camera (USB `vid:pid`) with a default; `CalibrationTable::pair_far` interpolates as `sub_1800397B4` (linear, exponential past the best row), × the template's prints × 16 pairs (the comparisons one attempt can make; 240 for 15 prints) stands in for Windows' × 1…4; recognised when `far < 1e-5` (`TARGET_FAR`), the former 0.50 for 15 prints, 0.503 for 16. Logs print `far=` next to `similarity=`. **Microsoft's own table** (the v5 container header, FAR 1e-5 at distance 0.350) is read from `converted/cefa/MODEL_FILES_123.json` (`calibration::load_windows`, git-ignored, not shipped) and gives the second opinion's FAR as Windows uses it (× 1: one verification condition); recognised only if ArcFace's FAR < 1e-5 and, with a second opinion, Microsoft's too (`PrintMatch::recognised`). Logs add `ms=` `ms_far=` (calibration.md "Microsoft's table"). Tooling for the IR check: `examples/impostor_capture` records gated pairs of a consenting volunteer, `examples/calibrate_ir` scores them with the pipeline's own crop against the template and both tables (`docs/calibration.md`, "IR impostor check") | Capture real IR impostors (none yet) and, if they sit above the table, build an IR-calibrated ArcFace table |
| ≥ 4 prints on the track before deciding (F48, F54) | **Done** per track: a new track starts a new `Attempt` (prints, best similarity, votes) within the 16-pair session | — |
| 1:N with "closer to another tracked/enrolled user" check (F48) | N/A by design: 1:1 for the PAM user | Could add: reject if another enrolled user scores higher |
| Recognise first, then liveness on the same track (F47, F48) | **Done** (`Attempt`) | — |

## Enrollment

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Multi-pose enrolment, 16 prints, quality-gated frames, registration per hardware model (F54, Q5) | **Done** (prompt instead of pose guidance): `enroll` collects 16 prints from gated good-lighting frames over a few seconds (10 s cap), tells the user once to look at the camera and move naturally, shows `7/16`; a print with cosine > 0.98 to one already collected does not count; ≥ 4 prints make a template, the command reports what was collected and skipped. `enroll --add` appends prints to the existing template ("improve recognition") up to 100 per user (`MAX_PRINTS`, F54), duplicate-user check included. The per-attempt FAR bound follows the real template size (prints × 16 comparisons, `CalibrationTable::far`) | Per-pose guidance in the UI if a graphical enroller appears |
| Duplicate-enrolment check (`CheckForDuplicateEnrollment`) | **Done**: `enroll` refuses when a new print matches another user's template (estimated FAR < 1e-5, i.e. similarity > 0.50) and names that user; `--force` overrides | — |
| Per-hardware-model registrations (`Registration`, `HardwareModelContainer`) (F51) | N/A (single camera); the calibration table is per camera (above), the prints are not | Store camera id with prints if a second camera is used |

## Template update

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Enroll-on-success: after a live success, borderline match (GFP in [FAR/2, FAR]), once per session, generation < 3, FIFO eviction (F31, F53) | **Done**: best live print with estimated FAR in [5e-6, 1e-5) (similarity 0.50–0.52 with the shipped table and 15 prints), once per session, generation ≤ 3, ≤ 10 augmented (FIFO); the user-level cap of 100 prints (F54, `MAX_PRINTS`) bounds enrolled + added prints, and the FAR estimate charges for every print in the template | — |
| Enroll-on-failure: add print after face fail + password success, relaxed threshold, must not match others (F31, `EnrollOnFailure*Threshold`) | **Done**, opt-in (`daemon --enroll-on-failure`): after `NOMATCH` of one real (≥ 2 live votes), unrecognised face, the best live print waits ≤ 60 s in the daemon; `pam_sm_setcred` after the password success of the same PAM transaction (token) commits it if similarity to the enrolled/augmented prints ≥ 0.40 and no other user matches; ≤ 5 such prints (FIFO); root only (`SO_PEERCRED`); an added print is logged at info by the daemon and announced to the user by the PAM module (`PAM_TEXT_INFO` "Face print added after password login", from the same `pam_sm_setcred`). Windows' threshold values are runtime policy data, not recovered | — |
| Print categories (normal, on-failure, on-success, flags) used as verification conditions (F28) | Partial: prints are tagged `enroll`, `augment`, `on_failure`; all are matched alike; on-failure prints are left out of the enroll-on-failure check and never seed augmentation | Per-category thresholds if needed |

## Storage & key protection

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Templates through WinBio storage adapter, AES-CBC + DPAPI (F33, F34) | Done (better): JSON 0600, atomic write; optional AES-256-GCM sealed by TPM, optional PCR binding (`tpm.rs`, `docs/tpm.md`) | — |
| VBS: matching + templates in VTL1, IUM sealing (F35, F36) | N/A (no VBS on Linux) | — |
| Match releases the NGC (Windows Hello) key; no secret in VTL0 (F36) | N/A by design: PAM `sufficient` returns success only | Could unseal a user secret (e.g. keyring/LUKS key) only on a match — larger design change |
| Model obfuscation (HelloFaceCipher XOR) (F61) | N/A | — |

## Service, timeouts & retries

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| WinBio presence monitor, capture every 125 ms, no service timeout; UI ends the session (F2, F55, F57) | Different: daemon on demand per PAM request, up to 16 frame pairs, PAM timeout 10 s | Optional: time-based budget |
| Auto-logon confirmation after 120 s on the lock screen; switch timeout 10 s (F56) | N/A (sudo only; screen lock on hold per docs/README.md) | Relevant once a screen locker is supported |
| Remote-session suppression | Done: PAM ignores `SSH_CONNECTION`/`SSH_TTY`/`PAM_RHOST` | — |
| Lid-closed / external-camera suppression (`IsLidClosedScenario`, `ShouldForbidExternalCameras`) | **Done**: `verify` first refuses with `UNAVAILABLE lid_closed` (PAM: ignore, password) when an ACPI lid reports closed and the IR camera is built in (its USB device's sysfs `removable` is `fixed`; unknown counts as built in). `/dev/video2` here: `fixed` | — |
| Daemon down → password (PAM_IGNORE) | Done | — |

## User feedback & telemetry

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Feedback reasons: too close/far, too bright/dark, too much ambient, backlight, occluded, large pose (F45, F46) | **Done** for too dark/bright/far, clipped (too close / lamp), edge, occluded, not frontal, no face, not live, not recognized (`NOFACE <reason>` reply, PAM `PAM_TEXT_INFO`); ambient Missing | — |
| Notifications: improve recognition, liveness suggestion, get help (days intervals) (F56) | N/A | — |
| Extensive telemetry (per-frame decisions, GFP, counts) | Partial: `watch` command, tracing logs | Log gate reasons and vote counts per session |

## Multi-user & multi-camera

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Multiple tracked faces, multiple enrolled users, best-user assignment (F47, F48) | **Done** for tracked faces: the 3 largest faces of each pair are analysed and tracked, one `Attempt` each; the verdict comes from the user's track (the recognised one, else the best match), and the user is recognised on a track only if no other face in the pair matches better. Multiple enrolled users: N/A by design (1:1 for the requested user) | Optionally reject if a second face is present (shoulder-surfer case) |
| Sensor allow-list, device watcher, camera re-selection (`SensorAllowListManager`, `SensorEnumeration`) | Partial: fixed device paths / CLI | udev-based discovery |
| Per-hardware calibration tables (F51, F60) | **Done** (structure): `models/calibration.json` maps the IR camera's USB `vid:pid` (sysfs `idVendor:idProduct`, `sysfs::usb_id`; here `30c9:00f3`) to a named table, with a default for unlisted cameras; the only ArcFace table is the LFW one. Microsoft's v5 table (five identical per-hardware-model copies) is loaded for the second opinion, first hardware model | An IR-calibrated table per camera; Windows' hardware-model ids are not mapped to cameras (cefa.md §8) |

## Anything else

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Face tracker with ids across frames (FaceTrackerInternal) | **Done** for the 3 largest faces of each pair (`src/tracker.rs`): a face continues the free track whose last box it overlaps best. Addition: a print below cosine 0.50 (`SAME_FACE_SIMILARITY`) to the track's last print cannot continue it, so a photo held where the face was starts a new attempt; Windows tracks by geometry alone. Faces beyond the 3 largest are not analysed (≈ 35 ms per analysed face per pair) | — |
| Head-pose estimator (IrFaceOrientation LBP/LDA/KNN) (F58) | **Done** as API, not a gate: `LbfAligner` + `IrFaceOrientation` read resources 102, 104–107 and match the bit-exact Python reference exactly (lbf.md "Rust port"); it reads ~+25° yaw on frontal faces from this camera, so the frontal gate uses the Retina pose | Prior (tracking) path of the KNN regression, now that a tracker exists |
| Presence / wake-on-approach (WinBio presence monitor, `PresenceLifetimeInSeconds`) (F55) | N/A (no lock-screen integration) | Would need a long-running capture loop; power cost |
| FaceBootstrapAdapter (virtual sensor `{063436EF-…}` bootstrap) | N/A (Windows WinBio plumbing) | — |
| Hardware-model selection via OneSettings (`HardwareModelContainer`) (F51) | N/A | — |
| Data retention of up to 3 raw frames (feature `FaceDataRetention`) (F32) | Partial: `watch` saves frames to `/tmp` on request | Keep off by default |
| Rotation to the sensor mount angle (`RotateImageToAlignWithDisplayOrientation`, ACPI `_PLD` or registry `OrientationCW`; F10, F63) | **Done** as an option: `--rotate 0\|90\|180\|270` (global, so also for `daemon` via `LINUX_HELLO_ARGS`) turns IR and RGB frames counter-clockwise after capture and maps the exposure region back; no automatic source (no IIO accelerometer or iio-sensor-proxy on this laptop; Windows reads the mount angle, not the live display orientation). Mirrored-frame handling Missing (no mirrored camera at hand) | Read the angle from an accelerometer (`/sys/bus/iio/devices/iio:device*/in_accel_*` or iio-sensor-proxy) where one exists |
