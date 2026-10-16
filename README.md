# linux_hello

Windows Hello-style face authentication for Linux, in Rust. It recognizes you with the IR camera,
checks that a live face (not a photo or a screen) is in front of it with the IR emitter and the RGB
camera, and plugs into PAM.

Developed and tested on a ThinkPad P16 Gen 3 (Realtek camera, USB `30c9:00f3`) running Gentoo/OpenRC.

## How it works

```
IR metadata (/dev/video3) ─ emitter on/off per frame ─┐
IR camera (/dev/video2) ──────────────────────────────┴▶ lit + ambient frame pair
   lit frame ─▶ SCRFD face ─▶ size/edge/brightness gates ─▶ ArcFace on (lit − ambient) ─▶ best of the user's prints
                                                        └▶ Microsoft's v5 recogniser on the same image (second opinion, when its model is present)
RGB camera (/dev/video0, 640x360) ─▶ SCRFD face ─▶ Windows' colour net veto (p(spoof) > 0.5; without its models FaceNox + MiniFASNetV2, veto if mean P(real) < 0.5)
   session: 4 prints + estimated FAR < 1e-5 (similarity ≈ 0.50; and Microsoft's FAR < 1e-5, distance < 0.35) → recognised; then 2 live votes → MATCH, 3 spoof votes → reject
            16 pairs → give up (once more if the face moved or the exposure changed); 4 occluded frames in a row → give up
```

Details and the Windows Hello findings behind each step: [docs/capture-and-liveness.md](docs/capture-and-liveness.md).

- **IR emitter:** needs no setup on this camera; it lights every other frame. The camera's
  metadata node says which frames are lit. Each lit frame is paired with the ambient frame next to
  it (< 300 ms apart), and recognition uses `lit − min(lit, ambient)`, the light the emitter added.
- **Frame gates:** the face must be ≥ 72 px wide, away from the frame edges, and its lit
  brightness between 35 and 205. The emitter must add ≥ 45 and ambient IR stay ≤ 25 for the pair
  to count as "good"; otherwise it is only used for matching. Screens emit no IR and show no face
  to the IR camera.
- **Recognise, then liveness:** like Windows, the user is recognised first (4 prints, best above
  the threshold); liveness votes count from then on. A good-lighting pair is a live vote; the RGB
  liveness models (trained on visible light) can veto it, which makes it a spoof vote. They are
  there to catch printed photos.
- **Second opinion:** with the converted Windows Hello models, every print also carries the feature of
  Microsoft's own face recogniser, and a face is recognised only when ArcFace and Microsoft's net both
  put its false-accept rate under 1e-5 (Microsoft's own calibration table; `ms=` `ms_far=` in the logs).
  Prints enrolled without the models are matched by ArcFace alone.
- **Several prints per user:** enrollment keeps 16 prints taken over a few seconds while you move
  your head a little (near-identical frames do not count); `enroll --add` appends more (glasses,
  other light) up to 100. A probe matches on the closest one, and the false-accept estimate is
  charged for every print in the template. After a successful unlock with a borderline match the
  frame is added as an extra print (at most 10, at most 3 generations), so the template follows
  slow changes in appearance.
- **Feedback:** a failed attempt says why (too dark, too bright, too far, at the edge, occluded,
  not frontal, no face, not live, not recognized). Before giving up on a face that moved or whose
  exposure changed, the session tries once more; a face covered for 4 frames in a row gives up at once.
- **1:1 verification:** a face is only ever compared with the template of the user being
  authenticated, so one enrolled face cannot unlock another account.
- **Two cameras at once:** works only if the RGB stream starts before the IR stream.

## Components

| Path | What |
|------|------|
| `src/` | `linux_hello` library and CLI: `enroll`, `auth`, `daemon`, `watch`, `camera`, `list`, `remove` |
| `pam/` | `pam_linux_hello.so`, a tiny PAM module (no ML code) that asks the daemon over a Unix socket |
| `contrib/openrc/linux_hello` | OpenRC service for the daemon |
| `scripts/download_models.sh` | Fetches the models and checks their SHA-256 |

## Build

```sh
./scripts/download_models.sh     # ~190 MB into models/
cargo build --release            # builds target/release/linux_hello and target/release/libpam_linux_hello.so
```

## Use

```sh
linux_hello enroll cole          # 16 IR prints over a few seconds (look at the camera, move a little); 10 s cap
linux_hello enroll cole --add    # add prints to the existing template (e.g. with glasses on), up to 100
linux_hello auth                 # verifies $USER: IR recognition, then liveness votes
linux_hello watch --seconds 30   # logs every frame pair's scores, saves frames to /tmp/linux_hello_watch
linux_hello camera               # saves one pair to /tmp/linux_hello_{lit,ambient,diff,rgb}.png
```

Flags: `--camera vid:pid` (the IR camera by USB id; default: the first known IR camera present,
`30c9:00f3` on this ThinkPad; its IR, metadata and RGB nodes are looked up in sysfs at every
request, so renumbering or re-plugging is followed; see [docs/capture-and-liveness.md](docs/capture-and-liveness.md#1-litambient-pairs-f10-f12-f13-f15-l1l3)),
`--no-ir-metadata` (tell lit frames by brightness), `--no-liveness` (no RGB veto; the lighting
status still votes), `--rotate 0|90|180|270` (turn the frames of a sideways or upside-down camera
upright, counter-clockwise), `--store`.
The face store is `/var/lib/linux_hello/faces.json` as root, `~/.local/share/linux_hello/faces.json`
otherwise; it is written atomically with mode 0600.

**Store format change:** templates are now lists of prints. A store written by an older version
(one averaged vector per user) is refused with an error; enroll again (`linux_hello enroll <user>`,
or `sudo contrib/install.sh` for the system store). There is no conversion.

## Install (as root)

```sh
sudo contrib/install.sh      # installs, enrolls $SUDO_USER, enables face login for sudo, swaylock, polkit, su and console login
sudo contrib/install.sh --no-lock --no-polkit --no-login --no-su   # any subset; sudo always
sudo contrib/uninstall.sh    # undo (add --purge to delete the face store)
```

sudo, polkit and su get one face session; the lock screen (swaylock) and console login get a
presence wait of up to 10 s after Enter (`timeout=`, `wait_services=` module arguments). The
daemon's socket is open to local users, who may only ask about themselves and cannot obtain a
credential; the PAM stack grants. Details, flow, what each application shows and caveats:
[docs/pam.md](docs/pam.md). Face login is skipped for SSH sessions
(`SSH_CONNECTION`/`SSH_TTY`/`PAM_RHOST`), because the camera sees whoever sits at the laptop.

## TPM sealing (optional)

Built with `--features tpm`, `linux_hello seal [--pcrs 7]` encrypts the face store with
AES-256-GCM under a key sealed by the TPM 2.0, so the file is useless away from this machine.
The daemon unseals it by itself; `linux_hello unseal` turns it back into plain JSON. The TPM is
chosen with `TPM2TOOLS_TCTI` (default `device:/dev/tpmrm0`). See [docs/tpm.md](docs/tpm.md) for
what this does and does not protect, PCR binding, enabling the TPM in the BIOS, and migration.

## Measured on the P16 Gen 3

| | |
|--|--|
| Owner, IR similarity to the closest print | 0.96–0.98 (recognised at FAR < 1e-5, i.e. about 0.50; `far` ≈ 1e-7) |
| Stranger faces (group photo) | −0.06…0.03 |
| Owner, Microsoft's recogniser distance to the closest print, same sitting / another sitting with the chin cut off | 0.007–0.025 (`ms_far` ≈ 8e-7) / 0.42–0.45 (refused: FAR 1e-5 is distance 0.35) |
| Strangers, Microsoft's recogniser distance | 0.85–1.0 (`ms_far` ≥ 0.07) |
| Microsoft's recogniser, crop + net per frame | 4–6 ms |
| Windows Hello anti-spoofing frame total, owner at face brightness ~100 | +1 on about 90 % of good frames |
| Owner at face brightness ~150 (close, lamp behind the head), before face-metered exposure | +1 on about 35 % of good frames |
| `sudo` unlock, end to end, 16-frame warm-up | ~2.6 s (1.75 s camera start + 0.5 s decision) |
| First usable frame pair after opening the cameras, 4-frame warm-up and IR first | 0.54 s (expected unlock ~1.5 s, not yet measured) |

## Known limitations

- This camera's emitter-off frames are blank, so there is no ambient image: the ambient subtraction
  is a no-op and strong sunlight is not detected (Windows' ambient > 25 rule cannot fire). The
  general-IR anti-spoofing net runs instead of the diffusion net (F42).
- Not yet tested against a real printed-photo or phone-screen attack.
- The IR anti-spoofing vote, the head-pose gate and the occlusion gate run only with the converted
  Windows Hello models (`models/windows_hello/converted`, git-ignored, personal use). Without them
  the IR evidence is only the lighting status and the open RGB models can only veto: a print or
  mask that reflects IR like skin passes the gate and the RGB veto has to catch it.
- With those models, recognition also depends on Microsoft's recogniser: it can refuse a face that
  ArcFace accepts (on the saved frames, the owner's chin-cut frames against a full-face template), and
  its table's FAR is Microsoft's estimate, not charged for the template size like ArcFace's. Prints
  enrolled before the models were there have no Microsoft feature and are matched by ArcFace alone;
  re-enroll to get the second opinion on them.
- The 3 largest faces of each frame are analysed and tracked (≈ 35 ms more per pair for each
  face after the first); smaller ones are ignored. A photo of the owner held exactly where the
  owner's face was continues its track (same place, same identity); the liveness votes have to
  catch it.
- Microsoft's anti-spoofing nets are sure of a live face at a face brightness near 100 and unsure
  near 150; the face-metered exposure keeps the face near the camera's target, but a close face
  with a lamp behind the head has not been re-tested since.
- The false-accept estimate behind the match decision (FAR < 1e-5 per attempt, charged for every
  print in the template × 16 frame pairs) comes from a calibration table built on grayscale LFW
  photos ([docs/calibration.md](docs/calibration.md)), not yet on IR captures.
- The RGB camera streams at 640×360: on this laptop's USB 2.0 link 640×480 arrives truncated in
  every frame while the IR stream runs. A broken or much darker frame is skipped (up to 4 per
  pair, then the pair has no RGB frame and the colour net is skipped); a frame with no face in
  the RGB image counts as a veto only without the Windows Hello models. An RGB camera that
  delivers no frame for 1 s is dropped for the session (the colour net is skipped; without the
  Windows Hello models the open RGB models then veto every frame).
- Templates are stored unencrypted (root-only file) unless the store is sealed with the TPM
  (optional, see below).

## Licenses

The code is MIT. The models are not part of this repository and keep their own licenses:
InsightFace pretrained models are for **non-commercial research use only**; FaceNox is MIT;
Silent-Face MiniFASNetV2 is Apache-2.0.
