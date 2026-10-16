# linux_hello docs

| Doc | Status |
|-----|--------|
| [sudo.md](sudo.md) | sudo face login: flow, measurements, install, caveats |
| [capture-and-liveness.md](capture-and-liveness.md) | IR lit/ambient pairs, ambient subtraction, lighting gate, liveness votes, multi-print matching with Microsoft's recogniser as second opinion, template augmentation |
| [windows-hello-re.md](windows-hello-re.md) | Windows Hello Face reverse engineering (IDA/idalib, public PDBs) + runtime checks on this camera |
| [feature-gap.md](feature-gap.md) | Every Windows Hello face-path behaviour vs linux_hello: done / in progress / missing / won't do |
| [calibration.md](calibration.md) | LFW threshold calibration (0.50 cosine for per-attempt FAR ≤ 1e-5 with 15 prints), the per-camera calibration table (`models/calibration.json`, similarity → FAR × prints × 16 pairs, as Windows' F51) Microsoft's own table for the second opinion, and the IR impostor check (`examples/impostor_capture`, `examples/calibrate_ir`) |
| [winhello-models/](winhello-models/) | Converters for Windows Hello model formats: convnet, cefa + cipher, lbf + head pose, ldmf |
| [tpm.md](tpm.md) | Optional TPM 2.0 sealing of the face store (`--features tpm`, `linux_hello seal/unseal`); tested on swtpm |
| Screen lock | On hold: swaylock dropped; needs a locker that starts PAM without waiting for Enter |
