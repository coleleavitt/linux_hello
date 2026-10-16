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
