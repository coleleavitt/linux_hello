using Microsoft.Xaml.WidgetSpinner;
using Microsoft.Xaml.WidgetSpinner.Model;
using Microsoft.Xaml.WidgetSpinner.Metadata;
using Microsoft.Xaml.WidgetSpinner.Metadata.KnownTypes;
using System.Text;
using System.Globalization;

static class Dumper
{
    static string Short(string full) { var i = full.LastIndexOf('.'); return i < 0 ? full : full[(i + 1)..]; }
    static string F(float v) => v.ToString("0.###", CultureInfo.InvariantCulture);
    public static string Val(object v)
    {
        switch (v)
        {
            case null: return "{x:Null}";
            case string s: return s;
            case bool b: return b ? "True" : "False";
            case float f: return F(f);
            case double d: return d.ToString("0.###", CultureInfo.InvariantCulture);
            case int i: return i.ToString();
            case long l: return l.ToString();
            case Thickness t: return (t.Left == t.Right && t.Top == t.Bottom) ? (t.Left == t.Top ? F(t.Left) : $"{F(t.Left)},{F(t.Top)}") : $"{F(t.Left)},{F(t.Top)},{F(t.Right)},{F(t.Bottom)}";
            case GridLength g: return g.GridUnitType == GridUnitType.Auto ? "Auto" : g.GridUnitType == GridUnitType.Star ? (g.Value == 1 ? "*" : g.Value + "*") : g.Value.ToString(CultureInfo.InvariantCulture);
            case Color c: return $"#{c.A:X2}{c.R:X2}{c.G:X2}{c.B:X2}";
            case EnumValue e: return $"{Short(e.EnumName)}({e.Value})";
            case TypeConvertedValue tc: return Val(tc.SourceValue);
            case XamlProperty p: return p.FullName;
            case XamlType ty: return ty.FullName;
            case XamlStaticResourceExtension sr: return "{StaticResource " + sr.ResourceKey + "}";
            case XamlThemeResourceExtension tr: return "{ThemeResource " + tr.ResourceKey + "}";
            case XamlTemplateBindingExtension tb: return "{TemplateBinding " + (tb.Property?.Name ?? "?") + "}";
            case XamlCustomResourceExtension cr: return "{CustomResource " + cr.ResourceKey + "}";
            case XamlNullExtension: return "{x:Null}";
            case XamlObject o: return null;
            default: return v.ToString();
        }
    }
    public static void Dump(StringBuilder sb, XamlObject o, int depth, string key = null)
    {
        string ind = new string(' ', depth * 2);
        var simple = new List<string>(); var complex = new List<(string, object)>();
        if (key != null) simple.Add($"x:Key=\"{key}\"");
        foreach (var kv in o.PropertyStore)
        {
            string name = kv.Key.IsAttached || (kv.Key.DeclaringType != null && !kv.Key.DeclaringType.IsAssignableFrom(o.Type) && !kv.Key.DeclaringType.Equals(o.Type)) ? Short(kv.Key.DeclaringType?.FullName ?? "") + "." + kv.Key.Name : kv.Key.Name;
            var sv = Val(kv.Value);
            if (sv != null) simple.Add($"{name}=\"{sv.Replace("\"", "&quot;")}\""); else complex.Add((name, kv.Value));
        }
        sb.Append(ind).Append('<').Append(Short(o.Type.FullName));
        foreach (var s in simple) sb.Append(' ').Append(s);
        if (complex.Count == 0 && o.CollectionItems.Count == 0 && o.DictionaryItems.Count == 0) { sb.AppendLine(" />"); return; }
        sb.AppendLine(">");
        foreach (var (name, val) in complex)
        {
            sb.Append(ind).Append("  <").Append(Short(o.Type.FullName)).Append('.').Append(name).AppendLine(">");
            if (val is XamlObject xo) Dump(sb, xo, depth + 2);
            sb.Append(ind).Append("  </").Append(Short(o.Type.FullName)).Append('.').Append(name).AppendLine(">");
        }
        foreach (var kv in o.DictionaryItems)
        {
            if (kv.Value is XamlObject xo) Dump(sb, xo, depth + 1, Val(kv.Key)); else sb.Append(ind).AppendLine($"  <!-- {Val(kv.Key)} = {Val(kv.Value)} -->");
        }
        foreach (var it in o.CollectionItems)
        {
            if (it is XamlObject xo) Dump(sb, xo, depth + 1); else sb.Append(ind).AppendLine($"  {Val(it)}");
        }
        sb.Append(ind).Append("</").Append(Short(o.Type.FullName)).AppendLine(">");
    }
}

class Program
{
    static void Main(string[] args)
    {
        ErrorService.Instance.Error += (s, e) => Console.Error.WriteLine("ERROR: " + e.Message);
        ErrorService.Instance.Warning += (s, e) => Console.Error.WriteLine("warn: " + e.Message);
        var ws = new WidgetSpinner();
        foreach (var path in args)
        {
            var file = ws.DeserializeXbfFile(path);
            if (file == null) { Console.WriteLine("FAIL deserialize " + path); continue; }
            var root = ws.CreateObjectGraph(file);
            if (root == null) { Console.WriteLine("FAIL graph " + path); continue; }
            var sb = new StringBuilder();
            Dumper.Dump(sb, root, 0);
            File.WriteAllText(path + ".xaml", sb.ToString());
            Console.WriteLine("ok " + path);
        }
    }
}
