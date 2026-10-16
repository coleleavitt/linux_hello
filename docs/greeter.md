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
written but has not run on this machine yet (greetd is not installed; needs root). The layout
and strings are placeholders until [windows-hello-ui.md](windows-hello-ui.md) (the sibling's
spec of LogonUI) lands; the code keeps them in one place each (`greeter/src/ui/screens.rs`,
`greeter/src/state.rs`).

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

The state machine (`greeter/src/state.rs`, unit-tested):

- **Lock screen** (clock): attempts follow each other while the looking window is open
  (`lock_face_seconds`, default 120 s after the screen appeared or the last key press, F56);
  each is the module's presence wait (`timeout=10`) plus the session at the deadline. A match
  signs in / unlocks at once (`dismiss_lock_screen = true`, Windows' "automatically dismiss the
  lock screen if we recognize your face"); with it off the screen says "Welcome" and waits for a
  key. After the window the last attempt's prompt stays pending and the cameras are left alone.
- **Key or click** → sign-in screen (the background blurs and dims, the stack slides up). The
  running attempt continues; a pending prompt is ended and a fresh attempt starts.
- **Sign-in screen**: the reason of a failed check is the status text next to the Hello icon,
  the password box is focused and the prompt stays pending (no camera use) until the password
  is typed or "Sign-in options → face" starts another attempt. A password typed while the
  module is still looking is queued and answers the prompt when it comes (at most the wait,
  10 s, less when a face is seen). A wrong password shows "The password is incorrect. Try
  again." and the next attempt starts (the face runs first again: on Windows the password tile
  would stay selected; here the stack decides the order).
- Typing anywhere goes to the password box. The greeter exits after greetd accepted
  `start_session`; in lock mode it unlocks and exits on `Success`.

## Assets

The real Windows sign-in assets are used when the directory exists (personal, local use, the
same rule as the Microsoft models): `--assets`, else `assets` in the config, else
`$LINUX_HELLO_ASSETS`, else `/usr/local/share/linux_hello/assets` (what
`contrib/install-greeter.sh` copies from `models/windows_hello/assets/`, git-ignored, extracted
by `scripts/extract_windows_hello_assets.py` — inventory in
[windows-hello-ui.md](windows-hello-ui.md)), else the repository's for `cargo run`.

| In the directory | Source on Windows | Used for | Without it |
|---|---|---|---|
| `fonts/*.ttf` | `Windows/Fonts/SegUIVar.ttf` (Segoe UI Variable), `segoeui*.ttf`, `SegoeIcons.ttf`, `segmdl2.ttf` | added to fontconfig at start (`FcConfigAppFontAddDir`, before GTK builds its font map); the CSS asks for "Segoe UI Variable Display" (clock), "Segoe UI Variable Text", "Segoe UI" | the system sans (here Noto Sans) |
| `screen/img100.jpg` (`background` in the config) | `Windows/Web/Screen/` | the lock-screen background, blurred and dimmed on the sign-in screen | a solid dark blue |
| `user-pictures/user-192.png` | `ProgramData/Microsoft/User Account Pictures/` | the user tile when the user has no `~/.face`, `~/.face.icon` or AccountsService icon | a grey disc with the initials |
| `hello/frames/NNN.png`, else `hello/hello.png` | LogonUI / the credential provider (to be extracted) | the Hello face icon; frames play at 30 fps while looking | a glyph of the icon font (`hello_glyph = "EB68"`-style code point in the config, when `SegoeIcons.ttf`/`segmdl2.ttf` is in `fonts/`), else the greeter's own Cairo face (eyes glance while looking, smiles when recognised) |

The log says what is in use at start (`assets: …; fonts: …; hello: …; background: …`), and the
`--window` dev window shows the same line in its corner. Verified with a scratch directory
holding two fonts renamed to the Segoe families, a generated `img100.jpg`, 12 frames and a
user picture: all four were picked up (screenshot below).

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

Screenshots (nested niri, fake greetd, 1600×1000 at scale 2, 2026-10-16):

| | |
|---|---|
| lock screen | `docs/img/greeter-lock.png` |
| sign-in, looking | `docs/img/greeter-looking.png` |
| sign-in, face not recognised, password | `docs/img/greeter-failed.png` |
| wrong password | `docs/img/greeter-wrong-password.png` |
| with a scratch assets directory (fonts, background, frames, picture) | `docs/img/greeter-assets.png` |

## What matches Windows and what does not

| Windows | hello-greeter |
|---|---|
| Lock screen: background, clock, date; any key/click → sign-in | same (placeholder layout: bottom-left clock) |
| Sign-in: avatar circle, name, Hello icon + status, password box, "Sign-in options" | same elements; strings are placeholders ("Looking for you...", "Welcome", the module's reasons) |
| Face runs on the lock screen; "dismiss the lock screen" setting | same (`dismiss_lock_screen`) |
| 120 s then a confirmation key press (F56); 10 s switch timeout | `lock_face_seconds` = 120, attempts of `timeout=10` each |
| Acrylic blur behind the sign-in screen | GSK blur + dim of the background image |
| Segoe UI Variable, Segoe Fluent Icons, Spotlight/`img100.jpg`, the Hello animation | the real files when present in the assets directory; fallbacks otherwise |
| Password tile stays selected after a wrong password | the face runs first again (the PAM stack's order); the typed password is queued |
| Other users / "Other user" tile, switch user | one account (`user` in the config) |
| Accessibility, ease-of-access, power buttons at the bottom right | missing |
