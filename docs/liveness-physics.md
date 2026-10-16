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
