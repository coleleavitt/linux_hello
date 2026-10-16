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
   face on the lit frame → size/edge/brightness gates → print (from `lit − ambient` when the
   lighting is good) → similarity with the closest of **that user's** prints. Once 4 prints are in
   and the estimated false-accept rate of the best one is < 1e-5 (similarity ≈ 0.50, depending on
   the template size, [calibration.md](calibration.md)) the user is recognised;
   from then on each good-lighting pair votes: live when the Windows Hello anti-spoofing nets
   agree (their colour net on the RGB frame can veto), or, without those models, unless the open
   RGB models veto it (mean of FaceNox and MiniFASNetV2 < 0.5 → spoof vote). See
   [capture-and-liveness.md](capture-and-liveness.md) §5.
   It replies `MATCH` after 2 live votes, `NOTLIVE` after 3 spoof votes, and after 16 pairs
   `NOTLIVE` (recognised but not live), `NOMATCH <similarity>` or `NOFACE <reason>`; if the face
   moved or the exposure changed during the session it tries one more round of 16 pairs before
   `NOMATCH`/`NOTLIVE` (so a failed attempt can take ≈ 4.5 s), and a face covered for 4 frames in a
   row ends the session with `NOFACE occluded` at once. After a borderline match it adds that
   pair's print to the template and saves the store (a sealed store stays sealed). See
   [capture-and-liveness.md](capture-and-liveness.md).
4. `MATCH` → `PAM_SUCCESS` → `sufficient` ends the stack: no password.
   `NOMATCH`/`NOTLIVE`/`NOFACE` → the module prints a short reason ("Face too dark: move closer
   to the camera", "Face not accepted as live", ...) and returns `PAM_AUTH_ERR`.
   In every case but `MATCH` the line is ignored and the normal password prompt runs
   (`sufficient` = `success=done`, every other result `ignore`, per Linux-PAM `pam_handlers.c`).

## Measured

| Case | Result | Time |
|------|--------|------|
| Owner at the laptop | `PAM_SUCCESS` (similarity 0.82–0.89, liveness 0.84–0.96) | 2.1 s |
| Daemon not running | `PAM_IGNORE` → password | 0 s |
| User not enrolled | `PAM_IGNORE` → password | 0 s |
| Lid closed, built-in IR camera (`UNAVAILABLE lid_closed`) | `PAM_IGNORE` → password, message "Face login not available: the lid is closed" (libpam test with a stub daemon) | 0 s |
| `SSH_CONNECTION` / `SSH_TTY` / `PAM_RHOST` set | `PAM_IGNORE` → password | 0 s |
| Nobody / someone else at the camera | `PAM_AUTH_ERR` → password | up to ~2–3 s (~4.5 s with a re-attempt; not measured) |

## Install / remove

```sh
cargo build --release && sudo contrib/install.sh     # enrolls $SUDO_USER, adds the PAM line, starts the service
sudo contrib/uninstall.sh [--purge]                  # removes everything (--purge also deletes the face store)
```

The installer backs up `/etc/pam.d/sudo` and `sudo-i` to `*.linux_hello.bak`. Keep a root shell
open the first time.

## Enrolling

`contrib/install.sh` enrolls `$SUDO_USER` into the system store `/var/lib/linux_hello/faces.json`:
look at the camera and move your head a little while 16 prints are taken (a few seconds, 10 s at
most; the terminal counts `7/16`). To enroll again, or another user, as root:

```sh
sudo LINUX_HELLO_MODELS=/usr/local/share/linux_hello/models linux_hello enroll <user>          # replaces the template
sudo LINUX_HELLO_MODELS=/usr/local/share/linux_hello/models linux_hello enroll <user> --add    # adds prints, e.g. with glasses on
```

`--add` is Windows' "improve recognition": the template keeps its prints and grows by up to 16
per run, 100 in all; the daemon re-reads the store at the next request. Both refuse a face that
matches another enrolled user (`--force` overrides). Details:
[capture-and-liveness.md](capture-and-liveness.md#enrollment-f54-f28).

## Enroll-on-failure (opt-in)

When your face is not recognised (`NOMATCH`) and you then type your password, the daemon can add
that face to your template, so the next sudo recognises you in the same conditions (glasses,
lighting). Details and limits: [capture-and-liveness.md](capture-and-liveness.md#9-enroll-on-failure-f31-opt-in).

Enable it in the daemon only:

```sh
echo 'LINUX_HELLO_ARGS="--enroll-on-failure"' > /etc/conf.d/linux_hello
rc-service linux_hello restart
```

Other daemon options go in the same variable, e.g. `LINUX_HELLO_ARGS="--enroll-on-failure --rotate 180"`
for a camera mounted upside down (`--rotate 0|90|180|270`, degrees counter-clockwise, applied to
the IR and RGB frames before detection; the same option works for `enroll`, `auth` and `watch`).

The PAM configuration stays as installed; no extra line is needed:

```
auth		sufficient	pam_linux_hello.so
auth	substack		system-auth
```

The commit is made from `pam_sm_setcred` of the same `auth` line. sudo calls `pam_setcred` (with
`PAM_REINITIALIZE_CRED`, sudo 1.9.17 `plugins/sudoers/auth/pam.c`) only after the auth stack
succeeded, and only in the PAM transaction whose face check failed holds the token. A session
hook (`pam_sm_open_session`) is not used: it would need a `session` line, and an `auth optional`
line after `system-auth` is not safe because it also runs after a wrong password (a substack
failure does not stop the parent stack). `Defaults !pam_setcred` in sudoers turns the commit off.

Checked with real libpam (`pam_start_confdir`, a test service with `pam_permit` standing in for the
password, and a stub daemon):

| Case | Daemon receives |
|------|-----------------|
| `NOMATCH … <token>`, password ok, `PAM_USER` changed to root, `pam_setcred(REINITIALIZE)` | `ENROLL_ON_FAILURE cole <token>` once |
| `NOMATCH … <token>`, password ok, `pam_setcred(ESTABLISH)`, daemon answers `ADDED 0.432` | the same, and the conversation gets `PAM_TEXT_INFO` "Face print added after password login" (`scripts/pam_harness.py`) |
| … daemon answers `REFUSED too_far 0.3` | the same, no message (syslog only) |
| `MATCH`, `pam_setcred` | nothing |
| `NOMATCH … <token>`, then a second face check `NOFACE`, password ok, `pam_setcred` | nothing |
| `NOMATCH` without token, `pam_setcred` | nothing |
| `NOMATCH … <token>`, `pam_setcred(DELETE)` only | nothing |
| `NOMATCH … <token>`, `pam_setcred(ESTABLISH)` twice | one `ENROLL_ON_FAILURE` |
| `NOMATCH … <token>`, password wrong | auth fails, no `pam_setcred` |

The real daemon, run as a normal user, refuses `ENROLL_ON_FAILURE` with `ERROR … from uid 1000:
only root may add prints`. Not tested: a full sudo run as root with the camera, so whether sudo
shows the "Face print added" line on the terminal at `pam_setcred` time is assumed from its
conversation function, not seen. `scripts/pam_harness.py <module.so> <socket> 'reply|reply'`
reproduces the libpam checks above without root or a camera.

**Risk:** anyone who knows your password can add their face this way if it is already similar to
yours (similarity ≥ 0.40), and then use sudo with their face. The daemon logs every
`enroll-on-failure` result, and when a print is added the PAM module prints "Face print added
after password login" right after the password, so an addition you did not expect is visible;
`linux_hello enroll <user>` resets the template.

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
