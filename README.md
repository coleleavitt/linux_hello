# linux_hello

Windows Hello-style face authentication for Linux, in Rust. It recognizes you with the IR camera,
checks with the RGB camera that a live face (not a photo) is in front of it, and plugs into PAM.

Developed and tested on a ThinkPad P16 Gen 3 (Realtek camera, USB `30c9:00f3`) running Gentoo/OpenRC.

## How it works

```
IR camera (/dev/video2) ──▶ SCRFD face detection ──▶ 5-point alignment ──▶ ArcFace embedding ──▶ compare with the user's template
RGB camera (/dev/video0) ─▶ SCRFD face detection ──▶ FaceNox + MiniFASNetV2 liveness (mean P(real) ≥ 0.5)
```

- **IR emitter:** needs no setup on this camera. It lights every other frame, so the camera code
  skips 16 warm-up frames and keeps the brighter frame of each pair.
- **Recognition on IR, liveness on RGB:** both liveness models were trained on visible-light
  images and reject IR frames. Screens emit no IR, so a phone replay shows no face to the IR camera;
  the RGB liveness models are there to catch printed photos.
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
linux_hello enroll cole          # averages 5 IR frames
linux_hello auth                 # verifies $USER: IR recognition + RGB liveness
linux_hello watch --seconds 30   # logs every frame's scores, saves frames to /tmp/linux_hello_watch
```

Flags: `--rgb` (recognize on RGB), `--no-liveness`, `--device`, `--liveness-device`, `--store`.
The face store is `/var/lib/linux_hello/faces.json` as root, `~/.local/share/linux_hello/faces.json`
otherwise; it is written atomically with mode 0600.

## Install (as root)

```sh
sudo contrib/install.sh      # installs, enrolls $SUDO_USER, enables face login for sudo and sudo -i
sudo contrib/uninstall.sh    # undo (add --purge to delete the face store)
```

Details, flow and caveats: [docs/sudo.md](docs/sudo.md). Face login is skipped for SSH sessions
(`SSH_CONNECTION`/`SSH_TTY`/`PAM_RHOST`), because the camera sees whoever sits at the laptop.

## Measured on the P16 Gen 3

| | |
|--|--|
| Owner, IR similarity | 0.82–0.99 (threshold 0.4) |
| Stranger (InsightFace sample photo) | −0.04 |
| Owner, RGB liveness | 0.92–0.99 |
| PAM `sudo`-style request, end to end | ~2.1 s |

## Known limitations

- Not yet tested against a real printed-photo or phone-screen attack.
- The RGB camera underexposes 2–3 frames about every 5 s; liveness rejects those frames and
  `auth` moves on to the next one.
- Templates are stored unencrypted (root-only file). No TPM sealing.

## Licenses

The code is MIT. The models are not part of this repository and keep their own licenses:
InsightFace pretrained models are for **non-commercial research use only**; FaceNox is MIT;
Silent-Face MiniFASNetV2 is Apache-2.0.
