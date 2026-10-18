# hello-greeter: the login screen and the lock screen

`greeter/` is a greetd greeter that is also a session locker, with the Windows 11 sign-in
screen's layout: the lock screen (clock, date, background) and, after a key press, the sign-in
screen (user tile, the Hello face icon with its status, password box, "Sign-in options"). The
same binary does both jobs:

| Command | Runs as | Surface | Who runs the PAM stack |
|---|---|---|---|
| `hello-greeter` (under greetd) | `greetd` user, inside `niri -c /etc/greetd/niri-greeter.kdl` | a layer-shell overlay per monitor (gtk4-layer-shell) | greetd's session worker, PAM service `greetd`; the conversation reaches the greeter as greetd IPC `auth_message`s |
| `hello-greeter --lock` | the user, inside the session | `ext-session-lock-v1` (gtk4-layer-shell's session-lock API, 1.1+) | the greeter itself, PAM service `hello-lock`, like swaylock |
| `hello-greeter --window [--lock]` | the user | a plain 1600×1000 window (development) | greetd mode: `GREETD_SOCK` (the fake greetd); lock mode: `--pam-dir` for a scratch stack |

Status (2026-10-16): built and verified in a nested niri with the fake greetd and with the real
`pam_linux_hello.so` through `pam_start_confdir` (screenshots below); the install script is
written but has not run on this machine yet (greetd is not installed; needs root). The strings,
the eye animation and the layout follow [windows-hello-ui.md](windows-hello-ui.md) (LogonUI's
decompiled XAML and string tables); one place each (`greeter/src/ui/screens.rs`,
`greeter/src/ui/eye.rs`, `greeter/src/state.rs`).

## Toolkit

GTK 4 + libadwaita (installed, 4.20 / 1.7), `gtk4-layer-shell` 1.1.1 for both the greeter
surface and the session lock: since 1.1 the library ships `gtk4-session-lock.h`
(`GtkSessionLockInstance`), so no extra C library is needed; the Rust crates `gtk4-layer-shell`
0.8 and `gtk4-session-lock` 0.4 (feature `v1_1`) bind it. The alternative, speaking
`ext-session-lock-v1` with `wayland-client` next to GTK, would need the same libwayland
interposition trick the library already does. niri supports both protocols.

## How an attempt works, and why the PAM module grew `password_prompt`

The greeter never runs the face check itself: it starts a PAM transaction (through greetd, or
`pam_start("hello-lock")`) and the face module speaks first (docs/pam.md). One attempt gives
the event stream `Info("Looking for your face...")` → `Success` (a match: start the session /
unlock) or `Info(<reason>)` → `Prompt("Password")`. Both backends produce the same
[`AuthEvent`](../greeter/src/auth/mod.rs)s on a worker thread.

A lock screen must look for the face again and again (Windows' presence monitor, F55/F57).
A PAM stack cannot be cancelled: every stack here ends in `pam_unix` + `pam_faillock authfail`
(`/etc/pam.d/system-auth`), and a cancelled `pam_unix` prompt (greetd `cancel_session`,
`PAM_CONV_ERR`) is a failed login for faillock — three in 15 minutes lock the password out for
10 minutes. So the module got a `password_prompt` argument for the greeter's own lines:

```
auth  [success=done conv_err=die default=ignore]  pam_linux_hello.so password_prompt wait_services=greetd timeout=10
auth  include  system-login
```

After anything but a match the module asks for the password itself (`PAM_PROMPT_ECHO_OFF`
"Password") and leaves the answer in `PAM_AUTHTOK` for `pam_unix try_first_pass`, which then
never prompts. An empty answer returns `PAM_CONV_ERR`, and the line's `conv_err=die` ends the
stack before the password modules: no tally, no prompt left behind. The greeter only ever
answers that prompt — with the typed password, or with nothing to start the next attempt — and
never answers any other prompt with nothing (`state::Prompt::Other`). `greeter/tests/pam.rs`
runs this through real libpam (`pam_start_confdir`) with the real module, a stub daemon,
`pam_exec` in place of `pam_unix` and `pam_echo` in place of `pam_faillock`: a match succeeds
without a prompt, the password reaches `pam_exec` without a second prompt, a wrong password
reaches the tally line, an empty answer does not.

The state machine (`greeter/src/state.rs`, unit-tested) and what the screens show
(`BioFeedbackState`, windows-hello-ui.md §1.2–1.4; resource ids of FaceCredentialProvider.dll.mui):

| Event | `Face` | Windows 11 eye (§1.3 markers) / Windows 10 glyph sequences | Label |
|---|---|---|---|
| attempt started, module silent yet | `Starting` | Face 0–59 once, held / 2 entrance, hold | "Getting ready..." (105) |
| `Looking for your face...` | `Looking` | same | "Looking for you..." (104) |
| `Making sure it's you...` (the daemon's `NOTICE recognized`, pam.md) | `Verifying` | FaceLoop 60–290 looped / 3 blink, then the look-around loop 8…18 with the 320–410 ms holds | "Making sure it's you..." (123) |
| `Blink once` (the daemon's `NOTICE blink` under `--blink-challenge`, liveness-physics.md §7) | `Blink` | as `Verifying` | "Blink once" (no Windows state) |
| `Success` | `Welcome` | FaceSuccess 291–408 once (wink, check mark, Windows logo) / 4 eye→smile, 7 wiggle | "Hello <name>" (101); greetd starts the session at once |
| a reason, then the prompt | `Failed(reason)` | FaceFail 409–477 once (the smile flattens) / 1 pupil hollows, 1↔19 ring blink | the module's line: "Couldn't recognize you. Please sign in with your password." (125) for `NOMATCH`, its hints otherwise |
| "Face login not available: …" | `Failed(reason)` | hidden (state 7) | "Camera is in use…" (157) / "Camera not available…" (124) |
| a prompt without any face line | `Failed(None)` | hidden | — |
| wrong password | — | — | "The password is incorrect. Try again." (INFERRED) under the box; the face runs again |
| lock screen, recognised, `dismiss_lock_screen = false` | `Welcome` | — | "Hello, <name>! Dismiss the lock screen to sign in." (113), 34 px at the top |

- **Lock screen** (clock): attempts follow each other while the looking window is open
  (`lock_face_seconds`, default 120 s after the screen appeared or the last key press, F56);
  each is the module's presence wait (`timeout=10`) plus the session at the deadline. A match
  signs in / unlocks at once (`dismiss_lock_screen = true`, Windows' "automatically dismiss the
  lock screen if we recognize your face"); with it off the greeting above waits for a key. After
  the window the last attempt's prompt stays pending and the cameras are left alone.
- **Key or click** → sign-in screen (the background blurs and dims, the stack slides up). The
  running attempt continues; a pending prompt is ended and a fresh attempt starts.
- **Sign-in screen**: the password box is focused and the prompt stays pending (no camera use)
  until the password is typed or "Sign-in options → face" starts another attempt; the selected
  provider button carries the 2 px border. A password typed while the module is still looking
  is queued and answers the prompt when it comes (at most the wait, 10 s, less when a face is
  seen). Typing anywhere goes to the password box; "Caps Lock is on" appears under it.
- The greeter exits after greetd accepted `start_session`; in lock mode it unlocks and exits on
  `Success`. The power button (bottom right) runs `loginctl suspend|poweroff|reboot`.

Layout (§3, XAML device-independent pixels at scale 1): lock screen = image + 20 % black, the
clock block's top at 15 % of the height, centred (Segoe UI Variable SemiBold, tabular: 136/28 px
from 1920 logical px wide, 98/20 below), greeting 34 px SemiBold 40 px from the top. Sign-in =
the same image blurred and darker; a 404×530 box centred 35 px above the middle with the content
pinned 72 px from its top: 192 px tile (the picture, or Windows' default tile: Segoe Fluent Icons
U+EA8C at 100 px on a 6 % white disc), name 28 px SemiBold (+12), eye 64 px (+8) over the 24 px
SemiBold label (+6), the 296 px field (+20; 6 % black, 4 px radius, underline, accent when
focused; the box 14 px, submit U+E72A 12 px in 28×24), "Sign-in options" 14 px, the 44×44
provider buttons (margin 10,0, 2 px #808080 when selected, logos 24 px) 32 px below; the power
button bottom right (U+E7E8 16 px, 40 px high). Without the assets the glyphs are themed icons.

## Assets

The real Windows sign-in assets are used when the directory exists (personal, local use, the
same rule as the Microsoft models): `--assets`, else `assets` in the config, else
`$LINUX_HELLO_ASSETS`, else `/usr/local/share/linux_hello/assets` (what
`contrib/install-greeter.sh` copies from `models/windows_hello/assets/`, git-ignored, extracted
by `scripts/extract_windows_hello_assets.py` — inventory in
[windows-hello-ui.md](windows-hello-ui.md)), else the repository's for `cargo run`.

| In the directory | Source on Windows | Used for | Without it |
|---|---|---|---|
| `fonts/*.ttf` | `Windows/Fonts/SegUIVar.ttf` (Segoe UI Variable), `segoeui*.ttf`, `SegoeIcons.ttf`, `segmdl2.ttf` | added to fontconfig at start (`FcConfigAppFontAddDir`, before GTK builds its font map); the CSS asks for "Segoe UI Variable" (fontconfig lists its Display/Text/Small instances as styles of that one family; the clock sets `opsz` 36, the Display instance) then "Segoe UI" | the system sans (here Noto Sans) |
| `screen/img100.jpg` (`background` in the config) | `Windows/Web/Screen/` | the lock-screen background, blurred and dimmed on the sign-in screen | a solid dark blue |
| `user-pictures/user-192.png` | `ProgramData/Microsoft/User Account Pictures/` | the user tile when the user has no `~/.face`, `~/.face.icon` or AccountsService icon | a grey disc with the initials |
| `hello/eye/solid.json` | `Windows.UI.Logon.dll`: the Windows 11 eye, the `HelloSolidAnimations` composition as data (§6.2, `tools/winhello_ui/lottie_extract.py`) | the Hello eye (`ui/eye_vector.rs`): the 64×64 composition played with Cairo at 60 fps, the state's marker segment looped or held as `UpdateMarker` does; `cargo run -p hello-greeter --example eye_solid -- solid.json out/` renders the frames |
| `hello/eye/animation.json`, `hello/eye/seqNN/FFF.png` | `FontGlyphAnimator.dll` + `NUIMDL2.ttf`: the Windows 10 glyph eye, frame-exact (§1.3) | the fallback eye without `solid.json` (`ui/eye.rs`): the transition table drives it per state |
| `hello/hello-face.png` | the provider's logo (RT_HTML 1101 SVG, rendered) | the 24 px logo in the "Sign-in options" button |
| `fonts/SegoeIcons.ttf` | Segoe Fluent Icons | submit arrow U+E72A, key U+E192, power U+E7E8, default tile U+EA8C | without `hello/eye/`: a glyph of the icon font (`hello_glyph`, hex code point, when `SegoeIcons.ttf`/`segmdl2.ttf` is in `fonts/`), else the greeter's own Cairo face shaped like `hello-face.svg` (eyes glance while looking, smile when recognised) |

The log says what is in use at start (`assets: …; fonts: …; hello: …; background: …`), and the
`--window` dev window shows the same line in its corner. Verified with the extracted
directory (fontconfig lists NUIMDL2Assets, Segoe Fluent Icons, Segoe MDL2 Assets, Segoe UI,
Segoe UI Symbol, Segoe UI Variable; screenshots below) and with a scratch one (fonts renamed
to the Segoe families, a generated image, a picture) for the matching itself.

## Configuration: `/etc/greetd/hello-greeter.toml`

```toml
user = "cole"                                   # the account shown (default: the first login account in /etc/passwd)
command = "dbus-run-session /home/cole/.local/bin/start-niri.sh"   # the session (default: that script if it exists, else the first wayland-sessions/*.desktop)
# assets = "/usr/local/share/linux_hello/assets"
# background = "screen/img100.jpg"              # relative to the assets, or absolute
# hello_glyph = ""                              # hex code point in the icon font, else frames/own icon
# dismiss_lock_screen = true
# lock_face_seconds = 120
```

`--config PATH`, `--user`, `--assets` override it; `-v debug` for more log.

## Install (root, opt-in, reversible)

```sh
emerge gui-libs/greetd            # not done by the script
cargo build --release --workspace
sudo contrib/install.sh           # the daemon, the module, the face store (if not done yet)
sudo contrib/install-greeter.sh
rc-service display-manager start  # or reboot; Ctrl+Alt+F1 gets back to the running session
```

What the script does, each with a `*.linux_hello.bak` backup or a marker comment for the
uninstaller: installs `/usr/local/bin/hello-greeter` and the module (with `password_prompt`);
copies the assets; writes `/etc/greetd/config.toml` (greetd on **VT 7** with
`niri -c /etc/greetd/niri-greeter.kdl` as the `greetd` user, PAM service `greetd-greeter`),
`/etc/greetd/niri-greeter.kdl` (niri runs `hello-greeter` and quits when it exits, so greetd
starts the user's session; `Mod+Shift+E` restarts the greeter), `/etc/greetd/hello-greeter.toml`
(kept if present); `/etc/pam.d/greetd` (the face line above, then `system-login`,
`pam_gnome_keyring`), `/etc/pam.d/greetd-greeter` (permit + `system-login` session, so that
`pam_elogind` gives niri its seat; without it `/etc/pam.d/other` denies the greeter's session),
`/etc/pam.d/hello-lock`; `/etc/conf.d/display-manager` → `DISPLAYMANAGER="greetd"`,
`CHECKVT=7`; `rc-update add display-manager default`. It does not start anything.

VT 7 rather than tty1: greetd switches to its VT when it starts, so the boot ends on the
greeter either way, and the agetty consoles on tty1–6 stay untouched for recovery (the task
asked for tty1 with the `c1` inittab line commented out; this is the less invasive way to the
same screen).

Lock screen: bind `hello-greeter --lock` in niri and swayidle, e.g.

```kdl
binds { Super+Alt+L { spawn "hello-greeter" "--lock"; } }
spawn-at-startup "swayidle" "-w" "timeout" "300" "hello-greeter --lock" "before-sleep" "hello-greeter --lock"
```

The log is `/var/log/hello-greeter.log` (greetd mode) or stderr.

### Recovery

If the greeter does not come up on VT 7: `Ctrl+Alt+F2`, log in on tty2, then
`rc-service display-manager stop` (or `sudo contrib/uninstall-greeter.sh`, which also restores
the PAM and greetd files and removes display-manager from the default runlevel). Start niri by
hand as before. The uninstaller refuses to run while display-manager is up. In lock mode a
locker that crashes unlocks the session (the compositor drops the lock with the client; niri
did so in the nested test), as with swaylock; a locker that is stuck is killed from another VT
(`pkill -f 'hello-greeter --lock'`).

## Development without root

```sh
cargo run -p hello-greeter --example fake_greetd -- /tmp/fake-greetd.sock nomatch --delay 4000   # match|noface|nomatch|notlive|busy
GREETD_SOCK=/tmp/fake-greetd.sock cargo run -p hello-greeter -- --window --config /dev/null --user $USER [--assets DIR]
cargo run -p hello-greeter -- --lock --window --pam-dir /tmp/pamdir --config /dev/null         # a scratch hello-lock stack, see greeter/tests/pam.rs
cargo test -p hello-greeter      # state machine, greetd client vs the fake, the real module through pam_start_confdir
```

Run the window inside a nested niri (`niri -c /tmp/niri-nested.kdl` with `window-rule {
open-fullscreen true }`, then `WAYLAND_DISPLAY=wayland-2`), never fullscreen in the real
session: it would take the keyboard. `grim -o winit` screenshots the nested output. Without
`--window` the greeter makes a layer-shell overlay (greetd mode) or locks the compositor it
runs in (`--lock`), which also works in the nested niri.

Screenshots (nested niri, fake greetd, 1600×1000 at scale 2, 2026-10-16), with the real assets
(Bloom `img100.jpg`, Segoe UI Variable, Segoe Fluent Icons, the default tile, the glyph eye)
and without them (`*-fallback.png`):

| State | File |
|---|---|
| lock screen | `docs/img/greeter-lock.png`, `greeter-lock-fallback.png` |
| lock screen, recognised, key press required | `docs/img/greeter-lock-greeting.png` |
| sign-in, looking (eye entrance held) | `docs/img/greeter-looking.png`, `greeter-looking-fallback.png` |
| sign-in, making sure it's you (look-around loop) | `docs/img/greeter-verifying.png` |
| sign-in, couldn't recognize you (ring-pupil blink) | `docs/img/greeter-failed.png` |
| the same two states with the Windows 11 eye (`solid.json`; 942×1022 nested output) | `docs/img/greeter-verifying-win11.png`, `greeter-failed-win11.png`; every segment: `docs/img/eye-solid-contact-sheet.png` |
| wrong password | `docs/img/greeter-wrong-password.png` |
| sign-in options row | `docs/img/greeter-sign-in-options.png` |

## What matches Windows and what does not

| Windows (windows-hello-ui.md) | hello-greeter |
|---|---|
| Lock screen: image + 20 % mask, clock block at 15 %, adaptive sizes; any key/click → sign-in | same (two of the five size steps: ≥ 1920 and below) |
| Sign-in: tile, name, eye + label, field with underline and submit arrow, "Sign-in options", provider buttons, system bar | same elements and numbers (§3.2); system bar = power only (no accessibility, IME, language, network); no user list / "Other user" |
| Strings per state (105, 104, 123, 101, 125, 157, 124, 113) | the same texts, "PIN" → "password"; "Welcome" page: not shown under greetd (the session starts at once) |
| The Hello eye | **Done**: Windows 11's `HelloSolidAnimations` composition (64×64, 2270 frames at 60 fps, the compiled LottieGen object graph extracted to `solid.json`, windows-hello-ui.md §6.2) played frame-exact with Cairo; segments per `UpdateMarker` (FaceLoop/TooFar/TooClose/Spinner loop, the rest play once and hold); the frame-59 face overlays `hello-face.svg` within 1.1 units of 64 (same proportions, IoU 0.88). Without `solid.json`: Windows 10's font-glyph animation (`FontGlyphAnimator.dll`), also frame-exact |
| Face runs on the lock screen; "dismiss the lock screen" setting; 120 s confirmation window | `dismiss_lock_screen`, `lock_face_seconds` = 120, attempts of `timeout=10` each |
| TooFar/TooClose eye states (5/6), "Adjusting brightness" (133), brightness override | not produced (the module's hints are text only) |
| Acrylic backdrop behind the sign-in screen | GSK blur + a darker mask (the tint is UNKNOWN on Windows too) |
| Password tile stays selected after a wrong password | the face runs first again (the PAM stack's order); the typed password is queued |
| Lock → sign-in transition (0.5 s reveal, image scale 1→1.02) | slide-up + 250 ms blur; no scale |
| Caps Lock warning | same |
| Sounds, toasts, enrolment, PLAP, Cloud PC | none |

### Replacing swaylock

`hello-greeter --lock --daemonize` returns once the session is locked and leaves the locker
running, like `swaylock -f`, so it can sit in `swayidle`'s `before-sleep` hook: the laptop never
suspends unlocked. In niri:

```kdl
Mod+Alt+L allow-inhibiting=false { spawn "hello-greeter" "--lock"; }
spawn-at-startup "swayidle" "-w" "timeout" "300" "hello-greeter --lock --daemonize" "timeout" "330" "niri msg action power-off-monitors" "resume" "niri msg action power-on-monitors" "before-sleep" "hello-greeter --lock --daemonize"
```

Unlike swaylock, it starts looking for the face on the first key press or mouse move, with no
Enter needed, and shows the Windows Hello status lines. If another locker already holds the
session, `--daemonize` exits with an error instead of hanging.
