#!/usr/bin/env python3
"""Extract the Windows Hello / LogonUI user-interface assets from a Windows 11 ISO (or install.wim).

Everything this writes is Microsoft-proprietary (Windows EULA): the output directory is git-ignored
(`models/windows_hello/*`) and is for personal, local use on the machine that owns the Windows licence.
Never commit or redistribute it. See docs/windows-hello-ui.md for the inventory and what each file is.

usage: extract_windows_hello_assets.py <ISO | install.wim | extracted-image-root> <out_dir>
                                       [--image 6] [--work DIR] [--xbf2xaml EXE]

  ISO                 Windows11_Client_x64_en-us_26300_9457.iso (7zz pulls sources/install.wim out of it)
  install.wim         the WIM itself; --image selects the edition (6 = Windows 11 Pro in that ISO)
  extracted root      a directory that already contains Windows/ and ProgramData/ (skips 7zz)
  --work              where the WIM and the extracted files are kept (default: <out_dir>/../wim_work)
  --xbf2xaml          optional tools/winhello_ui/xbf2xaml binary: decompiles the LogonUI XBF layouts to XAML

Needs 7zz (p7zip 7-Zip 2x), pefile, Pillow, and rsvg-convert (for hello.png). Run it with the
tools/winhello_models/.venv interpreter.
"""
import argparse, hashlib, json, os, re, shutil, struct, subprocess, sys

import pefile

# WIM paths (relative to the image root), grouped by what the greeter needs. "licence" is the same for all:
# Microsoft proprietary, Windows EULA, local personal use only.
FILES = {
    "fonts": [
        "Windows/Fonts/SegUIVar.ttf",      # Segoe UI Variable (LogonFontFamily; everything on the sign-in screen)
        "Windows/Fonts/segoeui.ttf", "Windows/Fonts/segoeuib.ttf", "Windows/Fonts/segoeuii.ttf", "Windows/Fonts/segoeuiz.ttf",
        "Windows/Fonts/segoeuil.ttf", "Windows/Fonts/seguili.ttf", "Windows/Fonts/segoeuisl.ttf", "Windows/Fonts/seguisli.ttf",
        "Windows/Fonts/seguisb.ttf", "Windows/Fonts/seguisbi.ttf", "Windows/Fonts/seguibl.ttf", "Windows/Fonts/seguibli.ttf",
        "Windows/Fonts/SegoeIcons.ttf",    # "Segoe Fluent Icons" (submit arrow, power/accessibility/network, default avatar)
        "Windows/Fonts/segmdl2.ttf",       # "Segoe MDL2 Assets" (secure-gesture icon on the CAD lock screen)
        "Windows/Fonts/seguisym.ttf",      # "Segoe UI Symbol" (PLAP disconnect glyph)
        "Windows/SystemResources/Windows.UI.BioFeedback/Fonts/NUIMDL2.ttf",  # "NUIMDL2Assets": Hello eye animation frames
    ],
    "screen": [f"Windows/Web/Screen/img{n}.jpg" for n in range(100, 106)],
    "wallpaper": [
        "Windows/Web/Wallpaper/Windows/img0.jpg", "Windows/Web/Wallpaper/Windows/img19.jpg",
        "Windows/Web/4K/Wallpaper/Windows/img0_1920x1200.jpg", "Windows/Web/4K/Wallpaper/Windows/img19_1920x1200.jpg",
        "Windows/Web/Wallpaper/Spotlight/img14.jpg", "Windows/Web/Wallpaper/Spotlight/img50.jpg",
        "Windows/Resources/Themes/spotlight.theme",
    ],
    "user-pictures": [
        "ProgramData/Microsoft/User Account Pictures/user.png", "ProgramData/Microsoft/User Account Pictures/user.bmp",
        "ProgramData/Microsoft/User Account Pictures/user-192.png", "ProgramData/Microsoft/User Account Pictures/user-48.png",
        "ProgramData/Microsoft/User Account Pictures/user-40.png", "ProgramData/Microsoft/User Account Pictures/user-32.png",
        "ProgramData/Microsoft/User Account Pictures/guest.png", "ProgramData/Microsoft/User Account Pictures/guest.bmp",
    ],
    "hello": [
        "Windows/System32/@WindowsHelloFaceToastIcon.png",
        "Windows/System32/@WindowsHelloFaceToastIconRejuv.png",
        "Windows/System32/@facial-recognition-windows-hello.gif",
        "Windows/System32/@facial-recognition-windows-hello-rejuv.gif",
        "Windows/SystemApps/MicrosoftWindows.Client.CBS_cw5n1h2txyewy/media/HelloFaceAnimation.gif",
    ],
    "sounds": ["Windows/Media/Windows Logon.wav", "Windows/Media/Windows Unlock.wav", "Windows/Media/Windows Logoff Sound.wav"],
    # binaries the resources below are pulled from (kept so the extraction is repeatable without the WIM)
    "bin": [
        "Windows/System32/Windows.UI.Logon.dll", "Windows/System32/LogonUI.exe",
        "Windows/SystemResources/Windows.UI.Logon/Windows.UI.Logon.pri",
        "Windows/SystemResources/Windows.UI.Logon/pris/Windows.UI.Logon.en-US.pri",
        "Windows/System32/en-US/FaceCredentialProvider.dll.mui", "Windows/System32/en-US/BioCredProv.dll.mui",
        "Windows/System32/en-US/credprovhost.dll.mui", "Windows/System32/en-US/authui.dll.mui",
        "Windows/System32/en-US/winlogon.exe.mui", "Windows/System32/en-US/LockScreenContent.dll.mui",
        "Windows/System32/en-US/LockAppBroker.dll.mui",
        "Windows/System32/facecredentialprovider.dll", "Windows/System32/BioCredProv.dll",
        "Windows/System32/FontGlyphAnimator.dll", "Windows/System32/Windows.UI.BioFeedback.dll",
        "Windows/SystemResources/Windows.UI.BioFeedback/Windows.UI.BioFeedback.pri",
        "Windows/SystemResources/Windows.UI.BioFeedback/pris/Windows.UI.BioFeedback.en-US.pri",
    ],
}
MUI_STRINGS = ["FaceCredentialProvider.dll.mui", "BioCredProv.dll.mui", "credprovhost.dll.mui", "authui.dll.mui",
               "winlogon.exe.mui", "LockScreenContent.dll.mui", "LockAppBroker.dll.mui"]


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def run(cmd):
    print("+", " ".join(cmd), flush=True)
    subprocess.run(cmd, check=True)


def image_root(src, work, image):
    """Return a directory containing Windows/ for the chosen WIM image, extracting with 7zz when needed."""
    if os.path.isdir(src):
        return src
    os.makedirs(work, exist_ok=True)
    wim = src
    if src.lower().endswith(".iso"):
        wim = os.path.join(work, "install.wim")
        if not os.path.exists(wim):
            run(["7zz", "e", "-y", src, "sources/install.wim", f"-o{work}"])
    listfile = os.path.join(work, "assets_list.txt")
    with open(listfile, "w") as f:
        for group in FILES.values():
            for p in group:
                f.write(f"{image}/{p}\n")
    out = os.path.join(work, "wim")
    run(["7zz", "x", "-y", wim, f"-i@{listfile}", f"-o{out}"])
    return os.path.join(out, str(image))


# --- PE resources -------------------------------------------------------------------------------------

def pe_resources(path):
    pe = pefile.PE(path, fast_load=True)
    pe.parse_data_directories([pefile.DIRECTORY_ENTRY["IMAGE_DIRECTORY_ENTRY_RESOURCE"]])
    img = pe.get_memory_mapped_image()
    for t in getattr(pe, "DIRECTORY_ENTRY_RESOURCE", None).entries if hasattr(pe, "DIRECTORY_ENTRY_RESOURCE") else []:
        tname = str(t.name) if t.name else pefile.RESOURCE_TYPE.get(t.id, f"TYPE{t.id}")
        for n in t.directory.entries:
            nname = str(n.name) if n.name else n.id
            for lang in n.directory.entries:
                e = lang.data.struct
                yield tname, nname, lang.id, bytes(img[e.OffsetToData:e.OffsetToData + e.Size])


def string_table(data, block):
    out, o = {}, 0
    for i in range(16):
        n = struct.unpack_from("<H", data, o)[0]
        o += 2
        if n:
            out[(block - 1) * 16 + i] = data[o:o + 2 * n].decode("utf-16-le")
        o += 2 * n
    return out


def mui_strings(path):
    strings = {}
    for tname, nname, _lang, data in pe_resources(path):
        if tname == "RT_STRING":
            strings.update(string_table(data, int(nname)))
    return dict(sorted(strings.items()))


# --- PRI (Windows resource index) ----------------------------------------------------------------------

def pri_sections(pri):
    toc_off, sec_off, nsec = struct.unpack_from("<IIH", pri, 16)
    secs = {}
    for i in range(nsec):
        o = toc_off + 32 * i
        tag = pri[o:o + 16].rstrip(b"\0").decode().strip()
        _flags, _sflags, _qual, off, ln = struct.unpack_from("<HHIII", pri, o + 16)
        secs[tag] = pri[sec_off + off + 32: sec_off + off + ln - 8]
    return secs


def pri_dataitems(sec):
    _unk, ns, nb, total = struct.unpack_from("<IHHI", sec, 0)
    o = 12
    strs = [struct.unpack_from("<HH", sec, o + 4 * i) for i in range(ns)]
    o += 4 * ns
    blobs = [struct.unpack_from("<II", sec, o + 8 * i) for i in range(nb)]
    o += 8 * nb
    data = sec[o:o + total]
    return [data[a:a + l] for a, l in strs], [data[a:a + l] for a, l in blobs]


def pri_item_names(sec):
    """[mrm_hschemaex]: full resource names of the items, keyed by item index."""
    o = 8 + 16
    _ver, _unk, _csum, nscopes, nitems = struct.unpack_from("<IIIII", sec, o)
    o += 20
    for _ in range(2):  # unique name, name (UTF-16, NUL-terminated)
        e = sec.index(b"\0\0", o)
        while (e - o) % 2:
            e = sec.index(b"\0\0", e + 1)
        o = e + 2
    o += 2
    _maxpath, _z, total, _ns, _ni, _z2, _unicode_len, ascii_len, _z3 = struct.unpack_from("<HHIIIIIII", sec, o)
    o += 4 + 4 * 7
    o += 8
    entries = [struct.unpack_from("<HHHBBHH", sec, o + 12 * i) for i in range(total)]
    # the NUL-separated ASCII name block sits at the end of the section and no header field gives its
    # start reliably: find it through the longest name
    k = max(range(total), key=lambda i: entries[i][3])
    _p, _fpl, upper, nlen, _flags, noff, _idx = entries[k]
    tables_end = o + 12 * total
    base = None
    for m in re.finditer(rb"\0([A-Za-z0-9_.\[\]:]{%d})\0" % nlen, sec[tables_end:]):
        cand = tables_end + m.start(1) - noff
        if cand >= tables_end and all(sec[cand + e[5]:cand + e[5] + e[3]].isascii() for e in entries):
            base = cand
            break
    names = sec[base:]

    def full(k):
        parts = []
        while k > 0:
            parent, _fpl, _upper, nlen, _flags, noff, _idx = entries[k - 1]
            parts.append(names[noff:noff + nlen].decode("latin1"))
            k = parent
        return "/".join(reversed(parts))

    items = {}
    for k, (_p, _fpl, _u, _nlen, flags, _noff, idx) in enumerate(entries):
        if not flags & 0x10:
            items[idx] = full(k + 1)
    return items


def pri_candidates(secs):
    """item index -> candidate (type byte, data ref) from [mrm_res_map2_]; one candidate per item here."""
    rm = secs["[mrm_res_map2_]"]
    nvt = struct.unpack_from("<H", rm, 10)[0]
    n_i2g, n_g, n_ii = struct.unpack_from("<HHH", rm, 12)
    ncand = struct.unpack_from("<I", rm, 20)[0]
    o = 32 + 8 * nvt
    i2g = [struct.unpack_from("<HH", rm, o + 4 * i) for i in range(n_i2g)]
    o += 4 * n_i2g
    groups = [struct.unpack_from("<HH", rm, o + 4 * i) for i in range(n_g)]
    o += 4 * n_g
    infos = [struct.unpack_from("<HH", rm, o + 4 * i) for i in range(n_ii)]
    o += 4 * n_ii
    cands = [struct.unpack_from("<BBHI", rm, o + 8 * i) for i in range(ncand)]
    out = {}
    for first_item, g in i2g:
        if g < n_g:
            size, first_info = groups[g]
            for j in range(size):
                out[first_item + j] = cands[infos[first_info + j][1]]
        else:
            out[first_item] = cands[infos[g - n_g][1]]
    return out


def pri_values(path):
    """resource name -> bytes for every item of a .pri (strings and file blobs)."""
    pri = open(path, "rb").read()
    secs = pri_sections(pri)
    names = pri_item_names(secs["[mrm_hschemaex]"])
    strs, blobs = pri_dataitems(secs["[mrm_dataitem]"])
    out = {}
    for item, (_t, _vt, _z, ref) in pri_candidates(secs).items():
        if (ref >> 16) != 4:
            continue
        idx = ref & 0xFFFF
        out[names.get(item, f"item{item}")] = strs[idx] if idx < len(strs) else blobs[idx - len(strs)]
    return out


def pri_strings(path):
    out = {}
    for name, b in pri_values(path).items():
        if name.startswith("Files/"):
            continue
        s = b.decode("utf-16-le", "replace") if len(b) > 1 and b[1] == 0 else b.decode("utf-8", "replace")
        out[name] = s.rstrip("\0")
    return out


def pri_files(path):
    return {name[len("Files/"):]: b for name, b in pri_values(path).items() if name.startswith("Files/")}



# --- Hello eye glyph animation (FontGlyphAnimator.dll data + NUIMDL2.ttf frames) ----------------------

def glyph_animation_table(dll_path, map_va=0x18000A5A0):
    """Decode AnimationDataProviderFactory's family table: sequences (glyph ranges) and the transition
    machine. The map VA is for FontGlyphAnimator.dll 10.0.26100.8521; the font URI check guards other builds."""
    pe = pefile.PE(dll_path)
    img = pe.get_memory_mapped_image()
    base = pe.OPTIONAL_HEADER.ImageBase

    def u32(va):
        return struct.unpack_from("<I", img, va - base)[0]

    def u64(va):
        return struct.unpack_from("<Q", img, va - base)[0]

    def wstr(va):
        o = va - base
        e = img.index(b"\0\0", o)
        while (e - o) % 2:
            e = img.index(b"\0\0", e + 1)
        return img[o:e].decode("utf-16-le")

    desc = u64(map_va + 8)
    nfonts, fonts, nseq, seqs, ntr, trs = u32(desc), u64(desc + 8), u32(desc + 16), u64(desc + 24), u32(desc + 32), u64(desc + 40)
    font_uris = [wstr(u64(fonts + 16 * i + 8)) for i in range(nfonts)]
    if not any("NUIMDL2" in u for u in font_uris):
        raise ValueError("FontGlyphAnimator.dll layout not recognised (map VA)")
    sequences = {}
    for i in range(nseq):
        d = seqs + 24 * i
        sid, begin, end, total_ms = u32(d), struct.unpack_from("<H", img, d + 4 - base)[0], struct.unpack_from("<H", img, d + 6 - base)[0], u32(d + 8)
        frames = end - begin + 1
        sequences[sid] = {"first_glyph": f"U+{begin:04X}", "last_glyph": f"U+{end:04X}", "frames": frames,
                          "frame_ms": (total_ms // (frames - 1)) if total_ms and frames > 1 else 33}
    transitions = {}
    for i in range(ntr):
        t = trs + 24 * i
        cur, n, pts = u32(t), u32(t + 8), u64(t + 16)
        rules = []
        for j in range(n):
            p = pts + 16 * j
            want = struct.unpack_from("<i", img, p - base)[0]
            hold = u32(p + 12)
            rules.append({"wanted": want, "play": u32(p + 4), "hold_ms": None if hold == 0xFFFFFFFF else hold})
        transitions[cur] = rules
    return {"font_uris": font_uris, "sequences": sequences, "transitions": transitions}


def render_glyph_frames(ttf, sequences, out_dir, px=60, canvas=72):
    """One PNG per frame of every sequence: white glyph on transparent, FontSize 60 like
    BioFeedbackUserControlAnimationTextFontSize (CommonStyles.xaml)."""
    from PIL import Image, ImageDraw, ImageFont
    font = ImageFont.truetype(ttf, px)
    for sid, seq in sequences.items():
        d = os.path.join(out_dir, f"seq{sid:02d}")
        os.makedirs(d, exist_ok=True)
        first = int(seq["first_glyph"][2:], 16)
        for f in range(seq["frames"]):
            im = Image.new("RGBA", (canvas, canvas), (0, 0, 0, 0))
            ImageDraw.Draw(im).text((canvas / 2, canvas / 2), chr(first + f), fill=(255, 255, 255, 255), font=font, anchor="mm")
            im.save(os.path.join(d, f"{f:03d}.png"))


# --- main ----------------------------------------------------------------------------------------------

def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("source")
    ap.add_argument("out")
    ap.add_argument("--image", type=int, default=6)
    ap.add_argument("--work")
    ap.add_argument("--xbf2xaml")
    args = ap.parse_args()
    out = os.path.abspath(args.out)
    work = args.work or os.path.join(os.path.dirname(out), "wim_work")
    root = image_root(args.source, work, args.image)

    manifest = []

    def add(rel, source, note, resource=None):
        p = os.path.join(out, rel)
        manifest.append({"file": rel, "source": source, "resource": resource, "size": os.path.getsize(p),
                         "sha256": sha256(p), "what": note,
                         "licence": "Microsoft proprietary (Windows EULA); local personal use only, never commit"})

    for group, paths in FILES.items():
        for rel in paths:
            src = os.path.join(root, rel)
            if not os.path.exists(src):
                print("missing in image:", rel)
                continue
            dst = os.path.join(out, group, os.path.basename(rel))
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            shutil.copy2(src, dst)
            add(os.path.relpath(dst, out), rel, group)

    # credential-tile icons: SVG in the MUI, 72x72 DIB in the DLL (RT_BITMAP has no BITMAPFILEHEADER)
    hello = os.path.join(out, "hello")
    os.makedirs(hello, exist_ok=True)
    for mui, dll, stem, note in [
        ("FaceCredentialProvider.dll.mui", "facecredentialprovider.dll", "hello-face",
         "Windows Hello face icon (credential tile / 'Sign-in options' button)"),
        ("BioCredProv.dll.mui", "BioCredProv.dll", "fingerprint", "fingerprint credential-tile icon"),
    ]:
        for tname, nname, _lang, data in pe_resources(os.path.join(out, "bin", mui)):
            if tname == "RT_HTML":
                p = os.path.join(hello, f"{stem}.svg")
                open(p, "wb").write(data)
                add(f"hello/{stem}.svg", f"Windows/System32/en-US/{mui}", note, f"RT_HTML {nname}")
                if shutil.which("rsvg-convert"):
                    png = os.path.join(hello, f"{stem}.png")
                    run(["rsvg-convert", "-w", "256", "-h", "256", "--background-color=none", p, "-o", png])
                    # the SVG has no fill: paint it white like LogonUI (the theme-adapted logo)
                    from PIL import Image
                    im = Image.open(png).convert("RGBA")
                    a = im.getchannel("A")
                    Image.merge("RGBA", (a.point(lambda v: 255), a.point(lambda v: 255), a.point(lambda v: 255), a)).save(png)
                    add(f"hello/{stem}.png", f"Windows/System32/en-US/{mui}", note + " (rendered 256 px, white)", f"RT_HTML {nname}")
        for tname, nname, _lang, data in pe_resources(os.path.join(out, "bin", dll)):
            if tname == "RT_BITMAP":
                hdr = struct.unpack_from("<IiiHHII", data, 0)
                bmp = b"BM" + struct.pack("<IHHI", 14 + len(data), 0, 0, 14 + hdr[0]) + data
                p = os.path.join(hello, f"{stem}-tile-72.bmp")
                open(p, "wb").write(bmp)
                add(f"hello/{stem}-tile-72.bmp", f"Windows/System32/{dll}", "legacy 72x72 credential tile image (CPFT_TILE_IMAGE)", f"RT_BITMAP {nname}")

    # string tables
    strings_dir = os.path.join(out, "strings")
    os.makedirs(strings_dir, exist_ok=True)
    for mui in MUI_STRINGS:
        p = os.path.join(out, "bin", mui)
        if os.path.exists(p):
            dst = os.path.join(strings_dir, mui.replace(".mui", "") + ".json")
            json.dump(mui_strings(p), open(dst, "w"), indent=1, ensure_ascii=False)
            add(f"strings/{os.path.basename(dst)}", f"Windows/System32/en-US/{mui}", "RT_STRING table (id -> text)")
    for pri, stem in [("Windows.UI.Logon.en-US.pri", "Windows.UI.Logon"), ("Windows.UI.BioFeedback.en-US.pri", "Windows.UI.BioFeedback")]:
        p = os.path.join(out, "bin", pri)
        if os.path.exists(p):
            dst = os.path.join(strings_dir, stem + ".json")
            json.dump(pri_strings(p), open(dst, "w"), indent=1, ensure_ascii=False)
            add(f"strings/{stem}.json", f"Windows/SystemResources/{stem}/pris/{pri}", "PRI string resources (name -> text)")

    # LogonUI layouts: compiled XAML (XBF v2.1) and the images embedded in the PRI
    logon = os.path.join(out, "logonui")
    os.makedirs(logon, exist_ok=True)
    xbfs = []
    for name, data in pri_files(os.path.join(out, "bin", "Windows.UI.Logon.pri")).items():
        p = os.path.join(logon, name.replace("/", "_"))
        open(p, "wb").write(data)
        add(f"logonui/{os.path.basename(p)}", "Windows/SystemResources/Windows.UI.Logon/Windows.UI.Logon.pri",
            "LogonUI XAML layout (XBF 2.1)" if name.endswith(".xbf") else "LogonUI image", f"Files/{name}")
        if name.endswith(".xbf"):
            xbfs.append(p)
    if args.xbf2xaml and xbfs:
        run([args.xbf2xaml] + xbfs)
        for p in xbfs:
            if os.path.exists(p + ".xaml"):
                add(f"logonui/{os.path.basename(p)}.xaml", "decompiled from the .xbf next to it", "LogonUI layout as XAML")

    # Hello eye: the glyph-frame animation LogonUI plays when the Lottie path is off (Windows 10 look, and
    # the exact frames/timing of the fallback on Windows 11); see docs/windows-hello-ui.md section 1.3
    eye = os.path.join(hello, "eye")
    table = glyph_animation_table(os.path.join(out, "bin", "FontGlyphAnimator.dll"))
    os.makedirs(eye, exist_ok=True)
    json.dump(table, open(os.path.join(eye, "animation.json"), "w"), indent=1)
    add("hello/eye/animation.json", "Windows/System32/FontGlyphAnimator.dll", "glyph animation sequences + transition table",
        "AnimationDataProviderFactory::CreateAnimationDataProvider map")
    render_glyph_frames(os.path.join(out, "fonts", "NUIMDL2.ttf"), table["sequences"], eye)
    for sid, seq in table["sequences"].items():
        for f in range(seq["frames"]):
            add(f"hello/eye/seq{sid:02d}/{f:03d}.png", "Windows/SystemResources/Windows.UI.BioFeedback/Fonts/NUIMDL2.ttf",
                f"eye animation frame {f} of sequence {sid} (rendered)", f"glyph {int(seq['first_glyph'][2:], 16) + f:04X}")

    manifest.sort(key=lambda m: m["file"])
    json.dump({"source_image": f"install.wim image {args.image}", "entries": manifest},
              open(os.path.join(out, "manifest.json"), "w"), indent=1, ensure_ascii=False)
    print(len(manifest), "files ->", out)


if __name__ == "__main__":
    main()
