# Physics-based liveness checks

What tells a face from a print or a screen when the room is rich in infrared, so that the
lighting gate (docs/capture-and-liveness.md §3) no longer does: with `--max-ambient` raised
above Windows' 25, a photo lit by the same lamps passes the emitter-gain rule like a face, and
spoof defence rests on the anti-spoofing nets. Each check below measures one physical
difference, is scored on every face with a print (`glint=0.8 shape=0.3 …` in the `auth` log
line and in `watch`'s `log.jsonl` under `physics`), and is benchmarked by
`examples/liveness_bench` (the `Check` trait, `src/liveness/check.rs`) and
`examples/physics_replay` on saved recordings. Code: `src/liveness/physics.rs`.

**Wiring rule.** A check may veto a frame (`Check::threshold`; the frame's live vote becomes an
abstention, −1 on Windows' frame total as its colour net, F24) only with a threshold at which
no genuine recording loses more than 1 % of its good frames. Everything else is logged and
shown by the bench. The attack recordings (`captures/bench/<label>_<n>`, sibling work) decide
which checks earn a threshold; until then every check is log-only, and each section says
what the genuine recordings show and what the attack set showed.

**Genuine recordings used.** Dark room: `re/validation_frames` (`watch_*` frontal frames of the
owner at ≈ 40–60 cm, 4 pairs; the `ir_*` frames are cut at the chin and fail the edge gate, so
they give no print and no scores). Café with incandescent-style lamps (ambient face brightness
27–45, so only scored with `--max-ambient 60`): `captures/lh_view` (28 pairs), `lh_now` (59
pairs, 5 not frontal), `lh_lock` (59 pairs). Numbers below are from `physics_replay` on these
(2026-10-16); the percentiles are over frames with a print, "session max" over 16-pair sessions.

## 1. Eye glints (`glint`)

**Physics.** The emitter is a point source next to the lens; the cornea is a convex mirror, so
each eye shows a specular highlight of the emitter: a spot of 1–3 px, brighter than anything
around it, within ≈ 0.15 of the inter-ocular distance of the eye centre, present in the lit frame
and absent from the ambient frame. Matte paper has no specular reflection; glossy paper or a
screen reflects the emitter off a flat surface as one broad patch wherever the geometry puts it,
not two points at the eyes. Room lamps make glints too, but they are in both frames.

**Measure.** On the emitter image `lit − min(lit, ambient)` (F13; the lamps' glints cancel), in
the window of radius 0.15·IOD around each eye centre (the Windows IR detector's five points, F23;
SCRFD's without the models), the brightest small spot: a pixel minus the mean of the ring
between the 3×3 and the 9×9 squares around it. Score = the larger of the two eyes, divided by the
face's emitter gain (F14 `d`), so that distance and exposure cancel. A flat window reads 0.

**Genuine.** The glint is gaze-dependent: when the user looks down at the screen, the lid covers
the reflection point, and the window reads only skin texture and the lid edge.

| Set | frames | min | p1 | p10 | p50 | p90 | max | session max (min / median) |
|---|---|---|---|---|---|---|---|---|
| lh_lock | 59 | 0.23 | 0.23 | 0.24 | 0.53 | 1.80 | 2.53 | 1.10 / 2.47 (4 sessions) |
| lh_now (eyes mostly down) | 54 | 0.13 | 0.15 | 0.17 | 0.28 | 0.41 | 0.58 | 0.30 / 0.54 (4) |
| lh_view | 28 | 0.37 | 0.37 | 0.46 | 0.53 | 0.67 | 0.80 | 0.67 / 0.80 (2) |
| validation_frames | 4 | 0.28 | — | — | 0.46 | — | 0.73 | 0.73 (1) |

The same measure on the cheeks (skin texture only, the control) reads 0.09–0.23 on the same
frames (Python prototype on the same landmarks), so a frame below ≈ 0.25 says nothing either
way, and a per-frame veto that spares 99 % of genuine frames would have to sit at 0.13 — the
texture level — and would veto nothing. A frame at ≥ 0.3 is evidence of a cornea; over a
session, every genuine 16-pair session reached 0.30 or more.

**Attack set.** Not recorded yet; the expectation is ≈ 0.1–0.25 (texture) on matte prints, and
a single broad patch (which the 9×9 ring subtracts) on glossy prints and screens.

**Role.** Logged; no threshold (the glint hides under the lid when the user looks down). It is
the "sure" signal of the blink challenge (§7): a session whose frames never reached
`GLINT_SURE` is the inconclusive case that asks for a blink.

## 2. Emitter falloff / face shape (`shape`)

**Physics.** The emitter is a point source a few centimetres from the lens. A face at 40–70 cm is
lit by 1/r² and by Lambert's cosine: the nose, cheeks and forehead face the emitter and are
nearest, the temples turn away and fall off to a third, and the emitter sits to one side of the
lens, so one cheek is brighter than the other. A flat print is lit evenly: what shading it shows
is printed, and most inks are transparent at 850 nm, so a print reads nearly flat in the emitter
image.

**Measure.** On the emitter image, the mean over nine patches (squares of half-side 0.1·IOD):
nose tip, left and right cheek (between eye and mouth corner), forehead (0.6 IOD above the eye
line), chin (0.45 IOD below the mouth), left and right temple (0.55 IOD outside each eye), left
and right brow (0.25 IOD above each eye), each divided by their mean, so exposure and distance
cancel. Patches off the frame (a chin cut at the edge) are left out. Score = RMS z-distance from
the genuine profile `GENUINE_PROFILE`, the mean and standard deviation per patch over 145 frontal
frames of the owner on this camera (fitted on the four genuine sets together; one person):

| patch | nose | l cheek | r cheek | forehead | chin | l temple | r temple | l brow | r brow |
|---|---|---|---|---|---|---|---|---|---|
| mean | 1.35 | 1.50 | 1.24 | 1.31 | 1.04 | 0.46 | 0.35 | 0.99 | 0.76 |
| σ | 0.03 | 0.06 | 0.08 | 0.05 | 0.03 | 0.11 | 0.07 | 0.05 | 0.04 |

The left/right asymmetry (l cheek 1.50 vs r cheek 1.24, temples 0.46 vs 0.35) is the emitter's
position beside the lens, the same on every frame.

**Genuine.** The profile is stable within a sitting and drifts with distance and pose:

| Set | frames | min | p10 | p50 | p90 | max | session max (min / median) |
|---|---|---|---|---|---|---|---|
| lh_lock | 59 | 0.36 | 0.52 | 0.69 | 0.96 | 1.09 | 0.66 / 0.99 |
| lh_now (head turning, within the frontal limits) | 54 | 0.35 | 0.47 | 0.76 | 2.34 | 3.13 | 0.76 / 3.03 |
| lh_view | 28 | 0.46 | 0.52 | 0.62 | 0.82 | 0.97 | 0.72 / 0.97 |
| validation_frames (dark room, closer: temples 0.6–0.7) | 4 | 1.80 | — | 1.92 | — | 1.98 | 1.98 |

Within the frontal limits (|yaw| ≤ 20°) a turned head moves one temple into the light and the
other into shadow, which is the 2–3 of `lh_now`. The dark-room frames (closer, whole-frame
metering) sit at 1.8–2.0 because the temples fall off less at a shorter distance relative to
the face size. A perfectly flat profile (every patch 1.0) scores ≈ 7 against this table (the
nose alone is 10 σ away, the temples 5–10 σ), so the expected margin is wide, but the genuine
spread across people, distances and cameras is unmeasured: the table is one person's.

**Attack set.** Not recorded yet.

**Role.** Logged; no threshold (a 99 % genuine-frame threshold would be ≈ 3; whether prints sit
above it is for the attack set).

## 3. Flicker fingerprint (`flicker`)

**Physics.** A lamp on mains flickers at 100/120 Hz and a screen refreshes at 60–120 Hz. A
rolling-shutter camera turns either into bands whose phase drifts from frame to frame, so the
mean brightness of a region wanders over the pairs of a session (in `lh_view` the ambient IR
frames wandered by 4.6 % from pair to pair, face and background together, correlation 0.98).
A face and the wall behind it are lit by the same lamps and wander together. A screen held up as
the face carries its own refresh pattern; a print carries its own only when it is lit by another
lamp than the background (a phone torch).

**Measure.** Over the ambient IR frames and the RGB frames of the face's track (the session's
pairs, at most 16): the mean of the face box (SCRFD's IR box; the colour face's box in RGB) and
of the background (outside the box grown by 50 % per side), every 4th pixel in both directions;
from pair to pair the relative change of the face minus the relative change of the background;
score = the standard deviation of those differences, the larger of the two frames' values. It
needs four pairs (three changes) and an ambient frame that is not blank, so in a dark room it
runs on the RGB frames alone (and reads nothing when the RGB frame is dark).

**Genuine** (running value as the session grows; the first three pairs read nothing):

| Set | frames | min | p10 | p50 | p90 | max | session max (min / median) |
|---|---|---|---|---|---|---|---|
| lh_lock | 47 | 0.003 | 0.004 | 0.008 | 0.017 | 0.019 | 0.006 / 0.011 |
| lh_now | 43 | 0.002 | 0.003 | 0.009 | 0.023 | 0.027 | 0.007 / 0.015 |
| lh_view | 22 | 0.009 | 0.010 | 0.017 | 0.022 | 0.022 | 0.013 / 0.022 |
| validation_frames (four frames from different moments, not a session) | 1 | 0.097 | | | | | |

The head moving changes what the face box holds, which is the 0.02 of the moving sessions; a
still sitter reads 0.003–0.01. The screen case is the one this measures; a print under the
room's lamps should read as a face does.

**Attack set.** Not recorded yet.

**Role.** Logged; no threshold (genuine 99th percentile ≈ 0.025 on three recordings).

## 4. IR/RGB material (`material`)

**Physics.** Skin reflects 850 nm at ≈ 0.5–0.6 and visible light at ≈ 0.3–0.4 (light skin).
Paper reflects both at ≈ 0.8–0.9, and most printer inks are transparent at 850 nm, so a printed
face is as dark as skin in the colour frame (that is what the print is for) but paper-white to
the emitter. A screen emits visible light and no infrared at all.

**Measure.** Per frame, the face's emitter gain (F14 `d`, the F43 block of the emitter image),
corrected for the emitter's 1/r² with (IOD / `IOD_REF` 50 px)², over the luminance of the colour
face's F43 block (Rec. 601, on the colour face's own five points; the colour face is the one
paired by F66, `FaceCheck::rgb_face`). `None` without a colour face or with the colour face
darker than `MIN_RGB_FACE` = 40: a dark room says nothing about the material. Both cameras
auto-expose (the IR one on the face), so this is a ratio of exposed values, not of albedos; it
holds as long as both exposures track the same scene.

**Genuine.**

| Set | frames | RGB face | min | p10 | p50 | p90 | max |
|---|---|---|---|---|---|---|---|
| lh_lock | 59 | ≈ 190 | 0.41 | 0.43 | 0.45 | 0.46 | 0.47 |
| lh_now | 54 | ≈ 220 | 0.41 | 0.42 | 0.45 | 0.47 | 0.52 |
| lh_view | 28 | ≈ 190 | 0.43 | 0.46 | 0.53 | 0.55 | 0.55 |
| validation_frames (dark room, face lit by the screen) | 3 | 40–60 | 0.81 | — | 0.85 | — | 0.90 |

Within one lighting the spread is ±5 %; between the café (lamps) and the dark room (screen
light) the ratio doubles, because the colour face's exposure follows the room. The genuine
range is therefore wide (0.4–0.9); the expectation for a print under the same lamps is a value
1.5–2× the face's at the same spot, and for a screen one near 0 (no emitter return from a
screen, and the IR face is not even found).

**Attack set.** Not recorded yet.

**Role.** Logged; no threshold.

## 5. Micro-motion (`motion`)

**Physics.** A live head is not a rigid plane: when it turns, the nose (2–3 cm nearer the camera)
moves more across the image than the eyes and the mouth, and breathing and expression move the
mouth against the eyes. A hand-held print is a plane; a plane's points move together under one
affine map (the homography of a small rotation is affine to first order), whatever the hand does.

**Measure.** The least-squares affine map from the face's five points in the oldest pair of its
track (the session's first sight of it, up to 16 pairs back) to the current five, and its RMS
residual over the points, as a fraction of the inter-ocular distance (`affine_residual`). The
Windows IR detector's points, as everywhere here. A still face reads the landmark jitter; the
measure only says something once the head has moved, and nothing in a single frame.

**Genuine.**

| Set | frames | min | p10 | p50 | p90 | max | session max (min / median) |
|---|---|---|---|---|---|---|---|
| lh_lock (still) | 55 | 0.000 | 0.001 | 0.004 | 0.014 | 0.025 | 0.005 / 0.011 |
| lh_now (head turning) | 51 | 0.001 | 0.006 | 0.020 | 0.088 | 0.112 | 0.029 / 0.058 |
| lh_view | 26 | 0.001 | 0.002 | 0.016 | 0.020 | 0.022 | 0.006 / 0.022 |
| validation_frames | 3 | 0.008 | — | 0.012 | — | 0.021 | 0.021 |

A 10° turn of the head moves the nose by ≈ 0.08 IOD against the eye/mouth plane
(3 cm nose depth × sin 10° over a 6.5 cm IOD), which `lh_now` shows (0.09–0.11); still
sessions stay under 0.025. A hand-held print that is tilted should stay at the jitter level.

**Attack set.** Not recorded yet.

**Role.** Logged; no threshold. As evidence it is one-sided: a high value is a head, a low value
is a still head or a print.

## 6. Stereo parallax (`parallax`)

**Physics.** The IR and the RGB camera sit side by side in the lid, so a point nearer the
cameras shifts more between the two views than a point farther away. The nose tip of a face at
40–70 cm is 2–3 cm nearer than the eyes and the mouth; on a print every point lies in one plane
and the relative shift is zero.

**Measure.** The least-squares similarity map (scale, rotation, translation) from the IR face's
eyes and mouth corners to the colour face's (SCRFD's points on both sides, so that the
detector's own landmark bias is the same in both views), then the nose's residual along the
baseline (x) as a fraction of the inter-ocular distance, signed as measured (`nose_disparity`).
No calibration beyond that: the registration (F66) is box-to-box and 3–4 px coarse, so the
similarity fit per frame replaces it.

**Genuine.**

| Set | frames | min | p10 | p50 | p90 | max |
|---|---|---|---|---|---|---|
| lh_lock | 59 | −0.020 | −0.016 | −0.009 | −0.004 | 0.005 |
| lh_now | 54 | −0.081 | −0.021 | −0.014 | 0.006 | 0.017 |
| lh_view | 28 | −0.022 | −0.020 | −0.011 | −0.006 | −0.002 |
| validation_frames | 4 | −0.014 | — | −0.010 | — | −0.010 |

The sign is consistent (−0.01 IOD ≈ −0.5 px on every set, at 40–70 cm), but so is its size
with the expected parallax of this laptop: with the cameras ≈ 1.5 cm apart, a focal length of
≈ 500 px and the nose 2.5 cm nearer at 60 cm, the relative disparity is f·b·Δz/z² ≈ 0.5 px —
the same 0.01 IOD, and within the landmark noise (σ 0.005–0.017 IOD within a set). Whether the
−0.01 is the parallax or SCRFD drawing the nose differently in IR and in colour cannot be told
from genuine data alone; a print would answer it (its value is the detector bias). Either way
the signal is at the noise floor at this baseline, so this check cannot separate a print from a
face on this laptop; it is kept as measured and may serve a camera pair with a wider baseline.

**Attack set.** Not recorded yet.

**Role.** Logged; no threshold, and none expected here.

## 7. Blink challenge (`blink`, `--blink-challenge`)

**Physics.** An eye closes and opens within ≈ 150 ms. A print cannot; a screen can only replay
a recording, which the challenge's timing does not fit unless the attacker loops blinks.

**Measure.** On the emitter image, the mean of the two eye patches (squares of half-side
0.12·IOD at the eye centres) over the mean of the two cheek patches: dark when the eye is open
(iris, pupil, lashes; 0.5–0.65 on the owner), skin-bright when the lid covers it. Score = that
ratio in the current pair minus its median over the track's earlier pairs (at least 3). A
closure is a rise of `BLINK_RISE` = 0.1 or more.

**Genuine.** Over the three café recordings (146 pairs ≈ 20 s): one clear blink (`lh_lock` pair
29, score 0.215; the eye crops show the lids closed and no glint) and three pairs at 0.10–0.11
in `lh_now` with the eyes closing while looking down; everything else −0.09…0.06, so the
threshold has a margin of ≈ 0.04 over a still eye. `lh_view` (28 pairs, 3.7 s) has no blink at
all. One blink in 20 s is the honest rate seen so far, which is why the challenge is opt-in: a
user who does not blink within the window is told "Face not accepted as live" and types the
password.

**Challenge.** With `--blink-challenge` (global option, so `daemon` and `auth`), a session that
ends in `MATCH` is confirmed by a blink when `needs_blink` holds for the matched track: its
good-lighting frames had ambient above Windows' 25 (the match leaned on `--max-ambient`, where
a photo lit by the room passes the emitter-gain rule) and no frame of the track reached
`GLINT_SURE` = 0.3 on the glint check (every genuine session so far did, §1). The daemon then
sends `NOTICE blink` (the PAM module shows "Blink once", `hello-greeter` shows it under the
verifying eye) and reads up to `BLINK_PAIRS` = 24 more pairs (≈ 3 s); a `blink` score of 0.1 on
the matched track keeps the `MATCH`, otherwise the reply is `NOTLIVE`. In a dark room (ambient
≤ 25) nothing changes.

**Attack set.** Not recorded yet.

**Role.** Opt-in challenge; log-only otherwise.

## 8. Trained lighting gate (`gate`, `--trained-gate`)

**What.** A logistic regression over the checks above (`glint`, `shape`, `flicker`,
`material`, `motion`, `parallax`) and the lighting numbers (`lit`, `ambient`, `d`), trained on
labelled recordings and run as one more check that vetoes below its threshold.

**Pipeline.** `examples/physics_replay` writes one JSON line per pair of a recording (the
scores, the lighting numbers, the Windows frame total); `tools/train_gate.py --genuine … --attack
… --out models/trained_gate.json` standardises the features, fits the regression (numpy
gradient descent, classes balanced, L2), sets the threshold at the score that keeps 99 % of the
genuine training pairs (`--genuine-accept`), and prints a leave-one-recording-out table: for
every recording held out, the share of its pairs the gate trained on the others accepts. Pairs
missing a feature (the first three of a session: no flicker or motion yet) are left out of
training, and the gate abstains on them. `linux_hello --trained-gate` (global option) loads
`<models>/trained_gate.json` (`physics::TrainedGate`), scores every face with a print
(`gate=0.98` in the log) and takes the live vote away below the threshold; without the option
the gate does not run. `physics_replay --gate <json>` scores a recording with it.

**Numbers.** No attack recording exists yet, so no gate is trained or shipped; the file is not
in the repository. A smoke test with a synthetic "attack" (the `lh_lock` scores with `shape` × 5
and `glint` × 0.3) trained and loaded end to end (`physics_replay --gate`) and accepted 99–100 %
of `lh_view`; it says nothing about real prints. When the attack set lands, this section gets
the cross-validated table.

**Caveat (honest).** The genuine side is one person, one camera, three café sittings and four
dark-room frames; the attack side will be a handful of prints and screens made by the owner.
A gate fitted on that separates those recordings, not prints in general: a different printer,
paper or screen, or another user's face, is outside the data. Hence opt-in, and the physics
checks' own numbers stay in the log next to it.
