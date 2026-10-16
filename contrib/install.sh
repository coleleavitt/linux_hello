#!/bin/sh
# Install linux_hello system-wide and enable face login for sudo, the screen lock (swaylock),
# polkit, su and console login. Run from the repository after `cargo build --release`:
#   sudo contrib/install.sh [--no-lock] [--no-polkit] [--no-login] [--no-su]
# Undo with:  sudo contrib/uninstall.sh
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
user=${SUDO_USER:?run this with sudo so the calling user can be enrolled}
share=/usr/local/share/linux_hello
line='auth		sufficient	pam_linux_hello.so'
created='# created by linux_hello contrib/install.sh (removed by uninstall.sh)'

# PAM service files that get the face line; su-l includes su, login includes system-local-login
# and swaylock includes login (docs/pam.md). Never system-login (shared with remote logins),
# system-remote-login or sshd.
lock=/etc/pam.d/swaylock
polkit=/etc/pam.d/polkit-1
login=/etc/pam.d/system-local-login
su=/etc/pam.d/su
for arg in "$@"; do
    case $arg in
        --no-lock) lock= ;;
        --no-polkit) polkit= ;;
        --no-login) login= ;;
        --no-su) su= ;;
        *) echo "usage: $0 [--no-lock] [--no-polkit] [--no-login] [--no-su]" >&2; exit 2 ;;
    esac
done

[ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 1; }
for f in target/release/linux_hello target/release/libpam_linux_hello.so \
         models/det_10g.onnx models/w600k_r50.onnx models/calibration.json \
         models/antispoof/facenox_antispoof.onnx models/antispoof/MiniFASNetV2.onnx; do
    [ -f "$repo/$f" ] || { echo "missing $f (build first / run scripts/download_models.sh)" >&2; exit 1; }
done

echo "== files"
install -m755 "$repo/target/release/linux_hello" /usr/local/bin/linux_hello
install -m755 "$repo/target/release/libpam_linux_hello.so" /lib64/security/pam_linux_hello.so
install -d -m755 "$share/models/antispoof"
install -m644 "$repo/models/det_10g.onnx" "$repo/models/w600k_r50.onnx" "$repo/models/calibration.json" "$share/models/"
install -m644 "$repo/models/antispoof/facenox_antispoof.onnx" "$repo/models/antispoof/MiniFASNetV2.onnx" "$share/models/antispoof/"
# Converted Windows Hello models (git-ignored, docs/winhello-models/): used when present.
ms="$repo/models/windows_hello/converted"
if [ -d "$ms" ]; then
    install -d -m755 "$share/models/windows_hello/converted/convnet" "$share/models/windows_hello/converted/occlusion" \
        "$share/models/windows_hello/converted/ldmf/ort" "$share/models/windows_hello/converted/cefa"
    install -m644 "$ms"/convnet/MODEL_FILES_12[1-5].onnx "$share/models/windows_hello/converted/convnet/"
    install -m644 "$ms/occlusion/MODEL_FILES_120.onnx" "$share/models/windows_hello/converted/occlusion/"
    install -m644 "$ms/ldmf/ort/MODEL_FILES_110.ort" "$share/models/windows_hello/converted/ldmf/ort/"
    # The face recogniser and, in the JSON, Microsoft's distance → FAR table of its container header.
    install -m644 "$ms/cefa/MODEL_FILES_123.onnx" "$ms/cefa/MODEL_FILES_123.json" "$share/models/windows_hello/converted/cefa/"
fi
install -m755 "$repo/contrib/openrc/linux_hello" /etc/init.d/linux_hello

# A store from before multi-print templates (one `features` vector per user) cannot be read:
# keep it as a backup and enroll from scratch.
store=/var/lib/linux_hello/faces.json
if [ -f "$store" ] && ! grep -q '"prints"\|"format"' "$store"; then
    mv "$store" "$store.pre-multiprint"
    echo "old face store moved to $store.pre-multiprint"
fi

echo "== enroll $user (look at the camera)"
LINUX_HELLO_MODELS="$share/models" /usr/local/bin/linux_hello -v warn enroll "$user"

echo "== service"
rc-update add linux_hello default
rc-service linux_hello restart

# Whether the auth stack of PAM file $1 already runs the face line, itself or through an
# `include`/`substack` (swaylock → login → system-local-login, su-l → su).
has_face_line() {
    grep -q pam_linux_hello "$1" && return 0
    for inc in $(awk '$1 == "auth" && ($2 == "include" || $2 == "substack") { print $3 }' "$1"); do
        [ -f "/etc/pam.d/$inc" ] && has_face_line "/etc/pam.d/$inc" && return 0
    done
    return 1
}

echo "== pam"
# polkit's own file lives in /usr/lib/pam.d (Linux-PAM's vendor directory); /etc/pam.d/polkit-1
# overrides it when present.
if [ -n "$polkit" ] && [ ! -f "$polkit" ]; then
    { echo "$created"; cat /usr/lib/pam.d/polkit-1 2>/dev/null || printf 'auth\tinclude\tsystem-auth\naccount\tinclude\tsystem-auth\npassword\tinclude\tsystem-auth\nsession\tinclude\tsystem-auth\n'; } > "$polkit"
    chmod 644 "$polkit"
    echo "created $polkit from /usr/lib/pam.d/polkit-1"
fi
# The login file first: swaylock includes it, so its own file is then left alone.
for f in /etc/pam.d/sudo /etc/pam.d/sudo-i $login $lock $polkit $su; do
    [ -f "$f" ] || { echo "$f does not exist; skipped"; continue; }
    if has_face_line "$f"; then
        echo "$f already runs pam_linux_hello"
        continue
    fi
    # The face line goes right before the password stack (the first `auth include`/`substack`
    # line): after pam_rootok/pam_wheel in su, so that root needs no face and non-wheel users
    # still get refused.
    if ! grep -Eq '^auth[[:space:]]+(include|substack)' "$f"; then
        echo "$f has no 'auth include/substack' line; add the face line by hand: $line"
        continue
    fi
    [ -f "$f.linux_hello.bak" ] || head -n1 "$f" | grep -q 'created by linux_hello' || cp -p "$f" "$f.linux_hello.bak"
    awk -v l="$line" '!done && /^auth[ \t]+(include|substack)/ { print l; done = 1 } { print }' "$f" > "$f.new"
    chmod 644 "$f.new" && mv "$f.new" "$f"
    echo "$f:"; sed 's/^/    /' "$f"
done

echo "== done. Test in a NEW terminal:  sudo -k; sudo true"
echo "   Lock screen: swaylock, then press Enter (the face is looked for up to 10 s; docs/pam.md)."
