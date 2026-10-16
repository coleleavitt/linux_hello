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
