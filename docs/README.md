# linux_hello docs

| Doc | Status |
|-----|--------|
| [sudo.md](sudo.md) | sudo face login: flow, measurements, install, caveats |
| [windows-hello-re.md](windows-hello-re.md) | Windows Hello Face reverse engineering (IDA/idalib, public PDBs) + runtime checks on this camera |
| [feature-gap.md](feature-gap.md) | Every Windows Hello face-path behaviour vs linux_hello: done / in progress / missing / won't do |
| [calibration.md](calibration.md) | LFW threshold calibration (0.50 cosine for per-attempt FAR ≤ 1e-5) |
| [winhello-models/](winhello-models/) | Converters for Windows Hello model formats: convnet, cefa + cipher, lbf + head pose, ldmf |
| [tpm.md](tpm.md) | Optional TPM 2.0 sealing of the face store (`--features tpm`, `linux_hello seal/unseal`); tested on swtpm |
| Screen lock | On hold: swaylock dropped; needs a locker that starts PAM without waiting for Enter |
