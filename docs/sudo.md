# sudo with face login

Status: built and tested with real libpam as a normal user (`pam_start_confdir` with a test
service in `/tmp`). The system-wide install (`contrib/install.sh`) needs root once.

## What happens when you run `sudo`

1. `sudo` runs the PAM `auth` stack of `/etc/pam.d/sudo` (or `sudo-i` for `sudo -i`).
2. The first line is `auth sufficient pam_linux_hello.so`. The module:
   - returns `PAM_IGNORE` at once for remote sessions (`SSH_CONNECTION`, `SSH_TTY` or `PAM_RHOST`
     set): the camera shows whoever sits at the laptop, not the remote caller;
   - connects to `/run/linux_hello.sock`; if the daemon is not running it returns `PAM_IGNORE`
     at once;
   - prints `Looking for your face...` and sends `AUTH <user>`.
3. The daemon (root, models already loaded) re-reads `/var/lib/linux_hello/faces.json`, opens the
   RGB camera, then the IR camera and its metadata node, warms up, and reads up to 16 IR frame
   pairs (lit + ambient, told apart by the camera's per-frame emitter flag). For each pair:
   face on the lit frame → lighting gate (emitter adds 45–205, ambient ≤ 25) → live vote, unless
   the RGB liveness models veto it (mean of FaceNox and MiniFASNetV2 < 0.5 → spoof vote) →
   similarity of `lit − ambient` with the closest of **that user's** prints > 0.50 ([calibration.md](calibration.md)).
   It replies `MATCH` once 2 pairs voted live and a live pair matched; `NOTLIVE` after 3 spoof
   votes or 16 pairs without 2 live votes; `NOMATCH` if the face was live but never matched.
   After a match it adds that pair's print to the template and saves the store (a sealed store
   stays sealed). See [capture-and-liveness.md](capture-and-liveness.md).
4. `MATCH` → `PAM_SUCCESS` → `sufficient` ends the stack: no password.
   Anything else → the line is ignored and the normal password prompt runs
   (`sufficient` = `success=done`, every other result `ignore`, per Linux-PAM `pam_handlers.c`).

## Measured

| Case | Result | Time |
|------|--------|------|
| Owner at the laptop | `PAM_SUCCESS` (similarity 0.82–0.89, liveness 0.84–0.96) | 2.1 s |
| Daemon not running | `PAM_IGNORE` → password | 0 s |
| User not enrolled | `PAM_IGNORE` → password | 0 s |
| `SSH_CONNECTION` / `SSH_TTY` / `PAM_RHOST` set | `PAM_IGNORE` → password | 0 s |
| Nobody / someone else at the camera | `PAM_AUTH_ERR` → password | up to ~2–3 s |

## Install / remove

```sh
cargo build --release && sudo contrib/install.sh     # enrolls $SUDO_USER, adds the PAM line, starts the service
sudo contrib/uninstall.sh [--purge]                  # removes everything (--purge also deletes the face store)
```

The installer backs up `/etc/pam.d/sudo` and `sudo-i` to `*.linux_hello.bak`. Keep a root shell
open the first time.

## Caveats

- Gentoo generates `/etc/pam.d/sudo` (pam eclass). A sudo upgrade may offer a new version through
  `etc-update`/`dispatch-conf`; keep the face line when merging.
- sudo caches successful authentication (default 15 min per terminal), so the camera is only used
  when sudo would otherwise ask for a password.
- Anyone (or any program, including an AI agent running as you) that can run `sudo` while your face
  is in front of the camera gets root. That is the point of the feature, but be aware of it.
- Liveness is not yet verified against a real printed photo or phone screen (see README).
- Face stores from before multi-print templates are refused; re-run `sudo contrib/install.sh` (or
  `linux_hello enroll <user>` as root) after upgrading.
- The RGB camera sometimes underexposes 2–3 frames every ~5 s; `OpenCameras` skips frames darker
  than half the running average. The skip logic is in place but the dips did not occur in the
  verification run, so it is not yet confirmed on live frames.
