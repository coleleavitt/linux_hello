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
install -m755 target/release/linux_hello /usr/local/bin/
install -m755 target/release/libpam_linux_hello.so /lib64/security/pam_linux_hello.so
mkdir -p /usr/local/share/linux_hello && cp -r models /usr/local/share/linux_hello/
install -m755 contrib/openrc/linux_hello /etc/init.d/ && rc-service linux_hello start
LINUX_HELLO_MODELS=/usr/local/share/linux_hello/models linux_hello enroll cole
```

Then add this line to `/etc/pam.d/sudo` **above** `auth substack system-auth`, and keep a root
shell open while testing:

```
auth  sufficient  pam_linux_hello.so
```

`sufficient` means a match skips the password; every other result is ignored and the normal
password prompt runs. The module returns `PAM_IGNORE` immediately if the daemon is not running,
and `PAM_AUTH_ERR` for a wrong, spoofed or missing face. Module options: `socket=PATH`
(default `/run/linux_hello.sock`), `timeout=SECONDS` (default 10).

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
