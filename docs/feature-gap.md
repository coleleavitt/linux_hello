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
| Warm-up: exposure settles ≈ 11 frames (L1) | Done: `WARMUP_FRAMES = 16` | — |
| RGB camera in parallel for colour liveness (F22) | Done: `OpenCameras` opens RGB first, then IR; RGB dark-frame skip (`DARK_RATIO 0.5`, 4 skips) | — |
| Secure camera path (`SECURE_MODE`, SecureUSBVideo trustlet, VTL1 frame buffers) (F8, F36) | N/A: needs VBS/trustlets; no Linux equivalent for UVC | Closest: run capture + matching only in the root daemon (already so) |
| Driver face-detection property (`FACEDETECTION`, id 29) (F8) | N/A: not used for auth; camera does not expose it | — |
| ROI / exposure control on the face (`SensorController`, `ToggleFaceDetectionOnDriverIfCapable`) (Q1) | Missing | V4L2 `exposure_absolute`/ROI controls if the IR node exposes them (it shows few controls); low value |

## Frame pairing & ambient light

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Pair unlit + next lit frame, ambient < 300 ms older (F12, F13) | **Done**: consecutive lit/ambient frames < 300 ms apart by buffer timestamps (either order) | — |
| Ambient crop = previous face crop + 100 px per side (F39) | Different: whole ambient frame is kept and subtracted | Crop around last known box to save work (optimisation only) |
| Recognition image = lit − min(lit, warped ambient), affine from eye/eye/mouth anchors, only when lighting status = 1 (F13, F40) | **Done without warp**: whole-frame `lit − min(lit, ambient)` for status 1, lit frame otherwise; no affine motion compensation (frames 67 ms apart) | Affine warp: 3 anchor means → 2×3 solve |
| Liveness image = lit − min(lit, ambient) unwarped, only if status 1 and ambient ≤ 25 (F40) | N/A until an IR liveness model exists (the difference image is computed for recognition) | — |
| Single-slot hand-off, newest frame wins (F11) | Done (sequential capture loop) | — |
| Driver background-subtraction mode (FACEAUTH_MODE 4) + background brightness check (F9, F44) | N/A (camera does alternate frames) | — |

## Face detection, crops & quality gates

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Detection on the lit frame, tracker across frames (FaceTrackerInternal, RetinaEngine ONNX or LBF aligner) (Q5, F58) | Partial: SCRFD per pair on the lit frame, largest face only, no tracking. The Windows IR Retina detector (MODEL_FILES_110, `MsRetinaDetector`) runs on the lit frame for the frontal check only; its box and landmarks are not used | Track id = IoU match between frames; needed for "same face" across pairing/votes |
| 31-point landmark set (5 RetinaFace + eye contours + mouth) (F58) | Partial: SCRFD 5 points | Enough for anchors (eyes, mouth); eye-contour box for brightness needs approximation from 5 points |
| Recognition crop 3× box, ≤ 400 px, 8-bit lit luminance (F38) | Different: ArcFace 112×112 aligned crop from the RGB-converted IR frame | Keep ArcFace alignment; do subtraction on a 3× crop before aligning |
| Face brightness = mean of 2×2-subsampled cheek/nose box (F43) | **Done** (approximation): box between eyes and mouth corners from SCRFD's 5 points (`lighting::face_brightness`) | — |
| Exposure status: > 205 too bright, < 35 too dark, clipped pixels > 1 % → reject (F44, F45) | **Done** for 205/35 (rejected, no print, no vote); clipped-pixel statuses 6/7 Missing | Add saturated-pixel count (pixel = 255 or equal to ±2 neighbours) |
| Lit − ambient ≥ 45, ambient ≤ 25 (F14, F44) | **Done**: otherwise status 4 (print kept for matching, no vote, no augmentation) | — |
| Min face width 72 px and ≥ 0.12 × min(frame w,h) (F46, F54) | **Done** (`too_far`) | — |
| Edge cut: box ≥ inter-ocular/5 from border (F46) | **Done** (`edge`, frame rejected) | — |
| Frontal pose: \|yaw\| ≤ 20°, \|pitch\| ≤ 25° (F46, F59) | **Done** (with the converted models): `FacePipeline::analyze` takes the pose of the Windows IR Retina detector (MODEL_FILES_110, `RetinaEngine::ComputeOrientation` angle order, F59) for the face overlapping SCRFD's; not frontal or no such face → `NOFACE not_frontal`. All 4 frontal owner frames pass (ldmf.md §11) | — |
| Occlusion detectors (eyes, mouth/nose, face; ONNX); 4 consecutive occluded frames → give up; occlusion mode after 30 frames ≥ 50 % (F46, F61) | **Partial**: face-level occlusion gate in `FacePipeline::analyze` (converted Windows Hello model, used when `models/windows_hello/converted` exists; 1 − p_none > 0.9, 0.5 while enrolling; face-box input as in `sub_18004B5B8`); occluded frames give no print and the reply `NOFACE occluded`. The 4-frame give-up and occlusion mode are Missing | — |
| Re-attempt by movement (centre range > 0.01, size ±20 %) or exposure (σ > 12) before rejecting (F49) | Missing (fixed 16 frame pairs) | Keep per-session stats; extend the session once on movement/exposure change |
| Colour-face correspondence + colour quality (edge cut, too close/far, low-confidence alignment) (`EvaluateColorFaceQuality`) | Partial: largest face on RGB, no geometric correspondence to IR face | Map IR box to RGB with a fixed extrinsic offset; reject if none |

## Liveness

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| IR CNNs (general on lit crop, diffusion on difference crop), 128×128×1, SqueezeNet, cosine 2×512 head (F22, F23, F42, F62) | **Done** (converted Windows Hello model, used when `models/windows_hello/converted` exists): good-lighting frames run the diffusion net (slot 12) on the difference image, flag-on mode-0 thresholds; the frame total (F25) must be > 0 together with the lighting gate for a live vote (`Vote::from_evidence`). The general net (slot 11) runs only in `examples/ms_check` | — |
| Colour CNN veto (BGR 128×128) (F22, F62) | **Done** differently: FaceNox + MiniFASNetV2 mean < 0.5 vetoes a frame (−2, so a vetoed frame is a spoof vote) | — |
| Multi-task IR border / HH vetoes, enabled after first live vote (F22, F42) | **Done** (converted Windows Hello model, used when `models/windows_hello/converted` exists); they run on every good-lighting frame, not only after the first live vote, which can only add vetoes. The colour net (slot 13) runs on the face detected in the RGB frame | — |
| Thresholds: IR live > 0.5 (strict 0.9), spoof > 0.5 strict only; colour 0.5; border 0.7 (F24) | Partial: 0.5 on RGB mean | Use per-model thresholds once IR model exists |
| Frame voting: ≥ 2 live → accept, ≥ 3 spoof → reject, 48 frames → reject; counted from the recognition frame (F25, F48) | **Done**: live vote = good lighting status; counted from recognition; give up after 16 pairs | — |
| Fail closed if liveness enabled but no model loaded (F25) | Done-ish: `--no-liveness` disables explicitly; missing model fails at load | — |
| Lit/ambient physics (screens emit no IR; lighting gate) as implicit liveness (F14) | **Done**: the lighting status is the only live vote | — |
| Enhanced anti-spoofing policy (`FacialFeaturesUseEnhancedAntiSpoofing`) (F22, F55) | N/A (no policy layer) | A config flag selecting the strict thresholds |

## Recognition & thresholds

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Embedding CNN on 128×128×1 IR crop, ~256-d (F30) | Done differently: ArcFace R50/MBF 112×112 RGB-replicated IR, 512-d | — (Windows models are proprietary) |
| Distance 1 − cos, clamped ≤ 1 (F50) | Done (cosine similarity) | — |
| Min distance over all stored prints (+coupled mean/pair prints) (F28) | **Done** (max cosine over prints); coupled prints Missing | Coupled prints optional |
| Distance → FAR via per-hardware calibration table, × 1…4 correction; accept FAR < 1e-5 (F27, F51) | Partial: fixed cosine `MATCH_THRESHOLD` 0.50 from `docs/calibration.md` (FAR ≤ 1e-5 per attempt on LFW) | Store a (FAR, distance) table per camera/model and threshold on FAR; collect IR genuine/impostor data on this camera |
| ≥ 4 prints on the track before deciding (F48, F54) | **Done** per session (no tracker: largest face of each pair) | Tracker, see below |
| 1:N with "closer to another tracked/enrolled user" check (F48) | N/A by design: 1:1 for the PAM user | Could add: reject if another enrolled user scores higher |
| Recognise first, then liveness on the same track (F47, F48) | **Done** (`Attempt`) | — |

## Enrollment

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Multi-pose enrolment, 16 prints, quality-gated frames, registration per hardware model (F54, Q5) | Partial: 5 individual prints, each from a gated good-lighting frame; no pose guidance | Guide the user through slight poses; more prints |
| Duplicate-enrolment check (`CheckForDuplicateEnrollment`) | Missing | Compare new prints against other users' prints at the FAR threshold |
| Per-hardware-model registrations (`Registration`, `HardwareModelContainer`) (F51) | N/A (single camera) | Store camera id with prints if a second camera is used |

## Template update

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Enroll-on-success: after a live success, borderline match (GFP in [FAR/2, FAR]), once per session, generation < 3, FIFO eviction (F31, F53) | **Done**: best live print with similarity in (0.50, 0.55], once per session, generation ≤ 3, ≤ 10 augmented (FIFO) | — |
| Enroll-on-failure: add print after face fail + password success, relaxed threshold, must not match others (F31, `EnrollOnFailure*Threshold`) | Missing | PAM `pam_sm_setcred`/session hook after password success → daemon stores the last unmatched print, only if it clears a relaxed threshold |
| Print categories (normal, on-failure, on-success, flags) used as verification conditions (F28) | Missing | Tag prints in the store |

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
| Lid-closed / external-camera suppression (`IsLidClosedScenario`, `ShouldForbidExternalCameras`) | Missing | Check lid switch (`/proc/acpi/button/lid/*/state`) and refuse |
| Daemon down → password (PAM_IGNORE) | Done | — |

## User feedback & telemetry

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Feedback reasons: too close/far, too bright/dark, too much ambient, backlight, occluded, large pose (F45, F46) | **Done** for too dark/bright/far, edge, occluded, not frontal, no face, not live, not recognized (`NOFACE <reason>` reply, PAM `PAM_TEXT_INFO`); ambient Missing | — |
| Notifications: improve recognition, liveness suggestion, get help (days intervals) (F56) | N/A | — |
| Extensive telemetry (per-frame decisions, GFP, counts) | Partial: `watch` command, tracing logs | Log gate reasons and vote counts per session |

## Multi-user & multi-camera

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Multiple tracked faces, multiple enrolled users, best-user assignment (F47, F48) | N/A by design (1:1 for the requested user); largest face only | Optionally reject if a second face is present (shoulder-surfer case) |
| Sensor allow-list, device watcher, camera re-selection (`SensorAllowListManager`, `SensorEnumeration`) | Partial: fixed device paths / CLI | udev-based discovery |
| Per-hardware calibration tables (F51, F60) | Missing | One table per camera model in the store or config |

## Anything else

| Windows behaviour | linux_hello status | What implementing it would take |
|---|---|---|
| Face tracker with ids across frames (FaceTrackerInternal) | Missing | IoU tracker (needed by pairing, votes, min-prints) |
| Head-pose estimator (IrFaceOrientation LBP/LDA/KNN) (F58) | Missing | `lbf.md` documents a converter (`pose.py`) |
| Presence / wake-on-approach (WinBio presence monitor, `PresenceLifetimeInSeconds`) (F55) | N/A (no lock-screen integration) | Would need a long-running capture loop; power cost |
| FaceBootstrapAdapter (virtual sensor `{063436EF-…}` bootstrap) | N/A (Windows WinBio plumbing) | — |
| Hardware-model selection via OneSettings (`HardwareModelContainer`) (F51) | N/A | — |
| Data retention of up to 3 raw frames (feature `FaceDataRetention`) (F32) | Partial: `watch` saves frames to `/tmp` on request | Keep off by default |
| Mirrored-frame handling, rotation to display orientation (F10, `RotateImageToAlignWithDisplayOrientation`) | Missing | Only for rotated panels/tablets |
