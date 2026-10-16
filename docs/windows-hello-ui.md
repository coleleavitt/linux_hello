# Windows Hello sign-in UI — what the greeter must copy

Static reverse engineering of the Windows 11 (build 26100.9444, ISO `Windows11_Client_x64_en-us_26300_9457.iso`,
install.wim image 6 "Pro") lock screen and sign-in screen, as the user sees them when Windows Hello Face is set
up, written so that `hello-greeter` (docs/greeter.md) can be built 1:1. Companion of
[windows-hello-re.md](windows-hello-re.md) (the capture/recognition side; F-ids are cited from there).

Evidence tags: **OBSERVED** = read from a binary or a resource (resource id, XAML element, function address cited);
**INFERRED** = reasoning from observed facts or from public Microsoft documentation/screenshots; **UNKNOWN** = not resolved.

Inputs (all from the ISO, see §6 for how they are extracted):

| Binary | What it is | Symbols |
|---|---|---|
| `Windows.UI.Logon.dll` 10.0.26100.x | LogonUI's XAML UI ("LogonUX"): 23 compiled XAML layouts (XBF 2.1) in `Windows.UI.Logon.pri`, the view models, the Hello animation player | public PDB (`Windows.UI.Logon.pdb 44A3092D…`) |
| `LogonUI.exe`, `credprovhost.dll`, `CredProvDataModel.dll`, `BioCredProv.dll`, `winlogon.exe`, `authui.dll`, `LockApp.exe` | host, credential-provider host, data model, fingerprint provider, winlogon, legacy UI, Windows 11 lock app | public PDBs |
| `facecredentialprovider.dll` 10.0.26100.9278 | the Windows Hello Face credential provider (state machine, timers, strings) | no PDB; ETW event names and strings identify the classes |
| `FontGlyphAnimator.dll`, `Windows.UI.BioFeedback/Fonts/NUIMDL2.ttf` | the Hello eye animation as font glyph frames + its transition table | public PDB |
| `*.dll.mui` (en-US) | string tables | — |

Raw dumps: `~/SiteResearch/windows_hello/re/ui/` (`<Binary>/functions.txt, names.txt, strings.txt, pseudocode.c`;
`xbf/*.xaml` = the decompiled layouts; `pdb/`). The XBF→XAML decompiler is Microsoft's own `WidgetSpinner`
(`dxaml/xcp/tools/XbfParser` in microsoft/microsoft-ui-xaml, MIT) with two one-line patches for types WinUI 3 deleted
(`ProgressRing`, `MediaElement`, `Control.IsTabStop`); `tools/winhello_ui/` holds the wrapper. The decompiled XAML is
Microsoft content and lives only in the git-ignored asset directory (§6).

## 1. The credential provider state machine, as the user sees it

### 1.1 Who does what

- **LogonUI.exe** hosts `Windows.UI.Logon.dll`. Its root page `LogonUX.Frame` (Frame.xaml) has the visual states
  `RequestCredential`, `RequestCredentialPicturePassword`, `Authenticating`, `AuthenticatingPicturePassword`,
  `DisplayFullScreenMessage`, `SecurityOptions`, `SecureLock`, `RequireCADSecureLock`, `SecureLockPicturePassword`,
  `UserLock`, `Shutdown` (OBSERVED, Frame.xaml). `SecureLock` is the lock screen LogonUI draws itself
  (`SecureLockScreenUserControl`); the everyday Windows 11 lock screen with Spotlight/widgets is `LockApp.exe`
  (Microsoft.LockApp, UI built in code, not analysed — its clock block is the design twin of the secure lock layout in §3.1).
- **facecredentialprovider.dll** runs inside LogonUI (credential provider `{D6886603-9D2F-4EB2-B667-1971041FA96B}`). It owns
  the face "listener" (WinBio presence monitor, F55), the timers, the texts and the *bio feedback state* it pushes to the UI
  through `ICredentialProviderCredentialEvents::OnBioFeedbackVisualStateChange(state, label, accessibilityText)`
  (`CCredProvDataModel::OnBioFeedbackVisualStateChange` 0x1800109a0 in CredProvDataModel.dll).
- **CredProvDataModel.dll** maps the provider's COM state to the WinRT `BioFeedbackState` the XAML binds to
  (`_WinRTBioFeedbackStateFromClassicCom` 0x180018d24): COM 1→2, 2→3, 3→4, 4→1, 5→5, 6→6, 7→7, 8…18 unchanged, 20→12
  (with feature `ImpB_WH_59294544` also 19→20).
- **Windows.UI.Logon.dll** `CredentialCommon::BioFeedbackViewModel::UpdateBioFeedback` (0x1800b7e50) stores the state and
  raises `State`, `Label`, `AccessibilityText`, `LottieAnimationVisibility`/`AnimationGlyphVisibility` (state 7 or an
  "interactive state disabled" setting hides the icon). `BioFeedbackUserControl` plays the eye (§1.3).

### 1.2 States (OBSERVED unless marked)

`BioFeedbackState` (WinRT enum, values from the Lottie marker switch `LottieAnimatorModel::UpdateMarker` 0x18015cf40 and the
glyph map in `BioFeedbackViewModel::BioFeedbackViewModel` 0x1800b07a0):

| Value | Meaning | Lottie segment (markers) | Glyph animation (§1.3) |
|---|---|---|---|
| 0 | None (nothing shown yet) | — | sequence 0 (none) |
| 1 | **FaceFail** — not recognised / error with a face | `FaceFailStart…FaceFailEnd` | 1 → 19 loop |
| 2 | **FaceStart** — camera on, looking | `FaceStart…FaceEnd` (entrance) | 2, then hold |
| 3 | **FaceLoop** — face seen, verifying | `FaceLoopStart…FaceLoopEnd` (looped) | look-around loop 3→8→…→18→3 |
| 4 | **FaceSuccess** — recognised | `FaceSuccessStart…FaceSuccessEnd` | 4 (eye→smile) then 7 (smile) loop |
| 5 | TooFar | `TooFarStart…TooFarEnd` | 5 loop |
| 6 | TooClose | `TooCloseStart…TooCloseEnd` | 6 loop |
| 7 | **Hidden** — icon collapsed (errors, "sign in with your PIN") | — | — |
| 8–18 | USB key, NFC, Bluetooth, **Spinner (11)**, Fingerprint (12), Phone, PhoneSend, BT2, PIN (16), Sync, SyncComp | same names | 20–35 |
| 20 | FaceLoop variant (feature flag) | `FaceLoop…` | 3 |

The face provider never emits TooFar/TooClose itself; its own text→state map (`sub_18000CA90`) only produces the COM states
2 (looking/verifying/greeting texts), 6/7 (failure texts: 6 keeps the icon, 7 hides it), 8 (PIN/password required) and
4/5 for the "improve recognition" texts; success/fail states come from the listener. (The 4/5/8 COM values map to
TooFar/TooClose/USB in the UI table above — that is what the binary does; whether those texts ever reach the UI with
the icon visible is UNKNOWN.)

**User-visible flow** (OBSERVED where a function/string is cited, otherwise INFERRED from the ETW event names of
`FaceBioListener`, `AutoLogonRestriction`, `TimeoutCredProv`, `FailureHandler`, `VisualFeedback` in facecredentialprovider.dll):

```
            key/mouse/touch, lid open, presence (F55), "unit arrival"
 LOCK SCREEN ──────────────────────────────────────────────────► SIGN-IN SCREEN (RequestCredential)
 (SecureLock / LockApp)                                            │
   face runs here too ("LockLogonUnifiedMode",                     ▼
   FaceBioListener::EnableLockLogonUnifiedMode)        state 2 FaceStart   "Looking for you..." (id 104)
   greeting at the top: "Hello, <name>! Dismiss the        │ face tracked
   lock screen to sign in." (id 113 / credprovhost 109)    ▼
                                                        state 3 FaceLoop    "Making sure it's you..." (id 123)
                                                           │                 (+ "Please look at the camera" 132,
                                                           │                  "Adjusting brightness..." 133 when it bumps the backlight)
                       ┌───────────────────────────────────┼───────────────────────────────┐
                       ▼ recognised + live                 ▼ not recognised (guest, spoof)  ▼ camera/driver/policy error
               state 4 FaceSuccess                 state 1 FaceFail                      state 7 Hidden
               "Hello <name>" (101) then           "Couldn't recognize you. Please       "Couldn't turn on the camera. Please
               winlogon "Welcome" (1002/1005)      sign in with your PIN." (125) /        sign in with your PIN." (106),
               → desktop                           PIN (52)                                "Camera not available..." (124), …
                                                   password box gets focus; the Hello    "Windows Hello is currently disabled
                                                   tile stays under "Sign-in options";   by your administrator." (121)
                                                   selecting it restarts at state 2
```

Rules and timers:

| Rule | Value | Evidence |
|---|---|---|
| Face auth is a presence monitor: frames every 125 ms, no per-attempt timeout in WinBio | F2, F55 | wbiosrvc |
| Confirmation before auto sign-in: if the lock screen has been up for longer than this, Windows does **not** sign in by itself when it recognises you; it shows the greeting and waits for a key/swipe ("Dismiss the lock screen to sign in") | `TimeToRequireConfirmationBeforeAutoLogonMilliSeconds` = **120000 ms** default (`sub_180059060`), under `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Authentication\LogonUI\FaceLogon`; `AutoLogonRestriction::IsConfirmationGreetingRequired`, `IsLockScreenSeenImmediatelyAfterTheModuleIsLoaded`, `IsFaceSeenImmediatelyAfterTheModuleIsLoaded` | OBSERVED |
| Scenarios that force the greeting+confirm instead of auto sign-in: `LockScreenDismissedAtLeastOnce`, `DisplayTurnedOffAtLeastOnce`, `LogOutScenario`, `UserSwitchScenario`, `ARSOLockScenario`, `ResumeFromHibernateOrScreenSaverScenario` (`AutoLogonRestriction::SetDisplayState`) | — | OBSERVED (names), semantics INFERRED |
| Setting "Automatically dismiss the lock screen if we recognize your face" (Settings › Accounts › Sign-in options) | registry value `AutoDismissOn`; `FaceProvider::CanAutoDismissLockScreenWhenNotRecognized`; off → the greeting is shown and a key/swipe is needed | OBSERVED (names) |
| Switch to another provider after failure: when the face fails and the user picks PIN/password, or `SwitchTimeoutInSeconds` policy (`HKLM\SOFTWARE\Policies\Microsoft\Biometrics\Credential Provider`) | **10 s** default (`sub_180049940`); `FailureHandler::SwitchProviderIfInFailedState`, `FaceCredential::ReportResult.RequireSwitchToPasswordProvider` | OBSERVED |
| Listener timeouts (`TimeoutCredProv::UpdateState` `sub_18001AA70`): the camera is switched off (`BioListenerDisabledDueToCredProvTimeout`) when nothing happens for: state 1 (after cancel) **8 s**; state 2 (presence monitor started, "looking") **20 s** (10 s when the display-state flag `+26` is set); state 3 (serialisation requested) **30 s** in lock-logon-unified mode else 5 s; state 4 (presence result without recognition) **30 s** unified else 10 s; state 5 **5 s** | | OBSERVED values, state meaning INFERRED from callers |
| Camera start-up: `BioListenerDisabledDueToCameraStartupTimeout`, texts 105 "Getting ready..." / 130 "Hold tight while we get everything ready..." / 180 "Hello! Getting things ready..." while the camera starts; `DoesBioServiceRequireExtraTimeToBootstrapFaceDriver` | value UNKNOWN | OBSERVED (names) |
| Hammering lockout: repeated failures increment a failure counter and lock the face provider (`FaceBioListener::ActivateLockoutDueToHammeringIfRequired`, states `LockedOut`, `LockedOutDueToLogonFailure`, `LockedOutDueToAdminPolicy`, `LockedOutDueToNoEnrollments`, `LockedOutDueToDriverIntegrityCheckFailure`) | threshold UNKNOWN | OBSERVED (names) |
| "Delay lock" (lid closed / display off → listener paused: `HandleDelayLockScenario.DelayLocked/DelayLockEnded`, `DisplayOnRequester`): Windows keeps the display on while it looks for you (`DisplayOnRequester::RequestActive`) | | OBSERVED (names) |
| Remote session active → no face ("Remote session is active. Dismiss the lock screen to sign in." id 128), power-saving mode (`EndPowerSavingMode`) | | OBSERVED |
| Enroll-on-failure (F31): after a failed face + successful PIN/password the last prints are committed (`EnrollOnFailureManager::ProcessCommitSignal`, registry `CommitEnrollmentOnFailure`/`DiscardEnrollmentOnFailure`) | | OBSERVED |
| Screen brightness: during face auth LogonUI raises the backlight and steps it back (`BrightnessOverrideHelper::SetScreenBrightness(BioFeedbackState)`, `OnBrightnessTimerTick`; text 133) | step/period UNKNOWN | OBSERVED |
| Toasts (F56): `HelloFace` toasts "Improve recognition in Windows Hello" (303/304), "Can't sign in with Windows Hello?" (301/302, 309/310), "Help Windows recognize you." (315/316), "Do you know that you can use face authentication to sign in?" (305/306, with the animated GIF), "There is a problem with Windows Hello." (307/308, 313/314), "Windows Hello Face needs a quick refresh." (311/312); intervals from OneSettings `IMPROVERECOGNITIONNOTIFICATIONINTERVALINDAYS`, `…WHENHIGHBACKLIGHTISDETECTEDINTERVALINDAYS`, `GETHELPNOTIFICATIONINTERVALINDAYS`, `CREATEENROLLMENTNOTIFICATIONINTERVALINDAYS`, `LIVENESSSUGGESTIONNOTIFICATIONINTERVALINDAYS`, `MAXCREATEENROLLMENTNOTIFICATIONS`, `MAXNOENROLLMENTSNOTIFICATIONS` (defaults UNKNOWN; last-shown times in `LastNotification`, `CreateEnrollmentNotificationTime`, `LivenessSuggestionNotificationTime`, `GetHelpNotificationTime`) | | OBSERVED |

Toast XML (`sub_1800777B4`; the `launch` URI is `ms-settings:signinoptions-launchfaceenrollment` for the enrollment toasts,
INFERRED from the string): `<toast launch="…" activationType="protocol"><visual><binding
template="ToastGeneric"><image id="1" src="C:\Windows\System32\@WindowsHelloFaceToastIconRejuv.png" placement="AppLogoOverride"/>
<text id="1">title</text><text id="2">body</text></binding></visual></toast>`; the `HelloCreateEnrollment` toast instead embeds
`@facial-recognition-windows-hello-rejuv.gif` as the hero image. So the two files the owner asked for are **toast art**,
not the sign-in icon.

### 1.3 The Hello eye

Two implementations exist in `Windows.UI.Logon.dll`; `BioFeedbackViewModel::IfNewAnimations` (0x1800fca24) picks the
Lottie one when WinUI 2 (`Microsoft.UI.Private.Controls.CBSPackageDetector`, shipped in `MicrosoftWindows.Client.CBS`)
is activatable — i.e. on every Windows 11 — and the font-glyph one otherwise (Windows 10 look, and the fallback).
Both sit in `BioFeedbackUserControl` (BioFeedbackUserControl.xaml):

```
<Grid>                                   rows: Auto, *
  <TextBlock x:Name="AnimationText"      row 0, Style=TitleTextBlockStyle, FontSize=60 (BioFeedbackUserControlAnimationTextFontSize),
             FontWeight=SemiBold, HorizontalAlignment=Center>      ← glyph animation (font NUIMDL2 "NUIMDL2Assets")
  <LottieUserControl x:Name="LottieUserControlElement" row 0>      ← Grid 64×64, Margin 0,14,0,6, AnimatedVisualPlayer 64×64 AutoPlay
  <TextBlock x:Name="Label"              row 1, Margin 0,6,0,0, TitleTextBlockStyle (24 px) SemiBold, centred, live region
</Grid>
```

**Lottie** (`AnimatedVisuals::HelloSolidAnimations`, LottieGen output, OBSERVED): one composition of **64×64** units,
**37.833 s** (`Duration` = 378,333,333 ticks, `?get@?QDuration…` 0x1803246a0) = **2270 frames at 60 fps**; the markers
(`Markers::get` 0x1803248e0, progress × 2270 = frame):

| Segment | Frames | Length | Played |
|---|---|---|---|
| Face (entrance) | 0 – 59 | 1.0 s | once, state 2 |
| FaceLoop | 60 – 290 | 3.85 s | looped, state 3 (INFERRED: `PlayAsync(from,to,looped)`'s bool is passed by `PlayLottieAnimation`; the name says loop) |
| FaceSuccess | 291 – 408 | 1.97 s | once, state 4 |
| FaceFail | 409 – 477 | 1.15 s | once, state 1 |
| Iris / IrisLoop / IrisError | 478 – 905 | (unused on PC) | |
| USB 906–965, PIN 966–1021, NFC 1022–1090, Finger 1091–1195, BT 1196–1259, BT2 1260–1299, Pass 1300–1356, Phone 1357–1420, PhoneSend 1421–1527 | | | other providers |
| TooFar | 1528 – 1758 | 3.85 s | looped, state 5 |
| TooClose | 1759 – 1989 | 3.85 s | looped, state 6 |
| Spinner | 1990 – 2109 | 2.0 s | looped, state 11 |
| Sync 2110–2220, SyncComp 2221–2269 | | | |

The vectors are compiled C++ (862 functions, `PathGeometry_00…40`, `PathKeyFrameAnimation_00…11`, scale/offset
animations, under `HelloSolidAnimations_AnimatedVisual_UAPv15` in re/ui/Windows.UI.Logon/pseudocode.c). They can be
rebuilt into a Lottie/SVG animation from that pseudocode (D2D path figures with literal coordinates + keyframes) but that
is a separate job: **not done**. The colour is a single `SolidColor` theme property (white on the dark logon theme,
`HelloSolidAnimations::SolidColor::set`), so the whole animation is one-colour line art like the glyph version.

**Font glyph animation** (`FontGlyphAnimator.dll` + `NUIMDL2.ttf`, fully OBSERVED and extracted): the control shows one
glyph of the font `NUIMDL2Assets` in a 60 px TextBlock and steps through codepoint ranges at **33 ms per frame**
(`AnimationDataSet::get_SubAnimations`: duration 0 → 33 ms). The sequences and the transition table are in
`FontGlyphAnimator.dll` (`AnimationDataProviderFactory::CreateAnimationDataProvider`'s map, decoded into
`assets/hello/eye/animation.json`; frames rendered to `assets/hello/eye/seqNN/FFF.png`):

| Seq | Glyphs | Frames | What it draws |
|---|---|---|---|
| 2 | U+E820–E830 | 17 | entrance: a dot grows into the open eye (state 2) |
| 3 | U+E600–E606 | 7 | blink |
| 8, 10, 12, 14, 16, 18 | E610–E615, E630–E633, E650–E656, E670–E674, E690–E693, E6B0–E6B4 | 6/4/7/5/4/5 | pupil glances (left→centre, →right, →down, →left, →up, →centre) |
| 9, 11, 13, 15, 17 | E620–E626, E640–E646, E660–E666, E680–E686, E6A0–E6A6 | 7 each | blinks between the glances |
| 4 | U+E757–E75D | 7 | the eye turns into a smile (state 4) |
| 7 | U+E770–E776 | 7 | smile wiggle |
| 1 | U+E7E0–E7EA | 11 | blink, then the pupil hollows into a ring (state 1) |
| 19 | U+E860–E866 | 7 | ring-pupil blink |
| 5 | U+E720–E73D | 30 | eye shrinks and grows back (TooFar) |
| 6 | U+E700–E71D | 30 | eye grows and shrinks back (TooClose) |
| 23, 26, 27 | EAA7–EAAF, EAC2–EAD3, EADD–EAE0 | 9/18/4 | spinner start / loop / end (state 11) |
| 37 | U+E100–E176 | 119 | full spinner ring |
| 20–22, 24, 25, 28–36 | | | USB/NFC/BT/fingerprint/phone/PIN/sync glyph animations |

Transition machine (`transitions` in animation.json: for the sequence now playing and the *wanted* state, play `play`
then hold for `hold_ms`; `null` = hold until the state changes, `0` = go on at once; `-1` = any other wanted state):

- wanted **2** (looking): from 0 → play 2, hold.
- wanted **3** (verifying): from 2 → 3 (blink, 0 ms) → 8 (hold 320) → 9 (320) → 10 (410) → 11 (320) → 12 (320) → 13 (320)
  → 14 (380) → 15 (320) → 16 (410) → 17 (320) → 18 (380) → 3 (380) → 8 … (the eye looks around and blinks, ≈ 6 s per lap).
- wanted **4** (success): from any loop step → 4 (eye→smile) → 7 (smile wiggle) → 0 → 7 → … (the smile keeps wiggling).
- wanted **1** (fail): → 1 (pupil hollows, hold) ; from 1 → 19 → 1 → 19 … (ring-pupil blinks forever).
- wanted **5 / 6**: 5 / 6 looped. wanted **0** (hide): → 0.
- wanted **11** (spinner): → 23 → 26 (loop) ; leaving → 27 → 0.

### 1.4 Text per state (which binary, resource id)

The face provider loads every text from its own MUI (`FaceCredentialProvider.dll.mui`, RT_STRING ids; full table in
§2) and sends it as the bio-feedback *label* (under the eye) or as the credential's status text; the greeting after
success comes from `credprovhost.dll.mui` (107–153: "Hello, %1!s!!", "Good morning/afternoon/evening, %1!s!!",
"Welcome back, %1!s!!", with the suffixes "Dismiss the lock screen to sign in." / "Sign in to continue." /
"Getting things ready...") — `VisualFeedback::UpdateAuthenticated.GreetingType` picks one; the exact choice rule is UNKNOWN
(time of day for Good morning/afternoon/evening is INFERRED). `VisualFeedback::GetUpdatedResourceIDBasedOnCurrentState`
switches between a lock-screen version, a logon-screen version and a "blank" version of each text (the pairs 50/52,
100/106, 107/122, 125/150, 152/153 …: the long form with "Please sign in with your PIN." is the logon-screen one, the
short form the lock-screen one).

## 2. Every user-visible string

### 2.1 facecredentialprovider (FaceCredentialProvider.dll.mui, RT_STRING id → text), in order of appearance

| id | text | when |
|---|---|---|
| 1 | Face | credential name (field 0 label; shown as the tooltip/accessible name of the Hello tile in "Sign-in options") |
| 3 | Sign in button | accessible name of the submit button |
| 4 | Context message | accessible name of the status field |
| 5 | Windows Hello | provider name |
| 105 | Getting ready... | camera starting |
| 130 | Hold tight while we get everything ready... | camera starting (slow driver bootstrap) |
| 180 | Hello! Getting things ready... | greeting while starting |
| 104 | Looking for you... | state 2 |
| 131 | Please remove anything that blocks your camera. | camera occluded (`CameraOcclusionStateReportCallback`) |
| 123 | Making sure it's you... | state 3 (recognised, liveness pending) |
| 132 | Making sure it's you. Please look at the camera. | state 3, face not frontal |
| 133 | Adjusting brightness for better face recognition... | state 3 with brightness override |
| 101 | Hello %1!s! | success (name = display name / first name) |
| 51 | Hello %1!s!! Select OK to continue. | success, confirmation dialog variant |
| 113 | Hello, %1!s!! Dismiss the lock screen to sign in. | success on the lock screen when confirmation is required |
| 129 | Dismiss the lock screen to sign in. | same, no name |
| 127 | Swipe up to dismiss the lock screen. | same, touch |
| 128 | Remote session is active. Dismiss the lock screen to sign in. | remote session |
| 125 / 150 | Couldn't recognize you. Please sign in with your PIN. / Couldn't recognize you. | fail (logon / lock) |
| 52 | Couldn't recognize you. Please enter your PIN. | fail, PIN variant |
| 200 | Your device couldn't detect you. Make sure your camera lens is clean. | no face for a long time |
| 201–205 | Too bright! Turn off some lights or go inside. / Too dark! Turn on some lights or move somewhere brighter. / Make sure you're centered and looking directly at the camera. / Too close! Try moving a little farther away. / Too far away! Try moving a bit closer. | frame-quality hints |
| 107 / 122 | Your PIN is required to sign in. | PIN required (policy, first sign-in after boot) |
| 156 | Your password is required to sign in. | password required |
| 109 / 116 | Before you can start using Windows Hello, you have to set up a PIN. | no PIN |
| 111 / 118 | Before you can start using Windows Hello, you have to enter your PIN. | PIN not unlocked yet |
| 106 / 152 | Couldn't turn on the camera. Please sign in with your PIN. / Couldn't turn on the camera. | camera error |
| 124 / 153 | Camera not available. Please sign in with your PIN. / Camera not available. | no camera |
| 157 | Camera is in use. Please sign in with your PIN. | camera busy |
| 158 | Camera driver is in a bad state. Please sign in with your PIN. | driver |
| 159 | Hello requires more time to update. Please sign in with your PIN. | update |
| 160 / 161 | Sorry something went wrong with face setup. (Please sign in with your PIN.) | enrollment broken |
| 162 / 163 | Sorry something went wrong with initialization. (Please sign in with your PIN.) | init |
| 114 / 155, 120 | Sorry something went wrong. (Please sign in with your PIN. / Please enter your PIN.) | other |
| 108 | Your account has been disabled. Please contact your support person. | |
| 121 | Windows Hello is currently disabled by your administrator. | policy |
| 100 | Windows couldn't sign you in. | logon failure |
| 50 | Please use a different method to identify yourself. | lock-out |
| 301–316 | toasts (§1.2) | |

### 2.2 LogonUI (Windows.UI.Logon.en-US.pri, resource name → text)

`SignInOptions/Text` = **Sign-in options**; `SignInOptionsHelpTextCollapsedList` = "Shows a list of the available sign in
options for the selected user" / `…ExpandedList` = "Hides the list…"; `…SelectedCredential` = "Selected", `…Unselected…` =
"Not selected" (accessibility); `Submit` (name+tooltip) = **Submit**; `CapsLockWarning/Text` = **Caps Lock is on**;
`DirectSignIn/Content` = **Sign in**; `SwitchUser/Text` = **Switch user**; `SwitchUserWithSignOff/Text` = "This user needs to sign
out before another user can use this PC. They could lose unsaved work."; `SignOut/Text` = Sign out; `SignOutAnywayButton` =
Sign out anyway; `Lock/Text` = Lock; `TaskManager/Text` = Task Manager; `ChangePassword/Text` = Change a password;
`PowerOptions` name/tooltip = **Power**, `PowerOptionsMenuFlyout` = Power options, `NoPowerOptionsAvailable` = "There are
currently no power options available."; `Accessibility` = **Accessibility**; `NetworkStatus/ToolTip` = Internet; `PLAP` = Network
sign-in, `PLAPConnecting` = Connecting...; `OK/Content`, `Yes/Content`, `No/Content`, `Cancel/Content`, `CancelPLAP` = Back,
`StartOver` = Start over; `WindowTitle` = Windows sign-in; `WindowsSecurityString` = Windows Security;
`UserOrCredListSwitch` = "Switch to %1"; `EmergencyRestartCaption/MessageBody`; `InputIndicatorTooltipFormatString` =
"%1\n%2\n\nTo switch input methods, press\nWindows key+Space"; access keys T/W/S/K/C/M/P/N for the Ctrl-Alt-Del menu.

### 2.3 credprovhost (credprovhost.dll.mui)

100 Other user, 101 Unlock the PC, 102 Change a password, 103 Signed in, 104 Signed in remotely from %s, 105 Locked,
107–153 the greetings (§1.4; e.g. 107 "Hello, %1!s!!", 109 "Hello, %1!s!! Dismiss the lock screen to sign in.",
113 "…sign back into Windows.", 114 "Hello, %1!s!! Getting things ready...", 120–151 "Good morning/afternoon/evening /
Welcome back" × {plain, "Dismiss the lock screen to sign in.", "…sign back into Windows.", "Sign in to continue."}),
152 "Hello, %1!s!! Sign in to continue.", 112 "FirstName" (greeting uses the first name).

### 2.4 winlogon (winlogon.exe.mui) — the full-screen status after the credential is accepted

1002/1005 **Welcome**, 7140 Signing in, 7120 Signed in, 1003 Signing out, 1006 Locking, 1008 Please wait, 1013 Just a
moment..., 1016 "Invalid credentials, delaying next attempt...", 1400 "This computer is locked. Only the signed-in user
can unlock the computer.", 7100 Returning to sign-in. Shown by `DisplayMessageUserControl` (Frame state
`DisplayFullScreenMessage`): `MessageStatusAndCaptionStyle` 24 px SemiBold centred under a 40×40 `ProgressRing`
(`CredentialDisplayMessageUserControl.xaml`).

### 2.5 BioCredProv (fingerprint), for completeness

1 Fingerprint, 2 "To sign in, scan your finger on the fingerprint reader.", 3 "To unlock your device, scan your finger…",
110/153 "Your device couldn't recognize you. Please try again.", 111 "Your fingerprint couldn't be recognized. Try using a
different finger.", 143–150 finger placement hints, 1011 Fingerprint sign-in, 1014 Fingerprint sign-in prompt.

## 3. Layout

All numbers are XAML device-independent pixels (1 px = 1/96 in; Windows scales by the display scale factor,
e.g. ×1.5 on this 2560×1600 16" panel). Enum values decompile as numbers: `HorizontalAlignment(1)` = Center,
`(0)` = Left, `(2)` = Right, `(3)` = Stretch; `VerticalAlignment(0)` = Top, `(1)` = Center, `(2)` = Bottom;
`TextAlignment(0)` = Center, `(1)` = Left; `Stretch(3)` = UniformToFill; `Visibility(1)` = Collapsed.

### 3.1 Lock screen (`SecureLockScreenUserControl.xaml`, OBSERVED; the LockApp lock screen is its twin, INFERRED)

```
┌──────────────────────────────────────────────────────────────────────────┐
│                     "Hello, Cole! Dismiss the lock screen to sign in."    │ ← Label: Margin 0,40,0,0, SubheaderTextBlockStyle
│                                                                           │    34 px SemiBold, centred, fades in; only while a
│   row 0 = 15*                                                             │    bio-feedback state is set
│                                                                           │
│                                10:42                                      │ ← TimeText: Segoe UI Variable SemiBold,
│                       Thursday, October 16                               │    tabular numerals; DayOfWeekText below it
│                                                                           │    (sizes per window width, table below)
│   row 2 = 85*                                                             │
│                                                                           │
│                                                                           │
│                                                     [wifi] [battery]      │ ← StatusIndicatorContainer bottom-right
└──────────────────────────────────────────────────────────────────────────┘
```

- Background: the lock-screen image `Stretch(3)` = UniformToFill with a black `Rectangle` **opacity 0.2** over it
  (`ImageMask`); on the sign-in screen the mask animates to 0 and the image scales 1→1.02 while the acrylic backdrop
  fades in (0.5 s, `LandscapeGridReveal`); going back (`LockReveal`) reverses it (0.5 s).
- Clock block: `Grid LockScreenContent` rows `15*`, `Auto`, `85*` → the time/date `StackPanel` sits with its top at 15 %
  of the height, horizontally centred (`HorizontalAlignment(1)` + `TextAlignment(0)` = Center). Adaptive sizes
  (`AdaptiveTrigger MinWindowWidth`):

| Width ≥ | TimeText FontSize / LineHeight | DayOfWeekText | Status icons (size, margin left) | StatusIndicatorContainer margin (right, bottom) |
|---|---|---|---|---|
| 0 ("Small") | 72 / 92 | 20 / 28 | 14, 32 | hidden |
| 600 (Medium) | 98 / 98 | 20 / 28 | 14, 32 | 24, 22 |
| 1366 (Large) | 98 / 98 | 20 / 28 | 18, 40 | 36, 34 |
| 1920 (XLarge) | 136 / 136 | 28 / 36 | 18, 40 | 72, 68 |
| 3840 (XXLarge) | 196 / 196 | 40 / 52 | 18, 40 | 100, 80 |

  `TimeText_Default`: FontFamily "Segoe UI Variable", FontWeight SemiBold, `Typography.NumeralAlignment=Tabular`,
  BasedOn HeaderTextBlockStyle; `DayOfWeekText_Default`: SemiBold, Margin 0; colour `BaseWhiteBrush` =
  `SystemChromeWhiteColor` (#FFFFFF). Network icon: `FontIcon` Segoe Fluent Icons 14/18 px, three layers (underlay at
  opacity 0.5, base, accent overlay `SystemAccentColorLight2`); battery: image 14/18 px, margin-left 24.
- `UnLockMessageText` (above the time, lost-mode text) MaxWidth 432/500/600/800/1000, `TextWrapping(2)`=Wrap.
- Reveal hint: `SelfRevealStoryboard` scrolls the content 40 px up over 0.4 s (cubic ease-out) and back over 0.4 s
  (ease-in) — the nudge when the user touches/clicks without swiping. Dismissal: scroll/swipe up (`ScrollViewer`,
  vertical snap points), any key, click (INFERRED for key/click: `LockScreenDismissed`/`FaceCredential::DismissLockScreen`).
- The hardened variant `RequireCADSecureLockScreenUserControl` adds the Segoe MDL2 Assets glyph U+E785 (keyboard) above a
  `RequireSecureGestureText` line (resource `SecureLockRequiresSecureGestureText`, text set from code; "Press Ctrl+Alt+Delete
  to unlock" INFERRED).

### 3.2 Sign-in screen (`RequestCredentialUserControl.xaml` + `CommonStyles.xaml`, OBSERVED)

```
┌──────────────────────────────────────────────────────────────────────────┐
│ col 1*      │                 col 2* (content)              │   col 1*   │
│             │   ┌─ Margin 0,72,0,0, VerticalAlignment Top ─┐│            │
│             │   │          ╭────────╮                      ││            │
│  [Sign Out] │   │          │ avatar │  192 × 192 circle    ││            │
│  (shared PC │   │          ╰────────╯                      ││            │
│   only)     │   │        Cole Leavitt     28 px SemiBold   ││            │
│  [user list]│   │      cole@example.com   14 px (optional) ││            │
│             │   │                                          ││            │
│             │   │            (eye)        64×64 Lottie /   ││            │
│             │   │      Making sure it's you...  24 px SB   ││            │
│             │   │   ┌──────────────────────┐ ┌─┐           ││            │
│             │   │   │ Password / PIN       │ │→│  296 wide ││            │
│             │   │   └──────────────────────┘ └─┘           ││            │
│             │   │       Caps Lock is on   (when on)        ││            │
│             │   │         Sign-in options                  ││            │
│             │   │       [☺] [⌨] [🖐]  44×44 buttons          ││            │
│             │   └──────────────────────────────────────────┘│            │
│             │   MinHeight 530, Margin bottom 70, centred    │            │
│                                        [IME] [lang] [net] [access] [power]│ ← SystemProviderUserControl bottom-right
└──────────────────────────────────────────────────────────────────────────┘
```

- Grid `RequestCredentialUserControlGridLandscape`: columns `*`, `2*`, `*`; the middle column holds a `Grid`
  Margin `0,0,0,70`, `VerticalAlignment=Center`, **MinHeight 530**; inside it `SelectedUserOrV1OrPLAPContentGridLandscape`
  Margin `0,72,0,0`, `VerticalAlignment=Top`, rows `*`, `Auto` (row 1 = the "Back"/"Cancel" button for PLAP/change-password,
  Margin 0,20,0,0). Portrait (`RequestCredentialUserControlGridPortrait`): rows `4*`, `*`; content Margin 0,0,0,12,
  inner Margin 0,36,0,0. So the tile block is **centred horizontally, and vertically centred in a 530 px-high box that
  is itself centred 35 px above the middle** (70/2) with the content pinned to the top of that box.
- `SelectedUserTemplate`: outer grid rows Auto (identity) + `*` (credential); both `MinWidth 404`.
  - Identity rows: company logo `Image` 200×80 (only when configured) **or** `SelectedUserTileUserControl` = a
    `ContentControl` **192×192** (`SelectedUserTileUserControl.xaml`): `ImageUserTileTemplate` = `Ellipse` 192×192 with
    `ImageBrush` of the account picture; `DefaultUserTileTemplate` (no picture) = `Ellipse` 192×192 fill white
    **opacity 0.061** + Segoe Fluent Icons glyph **U+EA8C** (Contact) `FontSize 100` white opacity 0.9 centred;
    `VideoUserTileTemplate` = MediaElement 192×192 (animated tile). Default account picture file: `user.png`/`user-192.png`
    (§6) — the grey bust on a light grey disc; Windows draws it through the Ellipse.
  - `DisplayName`: `DisplayNameTextBlockStyle` = Segoe UI Variable **Semibold 28 px**, Margin `0,12,0,0`, centred,
    `TextWrapping(3)`=WrapWholeWords, MaxLines 2, `TextTrimming(1)`=CharacterEllipsis. Then optional `EmailAddress` and
    `QualifiedName` rows in `BaseTextBlockStyle` (14 px Normal).
  - Credential rows: `*` (scrollable area), `Auto`, `Auto MinHeight 20` (Caps Lock warning), `Auto` (Sign-in options).
    In the scroll area: `BioFeedbackUserControl` Margin `0,8,0,0` centred (row 0) and, row 1, either the
    `CredentialFieldsList` (`CredentialFieldsListStyle`: Margin `0,20,0,0`, Padding `15,0`, items Padding `0,8`,
    centred) or `CredentialDisplayMessageUserControl` (Margin `0,35,0,0`: the status text / message with OK/Yes/No/Cancel)
    or the `DirectSignIn` button (Margin 0,12,0,0: "Sign in" for credentials without fields).
  - Password/PIN field (`PasswordFieldTemplate`): `Border` **Width 296** (`CredentialControlsWidth`), `CornerRadius 4`
    (`TextfieldCornerRadius`), background `#000000` **opacity 0.06** (`PasswordBoxBorderBackgroundNormal`;
    focused/hover: `#1E1E1E` opacity 0.7), bottom-edge border brush = vertical gradient `#14FFFFFF` → `#8AFFFFFF` in the
    last 5 % (focused: `SystemAccentColor` in the last 5 %) i.e. the Fluent "underline" field; inside it a `Grid` of
    `*` + `Auto`: `PasswordBox` (`PasswordBoxStyle`: Padding `8,6,8,4`, BorderThickness 0, Segoe UI Variable 14 px
    Normal, placeholder colour `#C7FFFFFF`, text `#C7FFFFFF`, reveal button = Segoe Fluent Icons **U+E052** 12 px in a
    26×22 box with corner radius 3, Margin `4,4,0,4`) and the **submit button** (`SubmitButtonStyle`: MinWidth 28,
    MinHeight 24, Margin `3,3,3,4`, CornerRadius 3, BorderThickness 1, Segoe Fluent Icons **U+E72A** (Forward →) 12 px,
    foreground `#C7FFFFFF`, background transparent; hover/pressed: background `SystemAccentColor`, foreground black).
    Text fields (`CredentialEditFieldStyle`) are the same box with a `TextBox`.
  - `CapsLockWarning`: "Caps Lock is on", `BaseTextBlockStyle` (14 px) **SemiBold**, centred, row MinHeight 20.
  - `CredentialsListUserControl` (`Sign-in options`): `HyperlinkButton` Padding 0, centred, text `BodyTextBlockStyle`
    (14 px Normal) — the Fluent hyperlink colour `SystemControlHyperlinkTextBrush` = `SystemBaseMediumHighColor`; below it
    `SignInOptionsItemsControl` (`SignInOptionsItemsControlStyle`: Margin `0,32`), a horizontal `ItemsStackPanel` of
    `Button`s `SignInOptionsButtonStyle`: **44×44**, Margin `10,0`, Padding 0, CornerRadius 3, BorderBrush `#808080`,
    BorderThickness **2** when selected (`SelectedCredProvBorderThickness`) else 0, content = the provider's SVG logo
    **24×24** (`CredentialLogoIcon`; `ThemeAdaptedLogo` = the SVG recoloured for the theme — white on the dark logon
    theme). The face logo is the SVG in `FaceCredentialProvider.dll.mui` RT_HTML 1101 (`assets/hello/hello-face.svg`:
    two round eyes + a smile on a 2048 grid); fingerprint = `BioCredProv.dll.mui` RT_HTML 1024; PIN/password/key
    logos are credprov SVGs not extracted. Tooltip = the credential name (`Face`).
- Bottom-right system bar (`SystemProviderUserControl.xaml`, Grid.Column 1+2, HorizontalAlignment Right,
  VerticalAlignment Bottom; portrait Margin `0,0,12,22`): columns IME indicator (MinWidth 48), input language switch
  (MinWidth 40), `PLAP` U+EC19, `DisconnectPLAP` U+E24B (Segoe UI Symbol), `NetworkStatus` (three 16 px layers), `Accessibility`
  **U+E776** and `PowerOptions` **U+E7E8** — `CustomSystemProviderButtonStyle`: Segoe Fluent Icons **16 px**, Height 40,
  MinWidth 40, Margin `8,0,0,0`, Padding `8,4`, CornerRadius 4, transparent background, foreground
  `SystemControlForegroundBaseHighBrush`. Power opens a `MenuFlyout` (Sleep/Shut down/Restart from authui: "Sleep",
  "Shut down", "Restart", 11/12 px semilight; `MenuFlyoutItemThemePadding` 11,8,11,9, min height 32, corner radius 8,
  acrylic presenter tint `#2C2C2C` 15 %/luminosity 0.96 dark, `#FCFCFC` light).
- Left column (shared-PC/other users): `UsersListUserControl` top-left, Margin `24,36,16,28`, `VerticalAlignment Bottom`:
  each user = `Ellipse` **48×48** (`EllipseWithMarginStyle`, Margin `0,6`) + name (`CustomHighColorBodyTextStyle` 14 px,
  `SystemBaseHighColor`) and status line (`CustomMidColorTextStyle`, `SystemBaseMediumColor`), Margin-left 12; credentials
  without a user (Other user, smart card) = 48×48 image + name, row height 56. `ChangeUserButton` "Sign Out"/"Switch user"
  above it (Margin 0,0,0,12). On a single-user PC the list is collapsed.
- Keyboard: when the touch keyboard appears the content translates up (`LandscapeKeyboardStoryboard`, 0.25 s).

### 3.3 Typography, colours, materials

| Element | Font / size / weight | Resource |
|---|---|---|
| everything (default) | **Segoe UI Variable** 14 px Normal (`LogonFontFamily`, `LogonDefaultFontSize`, `LogonFontStyle`, `BaseTextBlockStyle`, `BodyTextBlockStyle`) | CommonStyles.xaml |
| display name | 28 px Semibold (`DisplayNameTextBlockStyle`) | |
| status / message caption, eye label | 24 px Semibold (`TitleTextBlockStyle`, `MessageStatusAndCaptionStyle` Margin 0,20,0,0) | |
| lock-screen greeting | 34 px Semibold (`SubheaderTextBlockStyle`) | |
| header | 46 px Semibold (`HeaderTextBlockStyle`), base of the clock styles | |
| clock | 72/98/136/196 px SemiBold tabular (`TimeText_*`), date 20/28/40 px SemiBold | LockScreenStyles.xaml |
| glyph eye | NUIMDL2Assets 60 px | BioFeedbackUserControl.xaml |
| icons | Segoe Fluent Icons (`SegoeIcons.ttf`) 12/16 px; MDL2 U+E785 on the CAD lock | |
| Cloud PC disclaimer | "Segoe UI Variable Static Display" 16 px weight 600 | |
| authui power dialog | "11;semibold;none;segoe ui", "20;Light;none;Segoe UI" | authui.dll.mui 3011–3132 |

Colours (dark theme = the logon theme; `CommonStyles.xaml` ThemeDictionaries): text primary `#FFFFFFFF`, secondary
`#C5FFFFFF`, tertiary `#87FFFFFF`, disabled `#5DFFFFFF`; field text/placeholder `#C7FFFFFF`; divider `#15FFFFFF`;
surface stroke `#66757575`; list item hover `#0FFFFFFF`, selected `#0FFFFFFF`; accent = the user's `SystemAccentColor`
(submit hover, focused field underline, `SystemAccentColorLight2` for network overlay). Light theme equivalents
(`#E4000000`, `#9E000000`, `#72000000`, `#5C000000`, hover `#0A000000`) exist but LogonUI runs dark over the image.
Backgrounds: lock = image + 20 % black; sign-in = the same image blurred through `LogonBackgroundBackdrop`
(`RequestCredentialsViewModel.LogonBackgroundBrush`, an `AcrylicBrush BackgroundSource=Backdrop`; the tint values are
set in code — the flyout one is `TintColor #2C2C2C, TintOpacity 0.15, TintLuminosityOpacity 0.96` (dark) and
`#FCFCFC / 0 / 0.85` (light), `OverlayCornerRadius 8`). The sign-in blur radius/tint of the main backdrop is UNKNOWN
(set from code, not XAML); public screenshots show a strong blur with a dark tint (INFERRED).

Proprietary vs substitutes: Segoe UI Variable, Segoe Fluent Icons, the Spotlight/`img100` images and the account
picture are Microsoft's (extracted locally only, §6). Metric-compatible open substitutes: **Selawik** (Microsoft, OFL —
Segoe UI metrics), **Noto Sans / Open Sans** (similar x-height), Fluent icons → **Fluent UI System Icons** (MIT) or
**Segoe-free glyphs from Fluent UI Emoji/`fluentui-system-icons`**; background → our own image.

## 4. Behaviours

- **Wake**: any key, mouse move/click, touch, lid open, and (F55) presence from the camera bring the lock screen from
  display-off; a key/click/swipe-up on the lock screen reveals the sign-in screen (lock → RequestCredential transition
  §3.1). With Hello the face listener already runs **on the lock screen** (`EnableLockLogonUnifiedMode`), so a known user
  is usually recognised before the sign-in screen appears: the lock screen shows "Hello, <name>! Dismiss the lock screen
  to sign in." at the top (Label, 34 px) and the next key/swipe signs in without showing the password screen. Within the
  120 s window after lock (`TimeToRequireConfirmationBeforeAutoLogon…`, §1.2) and with `AutoDismissOn`, recognition alone
  dismisses the lock screen and signs in ("automatically dismiss the lock screen if we recognize your face").
- **Password field focus**: the Hello credential has no text field; when it fails (state 1 → `SwitchProviderIfInFailedState`,
  `FaceCredential::ReportResult.RequireSwitchToPasswordProvider`) LogonUI selects the PIN/password credential and the field
  gets focus (`FocusHelper.HasDefaultFocus`, `FieldIDWithFocus`); the Hello tile remains under "Sign-in options" and
  re-selecting it restarts at state 2. Typing while the face is being checked switches to the password credential at once
  (INFERRED: the password credential's field is the default-focus element whenever it is the selected credential).
- **Retry**: no fixed count in the UI; the listener keeps tracking (F57) until the presence timeouts (§1.2) or a failure
  report; `mismatchCount` and the hammering counter lock the provider after repeated failures (threshold UNKNOWN).
- **Lid / display**: lid close pauses the listener (`DelayLocked`, lid state read through
  `WinBioSession::StartLidStatusControlOp`, `FaceUtils::IsLidNotifyReliable`); display off → `DisplayTurnedOffAtLeastOnce`
  → confirmation greeting required when it comes back.
- **Error strings**: §2.1. Full-screen "Welcome"/"Signing in" (winlogon) replaces the sign-in screen after the credential
  is accepted; "Windows Logon.wav"/"Windows Unlock.wav" are the sound-scheme events (disabled by default on Windows 11,
  INFERRED from the default sound scheme).
- **Toasts** (F56, §1.2): shown later in the session, not on the sign-in screen.

## 5. Parity map: Windows → hello-greeter / pam_linux_hello

The greeter sees PAM through greetd `auth_message` (`info`/`error`/`secret`) or its own PAM conversation; the module
prints `PAM_TEXT_INFO` lines. To drive the eye and the texts 1:1 the module's lines should be machine-mappable. Proposed
contract (the greeter maps the exact strings; humans see the same text on a terminal):

| Windows state / text | Daemon → module | Module prints (proposal) | Greeter |
|---|---|---|---|
| camera starting (105) | (before the request) | `Looking for your face...` (existing) | state 2: eye entrance, label "Looking for you..." |
| state 2 Looking (104) | request sent | — | keep state 2 |
| state 3 "Making sure it's you..." (123) | `NOTICE recognized` *(new, sent when the first 4 prints match before the liveness votes end)* | `Making sure it's you...` | state 3: eye look-around loop, label |
| 132 look at the camera | `NOFACE not_frontal` (end) | `Face turned away: look at the screen` | state 1 + label |
| success (101 → Welcome) | `MATCH` | *(nothing; PAM_SUCCESS)* | state 4: eye→smile, label "Hello, <name>", then "Welcome" page, start session |
| fail (125) | `NOMATCH …` | `Face not recognized` | state 1: ring-pupil blink; label "Couldn't recognize you. Please sign in with your password."; focus the password box |
| spoof | `NOTLIVE` | `Face not accepted as live` | state 1 |
| too dark/bright/far/edge/occluded (201–205, 131) | `NOFACE too_dark` … | existing hints | state 1, label = the hint |
| camera busy (157) / lid (124-ish) | `BUSY`, `UNAVAILABLE lid_closed` | existing "Face login not available: …" | state 7 hidden icon + label |
| daemon down / not enrolled | `PAM_IGNORE` silently | — | no eye, password box only |
| enroll-on-failure (F31) | `ADDED` | `Face print added after password login` | toast-style line after sign-in |

Requested changes in `pam/src/lib.rs` (request to the parent; not edited here):

1. Print `Making sure it's you...` when the daemon reports the recognition step. This needs a daemon-side interim line
   (e.g. `NOTICE recognized` streamed before the verdict) — today the socket reply is one verdict line plus trailing
   notices, so the module would have to read lines as they arrive and print each `NOTICE` immediately instead of after
   the verdict. Without it the greeter can only show states 2 → 4/1.
2. Make the failure message for `NOMATCH` match Windows' wording: `Couldn't recognize you. Please sign in with your
   password.` (Windows id 125, "PIN" → "password"), keep the `NOFACE` hints as they are (they are Windows' 201–205
   rephrased), and print `Face login not available: …` lines unchanged.
3. Optionally prefix machine-readable lines (`[hello] state=looking`) — rejected here: the greeter can map the exact texts
   above, and terminals stay clean.

Windows behaviours the greeter does not need: PLAP/network sign-in, Cloud PC, picture password, user list on a
shared PC, PIN (we have password only), toasts (separate), brightness override (optional).

## 6. Assets (local, git-ignored, Microsoft-proprietary)

`scripts/extract_windows_hello_assets.py <ISO | install.wim | extracted root> models/windows_hello/assets [--image 6]
[--xbf2xaml tools/winhello_ui/xbf2xaml/out/xbf2xaml]` (run with `tools/winhello_models/.venv/bin/python`; needs `7zz`,
`pefile`, `Pillow`, `rsvg-convert`) writes `models/windows_hello/assets/` (git-ignored by `/models/windows_hello/*`) and
`manifest.json` (path, source, resource id, size, SHA-256, licence note per file; 655 entries). Licence for everything in
it: Microsoft Windows EULA — personal use on the licensed machine, never commit, never redistribute.

| Directory | Files | Source in the image | Used for |
|---|---|---|---|
| `fonts/` | `SegUIVar.ttf` (Segoe UI Variable), `segoeui*.ttf`/`segui*.ttf` (Segoe UI static family: regular, bold, italic, light, semilight, semibold, black), `SegoeIcons.ttf` (**Segoe Fluent Icons**), `segmdl2.ttf` (Segoe MDL2 Assets), `seguisym.ttf` (Segoe UI Symbol), `NUIMDL2.ttf` (NUIMDL2Assets: the eye frames) | `Windows/Fonts/`, `Windows/SystemResources/Windows.UI.BioFeedback/Fonts/` | all text (§3.3), icons, the glyph eye |
| `screen/` | `img100.jpg` … `img105.jpg` | `Windows/Web/Screen/` | the stock lock-screen pictures (img100 = the default Windows 11 "Bloom" lock image, 3840×2160; img101 dark bloom, img102 lake sunset, img103 ribbons, img104 light bloom, img105 plain blue) |
| `wallpaper/` | `img0.jpg`, `img19.jpg` (+1920×1200), Spotlight `img14.jpg`, `img50.jpg`, `spotlight.theme` | `Windows/Web/Wallpaper/`, `Windows/Web/4K/`, `Windows/Resources/Themes/` | desktop/Spotlight defaults (Spotlight itself downloads images; proprietary, we use our own) |
| `user-pictures/` | `user.png` (448×448, grey bust on #F2F2F2), `user-192.png`, `user-48/40/32.png`, `user.bmp`, `guest.*` | `ProgramData/Microsoft/User Account Pictures/` | default avatar (Windows draws it in the 192 px ellipse) |
| `hello/` | `hello-face.svg` (+ `.png` 256 px white render) = FaceCredentialProvider.dll.mui RT_HTML 1101; `fingerprint.svg` = BioCredProv.dll.mui RT_HTML 1024; `hello-face-tile-72.bmp`, `fingerprint-tile-72.bmp` = RT_BITMAP 1001 (legacy 72×72 tiles); `@WindowsHelloFaceToastIcon.png`, `@WindowsHelloFaceToastIconRejuv.png` (toast app logo), `@facial-recognition-windows-hello.gif`, `…-rejuv.gif` (toast hero animation), `HelloFaceAnimation.gif` (OOBE/Settings) | `Windows/System32/`, `…/en-US/*.mui`, `SystemApps/MicrosoftWindows.Client.CBS/media/` | the "Sign-in options" face button (SVG), toasts |
| `hello/eye/` | `animation.json` (sequences + transition table), `seq01…seq37/000.png…` (72×72, glyph at 60 px, white on transparent) | `FontGlyphAnimator.dll` + `NUIMDL2.ttf` | the Hello eye (§1.3) |
| `sounds/` | `Windows Logon.wav`, `Windows Unlock.wav`, `Windows Logoff Sound.wav` | `Windows/Media/` | optional sound events |
| `strings/` | `FaceCredentialProvider.dll.json`, `BioCredProv.dll.json`, `credprovhost.dll.json`, `authui.dll.json`, `winlogon.exe.json`, `LockScreenContent.dll.json`, `LockAppBroker.dll.json` (RT_STRING id → text), `Windows.UI.Logon.json`, `Windows.UI.BioFeedback.json` (PRI name → text) | en-US MUIs and PRIs | §2 |
| `logonui/` | the 23 `*.xbf` layouts and their `.xaml` decompilation, `Assets_CloudIcon.png`, `Assets_CloudSpinner*.gif`, `Assets_RemoteDesktopIcon*.png` | `Windows.UI.Logon.pri` | §3 |
| `bin/` | the DLL/EXE/MUI/PRI files the above were pulled from | | reproducibility without the WIM |

The greeter's loader layout (agreed with the `greeter` branch): `fonts/*.ttf` → fontconfig, `screen/img100.jpg` default
background, `user-pictures/user.png` default avatar, `hello/hello-face.png` static icon, `hello/eye/` frames + json for
the animation.

## 7. Open items

- Lottie geometry of `HelloSolidAnimations` (the Windows 11 eye) is not reconstructed; the glyph eye (Windows 10 look,
  still the fallback) is complete. The 64×64 canvas, 60 fps timing and segment lengths above are exact.
- LockApp.exe (Spotlight lock screen) layout values are INFERRED from the LogonUI secure-lock twin; the main acrylic
  tint of the sign-in backdrop and the hammering/camera-startup thresholds are UNKNOWN.
- `BioFeedbackState` 19 and the feature-flagged 20 are not used by the face provider in this build.
