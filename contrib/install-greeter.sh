#!/bin/sh
# Install hello-greeter as the login screen (greetd on VT 7) and as the lock screen
# (`hello-greeter --lock`), opt-in and reversible. Run from the repository after
# `cargo build --release` (both crates) and `contrib/install.sh` (the daemon and the face store):
#   sudo contrib/install-greeter.sh
# Undo with:  sudo contrib/uninstall-greeter.sh
# Nothing is emerged here: greetd (gui-libs/greetd) must be installed first. docs/greeter.md.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
user=${SUDO_USER:?run this with sudo so the sign-in screen shows the calling user}
share=/usr/local/share/linux_hello
log=/var/log/hello-greeter.log
created='# created by linux_hello contrib/install-greeter.sh (removed by uninstall-greeter.sh)'
# The face line for the greeter and the locker: the module asks for the password itself after
# a failed face check and an empty answer ends the stack (password_prompt, docs/pam.md), so the
# greeter can look for the face again without pam_unix/pam_faillock seeing a failed login.
face_line() {
    printf 'auth\t[success=done conv_err=die default=ignore]\tpam_linux_hello.so password_prompt wait_services=%s timeout=10\n' "$1"
}

[ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 1; }
command -v greetd >/dev/null || { echo "greetd is not installed: emerge gui-libs/greetd, then run this again" >&2; exit 1; }
id greetd >/dev/null 2>&1 || { echo "no greetd user (acct-user/greetd comes with gui-libs/greetd)" >&2; exit 1; }
command -v niri >/dev/null || { echo "niri is not in root's PATH; it runs the greeter (docs/greeter.md)" >&2; exit 1; }
for f in target/release/hello-greeter target/release/libpam_linux_hello.so; do
    [ -f "$repo/$f" ] || { echo "missing $f (cargo build --release --workspace)" >&2; exit 1; }
done
[ -S /run/linux_hello.sock ] || echo "note: the linux_hello daemon is not running; the greeter falls back to the password (contrib/install.sh)"

backup() { [ -f "$1" ] && [ ! -f "$1.linux_hello.bak" ] && ! head -n1 "$1" | grep -q 'created by linux_hello' && cp -p "$1" "$1.linux_hello.bak" || true; }

echo "== files"
install -m755 "$repo/target/release/hello-greeter" /usr/local/bin/hello-greeter
# The module with password_prompt; the same file contrib/install.sh installs.
install -m755 "$repo/target/release/libpam_linux_hello.so" /lib64/security/pam_linux_hello.so
# The Windows sign-in assets (git-ignored, docs/windows-hello-ui.md): used when present.
if [ -d "$repo/models/windows_hello/assets" ]; then
    rm -rf "$share/assets"
    mkdir -p "$share"
    cp -r "$repo/models/windows_hello/assets" "$share/assets"
    find "$share/assets" -type d -exec chmod 755 {} + -o -type f -exec chmod 644 {} +
    echo "assets: $share/assets"
else
    echo "assets: none (models/windows_hello/assets missing; the greeter uses its own icon and the system sans)"
fi
touch "$log" && chown greetd:greetd "$log" && chmod 640 "$log"

echo "== greetd"
mkdir -p /etc/greetd
backup /etc/greetd/config.toml
cat > /etc/greetd/config.toml <<EOF
$created
[terminal]
# VT 7 (display-manager's CHECKVT): the agetty consoles on tty1-6 stay for recovery.
vt = 7

[default_session]
# niri hosts the greeter; the greeter exits after start_session and niri quits with it.
command = "niri -c /etc/greetd/niri-greeter.kdl"
user = "greetd"
service = "greetd-greeter"
EOF
cat > /etc/greetd/niri-greeter.kdl <<EOF
// $created
// niri as greetd's compositor: only the greeter runs here.
spawn-at-startup "sh" "-c" "hello-greeter >>$log 2>&1; niri msg action quit --skip-confirmation"
hotkey-overlay { skip-at-startup; }
prefer-no-csd
cursor { hide-when-typing; }
binds {
    // If the greeter dies: Ctrl+Alt+F2 (agetty) to recover, Mod+Shift+E restarts the greeter.
    Mod+Shift+E { quit skip-confirmation=true; }
}
EOF
if [ ! -f /etc/greetd/hello-greeter.toml ]; then
    home=$(getent passwd "$user" | cut -d: -f6)
    cat > /etc/greetd/hello-greeter.toml <<EOF
$created
# The account on the sign-in screen and its session (docs/greeter.md, greeter/src/config.rs).
user = "$user"
command = "dbus-run-session $home/.local/bin/start-niri.sh"
# background = "screen/img100.jpg"
# dismiss_lock_screen = true
# lock_face_seconds = 120
EOF
fi
echo "/etc/greetd/config.toml, niri-greeter.kdl, hello-greeter.toml written"

echo "== pam"
# greetd: the greeter's own stack; Gentoo's file (`auth include login`) would reach the face
# line in system-local-login with the wrong arguments.
backup /etc/pam.d/greetd
{
    echo "$created"
    face_line greetd
    printf 'auth\tinclude\t\tsystem-login\nauth\toptional\tpam_gnome_keyring.so\n'
    printf 'account\tinclude\t\tsystem-login\npassword\tinclude\t\tsystem-login\n'
    printf 'session\toptional\tpam_gnome_keyring.so auto_start\nsession\tinclude\t\tsystem-login\n'
} > /etc/pam.d/greetd
# greetd-greeter: the session greetd opens for the greeter itself (no authentication); without
# this file /etc/pam.d/other denies it. pam_elogind gives niri its seat.
backup /etc/pam.d/greetd-greeter
{
    echo "$created"
    printf 'auth\trequired\tpam_permit.so\naccount\trequired\tpam_permit.so\npassword\trequired\tpam_deny.so\nsession\tinclude\t\tsystem-login\n'
} > /etc/pam.d/greetd-greeter
# hello-lock: `hello-greeter --lock`, as the user.
{
    echo "$created"
    face_line hello-lock
    printf 'auth\tinclude\t\tsystem-login\n'
} > /etc/pam.d/hello-lock
chmod 644 /etc/pam.d/greetd /etc/pam.d/greetd-greeter /etc/pam.d/hello-lock
for f in greetd greetd-greeter hello-lock; do echo "/etc/pam.d/$f:"; sed 's/^/    /' "/etc/pam.d/$f"; done

echo "== openrc"
backup /etc/conf.d/display-manager
# gui-libs/display-manager-init starts greetd when DISPLAYMANAGER="greetd"; CHECKVT must match vt.
sed -i -e 's/^DISPLAYMANAGER=.*/DISPLAYMANAGER="greetd"/' -e 's/^CHECKVT=.*/CHECKVT=7/' /etc/conf.d/display-manager
grep -q '^DISPLAYMANAGER=' /etc/conf.d/display-manager || echo 'DISPLAYMANAGER="greetd"' >> /etc/conf.d/display-manager
grep -q '^CHECKVT=' /etc/conf.d/display-manager || echo 'CHECKVT=7' >> /etc/conf.d/display-manager
rc-update add display-manager default
echo "display-manager (greetd) added to the default runlevel; it is NOT started now."

echo "== done"
echo "  Login screen: rc-service display-manager start   (switches to VT 7; Ctrl+Alt+F1 gets back here)"
echo "  Lock screen:  hello-greeter --lock                (bind it in niri; see docs/greeter.md)"
echo "  Recovery:     Ctrl+Alt+F2 → tty2 login → rc-service display-manager stop; sudo contrib/uninstall-greeter.sh"
