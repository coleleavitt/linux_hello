#!/bin/sh
# Install linux_hello system-wide and enable face login for sudo and sudo -i.
# Run from the repository after `cargo build --release`:  sudo contrib/install.sh
# Undo with:  sudo contrib/uninstall.sh
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
user=${SUDO_USER:?run this with sudo so the calling user can be enrolled}
share=/usr/local/share/linux_hello
line='auth		sufficient	pam_linux_hello.so'

[ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 1; }
for f in target/release/linux_hello target/release/libpam_linux_hello.so \
         models/det_10g.onnx models/w600k_r50.onnx \
         models/antispoof/facenox_antispoof.onnx models/antispoof/MiniFASNetV2.onnx; do
    [ -f "$repo/$f" ] || { echo "missing $f (build first / run scripts/download_models.sh)" >&2; exit 1; }
done

echo "== files"
install -m755 "$repo/target/release/linux_hello" /usr/local/bin/linux_hello
install -m755 "$repo/target/release/libpam_linux_hello.so" /lib64/security/pam_linux_hello.so
install -d -m755 "$share/models/antispoof"
install -m644 "$repo/models/det_10g.onnx" "$repo/models/w600k_r50.onnx" "$share/models/"
install -m644 "$repo/models/antispoof/facenox_antispoof.onnx" "$repo/models/antispoof/MiniFASNetV2.onnx" "$share/models/antispoof/"
install -m755 "$repo/contrib/openrc/linux_hello" /etc/init.d/linux_hello

echo "== enroll $user (look at the camera)"
LINUX_HELLO_MODELS="$share/models" /usr/local/bin/linux_hello -v warn enroll "$user"

echo "== service"
rc-update add linux_hello default
rc-service linux_hello restart

echo "== pam"
for svc in sudo sudo-i; do
    f=/etc/pam.d/$svc
    [ -f "$f" ] || continue
    if grep -q pam_linux_hello "$f"; then
        echo "$f already has pam_linux_hello"
        continue
    fi
    [ -f "$f.linux_hello.bak" ] || cp -p "$f" "$f.linux_hello.bak"
    # Insert the face line before the first auth line.
    awk -v l="$line" '!done && /^auth[ \t]/ { print l; done = 1 } { print }' "$f" > "$f.new"
    chmod 644 "$f.new" && mv "$f.new" "$f"
    echo "$f:"; sed 's/^/    /' "$f"
done

echo "== done. Test in a NEW terminal:  sudo -k; sudo true"
