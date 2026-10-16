# Face login through PAM: sudo, screen lock, polkit, su, console login

Status: a `NOFACE` session whose last pair had no face at all used to panic the daemon
(`Session::decided` indexed a track that did not exist; fixed on 2026-10-16, with the daemon now
surviving a panicked request). The module and the daemon are tested with real libpam as a normal user
(`pam_start_confdir`, `scripts/pam_harness.py`, stub daemon) for every service below, and the
daemon's access control and presence wait with the daemon running as a user on a scratch socket
and the real cameras. The live `sudo` path is tested end to end; `swaylock` and `polkit` live
runs are pending (2026-10-16). The system-wide install (`contrib/install.sh`) needs root once.

## Which services, and what each one does

| PAM service (`PAM_SERVICE`) | Where the line goes | Who runs the PAM stack | Behaviour |
|---|---|---|---|
| `sudo`, `sudo-i` | `/etc/pam.d/sudo`, `sudo-i` | sudo, as root | one session (2–4.5 s) |
| `polkit-1` (polkit-gnome, any polkit agent) | `/etc/pam.d/polkit-1` | `polkit-agent-helper-1`, setuid root | one session |
| `su`, `su-l` | `/etc/pam.d/su` (`su-l` includes it), after `pam_rootok` and `pam_wheel` | su, setuid root | one session for the **target** account (`PAM_USER` is who you become, unlike sudo): `su` to root looks for root's template, which does not exist unless you enroll root with your face (`linux_hello enroll root`; then anyone pam_wheel lets through can become root with that face); `su alice` looks for alice's. Root needs no face (`pam_rootok`), non-wheel users are still refused |
| `swaylock` | `/etc/pam.d/system-local-login` (`swaylock` includes `login`, which includes it); `/etc/pam.d/swaylock` with `--no-login` | swaylock's PAM child, **as the user** | presence wait, up to 10 s |
| `login` (console) | `/etc/pam.d/system-local-login` | login, as root | presence wait, up to 10 s |
| `greetd`, `hello-lock` (hello-greeter, [greeter.md](greeter.md)) | `/etc/pam.d/greetd`, `/etc/pam.d/hello-lock`, own files written by `contrib/install-greeter.sh` | greetd's worker as root / the locker as the user | presence wait, `password_prompt` |
| `system-login`, `system-remote-login`, `sshd` | never | | the module also returns `PAM_IGNORE` on its own when `SSH_CONNECTION`, `SSH_TTY` or `PAM_RHOST` is set |

The line is the same everywhere, placed right before the password stack (the first
`auth include`/`auth substack` line of the file):

```
auth		sufficient	pam_linux_hello.so
```

Module arguments: `socket=PATH` (default `/run/linux_hello.sock`), `timeout=SECONDS` (the
presence wait, default 10), `wait_services=NAME,NAME` (the services that get a presence wait,
default `swaylock,login`; `wait_services=` turns it off, `wait_services=swaylock,login,hyprlock`
adds a locker), `password_prompt` (below, for `hello-greeter` only). The module reads
`PAM_SERVICE` (the name the application gave `pam_start`; an `include` does not change it) and
sends `AUTH <user> <timeout> progress` for a wait service, `AUTH <user> progress` otherwise.
The verdict line's first word decides, as before; `NOTICE` lines may come before it (progress,
shown at once) and after it (notices, shown after the reason).

### `password_prompt` (the greeter's lines)

A greeter that looks for the face over and over cannot cancel a PAM stack: a cancelled
`pam_unix` prompt is a failed login for `pam_faillock authfail` (`system-auth`; three in 15 min
lock the password for 10 min). On a line

```
auth  [success=done conv_err=die default=ignore]  pam_linux_hello.so password_prompt wait_services=greetd timeout=10
```

the module asks for the password itself after anything but a match (`PAM_PROMPT_ECHO_OFF`
"Password") and leaves the answer in `PAM_AUTHTOK`, so `pam_unix try_first_pass` never prompts;
an empty answer or a failed conversation returns `PAM_CONV_ERR` and `conv_err=die` ends the
stack before the password modules. The greeter answers only that prompt (its text tells it from
`pam_unix`'s "Password: "). Checked through real libpam with the real module in
`greeter/tests/pam.rs`; details in [greeter.md](greeter.md).

### One session (sudo, polkit, su)

1. The application runs its `auth` stack. The first line is the face module, which:
   - returns `PAM_IGNORE` at once for remote sessions;
   - connects to `/run/linux_hello.sock`; if the daemon is not running it returns `PAM_IGNORE`
     at once;
   - prints `Looking for your face...` (`PAM_TEXT_INFO`) and sends `AUTH <user>`.
2. The daemon (root, models loaded) re-reads `/var/lib/linux_hello/faces.json`, finds the
   camera nodes in sysfs, opens the IR camera and its metadata node, then the RGB camera, and
   reads up to 16 IR frame pairs (lit + ambient, told apart by the camera's per-frame emitter
   flag). For each pair: face on the lit frame → size/edge/brightness gates → print (from
   `lit − ambient` when the lighting is good) → similarity with the closest of **that user's**
   prints. Once 4 prints are in and the estimated false-accept rate of the best one is < 1e-5
   (similarity ≈ 0.50, depending on the template size, [calibration.md](calibration.md)) the user
   is recognised; from then on each good-lighting pair votes: live when the Windows Hello
   anti-spoofing nets agree (their colour net on the RGB frame can veto), or, without those
   models, unless the open RGB models veto it (mean of FaceNox and MiniFASNetV2 < 0.5 → spoof
   vote). See [capture-and-liveness.md](capture-and-liveness.md) §5.
   It replies `MATCH` after 2 live votes, `NOTLIVE` after 3 spoof votes, and after 16 pairs
   `NOTLIVE` (recognised but not live), `NOMATCH <similarity>` or `NOFACE <reason>`; if the face
   moved or the exposure changed during the session it tries one more round of 16 pairs before
   `NOMATCH`/`NOTLIVE` (so a failed attempt takes ≈ 4.2 s, measured), and a face covered for 4
   frames in a row ends the session with `NOFACE occluded` at once. After a borderline match it
   adds that pair's print to the template and saves the store (a sealed store stays sealed).
3. `MATCH` → `PAM_SUCCESS` → `sufficient` ends the stack: no password.
   `NOMATCH`/`NOTLIVE`/`NOFACE` → the module prints a short reason ("Couldn't recognize you.
   Please sign in with your password." — Windows' text, id 125 — for `NOMATCH`; "Face too dark:
   move closer to the camera", "Face not accepted as live", ...) and returns `PAM_AUTH_ERR`.
   The request carries the word `progress`, so the daemon also streams `NOTICE recognized` the
   moment the face is recognised and the liveness votes begin; the module shows it at once as
   "Making sure it's you..." (Windows' id 123; [windows-hello-ui.md](windows-hello-ui.md) state
   3), and `NOTICE blink` ("Blink once") when the daemon runs with `--blink-challenge` and asks
   the matched face to blink before it accepts ([liveness-physics.md](liveness-physics.md) §7).
   Without `progress` (older modules) the first line of the reply is the verdict, as before.
   `BUSY` (another request has the cameras), `UNAVAILABLE lid_closed`, `NOTENROLLED`, `ERROR`
   → `PAM_IGNORE`.
   In every case but `MATCH` the line is ignored and the normal password prompt runs
   (`sufficient` = `success=done`, every other result `ignore`, per Linux-PAM `pam_handlers.c`).

### Presence wait (swaylock, console login)

A lock screen or a `login:` prompt is where Windows keeps looking for the face until the UI
stops it (F55, F57: WinBio runs face as a presence monitor, no service timeout). For the
services in `wait_services` the module asks for `AUTH <user> <timeout>` and the daemon runs
`Authenticator::verify_within`: sessions back to back with the cameras kept open, while a
session ends in `NOFACE` (nobody there yet, face turned away, too dark) and the deadline has
not passed; the session running at the deadline finishes (so the reply can come up to one
session, ≈ 2–4 s, after `timeout`; the module waits `timeout` + 10 s for it). `NOMATCH` and
`NOTLIVE` end the wait as they end a single session: a stranger gets the same one session plus
re-attempt as with `sudo`, not unlimited tries. The daemon caps the wait at 120 s
(`daemon::MAX_WAIT`).

**How this maps to Windows' lock-screen timing (F56).** Windows' credential provider asks for a
confirmation (key press, "swipe up") after 120 s of auto-logon readiness, and gives the face 10 s
(`SwitchTimeoutInSeconds`) when the user switches to it. swaylock only runs PAM when the user
submits (Enter), so a key press is always required and the wait starts from it: the 120 s
confirmation is implicit, and the 10 s switch timeout is the default `timeout=`.

**What the user sees.**

- **swaylock 1.7 (swaylock-effects 1.7.0)** shows nothing: its conversation function (`pam.c`,
  `handle_conversation`) only answers prompts and drops `PAM_ERROR_MSG`/`PAM_TEXT_INFO`, and it
  ignores key presses while validating (`password.c`, `AUTH_STATE_VALIDATING`). So: press Enter
  (with an empty password unless swaylock runs with `--ignore-empty-password`) → the indicator
  shows "verifying" for up to ≈ 10 s while the daemon looks for the face → unlocked on `MATCH`;
  otherwise `pam_unix` checks the (empty) password, the indicator shows "wrong", and the user
  types the password. Typing the password and Enter also works: the face is checked first (up to
  the full `timeout` when nobody is seen), then the password, so a user who types the password
  with the camera covered waits `timeout` before it is checked. Set `timeout=` to taste in
  `/etc/pam.d/system-local-login`. swaylock calls `pam_setcred(PAM_REFRESH_CRED)` only, so
  enroll-on-failure never commits from the lock screen (below).
- **polkit-gnome** shows `PAM_TEXT_INFO` in bold in its dialog (`polkitgnomeauthenticator.c`,
  `session_show_info`) and starts the PAM helper as soon as the dialog opens
  (`polkit_agent_session_initiate` in `do_initiate`), so "Looking for your face..." appears
  with the dialog, and on `MATCH` the dialog closes without a password. The helper does not call
  `pam_setcred`: no enroll-on-failure from polkit.
- **sudo, su, login** print the messages on the terminal, as `sudo` does today.

## Who may ask (access control, threat model)

The socket is `srw-rw-rw-` so that swaylock's PAM child, which runs as the user, can connect.
Every request is checked against the peer's uid (`SO_PEERCRED`):

| Peer | `AUTH <own user>` | `AUTH <other user>` | `ENROLL_ON_FAILURE` |
|---|---|---|---|
| root (sudo, polkit's helper, login, su) | yes | yes | yes |
| any other uid | yes (`getpwnam_r(user).pw_uid == uid`) | `ERROR … a user may only ask about itself` | `ERROR … only root may add prints` |

One request runs at a time: a request that arrives while another is running is answered `BUSY`
at once (the module: `PAM_IGNORE`, "Face login not available: another program is using the
camera"). A user process therefore can

- learn whether its own user is in front of the camera (`MATCH`/`NOMATCH`/`NOFACE`), and
- occupy the cameras: one presence wait of up to 120 s per request, or back-to-back requests
  (nothing stops a loop), which makes everyone else's face login fall back to the password and
  keeps the IR emitter on;

and cannot obtain a credential: the daemon only answers a question, and it is the PAM stack
run by the application (sudo as root, polkit's setuid helper, swaylock's own child for its own
user) that grants access. Measured with the daemon on a scratch socket as uid 1000: `AUTH root`
→ `ERROR AUTH root from uid 1000: a user may only ask about itself`, `ENROLL_ON_FAILURE cole …`
→ `ERROR … only root may add prints`, two `AUTH cole` 0.5 s apart → `MATCH 0.840` after 1.9 s
and `BUSY` at once.

## Measured

| Case | Result | Time |
|------|--------|------|
| Owner at the laptop, `sudo` | `PAM_SUCCESS` (similarity 0.82–0.89, liveness 0.84–0.96); "Making sure it's you..." is printed when the face is recognised, before the votes (added 2026-10-16, checked with the stub daemon only) | 2.1 s |
| Owner at the laptop, scratch daemon, `AUTH cole` / `AUTH cole 6` | `MATCH 0.84–0.86` | 1.9 s / 2.0 s |
| Stranger's template, owner at the camera (`NOMATCH` with the re-attempt) | `PAM_AUTH_ERR` → password | 4.2 s |
| Daemon not running | `PAM_IGNORE` → password | 0 s |
| User not enrolled | `PAM_IGNORE` → password | 0 s |
| Lid closed, built-in IR camera (`UNAVAILABLE lid_closed`) | `PAM_IGNORE` → password, message "Face login not available: the lid is closed" (libpam test with a stub daemon) | 0 s |
| Another request running (`BUSY`) | `PAM_IGNORE` → password, message (libpam test with a stub daemon) | 0 s |
| `SSH_CONNECTION` / `SSH_TTY` / `PAM_RHOST` set | `PAM_IGNORE` → password | 0 s |
| Nobody at the camera, `sudo` | `PAM_AUTH_ERR` → password | up to ~2–3 s |
| Nobody at the camera, lock screen (`AUTH cole 6`, frames rotated 180° so that no face is found) | three `NOFACE` sessions back to back, then `NOFACE no_face` → `PAM_AUTH_ERR` → password | 7.2 s (6 s + the session running at the deadline) |
| Nobody at the camera, one session (same setup, `AUTH cole`) | `NOFACE no_face` | 2.8 s |

## Install / remove

```sh
cargo build --release && sudo contrib/install.sh     # enrolls $SUDO_USER, adds the PAM lines, starts the service
sudo contrib/install.sh --no-lock --no-polkit --no-login --no-su   # sudo only (any subset)
sudo contrib/uninstall.sh [--purge]                  # removes everything (--purge also deletes the face store)
```

The installer backs up each edited file to `*.linux_hello.bak` and is idempotent: a file whose
auth stack already reaches the line through an `include` (swaylock → login →
system-local-login, su-l → su) is left alone, so no stack checks the face twice. polkit has no
`/etc/pam.d/polkit-1` on Gentoo (its file is `/usr/lib/pam.d/polkit-1`, Linux-PAM's vendor
directory, which `/etc/pam.d` overrides): the installer creates one from it with a marker
comment on the first line, and the uninstaller deletes a file with that marker. Keep a root
shell open the first time.

`scripts/pam_harness.py <module.so> <socket> '<reply>|<reply>' [service [module args...]]`
runs the module through real libpam under any service name with a stub daemon, without root
or a camera: `swaylock` and `login` send `AUTH cole 10 progress` (`AUTH cole 5 progress` with
`timeout=5`), `polkit-1`, `su` and `sudo` send `AUTH cole progress`, `BUSY` gives the message
above and falls through, a reply `NOMATCH 0.300\nNOTICE …` prints the notice after "Couldn't
recognize you. …", and `NOTICE recognized\nMATCH 0.85` prints "Making sure it's you..." before
the success. The same
was checked on copies of this machine's `/etc/pam.d` after `install.sh` edited them: through
the real include chains each service reaches the module once and sees its own name.

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

## Notices

Windows reminds the user to improve recognition (F56, `IMPROVERECOGNITIONNOTIFICATION…`). The
daemon does two such things, each logged at `warn` and sent as `NOTICE <text>` lines after the
verdict, which the module prints as `PAM_TEXT_INFO` (swaylock drops them; sudo, su, login and
polkit-gnome show them):

- every 5th `NOMATCH` in a row for a user (`daemon::FAILURES_PER_NOTICE`; the face was seen but
  not recognised, so the password was typed instead each time; `NOFACE` and any other verdict
  reset the count): "Face login has fallen back to the password 5 times in a row: run
  `linux_hello enroll --add`". Measured with a stranger's template and the owner at the camera:
  the 5th `NOMATCH` reply carried the line.
- once, in the next `AUTH` reply for the user, after enroll-on-failure added a print: "A face
  print was added after a password login (enroll-on-failure); run `linux_hello enroll <user>` if
  that was not you". This reaches users of applications that never call `pam_setcred`, where the
  immediate "Face print added" line (below) is never shown. Unit-tested (`daemon::tests`); the
  live path needs root.

No desktop notifications: the daemon runs as root with no session bus.

## Enroll-on-failure (opt-in)

When your face is not recognised (`NOMATCH`) and you then type your password, the daemon can add
that face to your template, so the next sudo recognises you in the same conditions (glasses,
lighting). Details and limits: [capture-and-liveness.md](capture-and-liveness.md#9-enroll-on-failure-f31-opt-in).

Enable it in the daemon only:

```sh
echo 'LINUX_HELLO_ARGS="--enroll-on-failure"' > /etc/conf.d/linux_hello

To keep face login working next to windows and incandescent lamps, `LINUX_HELLO_ARGS="--max-ambient 50"` raises the ambient-IR limit above Windows' 25 (docs/capture-and-liveness.md §3, weaker lighting check).
rc-service linux_hello restart
```

Other daemon options go in the same variable, e.g. `LINUX_HELLO_ARGS="--enroll-on-failure --rotate 180"`
for a camera mounted upside down (`--rotate 0|90|180|270`, degrees counter-clockwise, applied to
the IR and RGB frames before detection; the same option works for `enroll`, `auth` and `watch`),
or `--camera vid:pid` for an IR camera other than the known ones.

The PAM configuration stays as installed; no extra line is needed. The commit is made from
`pam_sm_setcred` of the same `auth` line. sudo calls `pam_setcred` (with
`PAM_REINITIALIZE_CRED`, sudo 1.9.17 `plugins/sudoers/auth/pam.c`) only after the auth stack
succeeded, and only in the PAM transaction whose face check failed holds the token. A session
hook (`pam_sm_open_session`) is not used: it would need a `session` line, and an `auth optional`
line after `system-auth` is not safe because it also runs after a wrong password (a substack
failure does not stop the parent stack). `Defaults !pam_setcred` in sudoers turns the commit off.
swaylock (`PAM_REFRESH_CRED` only) and polkit's helper (no `pam_setcred`) never commit; `login`
(shadow `src/login.c`) and `su` (util-linux `login-utils/su-common.c`, shadow likewise) call
`pam_setcred(PAM_ESTABLISH_CRED)` after a successful auth and do.

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
conversation function, not seen.

**Risk:** anyone who knows your password can add their face this way if it is already similar to
yours (similarity ≥ 0.40), and then use sudo with their face. The daemon logs every
`enroll-on-failure` result (an addition at `warn`), the PAM module prints "Face print added
after password login" right after the password where the application calls `pam_setcred`, and
the next face check for that user carries the notice above; `linux_hello enroll <user>` resets
the template.

## Caveats

- Gentoo generates `/etc/pam.d/sudo`, `su`, `system-local-login` and friends (pam eclass). An
  upgrade may offer a new version through `etc-update`/`dispatch-conf`; keep the face line when
  merging.
- sudo caches successful authentication (default 15 min per terminal), so the camera is only used
  when sudo would otherwise ask for a password.
- Anyone (or any program, including an AI agent running as you) that can run `sudo` while your face
  is in front of the camera gets root. That is the point of the feature, but be aware of it. The
  same holds for polkit and su.
- With the face line in `system-local-login`, every local login that includes it gets the
  presence wait: `login` on a console, swaylock, and any other locker or display manager whose
  PAM file includes `login` or `system-local-login`; a locker with its own service name gets one
  session unless it is added to `wait_services`.
- Liveness is not yet verified against a real printed photo or phone screen (see README).
- Face stores from before multi-print templates are refused; re-run `sudo contrib/install.sh` (or
  `linux_hello enroll <user>` as root) after upgrading.
- The RGB camera sometimes underexposes 2–3 frames every ~5 s; `OpenCameras` skips frames darker
  than half the running average. The skip logic is in place but the dips did not occur in the
  verification run, so it is not yet confirmed on live frames.
- A request fails with `ERROR Device or resource busy` when another program holds a camera
  (seen twice during testing while something else used `/dev/video0`); PAM falls through to the
  password.
