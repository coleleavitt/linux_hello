# TPM sealing of the face store

`linux_hello` can encrypt its face store with a key that only this machine's TPM 2.0 can
unseal. This is optional: it needs the `tpm` Cargo feature and an enabled TPM, and it is off by
default.

```sh
cargo build --release --features tpm      # needs tpm2-tss (libtss2-esys, libtss2-tctildr)
linux_hello seal                          # seal the store (an empty one if none exists yet)
linux_hello seal --pcrs 7                 # same, and bind the key to PCR 7
linux_hello unseal                        # back to plain JSON
```

After `seal`, every later `enroll` and `remove` keeps the store sealed. The daemon and `auth`
detect a sealed store by themselves; they need no flag.

## How it works

Envelope encryption:

1. `seal` makes a random 256-bit data key (from the OS random generator).
2. The TPM seals the data key: it becomes a keyed-hash *sealed data object* under a storage
   primary key (ECC P-256, owner hierarchy, empty auth). The TPM derives the primary key from
   its owner seed, so the same key comes back every time until the TPM is cleared. The sealed
   object is `fixedTPM` and `fixedParent`: no other TPM can load it.
3. The store JSON is encrypted with AES-256-GCM under the data key. The file then contains:

   ```json
   {
     "format": "linux_hello-tpm-v1",
     "key": { "public": "<TPM2B_PUBLIC, base64>", "private": "<TPM2B_PRIVATE, base64>", "pcrs": [] },
     "nonce": "<96-bit GCM nonce, base64>",
     "ciphertext": "<encrypted store JSON + GCM tag, base64>"
   }
   ```

   The sealed key is stored inside the store file, not in a second file, so one atomic rename
   still replaces everything at once. Each save uses a fresh nonce and the same data key.
4. On load, `linux_hello` recreates the primary key, loads the sealed object, unseals the data
   key and decrypts the store. GCM authenticates the file: a changed byte, or a key taken from
   another store, gives an error instead of wrong templates.

Sessions for sealing and unsealing are salted with the primary key and use parameter
encryption, so the data key does not cross the bus to a discrete TPM in the clear.

The daemon re-reads the store on every request. It keeps the unsealed data key in memory and
uses it again as long as the sealed key in the file is unchanged, so requests do not wait for
the TPM. A new `seal` changes the sealed key, and the daemon unseals the new one on the next
request.

### Choosing the TPM

The TPM is reached through a TCTI string, read like tpm2-tools does it: `TPM2TOOLS_TCTI`, else
`TCTI`, else `device:/dev/tpmrm0` (the kernel resource manager). Examples:

| TCTI | Use |
|------|-----|
| `device:/dev/tpmrm0` | real TPM (default) |
| `swtpm:host=127.0.0.1,port=2321` | software TPM, for tests |

If the store is sealed and the TPM cannot be reached, loading fails with
`cannot reach the TPM through "device:/dev/tpmrm0": enable the TPM in the firmware, or set
TPM2TOOLS_TCTI`. A binary built without the `tpm` feature says
`face store ... is sealed with a TPM, but linux_hello was built without the tpm feature`. In
both cases the daemon answers `ERROR ...`, the PAM module does not authenticate, and the normal
password prompt runs. tpm2-tss also prints its own `ERROR:tcti:...` lines to stderr.

## PCR binding

Without PCRs (the default), the TPM unseals the key for any software that runs on this machine
and asks for it. `seal --pcrs 7` adds a policy: the key unseals only while the SHA-256 PCRs have
the values they had at seal time. Then the object has no password access (`userWithAuth`
clear), only the policy.

PCR 7 records the Secure Boot state: on/off, the keys in PK/KEK/db/dbx, and which db entry
verified the boot loader. Trade-offs:

- **It breaks on legitimate changes.** A firmware update, a dbx update, turning Secure Boot on or
  off, or enrolling a new key changes PCR 7. Unsealing then fails with `PCRs [7] changed since
  sealing`, and the faces must be enrolled again. Run `linux_hello unseal` before such a
  change and `linux_hello seal --pcrs 7` after it.
- **It protects less than it seems.** PCR 7 does not measure the kernel, the initramfs or the
  root file system. Any OS that boots under the same Secure Boot settings gets the same PCR 7.
  On this ThinkPad **Secure Boot is off**, so PCR 7 only says "Secure Boot off": a live USB stick
  gets the same value. PCR 7 binding is only worth its cost with Secure Boot on and an OS
  that does not boot unsigned code.
- PCRs 4, 8, 9 or 11 (boot loader, kernel, UKI) would bind more tightly but change on every
  kernel update. That would need a signed PCR policy (`PolicyAuthorize`), which this code does
  not implement.

## What is protected, and what is not

The templates are biometric data. They do not unlock anything by themselves: the daemon
checks a live face against them. The risk is a privacy one: a stolen template stays linked to
a face for life. Sealing makes the store file useless away from this machine's TPM.

| Attack | Plain store (root-only file) | Sealed store |
|--------|-----------------------------|--------------|
| Someone copies `faces.json` (a backup, a disk image, a copied directory) | templates exposed | **protected**: the file cannot be decrypted without this TPM |
| The SSD is taken out and read in another computer | templates exposed | **protected** (but see swap and hibernation below) |
| The laptop is stolen and booted from a USB stick | templates exposed | **not protected** without PCRs: the TPM unseals for any OS, and the owner hierarchy has no password. With `--pcrs 7` and Secure Boot on, a live USB signed by a different CA is refused. |
| The laptop is stolen; the disk is not encrypted | templates exposed | **not protected**: the thief can change the installed system (for example add a root shell), boot it normally, and the TPM unseals. Only full-disk encryption closes this. |
| Root (or the daemon) on the running machine | templates exposed | **not protected**: root can ask the TPM to unseal, or read the key from the daemon's memory |
| Someone edits the store to swap in their own template | not detected | **detected**: GCM authentication fails |

Further limits:

- **Swap and hibernation.** The daemon holds the data key and the decrypted templates in
  memory. Memory can be written to swap, and hibernation writes all of RAM to the swap partition.
  This machine swaps to an unencrypted partition (`nvme0n1p2`) and hibernates to a swap file on
  the unencrypted root file system (`resume=/dev/nvme0n1p3 resume_offset=...`), so a hibernated
  laptop has the key on disk. Encrypted swap, or no hibernation, avoids this.
- **Plaintext left behind.** `linux_hello seal` replaces the plain file by a rename. The old
  blocks are not wiped and may still be readable on the disk; backups keep their copies. To
  start clean, delete the store and run `linux_hello seal` before the first `enroll`: then no
  plaintext is ever written.
- **Clearing the TPM** (BIOS "Clear Security Chip", or `tpm2_clear`) destroys the owner seed. A
  sealed store is then lost; enroll again. Run `linux_hello unseal` first if you plan to clear it.
- The TPM is not asked for the face comparison itself. Matching runs on the CPU, as before.

## Windows Hello comparison

_Placeholder: see `docs/windows-hello-re.md` (being written separately) for how Windows Hello
protects its templates. This section will summarize the differences once that document exists._

## Enabling the TPM on the ThinkPad P16 Gen 3

The kernel already has the drivers (`CONFIG_TCG_TPM`, `CONFIG_TCG_TIS`, `CONFIG_TCG_CRB`), but
the TPM is disabled in the firmware, so there is no `/dev/tpm*`.

1. Reboot and press **F1** at the Lenovo logo to open ThinkPad Setup.
2. Go to **Security → Security Chip**.
3. Set **Security Chip** to **Enabled** (the type is TPM 2.0). Do **not** choose *Clear Security
   Chip* unless you mean to destroy everything sealed by it.
4. Save with **F10** and boot.
5. Check: `ls -l /dev/tpm0 /dev/tpmrm0` and `cat /sys/class/tpm/tpm0/tpm_version_major` (prints `2`).

The daemon runs as root and can open `/dev/tpmrm0`. To use the TPM as a normal user (for
example with `--store` under your home), the user needs access to `/dev/tpmrm0` (usually group
`tss`).

## Migrating an existing plain store

```sh
rc-service linux_hello stop
cp -a /var/lib/linux_hello/faces.json /root/faces.json.plain   # optional: keep a copy somewhere safe, then delete it
linux_hello seal                                                # or: linux_hello seal --pcrs 7
linux_hello list                                                # opens the sealed store through the TPM
rc-service linux_hello start
```

`seal` keeps the file's path, mode 0600 and the enrolled faces. If the TPM is not reachable,
`seal` fails before it writes anything. To undo: `linux_hello unseal`.

## How it was tested

Without a hardware TPM, all tests ran against **swtpm 0.10.1** (libtpms) through tpm2-tss
4.1.3 and the `swtpm` TCTI.

`cargo test --features tpm` runs `tests/tpm.rs`. It starts its own swtpm on free local ports in a
temporary directory and checks:

| Test | Checks |
|------|--------|
| `round_trip` | seal → save → load gives the same templates; the file has no user names or features in clear; saving again keeps it sealed; `reload` with the cached key |
| `unseal_to_plain` | `unseal` writes plain JSON that the old format reads |
| `tampered_ciphertext` | one changed base64 character in `ciphertext` → `does not decrypt` |
| `swapped_key` | a valid sealed key from another store → `does not decrypt` |
| `tampered_blob` | one changed character in the sealed `private` blob → the TPM refuses to load it |
| `other_tpm` | a second, fresh swtpm cannot load the sealed key (the "disk moved to another machine" case) |
| `pcr_policy` | sealed to PCR 7, opens; after a `PCR_Extend` of PCR 7 → `PCRs [7] changed` |
| `tpm_unavailable` | TCTI pointing at a closed port → `cannot reach the TPM` |
| `no_leaked_handles` | 20 more loads; swtpm has no resource manager, so leaked objects or sessions would fill it (checked by forcing a leak: the test then fails with `out of memory for object contexts`) |

The tests run without the libtest harness (`harness = false`) because the TPM is selected
with an environment variable, which must not change while other test threads run.

End to end with the release binary and swtpm (no cameras):
`enroll dummy --image` into a temporary store → `seal` (no user names left in the file) →
`list` → `enroll` a second user (still sealed) → `seal --pcrs 7` → `list` → `unseal` → plain
JSON. A binary built without `tpm` and a missing `/dev/tpmrm0` both give the errors quoted above.
The daemon was started on a sealed store and asked `AUTH nobody`: replies were
`NOTENROLLED` (key unsealed once at start, reused for each request); after a tampered
ciphertext it answered `ERROR ... does not decrypt`; after a re-seal it unsealed the new key.

Timing on swtpm (debug build): opening a sealed store (create primary + load + unseal +
decrypt) took 18 ms; a reload with the cached key took 3 ms. A hardware TPM will be slower,
mainly for creating the primary key; this is not yet measured.
