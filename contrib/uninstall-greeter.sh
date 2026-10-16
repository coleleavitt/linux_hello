#!/bin/sh
# Undo contrib/install-greeter.sh: greetd stays installed, the files it wrote go, the backups
# come back, the display manager leaves the default runlevel. Run as root.
set -eu

[ "$(id -u)" -eq 0 ] || { echo "must run as root" >&2; exit 1; }
share=/usr/local/share/linux_hello

restore() {
    if [ -f "$1.linux_hello.bak" ]; then
        mv "$1.linux_hello.bak" "$1"
        echo "restored $1"
    elif [ -f "$1" ] && head -n2 "$1" | grep -q 'created by linux_hello contrib/install-greeter.sh'; then
        rm "$1"
        echo "removed $1"
    fi
}

echo "== openrc"
if rc-update show default | grep -q display-manager; then
    rc-service display-manager status >/dev/null 2>&1 && echo "display-manager is running: rc-service display-manager stop first (from a tty)" >&2 && exit 1
    rc-update del display-manager default
fi
restore /etc/conf.d/display-manager

echo "== pam"
restore /etc/pam.d/greetd
restore /etc/pam.d/greetd-greeter
restore /etc/pam.d/hello-lock

echo "== greetd"
restore /etc/greetd/config.toml
restore /etc/greetd/niri-greeter.kdl
restore /etc/greetd/hello-greeter.toml

echo "== files"
rm -f /usr/local/bin/hello-greeter /var/log/hello-greeter.log
rm -rf "$share/assets"
echo "done (the pam module and the daemon stay; contrib/uninstall.sh removes them)"
