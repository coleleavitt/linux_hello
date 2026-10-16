#!/usr/bin/env python3
"""Patch WidgetSpinner so stable XBF indices WinUI 3 deleted resolve to placeholders (see README.md)."""
import pathlib

ws = pathlib.Path(__file__).parent / "muix/dxaml/xcp/tools/XbfParser/WidgetSpinner/Metadata"
THROW_TYPE = ('                throw new ArgumentException(string.Format("The StableXbfTypeIndex {0} does not match any known type", index), nameof(index));')
FIX_TYPE = ('                xamlTypeInfo = new XamlTypeInfo("Windows.UI.Xaml.Controls." + index.ToString().Replace("_Deleted0", ""), StableXbfTypeIndex.UnknownType, XamlTypeFlags.None);')
THROW_PROP = ('                throw new ArgumentException(string.Format("The StableXbfPropertyIndex {0} does not match any known property", index), nameof(index));')
FIX_PROP = """                var enumName = index.ToString().Replace("_Deleted0", "");
                var us = enumName.IndexOf('_');
                var typeName = us > 0 ? enumName.Substring(0, us) : "Unknown";
                StableXbfTypeIndex declIndex;
                if (!Enum.TryParse(typeName, out declIndex)) declIndex = StableXbfTypeIndex.UnknownType;
                xamlPropertyInfo = new XamlPropertyInfo("Windows.UI.Xaml." + typeName + "." + enumName.Substring(us + 1), StableXbfTypeIndex.UnknownType, declIndex, XamlPropertyFlags.None);"""

for name, old, new in [("XamlTypeRegistry.cs", THROW_TYPE, FIX_TYPE), ("XamlPropertyRegistry.cs", THROW_PROP, FIX_PROP)]:
    p = ws / name
    s = p.read_text()
    assert old in s, name
    p.write_text(s.replace(old, new))
print("patched")
