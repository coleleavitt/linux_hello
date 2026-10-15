
import sys, os, json, hashlib, pefile
out, srcs = sys.argv[1], sys.argv[2:]
STD = set(pefile.RESOURCE_TYPE.values())
def kind(b):
    h = b[:64]
    if b"conv_net_v1" in h: return "convnet"
    if h[4:8] == b"ORTM": return "ort"
    if h[:4] == b"\xce\xfa\x0a\x00": return "cefa"
    if h[:4] == b"LDMF": return "ldmf"
    if h[:1] == b"\x08" and (b"pytorch" in b[:200] or b"tf2onnx" in b[:200] or b"onnx" in b[:200].lower()): return "onnx"
    return "bin"
manifest = []
for src in srcs:
    pe = pefile.PE(src, fast_load=True); pe.parse_data_directories([pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_RESOURCE"]])
    if not hasattr(pe, "DIRECTORY_ENTRY_RESOURCE"): continue
    stem = os.path.splitext(os.path.basename(src))[0]
    for t in pe.DIRECTORY_ENTRY_RESOURCE.entries:
        if t.id in STD and t.name is None: continue
        tname = str(t.name) if t.name else f"TYPE{t.id}"
        for n in t.directory.entries:
            nname = str(n.name) if n.name else str(n.id)
            for lang in n.directory.entries:
                e = lang.data.struct
                data = pe.get_memory_mapped_image()[e.OffsetToData:e.OffsetToData + e.Size]
                k = kind(data)
                rel = f"{stem}/{tname}_{nname}.{k}"
                os.makedirs(os.path.join(out, stem), exist_ok=True)
                open(os.path.join(out, rel), "wb").write(data)
                manifest.append({"file": rel, "source": os.path.basename(src), "type": tname, "id": nname,
                                 "format": k, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()})
# validate ONNX / ORT with onnxruntime
try:
    import onnxruntime as ort
    for m in manifest:
        if m["format"] in ("onnx", "ort"):
            try:
                s = ort.InferenceSession(os.path.join(out, m["file"]), providers=["CPUExecutionProvider"])
                m["loads"] = True
                m["inputs"] = [(i.name, i.shape, i.type) for i in s.get_inputs()]
                m["outputs"] = [(o.name, o.shape, o.type) for o in s.get_outputs()]
            except Exception as ex:
                m["loads"] = False; m["error"] = str(ex)[:300]
except ImportError:
    pass
json.dump(manifest, open(os.path.join(out, "manifest.json"), "w"), indent=1)
print(len(manifest), "resources")
