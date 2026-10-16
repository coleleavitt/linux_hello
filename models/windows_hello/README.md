# Windows Hello Face models (extracted, NOT redistributable)

Extracted from the Windows 11 build 26300 image (`Windows11_Client_x64_en-us_26300_9457.iso`)
by `pefile` from the custom resource types (`MODEL_FILES`, `CONFIG_FILES`) of the Windows Hello Face DLLs.
These are **Microsoft proprietary files**: they are git-ignored, must not be pushed or shared, and linux_hello
does **not** use them (it uses the open models in `../`). They are kept for research only.
Full list with sizes and SHA-256: `manifest.json`. Background: `docs/windows-hello-re.md` (F22–F30).

Formats:
- **convnet**: Microsoft `conv_net_v1` (DNNTestLib); not loadable by ONNX Runtime
- **cefa**: `cefa0a00` container, partly encrypted (HelloFaceCipher)
- **ldmf**: `LDMF` FaceSDK detector format
- **onnx**: ONNX (from PyTorch); loads in ONNX Runtime
- **ort**: ONNX Runtime ORT flatbuffer; loads in ONNX Runtime
- **bin**: opaque until decoded (110–113 and core 120 are HelloFaceCipher-obfuscated; decrypt with `tools/winhello_models/cipher.py`)

| File | Format | Bytes | SHA-256 | What it is |
|------|--------|------:|---------|------------|
| `FaceDetectorResources/MODEL_FILES_101.ldmf` | ldmf | 13,096,096 | `e98409c2d341f98c…` | Face detector: 101/102 LDMF "FaceSDK Team" infrared detectors; 110–113 HelloFaceCipher-obfuscated ORT IR face detectors (box, class, 5 landmarks, confidence; 110/112 also 6-DOF pose) (docs/winhello-models/ldmf.md) |
| `FaceDetectorResources/MODEL_FILES_102.ldmf` | ldmf | 3,872,256 | `02b4eafd06070471…` | Face detector: 101/102 LDMF "FaceSDK Team" infrared detectors; 110–113 HelloFaceCipher-obfuscated ORT IR face detectors (box, class, 5 landmarks, confidence; 110/112 also 6-DOF pose) (docs/winhello-models/ldmf.md) |
| `FaceDetectorResources/MODEL_FILES_110.bin` | bin | 495,928 | `27a97e68a5907ae5…` | Face detector: 101/102 LDMF "FaceSDK Team" infrared detectors; 110–113 HelloFaceCipher-obfuscated ORT IR face detectors (box, class, 5 landmarks, confidence; 110/112 also 6-DOF pose) (docs/winhello-models/ldmf.md) |
| `FaceDetectorResources/MODEL_FILES_111.bin` | bin | 457,736 | `9a193482d74cd056…` | Face detector: 101/102 LDMF "FaceSDK Team" infrared detectors; 110–113 HelloFaceCipher-obfuscated ORT IR face detectors (box, class, 5 landmarks, confidence; 110/112 also 6-DOF pose) (docs/winhello-models/ldmf.md) |
| `FaceDetectorResources/MODEL_FILES_112.bin` | bin | 487,704 | `de8b3961344e62ef…` | Face detector: 101/102 LDMF "FaceSDK Team" infrared detectors; 110–113 HelloFaceCipher-obfuscated ORT IR face detectors (box, class, 5 landmarks, confidence; 110/112 also 6-DOF pose) (docs/winhello-models/ldmf.md) |
| `FaceDetectorResources/MODEL_FILES_113.bin` | bin | 483,928 | `b371009d7368948a…` | Face detector: 101/102 LDMF "FaceSDK Team" infrared detectors; 110–113 HelloFaceCipher-obfuscated ORT IR face detectors (box, class, 5 landmarks, confidence; 110/112 also 6-DOF pose) (docs/winhello-models/ldmf.md) |
| `FaceRecognitionEngineAdapterResources/MODEL_FILES_120.cefa` | cefa | 1,988,167 | `77f9cddd5b320ffe…` | Recognition v1: ORT feature-converter MLPs (old CNN not shipped) |
| `FaceRecognitionEngineAdapterResources_v2/MODEL_FILES_121.cefa` | cefa | 1,988,151 | `45107227eaef6fb0…` | Recognition v1–v4: ORT feature-converter MLPs mapping embeddings into a shared space (old CNNs not shipped) |
| `FaceRecognitionEngineAdapterResources_v3/MODEL_FILES_122.cefa` | cefa | 1,988,167 | `f018e4be2f44b1d5…` | Recognition v1–v4: ORT feature-converter MLPs mapping embeddings into a shared space (old CNNs not shipped) |
| `FaceRecognitionEngineAdapterResources_v4/MODEL_FILES_124.cefa` | cefa | 2,119,279 | `816c6a04468e1b53…` | Recognition v1–v4: ORT feature-converter MLPs mapping embeddings into a shared space (old CNNs not shipped) |
| `FaceRecognitionEngineAdapterResources_v4/MODEL_FILES_126.cefa` | cefa | 2,149,087 | `5a87b237150f98b9…` | Recognition v1–v4: ORT feature-converter MLPs mapping embeddings into a shared space (old CNNs not shipped) |
| `FaceRecognitionEngineAdapterResources_v5/MODEL_FILES_123.cefa` | cefa | 12,374,070 | `d39f42ba96fa6f4d…` | Face recognition v5 (tech 11): plaintext 0xFACE container with 5 FAR calibration tables + LDMF/NNDR conv_net_v1 Inception net, 128×128×1 → 256-d; 123 and 125 compute the same function (docs/winhello-models/cefa.md) |
| `FaceRecognitionEngineAdapterResources_v5/MODEL_FILES_125.cefa` | cefa | 12,395,400 | `aea1f1b879eaf22d…` | Face recognition v5 (tech 11): plaintext 0xFACE container with 5 FAR calibration tables + LDMF/NNDR conv_net_v1 Inception net, 128×128×1 → 256-d; 123 and 125 compute the same function (docs/winhello-models/cefa.md) |
| `facerecognitionengineadapterresourcescore/CONFIG_FILES_107.bin` | bin | 6 | `82ff4b265be00492…` | 6-byte config |
| `facerecognitionengineadapterresourcescore/CONFIG_FILES_108.bin` | bin | 6 | `82ff4b265be00492…` | 6-byte config |
| `facerecognitionengineadapterresourcescore/MODEL_FILES_103.bin` | bin | 117,648 | `f80fe1ab732f627a…` |  |
| `facerecognitionengineadapterresourcescore/MODEL_FILES_105.ort` | ort | 1,032,952 | `3d8477f32e43bed7…` | UNKNOWN; has `net.fc_mask` layer → likely mask/occlusion classifier (inferred). In: 1×1×128×128, out: `probability` 1×12 |
| `facerecognitionengineadapterresourcescore/MODEL_FILES_106.onnx` | onnx | 2,119,103 | `9e63d5a49a05bccc…` | Occlusion classifier (plain predecessor of core 120, used when the feature flag is off): classes [none, mask, sunglasses, hand]. In: 1×1×128×128, out: 1×4 |
| `facerecognitionengineadapterresourcescore/MODEL_FILES_120.bin` | bin | 2,119,159 | `245cfafbdc80afeb…` | HelloFaceCipher-obfuscated ONNX occlusion classifier [none, mask, sunglasses, hand] (flag-on successor of 106) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_115.convnet` | convnet | 2,105,712 | `5f80d204419cffeb…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_116.convnet` | convnet | 2,105,712 | `7e3841992b4bab76…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_117.convnet` | convnet | 2,110,463 | `a097fb5bfd212302…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_118.convnet` | convnet | 2,105,855 | `f475d41876cf306a…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_119.convnet` | convnet | 2,105,855 | `6794192aec44ebd2…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_121.convnet` | convnet | 2,120,105 | `b48f48e6f40d001f…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_122.convnet` | convnet | 2,120,105 | `b1ce98efdc4caed8…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_123.convnet` | convnet | 2,119,647 | `b5edad5dfb28b575…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_124.convnet` | convnet | 2,115,040 | `d2e0c5e795ad7ede…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionengineadapterresourcessecure/MODEL_FILES_125.convnet` | convnet | 2,119,278 | `07a0c4d8c8c11e49…` | IR liveness (anti-spoofing) SqueezeNet, 128×128×1 → 2 classes; slots 11/12/14/15 (F22–F24, F30) |
| `facerecognitionsensoradapterresources/MODEL_FILES_102.cefa` | cefa | 7,349,136 | `5e5cb25371ccebc2…` | 102: LbfAligner database (0xFACE v10, 31 points, 5 stages × 310 trees); 104/105/106/107: IrFaceOrientation PITCH_KNN/PITCH_LDA/YAW_KNN/YAW_LDA head pose (docs/winhello-models/lbf.md) |
| `facerecognitionsensoradapterresources/MODEL_FILES_104.bin` | bin | 33,140 | `a7155c752378b7a4…` | 102: LbfAligner database (0xFACE v10, 31 points, 5 stages × 310 trees); 104/105/106/107: IrFaceOrientation PITCH_KNN/PITCH_LDA/YAW_KNN/YAW_LDA head pose (docs/winhello-models/lbf.md) |
| `facerecognitionsensoradapterresources/MODEL_FILES_105.bin` | bin | 2,598,600 | `da8b87b3e68c7878…` | 102: LbfAligner database (0xFACE v10, 31 points, 5 stages × 310 trees); 104/105/106/107: IrFaceOrientation PITCH_KNN/PITCH_LDA/YAW_KNN/YAW_LDA head pose (docs/winhello-models/lbf.md) |
| `facerecognitionsensoradapterresources/MODEL_FILES_106.bin` | bin | 58,580 | `74a1caed73446300…` | 102: LbfAligner database (0xFACE v10, 31 points, 5 stages × 310 trees); 104/105/106/107: IrFaceOrientation PITCH_KNN/PITCH_LDA/YAW_KNN/YAW_LDA head pose (docs/winhello-models/lbf.md) |
| `facerecognitionsensoradapterresources/MODEL_FILES_107.bin` | bin | 3,455,280 | `e0f66031beac17a9…` | 102: LbfAligner database (0xFACE v10, 31 points, 5 stages × 310 trees); 104/105/106/107: IrFaceOrientation PITCH_KNN/PITCH_LDA/YAW_KNN/YAW_LDA head pose (docs/winhello-models/lbf.md) |

Re-extract: `python3 scripts/extract_windows_hello_models.py models/windows_hello <dlls…>`; source DLLs are under `~/SiteResearch/windows_hello/windows_files/`.

`assets/` (same rules: Microsoft proprietary, git-ignored, local only) holds the sign-in UI assets — Segoe fonts, lock-screen images, the Hello face SVG/toast art, the eye animation frames (`hello/eye/`), string tables and the decompiled LogonUI layouts — written by `scripts/extract_windows_hello_assets.py <ISO> models/windows_hello/assets` and inventoried in `assets/manifest.json`; see docs/windows-hello-ui.md §6.

## Converters

| Tool | Handles | Doc |
|------|---------|-----|
| `tools/winhello_models/convnet` | `conv_net_v1` → ONNX (IR liveness 115–125, v5 recognizer) | docs/winhello-models/convnet.md |
| `tools/winhello_models/cefa` | 0xFACE recognition containers, calibration tables, feature converters, occlusion | docs/winhello-models/cefa.md |
| `tools/winhello_models/cipher.py` | HelloFaceCipher (17-byte XOR) | docs/winhello-models/cefa.md |
| `tools/winhello_models/lbf` | LbfAligner (102), IrFaceOrientation head pose (104–107), engine 128×128 crop | docs/winhello-models/lbf.md |
| `tools/winhello_models/ldmf` | LDMF detectors (101/102) and decrypted ORT detectors (110–113) | docs/winhello-models/ldmf.md |

Converted outputs go to `converted/` (git-ignored). Shared venv: `tools/winhello_models/.venv`.
