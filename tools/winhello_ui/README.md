# winhello_ui: decompile LogonUI's XBF layouts

`xbf2xaml` dumps a compiled XAML file (XBF 2.1, as stored in `Windows.UI.Logon.pri`) as readable XAML, using
Microsoft's own reader `WidgetSpinner` (`dxaml/xcp/tools/XbfParser` in the MIT-licensed microsoft/microsoft-ui-xaml
repo). `scripts/extract_windows_hello_assets.py --xbf2xaml <this binary>` runs it on every layout it pulls out of the PRI.

Build (needs the .NET 8 SDK):

```
cd tools/winhello_ui
git clone --depth 1 --filter=blob:none --sparse https://github.com/microsoft/microsoft-ui-xaml muix
git -C muix sparse-checkout set dxaml/xcp/tools/XbfParser
rm muix/dxaml/xcp/tools/XbfParser/WidgetSpinner/Directory.Build.props   # imports a repo-root props file we do not have
python3 patch_widgetspinner.py                                            # two fallbacks for types WinUI 3 deleted
cd xbf2xaml && dotnet build -c Release -o out
```

The two patches (`patch_widgetspinner.py`) make `XamlTypeRegistry.GetXamlTypeByIndex` and
`XamlPropertyRegistry.GetPropertyByIndex` return a placeholder named after the enum member instead of throwing for
stable indices WinUI 3 removed (`ProgressRing_Deleted0`, `MediaElement_Deleted0`, `Control_IsTabStop_Deleted0`), which
the system XAML in LogonUI still uses. Output: `<file>.xbf.xaml` next to each input; enum values print as
`Name(number)` (e.g. `HorizontalAlignment(1)` = Center), see docs/windows-hello-ui.md §3. `muix/` and `out/` are
git-ignored.

## lottie_extract.py: the Windows 11 Hello eye as data

`lottie_extract.py dump <Windows.UI.Logon.dll.i64> <funcs.json>` (idalib, IDA 9.5: every instruction of the
`HelloSolidAnimations*` functions and the vtable slot of each called thunk) and `lottie_extract.py extract <funcs.json>
<winrt/impl dir> <solid.json>` (a symbolic x64 interpreter replays `HelloSolidAnimations_AnimatedVisual_UAPv15`'s
constructor and writes the Windows.UI.Composition object graph) produce `models/windows_hello/assets/hello/eye/solid.json`,
which `hello-greeter` plays (`greeter/src/ui/eye_vector.rs`). The `winrt/impl` directory is any C++/WinRT projection
with `Windows.UI.Composition.0.h` (vtable order of the interfaces; the public PDB's thunk names are folded and wrong).
Schema, counts and the OBSERVED/INFERRED ledger: docs/windows-hello-ui.md §6.2.
