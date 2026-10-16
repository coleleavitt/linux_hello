#!/usr/bin/env python3
"""Run pam_linux_hello.so through real libpam against a stub daemon, without root or a camera.

Usage: pam_harness.py <libpam_linux_hello.so> <socket path> '<reply>|<reply>...'

Writes a one-off PAM service (`auth sufficient <module>`, then `pam_permit` standing in for the
password) under /tmp, serves the given daemon replies one per connection on the socket, then calls
pam_start_confdir, pam_authenticate and pam_setcred(PAM_ESTABLISH_CRED). Prints the requests the
stub received and every conversation message (style, text). Example (docs/sudo.md):

    pam_harness.py target/debug/libpam_linux_hello.so /tmp/lh.sock 'NOMATCH 0.430 tok|ADDED 0.432'
"""
import ctypes
import os
import socket
import sys
import threading
import time

sofile, sock, replies = sys.argv[1], sys.argv[2], sys.argv[3].split("|")
confdir = "/tmp/pam_linux_hello_harness"
os.makedirs(confdir, exist_ok=True)
with open(f"{confdir}/lhtest", "w") as f:
    f.write(f"auth sufficient {os.path.abspath(sofile)} socket={sock}\nauth required pam_permit.so\n")

received = []


def serve():
    if os.path.exists(sock):
        os.remove(sock)
    s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    s.bind(sock)
    s.listen(4)
    for reply in replies:
        c, _ = s.accept()
        received.append(c.makefile("r").readline().strip())
        c.sendall((reply + "\n").encode())
        c.close()


threading.Thread(target=serve, daemon=True).start()
time.sleep(0.2)


class PamMessage(ctypes.Structure):
    _fields_ = [("msg_style", ctypes.c_int), ("msg", ctypes.c_char_p)]


class PamResponse(ctypes.Structure):
    _fields_ = [("resp", ctypes.c_char_p), ("resp_retcode", ctypes.c_int)]


CONV = ctypes.CFUNCTYPE(
    ctypes.c_int,
    ctypes.c_int,
    ctypes.POINTER(ctypes.POINTER(PamMessage)),
    ctypes.POINTER(ctypes.POINTER(PamResponse)),
    ctypes.c_void_p,
)


class PamConv(ctypes.Structure):
    _fields_ = [("conv", CONV), ("appdata_ptr", ctypes.c_void_p)]


libc = ctypes.CDLL("libc.so.6")
libc.calloc.restype = ctypes.c_void_p
messages = []


def conv(n, msgs, resp, _):
    for i in range(n):
        m = msgs[i].contents
        messages.append((m.msg_style, m.msg.decode()))
    resp[0] = ctypes.cast(libc.calloc(n, ctypes.sizeof(PamResponse)), ctypes.POINTER(PamResponse))
    return 0


libpam = ctypes.CDLL("libpam.so.0")
callback = CONV(conv)
pam_conv = PamConv(callback, None)
pamh = ctypes.c_void_p()
user = os.environ.get("USER", "nobody").encode()
print("pam_start_confdir", libpam.pam_start_confdir(b"lhtest", user, ctypes.byref(pam_conv), confdir.encode(), ctypes.byref(pamh)))
print("pam_authenticate", libpam.pam_authenticate(pamh, 0))
print("pam_setcred(ESTABLISH)", libpam.pam_setcred(pamh, 0x2))
libpam.pam_end(pamh, 0)
print("daemon received:", received)
print("messages:", messages)
