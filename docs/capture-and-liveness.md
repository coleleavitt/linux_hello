# Capture, liveness and matching

How `linux_hello` turns camera frames into a verdict, and how this follows Windows Hello Face.
F- and L-ids refer to [windows-hello-re.md](windows-hello-re.md).

## Pipeline per frame pair

```
/dev/video3 (UVCM metadata) ─ illumination flag per frame ─┐
/dev/video2 (IR 640x360 @15) ───────────────────────────────┴─▶ lit/ambient pair (< 300 ms apart)
        lit frame ──▶ SCRFD face ──▶ lighting gate on the face box ──▶ (good) ──▶ embed lit − min(lit, ambient)
/dev/video0 (RGB) ──▶ SCRFD face ──▶ FaceNox + MiniFASNetV2 mean P(real) ──▶ veto if < 0.5
                                         │
                       frame vote ──▶ liveness session ──▶ unlock decision ──▶ template augmentation
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

### 2. Ambient subtraction (F12, F13)

The face is detected on the lit frame. Recognition uses `diff = lit − min(lit, ambient)` per pixel,
the light the emitter added. This removes sunlight and other ambient IR, so enrollment and
unlock see the same lighting. Enrollment embeds the same difference image, so the prints match.

Difference from Windows: Windows warps the ambient face crop onto the lit one with an affine
transform from eye and nose/mouth anchors before subtracting (F13). `linux_hello` subtracts the
whole frames without motion compensation: the two frames are 67 ms apart, and a face moves little
in that time.

### 3. Lighting gate (F14)

On the face box of the lit and ambient frames:

- `d = mean(lit face) − mean(ambient face)`
- good if `45 ≤ d ≤ 205` and `mean(ambient face) ≤ 25`

A frame with bad lighting casts no liveness vote and is not used for matching. Windows also
smooths the ambient brightness over frames (`avg = 0.5·avg + 0.5·new`); `linux_hello` uses each
pair as it is. Constants: `src/lighting.rs`.

### 4. Liveness votes (F22–F25)

Windows' live votes come from two IR anti-spoofing CNNs; the colour CNN can only veto (F24).
Those models are proprietary, and `linux_hello` has no IR anti-spoofing model. Instead:

| Evidence | Vote |
|---|---|
| Face passes the lighting gate (reflects the emitter like a nearby face, little ambient IR) | +1 |
| RGB models (FaceNox + MiniFASNetV2) mean P(real) < 0.5, or no face in the RGB frame | −2 |

Frame total > 0 is a live frame, < 0 a spoof frame. A frame that fails the gate has no vote.

The veto weighs −2, not −1 as Windows' colour veto: with a single IR vote, −1 would only cancel the
live vote, and the "3 spoof frames" rule could never fire. −2 is the weight Windows gives its
multi-task IR vetoes in strict mode (F25).

Session (`MakeFinalDecision`, F25), in `src/liveness.rs`:

- ≥ 2 live frames → live
- else ≥ 3 spoof frames → spoof (reject)
- else after 16 frame pairs → give up (reject). Windows stops after 48 frames or when its counter
  passes 16; one pair here is two camera frames.

If the liveness models are configured but fail to load, `auth` and `daemon` exit with an error
(fail closed, as Windows starts in "spoof" when anti-spoofing is on but no classifier is enabled).
`--no-liveness` turns off only the RGB veto; the lighting gate still votes.

### 5. Matching (F28) and the unlock decision

A user's template is a list of prints (512-float ArcFace embeddings), one per enrollment frame,
not an average. The similarity of a probe is the maximum cosine similarity over the user's prints,
i.e. the minimum distance (F28). A print matches above `MATCH_THRESHOLD` (`src/matcher.rs`).

Unlock is accepted when the liveness session is live **and** at least one live frame matched. The
verdict carries the best similarity of the matched live frames. Faces are compared only with the
template of the user being authenticated (1:1).

Difference from Windows: Windows converts the distance into an estimated false-positive rate and
accepts below 1e-5 (F27, F29). `linux_hello` thresholds the raw cosine similarity; the threshold is
being calibrated separately.

### 6. Template augmentation (F31)

After a successful unlock the best-matching live frame's print is added to the template:

- `generation` = generation of the stored print it matched best + 1 (enrolled prints are 0);
  the print is not added if that would exceed 3;
- `source` = `augment`; at most 10 augmented prints per user, the oldest augmented print is
  dropped first; enrolled prints are never dropped.

The store is saved right away (atomic rename, mode 0600). A TPM-sealed store stays sealed.

## Store format

```json
{ "cole": { "prints": [ { "features": [512 floats], "generation": 0, "source": "enroll" }, ... ] } }
```

Stores from before this format (one averaged `features` vector per user) are refused with an
error that asks to re-enroll: `linux_hello remove <user>` is not needed, `linux_hello enroll <user>`
replaces the template. A sealed old store must be re-enrolled the same way.

## Enrollment

`linux_hello enroll <user> [--frames 5]` reads frame pairs from the IR camera until it has
`--frames` pairs whose face passes the lighting gate (at most 4 pairs per print), and stores one
print per pair.

## Inspecting

`linux_hello watch [--seconds 30]` prints one line per pair: face brightness lit/ambient, `d`, gate,
RGB P(real), vote, similarity and session state. It saves `NNNN_{lit,ambient,diff,rgb}.png` and
`log.jsonl` to `--out` (default `/tmp/linux_hello_watch`). When a session reaches a verdict it
prints it and starts a new one. `watch` does not augment the template.
