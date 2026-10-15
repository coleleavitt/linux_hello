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
| 4 | Anti-spoofing / liveness | answered (diffusion-IR input unknown) |
| 5 | Recognition (detect, align, embed, match, thresholds) | mostly answered (metric/curve unknown) |
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
| F13 | OBSERVED | Subtraction rules (`AmbientLightSubtracterBase::AdjustFoundFacesForAmbientLight` 0x180036e78): only when the ambient crop is older than the lit frame by **< 3,000,000 ×100 ns = 300 ms**, the face is frontal in both frames, and matched by tracking id. Motion compensation: `ComputeAnchorPoints` (0x1800372c8) makes 3 anchors (mean of 5 left-eye landmarks, 5 right-eye landmarks, 6 nose/mouth landmarks); `AdjustFaceForAmbientLight` (0x180036c10) interpolates anchors by timestamp ratio and builds an affine transform (`DeriveAffineTransformationOfFacePoints`), then `RemoveAmbientLightFromFace` warps and subtracts. If lighting status is 1 ("good") and the face-ambient metric ≤ 25, it also does a direct per-pixel saturating subtraction `lit -= min(lit, ambient)` on the face crop. | FaceProcessorCore 0x180036e78, 0x180036c10, 0x1800372c8 |
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
  UNKNOWN: whether "diffusion IR" means the ambient-subtracted image, the raw lit image, or something else; both IR
  slots call the same function with a different model.
- Decision (F24, F25): IR nets vote live when `P(real) > 0.5` (mode byte 0) or `> 0.9` (mode byte 1), vote spoof
  when `P(spoof) > 0.5` with mode byte 1 (never with mode byte 0). INFERRED: mode 1 is the stricter
  "enhanced anti-spoofing" setting. Color (0.5), border (0.7) and HH (0.5)
  can only veto. Session: 2 live frames → accept; 3 spoof frames → reject; 48 frames → reject.
- When the anti-spoofing policy is on but no model is enabled, the result is spoof (fail closed).

## Q5 Recognition

**Status: mostly answered.**

- Detection/alignment run before the engine: FaceTrackerInternal + RetinaEngine (`RetinaEngine::ProcessNextFrame`
  reads `landmarks`/`confidence` outputs; ONNX via `OnnxTester` behind feature `OnnxInfraredFaceDetector`) or
  the legacy LBF aligner (`LbfAligner`) in FaceProcessor/FaceProcessorCore. Landmarks give 5 left-eye, 5 right-eye
  and 6 nose/mouth points (used for anchors, F13). Head pose: `IrFaceOrientation` (LBP + LDA regressor); only
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
- Engine distance metric (L2 vs cosine) and the distance→GFP curve (behind CFG-dispatched virtual calls; the
  model files may carry the calibration table).
- What "diffusion IR" input is.
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
