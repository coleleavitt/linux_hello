# Face-match threshold calibration

This note calibrates the cosine-similarity threshold that `FaceMatcher` uses to accept a face.
It uses a public dataset (LFW) and linux_hello's own recognition code. It was written on branch
`calibration`, based on `main` at `baea697`.

## Summary

- **Recommended threshold: 0.50** (accept when cosine > 0.50). The current value is 0.40. Since then the
  threshold is expressed as a calibration table and a FAR target (see "Calibration table" below): the table's
  0.50 row is the 1e-5 per-attempt point, so the decision is unchanged.
- Target: at most 1 false accept in 100,000 unlock attempts by a stranger (per-attempt FAR ≤ 1e-5).
  One attempt compares up to 16 frames against up to 15 stored prints, so up to 240 comparisons.
- At 0.50, on grayscale LFW (the closest public proxy for the IR camera):
  - single-comparison FAR ≈ 4e-8 (tail fit). That meets the worst-case union bound 240 × FAR ≤ 1e-5.
  - simulated per-attempt FAR = 1.8e-6 (1 hit in 548,832 attempts; that hit is a probable LFW label error).
  - FRR for one probe against 15 prints = 0.23%. FRR for one single comparison = 2.1%.
- The current 0.40 gives a single-comparison FAR of about 1e-6. That is 25× too high for the union bound
  (240 × 1e-6 = 2.4e-4 per attempt). The simulated per-attempt FAR at 0.40 is 9.1e-6 (5 hits), which is
  just under the target with no margin.
- These numbers come from visible-light photos of celebrities, not from an IR camera. Treat 0.50 as a
  starting point and check it on real IR captures (see Caveats).

## Pipeline

`examples/embed_dir.rs` calls the library exactly as `FacePipeline` does:

1. `FaceDetector::detect` (SCRFD `det_10g.onnx`, letterbox to 640×640, conf 0.5, NMS 0.4).
2. `FaceEmbedder::embed_face`: 5-point similarity alignment to the ArcFace 112×112 template, then
   `w600k_r50.onnx`; the output is L2-normalised.
3. Score = dot product of two normalised embeddings (cosine). Accept when score > threshold.

Each image runs through the pipeline twice:

- **rgb**: the JPEG as-is.
- **gray**: `to_luma8()` then `gray_to_rgb` (gray copied into three channels). This is what
  `detect_gray` does with IR frames. Detection runs on the gray image too.

Model files (sha256): `det_10g.onnx` `5838f7fe…5b91`, `w600k_r50.onnx` `4c06341c…9e43`. ONNX Runtime via `ort` 2.0.0-rc.14, CPU.

### Which face is used

linux_hello uses the largest face. On LFW this is wrong for group photos: the labelled person is
the face in the centre of the 250×250 crop, and the largest face is sometimes someone else.
With the largest face, 341 images had a face far from the centre, the standard LFW pairs accuracy
was only 97.3%, and the impostor tail was full of these wrong picks (for example
`Julianne_Moore_0001` vs `Todd_Haynes_0001` at 0.97). So for the 2,611 images where SCRFD found
more than one face, the example was rerun with `centre` selection (face nearest the image centre).
Images with one face are unchanged. After this fix the LFW pairs accuracy is 99.85%, which matches
the published accuracy of this model. This is a dataset labelling step. It does not change how
linux_hello compares faces; at the laptop there is normally one face in front of the camera.

## Dataset

- LFW, original (non-aligned) images: `lfw.tgz` from the scikit-learn mirror
  `https://ndownloader.figshare.com/files/5976018` (the umass.edu host did not resolve).
  sha256 `055f7d9c632d7370e6fb4afc7468d40f970c34a80d4c6f50ffec63f5a8d536c0`.
  13,233 images, 5,749 identities.
- Standard view-2 pairs file `pairs.txt` (10 folds × 300 + 300), from figshare file 5976006.
  sha256 `ea42330c62c92989f9d7c03237ed5d591365e89b3e649747777b70e692dc1592`.
- No face found: 18 images (rgb), 17 (gray). They are left out.

### LFW label noise

LFW has known label errors. They are false "impostor" pairs with very high scores, and at
FAR below 1e-6 they dominate the result. I looked at every identity pair with an impostor score ≥ 0.5
(44 pairs). 29 are clearly not impostors: the same person under two names (`Joseph_Blatter` /
`Sepp_Blatter`, `Bison_Dele` / `Brian_Williams`, …), the same photograph under two names, or an image
whose subject is plainly the other person. These are listed in `scripts/lfw_same_person.tsv`.
Pairs of the two labels are dropped from the impostor set and are not added as genuine pairs.

Everything else stays as an impostor, including twins and siblings (Mowry, Olsen, Phelps, Williams,
Moraes, Hentzen sisters), look-alikes, and doubtful cases such as `Mahmoud_Abbas_0012`, which looks
like Hamad bin Isa. So the cleaned numbers are still pessimistic. I did not audit pairs below 0.5.
Both raw and cleaned numbers are given below.

## Method

`scripts/calibrate.py` (numpy only) does the following:

- **Impostor scores**: all pairs of images of different people: 87,079,562 pairs (gray, cleaned).
  The scores are computed in blocks with a matrix product, and every score above 0.15 is kept
  exactly. So the tail quantiles are exact, not read from a histogram.
- **Genuine scores**: all pairs of images with the same label: 242,125 pairs.
- **Threshold at FAR f**: the smallest t with count(score > t) ≤ f × N_impostor.
- **Tail fit**: log10(FAR) fitted linearly in t on the band 1e-6 ≤ FAR ≤ 1e-3 (an exponential tail).
  This is used to extrapolate below about 1e-7, where only a few pairs are left and those are
  twins and leftover label errors. The fit agrees with the measured FAR to within about 1.6× from 0.30 to 0.425.
- **Union bound**: per-attempt FAR ≤ prints × frames × pair FAR = 15 × 16 × pair FAR.
  For a per-attempt FAR ≤ 1e-5 this needs pair FAR ≤ 4.2e-8.
- **Simulation**: each of the 96 identities with ≥ 15 images acts as an enrolled user. Its first 15 images are
  its 15 prints. Results:
  - *probe*: one image of another person; score = max over the 15 prints (1,264,946 trials).
  - *attempt*: up to 16 images of one other person act as the 16 frames of one unlock try; score = max over
    prints and images (548,832 trials). Real frames of one face in one sitting are
    much more alike than 16 different press photos, and real prints are 5 frames plus augmented copies,
    not 15 independent photos. So this simulation overstates the real per-attempt FAR.
  - *genuine probe*: the user's own images after the first 15, against the 15 prints (2,150 trials).
- **LFW pairs**: FRR on the 3,000 genuine pairs of `pairs.txt`. These pairs often span years, pose and
  expression, so this FRR is harsher than unlocking a laptop.

## Results

### Grayscale (IR proxy), label noise removed

| target FAR | threshold | impostor pairs above | FRR (all genuine pairs) |
|---|---|---|---|
| 1e-03 | 0.214 | 87079 | 0.09% |
| 1e-04 | 0.276 | 8707 | 0.11% |
| 1e-05 | 0.346 | 870 | 0.17% |
| 1e-06 | 0.401 | 87 | 0.32% |
| 1e-07 | 0.654 | 8 | 36.02% |

The 1e-7 row is not reliable. Only 8 pairs remain above that threshold, and they are twins, sisters and
probable label errors. The tail fit gives 1e-7 at about 0.47.

| threshold | pair FAR measured (pairs) | pair FAR tail fit | probe FAR, 15 prints (hits) | attempt FAR, 15 prints × ≤16 imgs (hits) | FRR per pair | FRR LFW pairs.txt | FRR per probe, 15 prints |
|---|---|---|---|---|---|---|---|
| 0.350 | 8.5e-06 (740) | 8e-06 | 4.3e-05 (55) | 2.7e-05 (15) | 0.18% | 0.94% | 0.05% |
| 0.375 | 2.7e-06 (235) | 3e-06 | 1.9e-05 (24) | 1.5e-05 (8) | 0.23% | 1.30% | 0.05% |
| 0.400 | 1.0e-06 (90) | 1e-06 | 1.0e-05 (13) | 9.1e-06 (5) | 0.32% | 1.84% | 0.05% |
| 0.425 | 3.7e-07 (32) | 5e-07 | 2.4e-06 (3) | 1.8e-06 (1) | 0.48% | 2.61% | 0.05% |
| 0.450 | 2.8e-07 (24) | 2e-07 | 7.9e-07 (1) | 1.8e-06 (1) | 0.75% | 3.75% | 0.05% |
| 0.475 | 2.3e-07 (20) | 9e-08 | 7.9e-07 (1) | 1.8e-06 (1) | 1.23% | 5.28% | 0.14% |
| 0.500 | 2.1e-07 (18) | 4e-08 | 7.9e-07 (1) | 1.8e-06 (1) | 2.06% | 7.09% | 0.23% |
| 0.550 | 1.6e-07 (14) | 6e-09 | 7.9e-07 (1) | 1.8e-06 (1) | 5.76% | 14.88% | 0.51% |
| 0.600 | 1.6e-07 (14) | 1e-09 | 7.9e-07 (1) | 1.8e-06 (1) | 15.15% | 27.22% | 1.49% |

### RGB, label noise removed

| target FAR | threshold | impostor pairs above | FRR (all genuine pairs) |
|---|---|---|---|
| 1e-03 | 0.208 | 87066 | 0.09% |
| 1e-04 | 0.269 | 8706 | 0.10% |
| 1e-05 | 0.329 | 870 | 0.13% |
| 1e-06 | 0.395 | 87 | 0.21% |
| 1e-07 | 0.634 | 8 | 19.03% |

| threshold | pair FAR measured (pairs) | pair FAR tail fit | probe FAR, 15 prints (hits) | attempt FAR, 15 prints × ≤16 imgs (hits) | FRR per pair | FRR LFW pairs.txt | FRR per probe, 15 prints |
|---|---|---|---|---|---|---|---|
| 0.350 | 4.4e-06 (386) | 5e-06 | 3.8e-05 (48) | 2.4e-05 (13) | 0.14% | 0.64% | 0.05% |
| 0.375 | 1.7e-06 (148) | 2e-06 | 1.8e-05 (23) | 1.1e-05 (6) | 0.17% | 0.84% | 0.05% |
| 0.400 | 8.4e-07 (73) | 7e-07 | 1.1e-05 (14) | 9.1e-06 (5) | 0.22% | 1.10% | 0.05% |
| 0.425 | 4.5e-07 (39) | 3e-07 | 4.0e-06 (5) | 1.8e-06 (1) | 0.30% | 1.74% | 0.05% |
| 0.450 | 3.0e-07 (26) | 1e-07 | 1.6e-06 (2) | 1.8e-06 (1) | 0.44% | 2.44% | 0.05% |
| 0.475 | 2.5e-07 (22) | 4e-08 | 7.9e-07 (1) | 1.8e-06 (1) | 0.70% | 3.75% | 0.19% |
| 0.500 | 2.3e-07 (20) | 2e-08 | 7.9e-07 (1) | 1.8e-06 (1) | 1.14% | 5.05% | 0.19% |
| 0.550 | 1.8e-07 (16) | 3e-09 | 7.9e-07 (1) | 1.8e-06 (1) | 3.36% | 9.84% | 0.37% |
| 0.600 | 1.5e-07 (13) | 4e-10 | 7.9e-07 (1) | 1.8e-06 (1) | 9.92% | 21.28% | 0.93% |

### Union bound (pair FAR ≤ 4.2e-8)

| variant | threshold from tail fit | measured |
|---|---|---|
| rgb | 0.476 | 0.745 (only 3 pairs above; twins / label errors) |
| gray | 0.497 | 0.717 (only 3 pairs above; twins / label errors) |

### Without label cleaning (raw LFW)

| threshold | gray pair FAR (pairs) | gray attempt FAR (hits) | rgb pair FAR (pairs) | rgb attempt FAR (hits) |
|---|---|---|---|---|
| 0.350 | 9.8e-06 (855) | 3.3e-05 (18) | 5.8e-06 (501) | 2.9e-05 (16) |
| 0.400 | 2.3e-06 (204) | 1.5e-05 (8) | 2.2e-06 (188) | 1.5e-05 (8) |
| 0.450 | 1.6e-06 (135) | 7.3e-06 (4) | 1.6e-06 (140) | 7.3e-06 (4) |
| 0.500 | 1.4e-06 (122) | 7.3e-06 (4) | 1.4e-06 (125) | 7.3e-06 (4) |
| 0.550 | 1.2e-06 (106) | 7.3e-06 (4) | 1.3e-06 (112) | 7.3e-06 (4) |
| 0.600 | 9.8e-07 (85) | 7.3e-06 (4) | 1.1e-06 (93) | 7.3e-06 (4) |

Above about 0.45 the raw pair FAR stays flat at about 1.5e-6. This comes from the same-person and
same-photo pairs listed above, not from the model.

### Current threshold 0.40

| | gray | rgb |
|---|---|---|
| pair FAR, cleaned (pairs) | 1.0e-06 (90) | 8.4e-07 (73) |
| pair FAR, raw (pairs) | 2.3e-06 (204) | 2.2e-06 (188) |
| union bound, 240 × pair FAR | 2.5e-04 | 2.0e-04 |
| simulated attempt FAR (hits) | 9.1e-06 (5) | 9.1e-06 (5) |

So 0.40 is about a 1-in-a-million operating point per comparison. Per attempt, it is roughly
1-in-100,000 in the simulation and 1-in-4,000 under the union bound.

## Recommendation: 0.50

- The union bound with the fitted tail needs 0.476 (rgb) and 0.497 (gray). 0.50 meets both.
  The union bound treats the 240 comparisons as independent, so it is the worst case.
- The simulation already reaches 1.8e-6 per attempt at 0.425, and stays there up to 0.60. The one
  remaining hit (`Mahmoud_Abbas_0012` vs `Hamad_Bin_Isa_al-Khalifa_0001`, 0.61) is very likely a label
  error that was kept to stay pessimistic. Between 0.42 and 0.45 the only other hits are Serena vs Venus Williams.
  The 95% upper bound for 1 hit in 548,832 attempts is 8.6e-6. So 0.45 is the lowest value that the
  simulation alone supports.
- Raising the threshold from 0.45 to 0.50 costs little. On gray, FRR per probe against 15 prints goes from
  0.05% to 0.23%, and a real attempt has up to 16 frames to find one match. It adds margin for the IR
  domain gap, which makes impostor scores less predictable (see below).
- Do not go above about 0.55 without IR data. Single-comparison FRR rises fast (5.8% at 0.55, 15% at 0.60
  on gray), and the IR shift probably lowers genuine scores further.

Identical twins and close siblings score above every usable threshold here (0.5–0.75 on gray).
Like any 2D face system, linux_hello cannot tell identical twins apart.

## Calibration table (F27, F51, F60)

Windows does not threshold the distance itself. Each recognition model carries per-camera-model tables of
(FPR, TAR, distance) rows (`docs/winhello-models/cefa.md`: 5 tables of 20 rows, FAR 1e-5 ↔ distance 0.350),
turns a distance into an estimated false-accept rate by linear interpolation (exponential extrapolation past
the best row, linear past the worst), scales it by the number of comparisons (× 1…4, F52) and recognises a face
when the result is below 1e-5. `linux_hello` does the same from `models/calibration.json`
(`src/calibration.rs`):

- `tables.<name>.rows` are `[FPR, TAR, cosine similarity]`: the false-accept and true-accept rates of **one
  print comparison** at that similarity. `cameras` maps the IR camera's USB `vid:pid` (sysfs `idVendor:idProduct`
  of the device behind `/dev/videoN`; this laptop's camera is `30c9:00f3`) to a table name; `default` names
  the table for cameras not listed.
- Estimated FAR of one comparison (`CalibrationTable::pair_far`, as `sub_1800397B4` with distance = 1 − similarity):
  linear interpolation of the FPR between the bracketing rows, extrapolated linearly below the first row;
  above the last row `FPR_top · exp((d − d_top) / |d_top|)`.
- Estimated FAR of an attempt (`far`) = pair FAR × the comparisons the attempt can make: the number of prints
  in the template being matched × 16 frame pairs (the union bound above), capped at 1. This stands in for
  Windows' comparison multiplier and follows the real template size: the 15 × 16 = 240 of the tables below is
  the case of 15 prints; a 16-print template (one Windows-style enrollment) gives 256, a 31-print one (16
  enrolled + 10 augmented + 5 on-failure) 496, and 100 prints (the per-user maximum, F54) 1,600. The
  duplicate-enrollment check (`FaceStore::duplicate`) uses the same estimate against each other user's template.
- Recognised when `far < 1e-5` (`TARGET_FAR`); augmentation candidate when `far ∈ [5e-6, 1e-5)` (F53).

### Rows (`lfw_gray`)

Generated by `scripts/calibrate.py` (`table` in its JSON output) from the grayscale, label-cleaned run above,
one row every 0.025 of similarity from 0.20 to 0.60. FPR is the measured pair FAR where at least 50 impostor
pairs score above the row (0.20–0.40) and the tail fit from 0.425 on, where the few remaining pairs are twins
and label errors. TAR = 1 − FRR over all genuine pairs.

| similarity | FPR (one comparison) | TAR | source | FAR per attempt (× 240) |
|---|---|---|---|---|
| 0.200 | 1.69e-03 | 0.99911 | measured (147,201 pairs) | 0.41 |
| 0.225 | 6.43e-04 | 0.99906 | measured (56,003) | 0.15 |
| 0.250 | 2.51e-04 | 0.99900 | measured (21,816) | 6.0e-02 |
| 0.275 | 1.05e-04 | 0.99888 | measured (9,106) | 2.5e-02 |
| 0.300 | 4.71e-05 | 0.99873 | measured (4,105) | 1.1e-02 |
| 0.325 | 2.16e-05 | 0.99855 | measured (1,885) | 5.2e-03 |
| 0.350 | 8.50e-06 | 0.99822 | measured (740) | 2.0e-03 |
| 0.375 | 2.70e-06 | 0.99775 | measured (235) | 6.5e-04 |
| 0.400 | 1.03e-06 | 0.99685 | measured (90) | 2.5e-04 |
| 0.425 | 5.29e-07 | 0.99517 | fit | 1.3e-04 |
| 0.450 | 2.18e-07 | 0.99249 | fit | 5.2e-05 |
| 0.475 | 8.97e-08 | 0.98771 | fit | 2.2e-05 |
| 0.500 | 4.17e-08 | 0.97942 | per-attempt bound 1e-5 / 240 | 1.0e-05 |
| 0.525 | 1.52e-08 | 0.96534 | fit | 3.7e-06 |
| 0.550 | 6.27e-09 | 0.94236 | fit | 1.5e-06 |
| 0.575 | 2.58e-09 | 0.90516 | fit | 6.2e-07 |
| 0.600 | 1.06e-09 | 0.84848 | fit | 2.6e-07 |

The 0.500 row carries the per-attempt bound for 15 prints exactly (the fit gives 3.7e-8 there and reaches
4.17e-8 at 0.497), so for a 15-print template the decision is the old one: a best similarity above 0.50 is
recognised, 0.50 itself is not. Between rows the linear interpolation overestimates the FAR of an exponential
tail by up to about 1.6× (the measured points of the first table above: 1e-5 at 0.346 reads 1.06e-5, 1e-6 at
0.401 reads 1.02e-6). The augmentation band [5e-6, 1e-5) is similarity 0.50–0.52 for 15 prints (it was a fixed
0.50–0.55 before).

Since the comparison count follows the template, the recognition point moves with its size (`cargo test`
checks these): a 16-print template needs a similarity above 0.503, a 31-print one above 0.525, and a single
print only 0.43 (16 comparisons). A bigger template is both more likely to contain a close print of the
owner and to be matched by a stranger by chance; the honest bound charges for the second. The simulation
above (1.8e-6 per attempt at 0.50 with 15 prints) says the union bound is loose by a good margin, so the
shift is conservative.

Above the last row (0.60) the Windows extrapolation decays slowly (e-fold per 0.4 of similarity), so the
reported FAR of a strong match is a floor of about 1e-7 per attempt, not a measurement.

Only the LFW table exists. `30c9:00f3` points at it because this camera's frames were checked against it
(`validation_frames`, template = two frontal owner frames: the other owner IR frames score 0.61–0.98, FAR
1e-7…2.5e-7 per attempt; the six faces of the visible-light stranger photo −0.06…0.03, FAR 1); an
IR-calibrated table for this camera needs impostor IR captures (Caveats). `cargo test` checks the shipped
file: the 0.50 ↔ 1e-5 point, the measured 1e-5 and 1e-6 points within 10 %, monotonic FAR over −1…1, and the
default for an unlisted camera.

## IR impostor check (`examples/impostor_capture`, `examples/calibrate_ir`)

The table above is LFW, not this camera. The check that closes that gap for one household or office is
real IR frames of people who are not the owner, scored exactly as an unlock would score them.

**Collect.** Ask a volunteer who is not enrolled; say what is recorded (IR face frames, kept on this
machine, deletable with the directory) and record only with their consent. Then, with the owner away
from the camera:

```sh
cargo build --release --examples
LINUX_HELLO_MODELS=models target/release/examples/impostor_capture /tmp/impostor_<name> --frames 32   # ≤ 60 s
```

The volunteer looks at the camera and moves a little, as when enrolling. Only pairs that would give a
print in an unlock with good lighting are kept (`FacePipeline::analyze`: face ≥ 72 px, away from the
edges, frontal, not occluded, lit 35–205, lit − ambient ≥ 45, ambient ≤ 25), with the face-metered
exposure, as `NNNN_lit.png` / `NNNN_ambient.png` plus `frames.jsonl` (box, brightness). Several
volunteers, several sittings (glasses, lighting, distance) are worth more than many frames of one
sitting: consecutive frames of one person score 0.96–0.99 against each other, so 32 frames of one
sitting are nearly one sample. Relatives are the sample that matters most. Genuine frames come the
same way from the owner (or from `linux_hello watch`, whose `NNNN_lit/ambient.png` have the same layout).

**Score.**

```sh
LINUX_HELLO_MODELS=models target/release/examples/calibrate_ir --store ~/.local/share/linux_hello/faces.json \
    cole /tmp/impostor_<name> [/tmp/genuine]          # one directory per run; or --template-from <owner frames dir>
```

Every pair goes through `FacePipeline::analyze` (the pipeline's own crop: largest face, ambient-
subtracted ArcFace input, every frame gate; the Windows Hello gates when the converted models are
present), so the print is the one an unlock compares. A `*_lit.png` without its ambient frame is
subtracted against a black frame (the lit crop as is); a visible-light photo (anything that is not a
lit/ambient/dark/diff/rgb frame) gets every SCRFD face embedded on the grayscale image, as the LFW
table was built — pseudo-impostors for a dry run, not IR data. Per frame it prints the best
similarity over the template and the estimated per-attempt FAR (`CalibrationTable::far`, charged
for the template size); then:

- impostor frames: median and max best-of-template similarity; how many are **recognised**
  (FAR < 1e-5); how many score ≥ 0.35; how many reach the enroll-on-failure threshold 0.40;
- genuine frames (if given): min and median similarity, how many would not be recognised on their own;
- every impostor print against every template print, per table row: the FPR the table claims, the
  comparisons expected above the row, the comparisons observed (rows where the table expects fewer
  than 0.5 are left out: 32 frames × 16 prints = 512 comparisons only reach the 0.200–0.225 rows).

**What counts as a pass.** `PASS` when no impostor frame is recognised and none scores ≥ 0.35.
0.35 is the point where, on grayscale LFW, a stranger's best similarity over 15 prints is seen in
about 4 of 100,000 probes: one real impostor frame there says the IR scores sit well above the
table and the table is not usable for this camera as it stands. The comparison rows are a
plausibility check, not a fit: with hundreds of comparisons of a few people only the first rows
are measurable, and if the observed counts run several times above the expected ones the IR
distribution is wider than LFW's. A genuine frame below 0.50 is a recognition miss worth a look
(lighting, glasses), not a failure of the impostor check. Any impostor frame ≥ 0.40 means
enroll-on-failure (off by default) could add that person after a password; keep it off, or note
who.

Checked on the saved validation frames (template = `watch_0014` + `watch_0147`, 2 prints): the six
faces of the stranger photo score −0.063…0.028 (FAR ≥ 0.29 per attempt, none recognised), the
other two frontal owner frames 0.983 and 0.965 (FAR 1.3e-8); the `ir_NN` pairs give no print (the
chin is cut, edge gate), which is the gate working. No real IR impostor has been captured yet.

## Caveats

- **IR vs grayscale LFW.** linux_hello sees an 850 nm IR image with its own illuminator. Grayscale LFW
  is visible-light photography with the colour removed. Under IR, skin texture, eyes, eyebrows, makeup and
  glasses look different, and the light comes from the camera. w600k_r50 was trained on RGB web photos
  (WebFace600K). Both genuine and impostor score distributions may shift on IR. The gray results are close to the
  RGB results (gray needs a slightly higher threshold for the same FAR). That suggests the model copes
  with losing colour, but it says nothing about the NIR spectrum. Validate on real IR data: log the
  `MATCH`/`NOMATCH` similarity of real unlocks for genuine headroom, and collect IR captures of other
  people for impostor scores.
- **LFW population bias.** LFW is mostly adult, male, white public figures (published audits put it at roughly
  three quarters male and over 80% white), in press photos. Impostor score distributions are known to differ by
  demographic group; for ArcFace-type models FAR is often higher within some non-white groups and for women, and within one
  family. A household with relatives is the realistic attack, and LFW barely measures it.
- **Possible train/test overlap.** WebFace600K is built from celebrity web images, so it may contain LFW people.
  That would make genuine scores (FRR) look better than they are, and has less effect on the impostor tail.
- **Statistics.** At pair FAR 1e-7 only about 9 impostor pairs are expected, and below that nothing is measured.
  The 4.2e-8 target is set by the tail fit, which assumes the exponential trend continues. The simulation has 1–5 hits
  in the region of interest, so its numbers are good to about a factor of 2–3.
- **Liveness is separate.** These numbers are for zero-effort impostors (a stranger's real face). They say
  nothing about photos, screens or masks; that is the anti-spoof stage.
- **Enrollment model.** The tables assume up to 15 prints per user and max-over-prints matching; the code
  now scales the bound by the real print count (above), so a Windows-style 16-print enrollment plus added,
  augmented and on-failure prints is charged for. `main` at `baea697` stored one mean template per user; with
  one print the union bound is 16 × pair FAR, which gives about 0.42 (gray) / 0.40 (rgb) with the same tail fit.

## Reproduce

```sh
cargo build --release --example embed_dir
ls /tmp/lfw/lfw/*/*.jpg | sed 's|/tmp/lfw/lfw/||' > /tmp/lfw/list.txt
target/release/examples/embed_dir models /tmp/lfw/lfw /tmp/lfw/list.txt /tmp/lfw/emb/lfw 1           # ~34 min CPU
# rerun the images where SCRFD found >1 face, keeping the centred face (list from the tsv 'faces' column)
target/release/examples/embed_dir models /tmp/lfw/lfw /tmp/lfw/list_multi.txt /tmp/lfw/emb/multi_centre 1 centre
python3 scripts/calibrate.py /tmp/lfw/emb/lfw,/tmp/lfw/emb/multi_centre /tmp/lfw/pairs.txt \
    scripts/lfw_same_person.tsv /tmp/lfw/emb/results.json rgb gray
# the `table` rows of the gray, label-cleaned result are the `lfw_gray` rows of models/calibration.json
```
