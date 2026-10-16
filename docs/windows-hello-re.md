# Windows Hello Face — static reverse engineering notes

Goal: learn how Windows Hello Face captures IR frames, runs liveness and recognition, and protects
templates, to inform `linux_hello`. Work is static only (IDA Pro 9.5 idalib + Hex-Rays, headless).

Evidence tags: **OBSERVED** = seen in a binary/INF (address, function or string cited);
**INFERRED** = reasoning from observed facts; **UNKNOWN** = not resolved.

Raw dumps (pseudocode, strings, imports, names, IDA databases): `/home/cole/SiteResearch/windows_hello/re/`
(one folder per DLL: `functions.txt`, `strings.txt`, `imports.txt`, `names.txt`, `pseudocode.c`;
`.i64` databases in `re/bin/`).

## Inputs and symbols

Build: all Microsoft face binaries are 10.0.26100.9444 (facecredentialprovider / FaceBootstrapAdapter 10.0.26100.9278).
PDBs fetched from msdl.microsoft.com by the CodeView GUID (`re/pdb/`):

| Binary | PDB from MS symbol server |
|---|---|
| FaceDetectorResources.dll | yes — `FaceDetectorResources.pdb` 510CFC8E86EA489F43F3714EBEAABE7A/1 |
| FaceProcessor.dll | yes — `FaceProcessor.pdb` 2CC3B41D1AC5F34838E3B67A68058E58/1 |
| FaceProcessorCore.dll | yes — `FaceProcessorCore.pdb` BC68660AAC61BB7EFF95E6724D54258E/1 |
| FaceRecognitionEngineAdapter.dll | no (404) — `FaceRecognitionEngineAdapter.pdb` 97211EC27BDB2E46BD17E85621F9D23C/1 |
| FaceRecognitionEngineAdapterResources.dll | no (404) — `FaceRecognitionEngineAdapterResources.pdb` 51D7C9BB98E086FFD51357ED4E8136C4/1 |
| FaceRecognitionEngineAdapterResources_v2.dll | yes — `FaceRecognitionEngineAdapterResources_v2.pdb` 5031FB22E527DF9334F7E6B11863E43F/1 |
| FaceRecognitionEngineAdapterResources_v3.dll | yes — `FaceRecognitionEngineAdapterResources_v3.pdb` 6E6CE553BE50647E0713A01C499ABEDA/1 |
| FaceRecognitionEngineAdapterResources_v4.dll | yes — `FaceRecognitionEngineAdapterResources_v4.pdb` 46C34D7CB2FC20B78A0AEDC1D8EFB3EC/1 |
| FaceRecognitionEngineAdapterResources_v5.dll | yes — `FaceRecognitionEngineAdapterResources_v5.pdb` 2BF7804A9632BE59FCD077004A971B97/1 |
| FaceRecognitionSensorAdapter.dll | no (404) — `FaceRecognitionSensorAdapter.pdb` 734F8D83103437FD53CEC9D45A564DE0/1 |
| FaceRecognitionSensorAdapterVsm.dll | yes — `FaceRecognitionSensorAdapterVsm.pdb` 162854E8021613D4E1B94393A70D147A/1 |
| FaceRecognitionSensorAdapterVsmSecure.dll | yes — `FaceRecognitionSensorAdapterVsmSecure.pdb` 0AC8EB5BAC58D4BC0EF4BDA709507475/1 |
| FaceTrackerInternal.dll | yes — `FaceTrackerInternal.pdb` E1CA2DCCB70B2C3F31E444E7D12444AE/1 |
| HelloFace.dll | yes — `HelloFace.pdb` DE23B22B7C1F3896CFA41E8FB8D286AB/1 |
| facerecognitionengineadapterresourcescore.dll | yes — `FaceRecognitionEngineAdapterResourcesCore.pdb` C425E4F1D725EC394B74211BCCF9E8B0/1 |
| facerecognitionengineadapterresourcessecure.dll | yes — `FaceRecognitionEngineAdapterResourcesSecure.pdb` 9D443F18EB9BF437F7D6221DBB7BC504/1 |
| facerecognitionsensoradapterresources.dll | no (404) — `FaceRecognitionSensorAdapterResources.pdb` B51EDE81C6135AC80EE7864703AF3E20/1 |
| RsDMFTIR64.dll | no (not a Microsoft binary) — `RsDMFTIR64.pdb` 02F894B5F5C6437481BE733F4ACDDF37/1 |
| facecredentialprovider.dll | no (404) — `facecredentialprovider.pdb` 5A11D87D34BA315C83B2FA0A74134A16/1 |
| FaceBootstrapAdapter.dll | yes — `FaceBootstrapAdapter.pdb` E62FD59FCAC8D7BAEC7D95ABF7411617/1 |

Not in this set: `FaceRecognitionEngineAdapter.pdb` and `FaceRecognitionSensorAdapter.pdb` are not on the public
server (404). `FaceRecognitionSensorAdapterVsm.dll` has symbols and shares most code with the plain sensor adapter,
so its names are used to identify functions in the plain one.

## Checklist

| # | Question | Status |
|---|---|---|
| 1 | Call flow, adapter vtables | answered |
| 2 | Frame acquisition (lit/ambient pairing, counts, timeouts) | answered (timeouts partial) |
| 3 | IR emitter control | answered (UVC selector inferred) |
| 4 | Anti-spoofing / liveness | answered (F22–F25, F40–F42) |
| 5 | Recognition (detect, align, embed, match, thresholds) | answered (F27–F31, F48–F54) |
| 6 | Template storage and protection | answered (component level) |
| 7 | Changes for linux_hello | answered |

## Finding ledger

| ID | Tag | Finding | Evidence |
|---|---|---|---|
| F1 | OBSERVED | Two WinBio configurations are installed. Config 0 (normal): engine `FaceRecognitionEngineAdapter.DLL`, sensor `FaceRecognitionSensorAdapter.DLL`, storage `WinBioStorageAdapter.DLL`, DatabaseId `DC576DA6-D676-4A15-906D-C0CEAF949543`. Config 1 (Virtual Secure Mode): normal-world engine `WinBioPassthroughAdapter`, sensor `FaceRecognitionSensorAdapterVsm.DLL`, storage `WinBioVsmStorageAdapter.DLL`; secure-world engine `FaceRecognitionEngineAdapter.DLL` (same DLL), secure sensor `FaceRecognitionSensorAdapterVsmSecure.DLL`, secure storage `WinBioVsmCacheAdapter`; DatabaseId `A61A7480-6A54-4D36-A0EF-B150B8545827`. `VirtualSecureConfiguration=1`. | helloface.inf `[DriverAddReg_BioSensorConfiguration0/1]` |
| F2 | OBSERVED | Both configs: `CaptureIntervalInMilliseconds=125` (0x7d), `PresenceStabilizationThreshold=300` (0x12c), `PresenceLifetimeInSeconds=0`, `SensorMode=1` (basic), `SystemSensor=1`. `HelloFaceRecognitionTechVersion=5` (matches `..._v5.dll` being the only "current" recognition resource; v1–v4 are copied as "legacy"). | helloface.inf `[DriverAddReg_BioSensor]`, `[CopyRecognitionResources]` |
| F3 | OBSERVED | HelloFace.dll is a root-enumerated UMDF 2.15 software device (`Root\WindowsHelloFaceSoftwareDriver`), class Biometric, exclusive. It is not the camera driver. | helloface.inf |
| F4 | OBSERVED | The IR camera (30C9:00F3 MI_02) installs as `RS_SE_IR_CAMERA`: `KSCATEGORY_SENSOR_CAMERA` interface, `SensorCameraMode=1`, `SkipCameraEnumeration=1` (hidden from normal camera apps), Device MFT `CameraDeviceMftCLSIDChain={09D98F23-EB50-4984-808F-DA19DCF0CA6E}` = RsDMFTIR64.dll. Both DMFT DLLs carry `SignatureAttributes.WindowsHello = true`. The `SecureUSBVideo` secure-companion section (TrustletIdentity 4096) exists in the INF but no install section for this IR camera references it. The sensor group is `{f66fa674-f2be-4d19-98c5-d132329c642f}` "Lenovo Camera Group" only for the `EX` variants; `RS_SensorGroup.AddReg` is empty. | RtLeJFIR.inf |
| F5 | OBSERVED | Camera capture is not done by the WinBio sensor adapter itself. `FaceRecognitionSensorAdapter*.dll` imports `FaceProcessor.dll`; `FaceProcessor.dll` opens the camera with WinRT `Windows.Media.Capture.MediaCapture` + `MediaFrameReader` (class `SensorInputManagerFS`, "FS" = Frame Server). A second input path `SensorInputManagerSDS` uses `Windows.Devices.Perception` (`PerceptionInfraredFrameReader`). | FaceProcessor.dll strings `SensorInputManagerFS::InitializeMediaCapture.*`, `CreateInfraredReader.CreateFrameReaderAsync`, `AddInfraredFrameArrived`; function list in `re/FaceProcessor/functions.txt` |
| F6 | OBSERVED | The IR stream is opened with the **Face Authentication camera profile** `KSCAMERAPROFILE_FaceAuth_Mode {81361B22-700B-4546-A2D4-C52E907BFC27}` (`SensorInputManagerFS::SetFaceAuthProfile` 0x180027908: StringFromCLSID of that GUID, `MediaCapture.FindKnownVideoProfiles/GetAt`, `PutVideoProfile`). | FaceProcessor.dll 0x180027908 |
| F7 | OBSERVED | IR format choice: `SetInfraredMediaFormat` (0x180018994) takes `SupportedFormats[0]` of the IR `MediaFrameSource` (no search), records width/height/fps, and logs `UnsupportedFrameFormatDetected` if fps < 7.5 or width < 340 or height < 340 (0x154); then `SetFormatAsync`. | FaceProcessor.dll 0x180018994 |
| F8 | OBSERVED | Emitter / frame-illumination control goes through the **extended camera control property set** `KSPROPERTYSETID_ExtendedCameraControl {1CB79112-C0D2-4213-9CA6-CD4FDB927972}` using `MediaFrameSourceController.GetPropertyAsync/SetPropertyAsync` with the string id `"{1CB79112-...} <id>"` built by `SensorInputManagerFS::SetPropertyString` (0x18003271c). Ids used: 29 (`FACEDETECTION`, `ToggleFaceDetectionOnDriverIfCapable`), 35 (`FACEAUTH_MODE`, `ToggleFaceMode`, stored at this+0x388), 36 (`SECURE_MODE`, `ToggleSecureSensor`, this+0x390). Id 38 (`IRTORCHMODE`) is never used. | `InitializeComponents@SensorInputManagerFS` calls `SetPropertyString(…,35,this+113)` and `(…,36,this+114)` |
| F9 | OBSERVED | `ToggleFaceMode(enable)` (0x180032a8c) reads the FACEAUTH_MODE capability word. Enable: if capability bit 0x2 (`KSCAMERA_EXTENDEDPROP_FACEAUTH_MODE_ALTERNATIVE_FRAME_ILLUMINATION`) → set flags=2 and set `AmbientLightSubtractionNeeded=1` (host does subtraction); else if bit 0x4 (`..._BACKGROUND_SUBTRACTION`, driver delivers already-subtracted frames) → flags=4; else fail `0x8007139F`. Disable → flags=1 (`..._DISABLED`). | FaceProcessor.dll 0x180032a8c |
| F10 | OBSERVED | Per IR frame, `PopulateInfraredFrameProperties` (0x1800146a8) stores `SystemRelativeTime` (descriptor+16), `MF_CAPTURE_FRAME_ISMIRRORED`, and — only when `AmbientLightSubtractionNeeded` — `InfraredMediaFrame.IsIlluminated` (descriptor+32). Otherwise every frame is marked illuminated. `IsIlluminated` is WinRT's view of the `MF_CAPTURE_METADATA_FRAME_ILLUMINATION` sample attribute set by the driver/DMFT. | FaceProcessor.dll 0x1800146a8; dump `re/FaceProcessor/PopulateInfraredFrameProperties_FS.c` |
| F11 | OBSERVED | Frame hand-off is a single slot per stream: `PreprocessorRuntime::SetFrameData` copies the descriptor into a static slot, increments a frame counter, stamps `GetTickCount64`, and signals the processing thread. A new frame overwrites the previous one if it was not consumed (no queue). | FaceProcessor.dll 0x18000f1e0 |
| F12 | OBSERVED | Pairing lit/ambient (`InfraredFaceProcessor::RunProcessorAndPerformAmbientLightSubtraction`, FaceProcessorCore 0x18002ec80): **ambient frame** → (optionally `LookForFacesInAmbientFrame`) → `CropAndStoreAmbientImage` crops the ambient image around the faces found in the previous lit frame. **Lit frame** → full face detection/alignment (`FaceProcessor::Run`) → `AdjustFoundFacesForAmbientLight` subtracts the stored ambient crop. When the driver does background subtraction (mode 4) every frame goes straight to detection and `ExposureEstimation::EvaluateAmbientLightLevelInCleanInfraredImage` is used instead. | FaceProcessorCore 0x18002ec80; dump `re/FaceProcessorCore/focus/` |
| F13 | OBSERVED | Subtraction rules (`AmbientLightSubtracterBase::AdjustFoundFacesForAmbientLight` 0x180036e78): only when the ambient crop is older than the lit frame by **< 3,000,000 ×100 ns = 300 ms**, the face is frontal in both frames, and matched by tracking id. Motion compensation: `ComputeAnchorPoints` (0x1800372c8) makes 3 anchors (mean of 5 left-eye landmarks, 5 right-eye landmarks, 6 mouth landmarks 3, 4, 23–26 — corrected per F58); `AdjustFaceForAmbientLight` (0x180036c10) interpolates anchors by timestamp ratio (`cur·(1−t') + prev·t'`, `t' = (t_ambient − t_prev_lit)/(t_lit − t_prev_lit)`; the 300 ms window is between the previous and the current lit frame, lbf.md L13) and builds an affine transform (`DeriveAffineTransformationOfFacePoints`, exact through the 3 anchor pairs, current → interpolated), then `RemoveAmbientLightFromFace` warps the ambient crop (`warped(p) = ambient(M·p)`, bilinear, zero padded) and subtracts. If lighting status is 1 ("good") and the face-ambient metric ≤ 25, it also does a direct per-pixel saturating subtraction `lit -= min(lit, ambient)` on the face crop. | FaceProcessorCore 0x180036e78, 0x180036c10, 0x1800372c8 |
| F14 | OBSERVED | Lighting status (`EvaluateLightingCondition` 0x180037c00): `d = litFaceBrightness − ambientFaceBrightness`; `d ≥ 45` → status 1 (good) or 2 if `d > 205`; `d < 45` and status was 1 → 4. With feature `ImproveFaceQualityFilter`: ambient face brightness > 25 and status 1 → 4. Ambient brightness is smoothed `avg = 0.5·avg + 0.5·new`. Status 3 and 6 skip the affine subtraction. | FaceProcessorCore 0x180037c00 |
| F15 | OBSERVED | `FrameStrobe::CalculateIsStrobeOnForInfraredFrame` (FaceProcessor 0x180089314) guesses lit/unlit **only for file replay** (`FileInputManager::SendInfraredFrameDataInternal` is the only caller): it sums every 5th pixel of every 5th row; a frame is "on" if its sum ≥ mean of the last ≤5 sums; once the coefficient of variation of the last 5 sums is < 0.1 it locks and just alternates on/off. Live capture relies on driver metadata instead. | FaceProcessor 0x180089314 |
| F16 | OBSERVED | Realtek IR DMFT (`RsDMFTIR64.dll`, class `CRsDMFTIR`/`IRController`/`CIROutPin`) intercepts KS property set `{1CB79112-…}` ids 35 (`FACEAUTH_MODE`) and 8 (`TORCHMODE`, only for "Mirametrix" LED models via Realtek XU control 14). For FACEAUTH_MODE it **passes through** to the UVC driver when the driver supports it, and emulates it (`IRController::FaceAuthModeHandler` 0x18000c270, caps=3 i.e. DISABLED+ALTERNATIVE_FRAME_ILLUMINATION) only for a hard-coded VID/PID list (04F2:B66A/B671/B690, 0408:5343, 0BDA:5667/5668/B6AF). The chosen mode is mirrored into a registry value `EXT_FACEAUTHMODE` (reset to 1 on `MFT_MESSAGE_NOTIFY_END_STREAMING` and in the destructor). | RsDMFTIR64 0x18000e420, 0x18000c270, 0x1800325b0, 0x18000b360 |
| F17 | OBSERVED | DMFT workaround in `CIROutPin::AddSample` (0x18000b570): on the first IR sample of a stream whose media type is 640×360 at frame-rate numerator 15, it sends `FACEAUTH_MODE` SET with flags = 2 (alternate-frame illumination) to the driver ("Add sample internal Set IR Face Auth enable"). | RsDMFTIR64 0x18000b570 |
| F18 | OBSERVED | The DMFT does **not** reference `MF_CAPTURE_METADATA_FRAME_ILLUMINATION` {6D688FFC-…} or the Microsoft UVC XU GUID {0F3F95DC-2632-4C4E-92C9-A04782F43BC8} from code (both GUIDs sit unreferenced in a GUID table). So the DMFT neither tags lit/unlit frames nor talks to the MS XU itself. | IDA operand/xref scan of all `.text` heads for those addresses |
| F19 | INFERRED | Per-frame lit/unlit flag therefore comes from the inbox UVC stack: `usbvideo.sys` maps the camera's Microsoft-UVC-1.5 metadata (`MetadataId_FrameIllumination`) to `MF_CAPTURE_METADATA_FRAME_ILLUMINATION`, and translates `FACEAUTH_MODE` to the MS XU face-authentication control. This matches Microsoft's public UVC extension docs, not code in this set (usbvideo.sys was not analysed). | F16–F18 + public MS docs (external) |
| F20 | OBSERVED | `WINBIO_ENGINE_INTERFACE` at 0x180121110: Version 5.0, Type 2 (engine), Size 0x168 (41 slots), AdapterId {5BB95E04-F2A0-4D08-9F75-6ABE9B5D06ED}. Slot→function map verified by callees (`CommitEnrollment`→`EngineAdapter::CommitEnrollment`, `ControlUnitPrivileged`, `RefreshCache`, `AcceptPrivateSensorTypeInfo` all land on the functions whose telemetry strings carry those names). `Reserved_1`, `CreateEnrollmentAuthenticated`, `IdentifyFeatureSetAuthenticated` are NULL. Table in Q1. | `re/FaceRecognitionEngineAdapter/focus/EngineIf__*.c` |
| F21 | OBSERVED | Engine adapter has no public PDB, but its TraceLogging event names embed `Windows::Analog::Face::<Class>::<Method>.<Step>`. Mapping each event blob (string − 14 bytes) to the referencing function named 187 functions (`re/FaceRecognitionEngineAdapter/namemap.json`), saved into the .i64 and a re-dump `pseudocode_named.c`. | method |
| F22 | OBSERVED | Anti-spoofing = `AntiSpoofingDecisionEngine` with 16 classifier slots; 5 are used (`sub_18003F578`): **11 general IR CNN v2** and **12 "diffusion" IR CNN v2** (both `RunDualIRAntiSpoofingClassifier`), **13 color CNN v2** (`RunColorCnnAntiSpoofingClassifier`), **14 multi-task IR "border"** and **15 multi-task IR "HH"** (`RunMultiTaskIRAntiSpoofingClassifier`). 11/12/13 are enabled iff anti-spoofing is enabled for the session; 14/15 default off. Names come from the `AntiSpoofingDecisionEngine::Initialize` telemetry fields (`useGeneralIRCnnAntiSpoofingV2`, `useDiffusionIRCnnAntiSpoofingV2`, `useColorCnnAntiSpoofingV2`, `useMultiTaskIRCnnBorder`, `useMultiTaskIRCnnHH`). | ENG 0x18003f578, 0x1800c42cc, event at 0x18010a727 |
| F23 | OBSERVED | IR CNN input: `sub_180015F68(img, landmarks, w, h, stride, 128, 128, 1)` → **128×128×1** crop; color CNN input **128×128×3**. Inference uses Microsoft's `DNNTestLib` (exceptions `DNNTestLib::ConvException`), a 512-wide feature vector, a separate 2-output dense layer (`sub_18003F8E8`) and softmax (`sub_18003C0B0`). Models are in `facerecognitionengineadapterresourcessecure.dll` `MODEL_FILES` 115–125 (10 files ~2.1 MB, header `0d000000 'conv_net_v1.'`). | ENG `RunDualIRAntiSpoofingClassifier` 0x18004a6d0, `RunColorCnn…` 0x180049ec0; `re/models/` |
| F24 | OBSERVED | Per-classifier decision. IR (11/12): `p[0] > realThr` → live; else `p[1] > spoofThr` → spoof; else none. Color (13): `p[0] > thr` → spoof else none (veto only). Multi-task (14/15): `p[1] > thr` → spoof else none (veto only). Thresholds are `map<classifier, map<mode,float>>` (mode = byte at engine+44, two values each): realThr{11,12} = {0.5, 0.9}; spoofThr{11,12} = {1.0 (never), 0.5}; spoofThr13 = 0.5; spoofThr14 = 0.7; spoofThr15 = 0.5. A feature-flagged alternative set: realThr11 {0.4711,0.4921}, realThr12 {0.5037,0.5197}, spoofThr11 {1.0,0.5289}, spoofThr12 {1.0,0.4963}. | static initialisers ENG 0x180014020, 0x180014380, 0x180015450, 0x180015590 |
| F25 | OBSERVED | Frame vote (`RunCascadedClassification` 0x18003a9f4): live votes from 11 and 12 add +1 each; spoof votes from 11/12/13 add −1, from 14/15 −1 (−2 when mode byte = 1). Frame total > 0 → "live frame", < 0 → "spoof frame" (`AggregateDecision` 0x18003a654). Session decision (`MakeFinalDecision` 0x18003a860): **≥ 2 live frames → real**; else **≥ 3 spoof frames → spoof**; else after **≥ 48 frames** (or counter > 16) → spoof. If anti-spoofing is enabled but no classifier is enabled the state starts at "spoof"; if disabled it starts at "real" (`Initialize`). Status 6/7 = classifier skipped (missing paired input / other flag). | ENG 0x18003a9f4, 0x18003a654, 0x18003a860, 0x1800439d0 |
| F26 | OBSERVED | The KS ids are confirmed twice: Windows Hello's `ToggleFaceMode` uses id 35, and the Realtek DMFT handles id 35 in `IRController::FaceAuthModeHandler` and logs it as `KSPROPERTY_CAMERACONTROL_EXTENDED_FACEAUTH_MODE`; DMFT id 8 is TORCHMODE. | F8, F16 |
| F27 | OBSERVED | Recognition decision uses a **target false-accept rate of 1e-5**: `sub_18006E430` calls `CreateFaceRecognition(tech, TargetFalseAcceptRate = 0x3727C5AC = 1.0e-5f, …)` (telemetry `CreateFaceRecognition.Succeeded {FaceVerificationTech, TargetFalseAcceptRate}`), stored at FaceRecognition+0x3CF70 (`FaceRecognition::Initialize` 0x1800516f0). `MakeDecisions` (0x180047600) recognises a tracked face only if `targetFAR > GFP` where GFP is the per-user score kept by `UpdateDistanceOnNewPrintComputed`, and only after a minimum number of prints. | ENG 0x18006e430, 0x18009b4e0, 0x180047600 |
| F28 | OBSERVED | Matching (`TrackedFaceToEnrolledUserDetail::UpdateDistanceOnNewPrintComputed` 0x180048f1c, `EnrolledUser::CalculateMinDistance` 0x180030294): for each new print the L2 norm is recorded; for each enrolled user (≤ 32 slots) the **minimum distance over all stored prints** (plus "coupled" mean/pair prints) is found, then converted by an indirect call into **GFP** (telemetry `GfpUpdate {UserId, GFP, HardwareModelUsed, MatchableUser, ClosestDistance, VerificationCondition}`). The best GFP per "verification condition" (0–3, from print flags: 0x4, sign bit 0x80, 0x400) is kept. A 24-step table of flags marks which GFP levels are < 1e-5. | ENG 0x180048f1c, 0x180030294, event 0x180106d00 |
| F29 | INFERRED | GFP is an estimated false-positive probability obtained from the distance through a calibration curve, so the system thresholds on calibrated FAR rather than a raw cosine/L2 distance. The exact distance metric (L2 vs cosine) sits behind a CFG-dispatched virtual call and was not resolved. | F27, F28 |
| F30 | OBSERVED | Recognition CNNs: `FaceRecognitionEngineAdapterResources_v5.dll` MODEL_FILES 123 and 125 (~12 MB each) in Microsoft `conv_net_v1.0` format; input `1×1×128×128` (single-channel IR crop); 123 is a GoogLeNet/Inception-BN net (`inception5b_*`, `pool5_flatten`, final `output` dense layer with weight shape 256×2048 → **256-float embedding**); 125 is an ONNX-converted GoogLeNet (`/net/conv1_7x7_s2/Conv` … `global_pool` → `feature_layer/Gemm`). Anti-spoofing nets (secure DLL 115–125) are SqueezeNet (`fire2..fire8`, `pool10`), also 128×128×1. | `re/models/*`, header bytes after `conv_net_v1` |
| F31 | OBSERVED | Template update: `TrackedFace::AugmentUserProfileOnSuccess` (0x180057f80) adds a print from a successful session to the user's profile with flag 0x400 and a generation counter; it refuses when the source print was already augmented ≥ 3 times. `EnrollOnFailure` / `AugmentUserProfileOnFailure` add prints after a failed face attempt (flag 0x4; only used for matching when policy allows). | ENG 0x180057f80, 0x1800a6064, 0x18009b940 |
| F32 | OBSERVED | The sensor adapter sends the engine **pre-processed face data**, not raw frames: `SensorAdapterProductWorker::PushDataToEngineInternal` → `BioDataComposer::ComposeDataForEngine` → `EngineInterface->AcceptSampleData(pipeline, buf, size, purpose)`; up to 3 `CapturedFrame`s are attached only when feature `FaceDataRetention` is on. | Vsm sensor adapter 0x180022520 |
| F33 | OBSERVED | Templates are stored through the WinBio storage-adapter API (`StorageManager::StoreSingleTemplateToDisk` → `WbioStorageUpdateRecordBegin`; `LoadFromDisk` → `WbioStorageGetRecordCount/GetFirst/NextRecord/GetCurrentRecord`). The engine itself imports no crypto. `winbiostorageadapter.dll` (config 0) imports BCrypt (`ChainingModeCBC`, `SHA256`, `KeyDataBlob`, symmetric key import/export) and `CryptProtectData/CryptUnprotectData`, and has field `encryptedTemplateBlobSize`. | ENG 0x18003ec10, 0x18002dda4; storage adapter imports/strings |
| F34 | INFERRED | Config 0: each template blob is AES-CBC encrypted with a per-database key that is itself DPAPI-protected (SYSTEM), integrity via SHA-256. "AES" (3 chars) was below the string-scan length; algorithm name not directly seen. | F33 |
| F35 | OBSERVED | VBS split (config 1): VTL0 `FaceRecognitionSensorAdapterVsm.dll` imports `FaceProcessor!CreateColorFaceProcessor` and forwards IR frames with `AsyncImportRawBuffer` / `AsyncImportSecureBuffer(GUID)`; VTL1 `FaceRecognitionSensorAdapterVsmSecure.dll` imports `FaceProcessorCore!CreateInfraredFaceProcessor/CreateFaceCache` (IR detection, alignment, `AmbientLightSubtracterSecure`) and pushes to the same engine DLL inside `BioIso.exe`. `BioIso.exe` imports `IumSdk!EncryptData/DecryptData/GetSecureIdentitySigningKey/GetSignedReport` and has `AuthorizeKeyRelease`/`CloseKeyAuthorizationSession` strings plus built-in `WinBioVsmCacheAdapter`. `winbiovsmstorageadapter.dll` has `DatabaseReencrypt`. | imports (`re/scripts/peinfo*.json`), class lists |
| F36 | INFERRED | In VBS mode templates are sealed with VTL1-only keys (IUM `EncryptData`) and a match in VTL1 authorises release of the user's NGC (Windows Hello) key (`AuthorizeKeyRelease`) — the face match never hands a secret to VTL0. With a secure camera (`SECURE_MODE`, id 36, plus the `SecureUSBVideo` companion trustlet) IR frames would only exist in VTL1 memory; this Realtek INF does not install that companion, so frames are copied from VTL0 (`AsyncImportRawBuffer`). | F35, F4 |
| F37 | OBSERVED | Per-face frame cache (`FaceFrameCacheData::GetBestInfraredFramesToAnalyze` FaceProcessorCore 0x180038f84): ring of ≤ 5 processed IR face frames per tracked face; each request returns the newest frame not yet handed out, skipping frames whose timestamp differs from "now" by > 1000 (0x3E8, ms); whole face entries are purged after 1000 ms without updates (`PurgeExpiredCacheEntries` 0x18001a1b8). | FaceProcessorCore |
| F38 | OBSERVED | **Recognition crop.** `FaceProcessor::ComposeLuminanceImagePerFace` (FaceProcessorCore 0x1800162d4) copies an 8-bit luminance crop of the **lit** frame around the detected face box: `ComputeCropRect` (0x1800168f0) pads each side by the box size (crop = 3 × box, centred) or, if 3 × box > 400 px, pads so the crop is exactly **400 px**; clamps to the frame; fails if the box is ≥ 400 px. Landmarks are shifted into crop coordinates. Detection/alignment run on the lit frame (F12). | FaceProcessorCore 0x1800162d4, 0x1800168f0 |
| F39 | OBSERVED | **Ambient crop.** `CropAndStoreAmbientImage` (0x18003753c) stores, from the unlit frame, the previous face crop rectangle grown by **100 px on every side** (x−100, y−100, w+200, h+200, clamped). | FaceProcessorCore 0x18003753c |
| F40 | OBSERVED | **Two subtracted images.** (a) Recognition image: `AmbientLightSubtracterSecure::RemoveAmbientLightFromFace` (0x18002b2f0) warps the ambient crop with the anchor affine (F13), re-evaluates lighting, and **only if lighting status = 1** subtracts in place, `crop[i] = lit − min(lit, warpedAmbient)` (`SubtractAmbientFrameFromIlluminatedFrame<uchar>` 0x18002ae80), into the luminance crop (FPO+56) that becomes the recognition input. (b) Liveness image: `InfraredFaceProcessor::ComposeAntiSpoofingInformation` (0x18002db60) copies the lit crop to a second buffer (FPO+256781) and sets "is difference image" = 1 when the driver already subtracts (mode 4); `AdjustFoundFacesForAmbientLight` overwrites that buffer with the **unwarped** `lit − min(lit, ambient)` and sets the flag when status = 1 and the ambient metric ≤ 25. It also stores a resized full-frame thumbnail (FPO+416781). | FaceProcessorCore 0x18002b2f0, 0x18002ae80, 0x18002db60, 0x180036e78 |
| F41 | OBSERVED | **Data hand-off mapping.** `BioDataComposer::ComposeDataForEngine` (VsmSecure 0x180007310) copies FPO+24.. (crop rect + pixels, 0x3E9A8 bytes) and FPO+256456.. (landmarks, liveness crop, flags, 0x2B390 bytes) plus status words into a 433528-byte per-face record; the engine reads the liveness crop at record+333, landmarks at record+44, crop size at record+177056, recognition crop at record+177048 (`ComputeTemplates` 0x18002b5b0). | VsmSecure 0x180007310; ENG offsets |
| F42 | OBSERVED | **"Diffusion IR" = difference image.** `AntiSpoofingDecisionEngine::UpdateInformation` (0x180028e10) enables, per frame, **general IR CNN (slot 11) iff the liveness crop is NOT a difference image** and **diffusion IR CNN (slot 12) iff it IS** (record+332 flag), both only when anti-spoofing is on. The multi-task IR nets (14/15) are switched on once the first live vote exists (or a global flag). This resolves the F22/Q4 unknown. | ENG 0x180028e10 |
| F43 | OBSERVED | **Face brightness definition.** `ExposureEstimation::EvaluateBrightnessPerFace` (FaceProcessorCore 0x180017140): mean of every 2nd pixel in every 2nd row over the box x ∈ [max(p3.x, p9.x), min(p4.x, p15.x)], y ∈ [max(p15.y, p9.y), min(p4.y, p3.y)] where pk are the IR aligner landmarks (p0/p1 eye centres, p3/p4 mouth corners, p7–p10 left-eye contour, p13–p16 right-eye contour — from the anchor groups in F13). Result in 0–255 (16-bit input divided by 256). Roughly: the cheek/nose block between the eyes and the mouth. Ambient brightness uses the same box on the warped ambient crop. | FaceProcessorCore 0x180017140, 0x18003799c |
| F44 | OBSERVED | **Exposure / lighting status** (`FacePreprocessorOutput` +433496): 0 = not evaluated (non-frontal); `EvaluateExposurePerFace` (0x180017604): lit brightness > 205 → **2**, < 35 → **3**, else **1**; then (feature `ImproveFaceQualityFilter` off) `CheckIfTooMuchSaturatedPixelOnFace` (0x180015960): count of clipped pixels (equal to their ±2 px neighbours both ways) inside the face ellipse > area·r/100 → **6**, around the face → **7** (or 6 if status ≠ 1). Ambient pairing (F14) then overrides: lit − ambient ≥ 45 → 1 (or 2 if > 205), else 1→**4**; ambient > 25 with status 1 → 4. Driver-subtraction path (`EvaluateAmbientLightLevelInCleanInfraredImage`): background left/right of the face brighter than `lit + 0.2·(255 − lit)` → 1→4, 2→**5**. | FaceProcessorCore 0x180017604, 0x180015960, 0x180037c00, 0x18003e5c0 |
| F45 | OBSERVED | **What each status does** (engine per-frame gate `sub_1800380CC` + `sub_180037F28`): status 2 or 6 → reject flag 0x100000 (counted as "too close"); 3 → 0x200000 ("too far"); 5 → reject. Status 4/5 → print flag 0x1, 7 → print flag 0x20 (prints still usable but tagged). Any reject flag (`TrackedFace+424 ≠ 0`) ⇒ `CheckIsFaceClearToUse` false and `ComputeTemplates` makes **no print** from that frame, and the frame is not used for liveness. Status 3/6 also skip the warped subtraction (F40a); status ≠ 1 skips both subtractions. | ENG 0x1800380cc, 0x180037f28, 0x180028b78, 0x18002b5b0 |
| F46 | OBSERVED | **Other frame gates** (`sub_1800380CC`): face box width < **72 px** or < **0.12 × min(frame w, h)** → reject 0x200000 (too far) — values from the v5 model parameter block (`sub_1800AF158` tech-3 block: +0x40 = 72, +0x44 = 0.12f), passed by `OnNewFaceTracked` via model slots 0x60/0x68 (disassembly 0x18002fd26–0x18002fd5f). Face closer to the image border than **inter-ocular distance / 5** → edge-cut flag 0x40. Eyes, mouth/nose or face occluded (ONNX occlusion detectors) → reject; while identifying, **≥ 4 consecutive occluded frames → give up** (`IdentifyAsGuest(3)`); an "occlusion mode" turns on after > 30 evaluated frames with ≥ 50 % occluded and off after 15 clean frames. Pose: `IsFrontal` = |yaw| ≤ 20° and |pitch| ≤ 25° (`RetinaEngine::ComputeOrientation` FaceProcessorCore 0x18003d6f0, flags +12..15; which Retina output is yaw/pitch: F59); subtraction, liveness crop and prints need frontal faces. | ENG 0x1800380cc, 0x1800af158, 0x18002fb90; FaceProcessorCore 0x18003d6f0, 0x18001a404 |
| F47 | OBSERVED | **Track state machine** (`TrackedFace+416`): 0 none/lost → 1 identifying (`OnNewTracking`) → **6 recognised, liveness pending** (`sub_18002D8B8` when a user is attached) → **2 recognised + live** (`UpdateAntiSpoofingStatus` with final decision 1) or 5 guest (`IdentifyAsGuest`: not recognised, spoof, or occluded); 3 enrolling, 4 enrolment committed. | ENG 0x18002d8b8, 0x1800a8ce4, 0x1800a8384 |
| F48 | OBSERVED | **Unlock = recognition then liveness on the same track.** `MakeDecisions` (0x180047600): a track in state 1 is recognised when it has **≥ max(4, engine+0x3CF94) prints** (model slot 0x48 → param +8 = **4** for the v5 block) and best GFP < target FAR 1e-5, and no other tracked face matches the same user better → state 6. Liveness votes are cast on every clear frame but only **counted from the frame the face is matched** (`UpdateInformation`/`AggregateDecision` count only when `isMatchedToUser`); in state 6 each cycle reads the anti-spoofing decision: 1 (real) → state 2 (success, presence reported), 0 → guest, 2 → keep waiting. So success needs ≥ 4 prints and then ≥ 2 live frames after recognition; reject after 3 spoof frames, or after 48 matched frames / > 16 frames of a second counter without a decision (F25). | ENG 0x180047600, 0x180028e10, 0x18003a654, 0x1800af158 |
| F49 | OBSERVED | **Re-attempt instead of reject** (`sub_1800575E0`, called when a track would become guest and engine flag +0x3CF8D is set, once per track): if the face moved (normalised centre range: ((Δx)² + (Δy)²)/2 > 0.01) or its size changed by > 20 %, or the σ of the last 10 face-brightness values > 12, the track is given another try (telemetry `ReAttemptRecognitionByMovement` / `…ByExposure`); not if the "too many saturated" counter > 1. | ENG 0x1800575e0, 0x18003878c, 0x1800386a4 |
| F50 | OBSERVED | **Distance metric = cosine.** The v5 model loader picks the distance function by the last layer name: `"joint_bayesian"` → `sub_1800AF9E0`, else `sub_180054190` (0x180033fe0). `sub_180054190` = `1 + (−cos(a,b))` from `sub_1800240F0` (dot / √(‖a‖²‖b‖²)), **clamped to ≤ 1.0**, FLT_MAX on FP error. So distance = 1 − cos θ ∈ [0, 1]. | ENG 0x180054190, 0x1800240f0, 0x180033fe0 |
| F51 | OBSERVED | **Distance → GFP curve** (`sub_1800397B4`, reached via model vtable slot 7 `sub_180039210`): a per-hardware-model table of (FPR, TAR, distance) triples. For d ≤ d₀: `GFP = FPR₀ · exp((d − d₀)/|d₀|)`; otherwise linear interpolation of FPR between the bracketing table distances (extrapolated past the last point); then × multiplier 1/2/3/4 chosen by an integer argument (1→×1, 2→×2, 3→×3, else ×4). The table lives in the header of `FaceRecognitionEngineAdapterResources_v5.dll` MODEL_FILES 123/125 (offset 27, five identical copies for hardware models 0,1,2,4,5; dumped to `re/models/v5_gfp_calibration_table.tsv`). At FAR 1e-5 the table gives **distance 0.350 (cos ≥ 0.65), TAR 0.99766**. | ENG 0x1800397b4 (disasm), 0x180039210; model header |
| F52 | INFERRED | The ×1…×4 multiplier is a multiple-comparison correction (number of conditions/users compared, capped at 4); the exact argument source was not resolved. | F51 |
| F53 | OBSERVED | **Augmentation conditions** (`AugmentUserProfileOnSuccess` 0x180057f80 + `sub_180058278`): only in track state 2 or 6 after the liveness decision is **real** (called from `MakeDecisions` right after `UpdateAntiSpoofingStatus(…, 1)`), at most once per track (flag +0x6A0F2); the candidate print must be the newest print of the matched user with **GFP in [targetFAR/2, targetFAR] = [5e-6, 1e-5]** (borderline matches only), not ambiguous (no second user under target), condition ≠ 4, plain print category; source generation < 3. New print: flags \|= 0x400, generation + 1. `sub_1800A2E94`: if the user's augmented-print count ≥ a model limit, the **oldest 0x400 print is evicted** (FIFO), then append. Registration data is also committed on the fly (`EnrolledUser::CommitRegistrationData`). | ENG 0x180057f80, 0x180058278, 0x1800a2e94, 0x180047600 |
| F54 | OBSERVED | v5 model parameter block (`sub_1800AF158`, tech id 3 block used for v5): +8 = 4 (min prints to identify), +0xC = 16, +0x10 = 100, +0x40 = 72 (min face px), +0x44 = 0.12 (min face/frame ratio), +0x60/+0x64/+0x68 = 20.0/25.0/30.0 (INFERRED pose or angle limits), +0x30/+0x34 = 57/60 class values. Print ring per track = 6 × 16 = 96 (`OnNewFaceTracked` passes `6·slot0x50`). INFERRED: +0xC = 16 prints needed per enrolment, +0x10 = 100 = max prints per user. | ENG 0x1800af158, vtable 0x1800d4bb0 |
| F55 | OBSERVED | **wbiosrvc.dll** (public PDB): reads `CaptureIntervalInMilliseconds` (values 1–19 are raised to 20 ms; face = 125) and `PresenceLifetimeInSeconds` (default 5 s if absent; face = 0) in `CSensorConfig::CSensorConfig`. Face runs as a **presence monitor**: `CPresenceAutoUpdateOperation::CaptureAndUpdate` loops StartCapture → wait → FinishCapture → `PushDataToEngine` and posts presence changes to client sessions; `CIdentifyAutoCaptureOperation` likewise. No face-specific attempt timeout or retry count exists in the service; waits are event-driven. `PresenceStabilizationThreshold` is not read by wbiosrvc nor by any analysed face DLL (reader UNKNOWN). | wbiosrvc 0x18001bdc5, 0x18000e05c, 0x180022370 |
| F56 | OBSERVED | **Client-side timing** (facecredentialprovider.dll): `TimeToRequireConfirmationBeforeAutoLogonMilliSeconds` default **120000 ms** (`sub_180059060`); policy `HKLM\SOFTWARE\Policies\Microsoft\Biometrics\Credential Provider\SwitchTimeoutInSeconds` default **10 s** (`sub_180049940`); a `CredProvTimeout` value exists (default not resolved). Notification intervals: `IMPROVERECOGNITIONNOTIFICATION…`, `LIVENESSSUGGESTIONNOTIFICATION…`, `GETHELPNOTIFICATION…` (days). | facecredentialprovider 0x180059060, 0x180049940; wide strings |
| F57 | INFERRED | Overall verify time on Windows is therefore bounded by the UI (credential provider / LogonUI), not by WinBio: the engine keeps tracking until success, guest (not recognised after re-attempts, spoof, occlusion) or the UI stops the session. | F55, F56 |
| F58 | OBSERVED (converter agent, `docs/winhello-models/lbf.md`) | `facerecognitionsensoradapterresources.dll` MODEL_FILES 102 is the **LbfAligner database** (0xFACE container v10); 104–107 are **IrFaceOrientation** head-pose models `PITCH_KNN`, `PITCH_LDA`, `YAW_KNN`, `YAW_LDA` (LBP feature → LDA → KNN over 91 classes −45…45°), loaded by `FaceProcessor::InitializeLegacyFaceAlignmentAndFaceOrientationIfNeeded` (FaceProcessorCore 0x18003a954). The F13 third anchor group is the **mouth** (points 3, 4, 23–26), not nose/mouth; RetinaFace's 5 points fill slots 0–4 of the 31-slot landmark array. **On the RetinaEngine path the LBF aligner never runs** (it is created only when no Retina alignment is installed, 0x18003a954; `IsRetinaFaceAlignmentEnabled` 0x18002f72c is forced by feature `IntegrateComplianceHelloModels`): `RetinaEngine::ComputeAlignment` 0x18003d270 fills slots 5–30 by copying the five points (eye slots = eye centres, nose slots = nose tip, mouth slots = mouth midpoint; with the compliance flag the eye corners sit ±0.1875·IOD along the eye line), so the F43 block is bounded by the eye centres and mouth corners and the F13 anchors are the eye centres and the mouth midpoint (lbf.md "Which aligner runs"). | lbf.md |
| F59 | OBSERVED (lbf.md; RetinaEngine part: pose-port agent, ldmf.md §7) | Frontal flags (FaceOrientationResult +12..15): yaw > 20, yaw < −20, pitch > 25, pitch < −25 → not frontal. Supersedes the A/B naming in F46. **RetinaEngine path:** `ProcessResultCandidates` (0x180005c3c, call at 0x18000615b) stores the three `DecodeRotationVec` outputs at candidate +0x38, +0x3C, +0x40; `ComputeOrientation` (0x18003d6f0, loads at 0x18003d710–0x18003d721) reads +0x38 as **pitch** (result +0, flags +14/+15 against ±25) = rotation about x `atan2(M₁₂, M₂₂)`, +0x3C as **yaw** (+4, flags +12/+13 against ±20) = rotation about y `asin(M₀₂)`, +0x40 as roll (+8); the frontal margin (+16) is 0 (no `EstimateFaceFrontalFromAlignmentPoints` on this path). | lbf.md, ldmf.md; FaceProcessorCore 0x18003d6f0, 0x180005c3c, 0x18001a8a8 |
| F60 | OBSERVED (converter agent, `docs/winhello-models/cefa.md`) | `.cefa` = **plaintext** 0xFACE container (not encrypted): 10-byte header, then 5 per-hardware-model FAR calibration tables (FPR, TAR, distance), then the payload. v5 (tech 11) payload = LDMF CNN body; v1–v4 carry only ORT "feature converter" MLPs. All five tables in a file are identical; FAR 1e-5 ↔ distance **0.350** (1 − cos) for v5. Confirms F50/F51 and closes the F29 item. | cefa.md |
| F61 | OBSERVED (cefa.md) | `HelloFaceCipher::EncryptDecrypt` (FaceProcessorCore 0x18001b104) is a fixed **17-byte-key XOR** (plus byte index). It hides the detector ORT models (`FaceDetectorResources` 110–113) and the occlusion classifier (`…resourcescore` 120; 106 is its plain predecessor). Not cryptographic protection. | cefa.md |
| F62 | OBSERVED (converter agent, `docs/winhello-models/convnet.md`) | Anti-spoofing resource → slot map (loader `sub_18009D320`; a feature flag picks the first or second id): 115/121 → 11 general IR, 116/122 → 12 diffusion IR, 117/123 → **13 color (BGR input)**, 118/124 → 14 border, 119/125 → 15 HH. Slots 11/12 carry a 2×512 cosine head (row-norm scale 30 with the flag off, 1 with it on) appended after the conv_net body. | convnet.md |

## Q1 Call flow and vtables

**Status: answered (static).**

```
LogonUI / Windows Hello UI
  └─ WinBio service (wbiosrvc.dll, svchost)            ── config 0 (no VBS) ──────────────────────────
       ├─ Sensor adapter  FaceRecognitionSensorAdapter.dll  (WbioQuerySensorInterface)
       │     └─ FaceProcessor.dll  PreprocessorRuntime + SensorInputManagerFS
       │           └─ WinRT MediaCapture/MediaFrameReader → Frame Server → Realtek DMFT (RsDMFTIR64) → usbvideo.sys
       │           └─ FaceProcessorCore.dll InfraredFaceProcessor (detect, align, ambient subtraction)
       │           └─ FaceTrackerInternal.dll (tracking), FaceDetectorResources.dll (detector models)
       │     PushDataToEngine → BioDataComposer → Engine.AcceptSampleData(preprocessed faces)
       ├─ Engine adapter  FaceRecognitionEngineAdapter.dll (WbioQueryEngineInterface)
       │     FaceRecognition (embedding + GFP decision) + AntiSpoofingDecisionEngine (CNN liveness)
       │     models: FaceRecognitionEngineAdapterResources_v5.dll, …resourcessecure.dll, …resourcescore.dll
       └─ Storage adapter WinBioStorageAdapter.dll  (AES-CBC + DPAPI, F33/F34)
                                                         ── config 1 (VBS) ─────────────────────────────
  VTL0: FaceRecognitionSensorAdapterVsm.dll (capture, colour faces) ─AsyncImportRaw/SecureBuffer→
  VTL1 BioIso.exe: FaceRecognitionSensorAdapterVsmSecure.dll (IR processing, FaceProcessorCore)
                   → FaceRecognitionEngineAdapter.dll → WinBioVsmCacheAdapter (built into BioIso)
  VTL0 engine slot = WinBioPassthroughAdapter, VTL0 storage = winbiovsmstorageadapter.dll
```

HelloFace.dll is only the root-enumerated UMDF "software device" that gives WinBio a biometric unit to
attach these adapters to (F3). It does no image work.

`WINBIO_ENGINE_INTERFACE` (FaceRecognitionEngineAdapter.dll 0x180121110, F20). Slot order follows
`winbio_adapter.h` (verified for 4 slots by callee names):

| # | Slot | Function | Notes |
|---|---|---|---|
| 1 | Attach | 0x1800750a0 | |
| 2 | Detach | 0x18004f0f0 | |
| 3 | ClearContext | 0x180019c10 | calls `EngineAdapter::RefreshCache` |
| 4–8 | QueryPreferredFormat, QueryIndexVectorSize, QueryHashAlgorithms, SetHashAlgorithm, QuerySampleHint | 0x1800766f0, 0x1800765f0, 0x180076540, 0x180076f00, 0x180076800 | thin |
| 9 | **AcceptSampleData** | 0x18001b2f0 | receives `BioDataComposer` output from the sensor adapter |
| 10 | ExportEngineData | 0x180075b70 | |
| 11 | VerifyFeatureSet | 0x1800771f0 | |
| 12 | IdentifyFeatureSet | 0x180075f00 | |
| 13–17 | CreateEnrollment, UpdateEnrollment, GetEnrollmentStatus, GetEnrollmentHash, CheckForDuplicate | 0x1800757b0, 0x180076fb0, 0x180075cc0, 0x180075c10, 0x1800752a0 | |
| 18 | CommitEnrollment | 0x1800753b0 | → `EngineAdapter::CommitEnrollment` 0x18006bfa4 |
| 19 | DiscardEnrollment | 0x180075950 | |
| 20/21 | ControlUnit / ControlUnitPrivileged | 0x1800755d0 / 0x180075690 | → `EngineAdapter::ControlUnitPrivileged` 0x18006cbb0 |
| 22 | NotifyPowerChange | 0x180055860 | |
| 23 | Reserved_1 | NULL | |
| 24/25 | PipelineInit / PipelineCleanup | 0x180076150 / 0x180076060 | |
| 26/27 | Activate / Deactivate | 0x1800430c0 / 0x180040e30 | |
| 28 | QueryExtendedInfo | 0x180040b50 | |
| 29 | **IdentifyAll** | 0x18002ccf0 | |
| 30–35 | SetEnrollmentSelector, SetEnrollmentParameters, QueryExtendedEnrollmentStatus, RefreshCache, SelectCalibrationFormat, QueryCalibrationData | 0x180076ce0 … 0x180076340 | RefreshCache → `EngineAdapter::RefreshCache` 0x18001c324 |
| 36 | SetAccountPolicy | 0x18002d550 | |
| 37 | CreateKey | 0x1800758a0 | |
| 38 | IdentifyFeatureSetSecure | 0x180075fb0 | |
| 39 | AcceptPrivateSensorTypeInfo | 0x180074fa0 | → `EngineAdapter::AcceptPrivateSensorTypeInfo` 0x18006bc8c |
| 40/41 | CreateEnrollmentAuthenticated / IdentifyFeatureSetAuthenticated | NULL | |

`WINBIO_SENSOR_INTERFACE` (FaceRecognitionSensorAdapterVsm.dll, public symbols): every slot is a thin
`Windows::Analog::Face::SensorAdapter<Slot>` export wrapper (0x180015880–0x180015ef0) → `SensorAdapter::<Slot>`
→ `SensorAdapterBase` → `SensorAdapterProductWorker[Vsm]`. The relevant ones: `Attach` (creates the worker,
`ReadOneSettings`, `CheckIfSensorAvailable`), `Activate`, `StartCapture`/`FinishCapture`,
`PushDataToEngine` (F32), `AsyncImportRawBuffer`/`AsyncImportSecureBuffer` (VBS hand-over, F35),
`ControlUnitPrivileged`, `QueryPrivateSensorType` (sends `ExtractEnhancedAntiSpoofingProperty` info that the
engine reads in `AcceptPrivateSensorTypeInfo`). Storage: the engine only uses the WinBio storage API (F33).
UNKNOWN: the exact order WinBio calls IdentifyAll vs AcceptSampleData during logon (lives in wbiosrvc.dll, not analysed).

## Q2 Frame acquisition

**Status: answered for Windows' side; the per-frame flag source is inferred (F19).**

1. Device: the IR camera is a `KSCATEGORY_SENSOR_CAMERA` hidden from apps (F4); FaceProcessor finds the
   source group via `SensorEnumeration` and opens it with WinRT `MediaCapture` (sets `MemoryPreference`, watches exclusive-control status) and the **FaceAuth video profile** `{81361B22-…}` (F6). Format = first supported format,
   warn if < 7.5 fps or < 340×340 (F7).
2. Mode: `FACEAUTH_MODE` (KS id 35) is set to **2 = ALTERNATIVE_FRAME_ILLUMINATION** when the driver
   advertises it; the host then does ambient subtraction. If the driver only offers **4 = BACKGROUND_SUBTRACTION**
   the driver delivers clean frames. Otherwise face mode fails (F9).
3. Per frame: `IsIlluminated` (from `MF_CAPTURE_METADATA_FRAME_ILLUMINATION`) tags lit vs ambient, plus
   `SystemRelativeTime` (F10). Brightness-based guessing (`FrameStrobe`) exists only for file replay (F15).
4. Hand-off: one slot per stream, newest frame wins (F11).
5. Pairing: ambient frame → crop around last known faces; next lit frame → detect faces, then subtract the
   stored ambient crop if it is < 300 ms older, frontal, same track id; affine motion compensation from eye/
   nose-mouth anchors; plain `lit − min(lit, ambient)` when the lighting status is good (F12–F14).
6. Quality gates: lit-minus-ambient face brightness must be ≥ 45 (status 1; > 205 = status 2 "too bright");
   ambient face brightness > 25 downgrades to status 4 (strong ambient IR, e.g. sun) (F14).
7. Frames per attempt / timeouts: the WinBio configuration asks for a capture every
   `CaptureIntervalInMilliseconds = 125` ms with `PresenceStabilizationThreshold = 300` (F2). Each tracked face
   keeps a ring of ≤ 5 processed frames, stale after 1 s (F37). Anti-spoofing decides after 2 live frames /
   3 spoof frames / 48 frames (F25). UNKNOWN: overall attempt timeout and retry count (driven by wbiosrvc /
   LogonUI; the engine logs `ReAttemptRecognitionByExposure` / `ReAttemptRecognitionByMovement`).

## Q3 IR emitter control

**Status: answered.**

- Windows Hello never drives the emitter directly. It sets **`KSPROPERTY_CAMERACONTROL_EXTENDED_FACEAUTH_MODE`
  (property set `KSPROPERTYSETID_ExtendedCameraControl {1CB79112-C0D2-4213-9CA6-CD4FDB927972}`, id 35)**
  through `MediaFrameSourceController.SetPropertyAsync("{1CB79112-…} 35")`, value flags 2 (alternate frame
  illumination), 4 (driver background subtraction) or 1 (disabled) (F8, F9).
- It also uses id 36 `SECURE_MODE` (VBS secure sensor) and id 29 `FACEDETECTION` (driver face detection, if
  supported). It never uses id 38 `IRTORCHMODE` (F8).
- The Realtek DMFT passes `FACEAUTH_MODE` through to usbvideo.sys when the driver supports it, and on the
  first 640×360@15 sample even pushes `FACEAUTH_MODE=2` down itself (F16, F17). The UVC-level control is,
  per Microsoft's public UVC 1.5 extension spec, the **Microsoft XU {0F3F95DC-2632-4C4E-92C9-A04782F43BC8},
  face-authentication control** (INFERRED, F19 — selector not seen in these binaries).
- Realtek's own XU controls `RTK_EXT_LED_TORCH_CTL` / `RTK_EXT_MIRAMATRIX_IR_LED_CTL` (control 14) are only for
  listed eye-tracker ("Mirametrix") models, not 30C9:00F3 (`IRController` ctor 0x18000ac80 checks VID_04CA&PID_706C,
  VID_04F2&PID_B5EA, or a working control 14).

## Q4 Anti-spoofing / liveness

**Status: answered for structure and thresholds; input images for "general" vs "diffusion" IR are partially resolved.**

- 5 classifiers (F22): general IR CNN v2, diffusion IR CNN v2, color CNN v2, multi-task IR border, multi-task IR "HH".
  Default session: the first three on when anti-spoofing is enabled; multi-task off.
- Inputs: IR classifiers take a **128×128 single-channel face crop** produced by `sub_180015F68` from the
  processed IR face (landmark-aligned); color takes a 128×128×3 crop of the matching face in the RGB stream
  (`FindCorrespondingFacesInColor`) (F23). The nets are SqueezeNet-style `conv_net_v1` models (F30).
  Resolved in F42: "general IR" runs on the plain lit crop, "diffusion IR" on the lit − ambient difference crop;
  each frame uses exactly one of them.
- Decision (F24, F25): IR nets vote live when `P(real) > 0.5` (mode byte 0) or `> 0.9` (mode byte 1), vote spoof
  when `P(spoof) > 0.5` with mode byte 1 (never with mode byte 0). INFERRED: mode 1 is the stricter
  "enhanced anti-spoofing" setting. Color (0.5), border (0.7) and HH (0.5)
  can only veto. Session: 2 live frames → accept; 3 spoof frames → reject; 48 frames → reject.
- When the anti-spoofing policy is on but no model is enabled, the result is spoof (fail closed).

## Q5 Recognition

**Status: mostly answered.**

- Detection/alignment run before the engine: FaceTrackerInternal + RetinaEngine (`RetinaEngine::ProcessNextFrame`
  reads `landmarks`/`confidence` outputs; ONNX via `OnnxTester` behind feature `OnnxInfraredFaceDetector`) or
  the legacy LBF aligner (`LbfAligner`) in FaceProcessor/FaceProcessorCore; one of the two is the processor's
  single `IFaceAlignment`, and on the Retina path the 31 points are the detector's 5 plus copies (F58, lbf.md).
  Landmarks give 5 left-eye, 5 right-eye and 6 mouth points (used for anchors, F13). Head pose: the Retina detector's 6-DoF output
  (`RetinaEngine::ComputeOrientation`, F59) or, on the legacy path, `IrFaceOrientation` (LBP + LDA regressor); only
  frontal faces are used for subtraction and liveness.
- Embedding: 128×128×1 IR crop → Inception-style CNN → ~256-float vector (F30).
- Matching: per user, the minimum distance over all stored prints (+ coupled prints) → GFP (estimated false-
  positive probability) → accept if GFP < target FAR **1e-5** (F27–F29). Matching is 1:N over enrolled users
  (≤ 32 detail slots), and `MakeDecisions` also checks the face is not closer to another tracked user.
- Template update: on success add a print (flag 0x400, ≤ 3 generations); on failure-then-fallback add a print
  (flag 0x4) (F31). `Registration` code (`EvaluateRegistration`, `GetAreaOfOverlap`) tracks per-device
  "registrations" (hardware model), UNKNOWN detail.

## Q6 Template storage and protection

**Status: answered at component level.**

- Normal mode: engine → WinBio storage API → `WinBioStorageAdapter.dll`, database id
  `DC576DA6-D676-4A15-906D-C0CEAF949543` (F1). Blob encryption AES-CBC (inferred) with key protected by DPAPI;
  SHA-256 present (F33, F34).
- VBS mode: matching and templates live in VTL1 (`BioIso.exe`); sealing with IUM `EncryptData`; a VTL1 match
  gates `AuthorizeKeyRelease` for the NGC key (F35, F36). DatabaseId `A61A7480-6A54-4D36-A0EF-B150B8545827`.
- TPM: not touched by the face DLLs. The TPM protects the NGC key that the face match unlocks (Windows Hello
  key flow, not analysed here).

## Q7 Consequences for linux_hello

Compared with `README.md` (IR SCRFD + ArcFace, RGB liveness, brighter-frame-of-pair, cosine ≥ 0.4, JSON store):

1. **Use the ambient frame instead of dropping it.** Windows subtracts the emitter-off frame from the emitter-on
   frame on the face crop (F12–F14). Do the same: pair consecutive frames (< 300 ms apart), compute
   `lit − min(lit, ambient)` on the face box (motion is small at 15–30 fps; Windows warps with an affine from
   eye/nose anchors), and run recognition on the result. This removes sunlight/IR ambient and makes
   lighting consistent between enrolment and auth.
2. **Add an IR lighting gate.** Reject frames where mean face brightness (lit − ambient) < 45 (out of 255), or
   ambient face brightness > 25, or lit − ambient > 205 (saturation) (F14). This is also a cheap liveness cue:
   a phone screen or a print under the emitter does not reflect 850/940 nm the way skin does, and screens give
   no IR at all.
3. **Liveness on IR, not only RGB.** Windows' primary liveness votes come from two IR CNNs on 128×128 crops;
   RGB is a veto (F22–F25). linux_hello currently has no IR liveness. A small IR anti-spoof model (or at least
   the lighting gate above plus a lit/ambient difference check) should be primary, RGB secondary.
4. **Copy the vote logic.** Require ≥ 2 frames judged live before accepting, reject after 3 spoof frames, give up
   after ~48 frames (F25). Fail closed if liveness is enabled but no model loaded.
5. **Threshold by calibrated FAR, not a raw cosine.** Windows targets FAR = 1e-5 and converts distance to an
   estimated false-positive rate (F27–F29). For linux_hello: measure an impostor score distribution (e.g. LFW or
   a few thousand IR faces) for the chosen ArcFace model and set the cosine threshold at the 1e-5 point; 0.4 is
   probably too permissive for an unlock with no PIN in front of it.
6. **Store several prints, match by minimum distance.** Do not average the enrolment frames into one vector
   (`enroll` "averages 5 IR frames"); keep each print and take the minimum distance (F28). Add
   enroll-on-success with a generation cap (≤ 3) to follow appearance drift (F31).
7. **Emitter control on Linux.** *(Checked: see Linux verification L1–L5. No MSXU face-auth control on this camera; the per-frame illumination flag is available on `/dev/video3`.)* Windows asks the camera for alternate-frame illumination through the MS UVC XU
   face-auth control (F19). On this camera the emitter already alternates without setup (README), which
   suggests the firmware defaults to that mode. Next step: check `lsusb -v -d 30c9:00f3` for an extension unit
   `{0F3F95DC-2632-4C4E-92C9-A04782F43BC8}` and query it with `UVCIOC_CTRL_QUERY`; and check whether the uvcvideo
   metadata node (`V4L2_META_FMT_UVC`) carries the MS `MetadataId_FrameIllumination` item, which would give the
   lit/unlit flag per frame instead of guessing by brightness.
8. **Template protection.** *(Done: optional TPM sealing, see [tpm.md](tpm.md).)* Windows encrypts templates (DPAPI/AES in normal mode, VTL1 sealing with VBS) and the
   match only releases a key. linux_hello stores plain JSON. A TPM-sealed key (e.g. `tpm2` seal to PCRs, or
   systemd-creds) encrypting the store, and running matching in the root daemon only, would be the closest analogue.
9. **Input resolution.** Windows' recognition and liveness nets use 128×128 single-channel crops, and it only
   warns below 340 px / 7.5 fps (F7, F30). The Realtek DMFT special-cases a 640×360 @ 15 fps IR mode (F17),
   so a low-resolution IR stream is the expected input.

## Linux verification on the P16 Gen 3 (runtime, 2026-10-15)

Checked on the actual camera (USB `30c9:00f3`) under Linux `uvcvideo`. These are OBSERVED at
runtime, unlike the static findings above.

| ID | Kind | Finding | Evidence |
|----|------|---------|----------|
| L1 | OBSERVED | The IR function strobes the emitter on alternate frames with **no control set** (odd frames lit, even frames dark; exposure settles in ≈11 frames from a cold start). Consistent with F17: Realtek's DMFT turns on alternate-frame illumination exactly for the 640×360@15 IR stream, which is the only IR mode this camera offers. | 45-frame capture, per-frame mean brightness |
| L2 | OBSERVED | The camera emits Microsoft standard metadata: `/dev/video3` offers format `UVCM` (UVC MSXU metadata). While `/dev/video2` streams, every frame carries item **id 6 = `MetadataId_FrameIllumination`** (size 16: `Flags`, `Reserved`), plus a vendor item `0x80000004` (size 12). | `v4l2-ctl -d /dev/video3 --set-fmt-meta=UVCM`; 40 records parsed as `uvc_meta_buf` + `KSCAMERA_METADATA_ITEMHEADER` items |
| L3 | OBSERVED | `Flags & 1` (`KSCAMERA_METADATA_FRAMEILLUMINATION_FLAG_ON`) matches the lit frames exactly: 35/35 after warm-up (lit mean 66–95, unlit ≈0). This is the per-frame "IsIlluminated" signal of F19, available on Linux. | flag vs. frame brightness for 40 frames |
| L4 | OBSERVED | The Microsoft XU `{0F3F95DC-2632-4C4E-92C9-A04782F43BC8}` (unit 8) sits on the **RGB** VideoControl interface and advertises `bmControls 06 21 00 00` = D1, D2, D8, D13 = EXPOSURE (0x02), EVCOMPENSATION (0x03), METADATA (0x09), FRAMERATE_THROTTLE (0x0E). It does **not** advertise FACE_AUTHENTICATION (0x06, D5) or IR_TORCH (0x0A, D9). The IR function's only XU (unit 11, `{45b5da73-23c1-4a3d-a368-610f078c4397}`) has no controls. | `lsusb -v -d 30c9:00f3`; selector table from Microsoft "UVC extensions 1.5" §2.2.2 |
| L5 | INFERRED | On this camera, Windows' `FACEAUTH_MODE = ALTERNATIVE_FRAME_ILLUMINATION` (F16/F17) cannot reach an MSXU face-auth control because none is advertised; alternate-frame illumination appears to be the IR function's built-in behaviour, and Windows tells lit from unlit frames through the L2 metadata. | L1, L2, L4 |

Consequence for linux_hello: read `MetadataId_FrameIllumination` from `/dev/video3` to pair lit and
ambient frames instead of comparing brightness, then do the ambient subtraction and lighting gate of
Q2/Q7.

## Open items (UNKNOWN)

- usbvideo.sys / Frame Server mapping of MS XU + metadata to `FACEAUTH_MODE` and `IsIlluminated` (not analysed).
- ~~Distance metric and GFP curve~~ resolved (F50, F51). ~~"Diffusion IR" input~~ resolved (F42).
- wbiosrvc.dll capture loop timeouts / retry counts.
- `cefa0a00` container format of `…Resources_v4/_v5` (partly encrypted; `HelloFaceCipher::EncryptDecrypt` exists in FaceProcessor/Core).

## Raw dump index

All under `/home/cole/SiteResearch/windows_hello/re/`:

| Path | Content |
|---|---|
| `bin/*.dll.i64` | IDA 9.5 databases (engine DB has the recovered names) |
| `pdb/` | Microsoft public PDBs fetched by GUID |
| `<Dll>/functions.txt`, `names.txt`, `imports.txt`, `strings.txt`, `wstrings.txt`, `pseudocode.c` | breadth-first dumps per DLL |
| `FaceRecognitionEngineAdapter/namemap.json`, `pseudocode_named.c`, `focus/*.c` | engine names from telemetry strings; key functions |
| `FaceProcessorCore/focus/*.c` | ambient-subtraction functions |
| `FaceProcessor/PopulateInfraredFrameProperties_FS.c`, `SetInfraredMediaFormat.c` | capture path |
| `models/<ResourceDll>/MODEL_FILES_*.bin` | extracted model resources (conv_net_v1, ORT, ONNX, LDMF, cefa containers) |
| `scripts/` | `dump.py` (idalib breadth dump), `q.py` (ad-hoc idalib query), `peinfo.py`, `wstrings.py`, `extract_res.py` |

Other model resources seen (not analysed further): `FaceDetectorResources.dll` 101/102 `LDMF` "FaceSDK Team" "Infrared"
detector models and 110–113 (encrypted/opaque); `…resourcescore.dll` 105 (ORT flatbuffer `ORTM`), 106 (ONNX from
pytorch), 103/120 (opaque); `facerecognitionsensoradapterresources.dll` 104/106 landmark tables (90/120 points),
105/107 float tables (LBF aligner), 102 `cefa` container.


## Follow-up pass: implementable parameters (F38–F62)

| Area | Windows behaviour | Value | Finding |
|---|---|---|---|
| Detection input | lit IR frame (not the difference) | — | F12, F38 |
| Recognition crop | 3 × face box, centred, max 400 px, 8-bit luminance of the lit frame | 3×, ≤ 400 px | F38 |
| Ambient crop | previous face crop grown by 100 px per side, from the unlit frame | +100 px | F39 |
| Pairing window | ambient older than lit by < 300 ms, same track, both frontal | 300 ms | F13 |
| Recognition image | lit − min(lit, warpedAmbient) (affine from eye/eye/mouth anchors), only if lighting status = 1; otherwise the lit crop unchanged | — | F40 |
| Liveness image | lit − min(lit, ambient) without warping, only if status = 1 and ambient ≤ 25; otherwise the lit crop | — | F40 |
| Liveness model choice | difference crop → "diffusion IR" net; plain lit crop → "general IR" net | — | F42 |
| Face brightness | mean of 2×2-subsampled pixels in the box between eye contours and mouth corners | 0–255 | F43 |
| Exposure status | lit > 205 → 2 (too bright/close); lit < 35 → 3 (too dark/far); clipped-pixel count > 1 % of face ellipse → 6; around face → 7 | 205 / 35 | F44 |
| Lighting status | lit − ambient ≥ 45 → good (> 205 → 2); < 45 → 4; ambient > 25 → 4 | 45 / 205 / 25 | F14, F44 |
| Status effect | 2, 3, 5, 6 → frame rejected (no print, no liveness); 4, 7 → print kept but tagged | — | F45 |
| Min face size | box width ≥ 72 px and ≥ 0.12 × min(frame w, h) | 72 px / 0.12 | F46, F54 |
| Edge cut | face box ≥ inter-ocular / 5 from the frame border | d/5 | F46 |
| Pose | frontal iff |yaw| ≤ 20°, |pitch| ≤ 25° | 20° / 25° | F59 |
| Occlusion | eyes / mouth-nose / face occluded → reject; 4 consecutive occluded frames while identifying → give up | 4 | F46 |
| Min prints before match | ≥ 4 clear frames (prints) on the track | 4 | F48, F54 |
| Distance | 1 − cos(a, b), clamped to ≤ 1 | — | F50 |
| Distance → FAR | table interpolation (exp below the first point), × 1…4 multiplier | table | F51 |
| Accept | estimated FAR < 1e-5 (Windows table: distance 0.350 ⇔ cos ≥ 0.65 for its own model) | 1e-5 | F27, F51 |
| Order | recognise first (state 6), then liveness votes counted from that frame | — | F48 |
| Liveness votes | IR nets: live +1, spoof −1; colour/multi-task: veto only | — | F24, F25 |
| Liveness decision | ≥ 2 live frames → accept; ≥ 3 spoof frames → reject; 48 matched frames without decision → reject | 2 / 3 / 48 | F25, F48 |
| Re-attempt | if not recognised but face moved (centre range > ~0.14 of frame or size ±20 %) or brightness σ > 12, retry once instead of rejecting | 0.01 / 0.2 / 12 | F49 |
| Augment | only after a live success; candidate GFP in [FAR/2, FAR]; once per session; generation < 3; FIFO eviction of oldest augmented print at a cap | 5e-6…1e-5, 3 | F53 |
| Capture rate | WinBio capture interval 125 ms (≥ 20 ms floor) | 125 ms | F2, F55 |
| Overall timeout | none in WinBio; UI-driven. Credential provider: auto-logon confirmation after 120 s on the lock screen; switch timeout 10 s | 120 s / 10 s | F55–F57 |

Remaining UNKNOWN: who reads `PresenceStabilizationThreshold` (300); the multiplier argument in F51/F52;
the user-level augmented-print cap and enrolment print count (INFERRED 16 / 100 from the param block, F54);
`CredProvTimeout` default; where augmented prints are flushed to the WinBio store (StorageManager is reached
only through a vtable).
