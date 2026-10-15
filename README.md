# linux_hello

Windows Hello-style face authentication for Linux, in Rust. It recognizes you with the IR camera,
checks that a live face (not a photo or a screen) is in front of it with the IR emitter and the RGB
camera, and plugs into PAM.

Developed and tested on a ThinkPad P16 Gen 3 (Realtek camera, USB `30c9:00f3`) running Gentoo/OpenRC.

## How it works

```
IR metadata (/dev/video3) ─ emitter on/off per frame ─┐
IR camera (/dev/video2) ──────────────────────────────┴▶ lit + ambient frame pair
   lit frame ─▶ SCRFD face ─▶ lighting gate ─▶ ArcFace on (lit − ambient) ─▶ best of the user's prints
RGB camera (/dev/video0) ─▶ SCRFD face ─▶ FaceNox + MiniFASNetV2 (veto if mean P(real) < 0.5)
   per pair: liveness vote ─▶ session: 2 live pairs + a live match → MATCH; 3 spoof pairs or 16 pairs → reject
```

Details and the Windows Hello findings behind each step: [docs/capture-and-liveness.md](docs/capture-and-liveness.md).

- **IR emitter:** needs no setup on this camera; it lights every other frame. The camera's
  metadata node says which frames are lit. Each lit frame is paired with the ambient frame next to
  it (< 300 ms apart), and recognition uses `lit − min(lit, ambient)`, the light the emitter added.
- **Lighting gate:** on the face, the emitter must add 45–205 (of 255) and ambient IR must stay
  ≤ 25. Other pairs are ignored. Screens emit no IR and show no face to the IR camera.
- **Liveness votes:** a pair that passes the gate is a live vote; the RGB liveness models (trained
  on visible light) can veto it, which makes it a spoof vote. They are there to catch printed photos.
- **Several prints per user:** enrollment keeps 5 prints; a probe matches on the closest one.
  After each successful unlock the matching frame is added as an extra print (at most 10, at most 3
  generations), so the template follows slow changes in appearance.
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
linux_hello enroll cole          # keeps 5 IR prints (each must pass the lighting gate)
linux_hello auth                 # verifies $USER: IR recognition + liveness votes
linux_hello watch --seconds 30   # logs every frame pair's scores, saves frames to /tmp/linux_hello_watch
linux_hello camera               # saves one pair to /tmp/linux_hello_{lit,ambient,diff,rgb}.png
```

Flags: `--device` (IR, default `/dev/video2`), `--ir-metadata` (default `/dev/video3`),
`--no-ir-metadata` (tell lit frames by brightness), `--liveness-device` (default `/dev/video0`),
`--no-liveness` (no RGB veto; the lighting gate still applies), `--store`.
The face store is `/var/lib/linux_hello/faces.json` as root, `~/.local/share/linux_hello/faces.json`
otherwise; it is written atomically with mode 0600.

**Store format change:** templates are now lists of prints. A store written by an older version
(one averaged vector per user) is refused with an error; enroll again (`linux_hello enroll <user>`,
or `sudo contrib/install.sh` for the system store). There is no conversion.

## Install (as root)

```sh
sudo contrib/install.sh      # installs, enrolls $SUDO_USER, enables face login for sudo and sudo -i
sudo contrib/uninstall.sh    # undo (add --purge to delete the face store)
```

Details, flow and caveats: [docs/sudo.md](docs/sudo.md). Face login is skipped for SSH sessions
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
| Owner, IR similarity | 0.82–0.99 (threshold 0.4) |
| Stranger (InsightFace sample photo) | −0.04 |
| Owner, RGB liveness | 0.92–0.99 |
| PAM `sudo`-style request, end to end | ~2.1 s |

## Known limitations

- Not yet tested against a real printed-photo or phone-screen attack.
- No IR anti-spoofing model: Windows votes "live" with two IR CNNs (proprietary). Here the IR
  evidence is only the lighting gate, and the RGB models can only veto. A print or mask that
  reflects IR like skin passes the gate; the RGB veto has to catch it.
- Ambient subtraction has no motion compensation (Windows warps with an affine transform); fast
  head movement between the two frames (67 ms) leaves edges in the difference image.
- The similarity threshold is a raw cosine value (0.4), not a calibrated false-accept rate like
  Windows' 1e-5; calibration is in progress.
- The RGB camera underexposes 2–3 frames about every 5 s; `OpenCameras` skips frames much darker
  than the recent average, and a frame with no face in the RGB image counts as a veto.
- Templates are stored unencrypted (root-only file) unless the store is sealed with the TPM
  (optional, see below).

## Licenses

The code is MIT. The models are not part of this repository and keep their own licenses:
InsightFace pretrained models are for **non-commercial research use only**; FaceNox is MIT;
Silent-Face MiniFASNetV2 is Apache-2.0.
