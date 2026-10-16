#!/bin/sh
# Remove linux_hello's system integration. Keeps the face store unless --purge is given.
set -eu
[ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 1; }

for svc in sudo sudo-i swaylock system-local-login su polkit-1; do
    f=/etc/pam.d/$svc
    [ -f "$f" ] && grep -q pam_linux_hello "$f" || continue
    if head -n1 "$f" | grep -q 'created by linux_hello'; then
        rm "$f"
        echo "removed $f (created by install.sh)"
        continue
    fi
    grep -v pam_linux_hello "$f" > "$f.new" && chmod 644 "$f.new" && mv "$f.new" "$f"
    echo "removed face line from $f"
done

rc-service linux_hello stop 2>/dev/null || true
rc-update del linux_hello default 2>/dev/null || true
rm -f /etc/init.d/linux_hello /lib64/security/pam_linux_hello.so /usr/local/bin/linux_hello /run/linux_hello.sock
rm -rf /usr/local/share/linux_hello
if [ "${1:-}" = "--purge" ]; then
    rm -rf /var/lib/linux_hello
    echo "removed face store"
fi
echo "done"
