"""HelloFaceCipher::EncryptDecrypt (FaceProcessorCore.dll 0x18001b104, FaceProcessor.dll 0x180061200,
FaceRecognitionEngineAdapter.dll sub_180096E44): a keyless-in-practice XOR obfuscation.

    out[i] = in[i] ^ KEY[i % 17] ^ (i & 0xFF)

KEY is 4 hard-coded dwords plus a zero byte. The operation is its own inverse.
Used on whole MODEL_FILES resources that are loaded as ONNX / ORT models by
OnnxTester::LoadAndInitialize (FaceDetectorResources 110-113) and
FaceOcclusionDetector::LoadOnnxOcclusionModel (engine core resource 120). The
`.cefa` recognition containers do NOT go through it.

CLI (from tools/winhello_models):  .venv/bin/python -m cipher IN... -o OUTDIR
writes OUTDIR/<stem>.onnx or <stem>.ort depending on the decrypted magic.
"""
import argparse
import struct
from pathlib import Path

import numpy as np

KEY = struct.pack("<4i", -1084033398, 172859493, 950223013, 1876138409) + b"\x00"


def decrypt(buf: bytes) -> bytes:
    a = np.frombuffer(buf, np.uint8)
    i = np.arange(a.size)
    k = np.frombuffer(KEY, np.uint8)[i % len(KEY)]
    return (a ^ k ^ (i & 0xFF).astype(np.uint8)).tobytes()


encrypt = decrypt


def main(argv=None) -> None:
    ap = argparse.ArgumentParser(prog="cipher", description="undo HelloFaceCipher on MODEL_FILES resources")
    ap.add_argument("files", nargs="+", type=Path)
    ap.add_argument("-o", "--out", type=Path, required=True)
    a = ap.parse_args(argv)
    a.out.mkdir(parents=True, exist_ok=True)
    for f in a.files:
        d = decrypt(f.read_bytes())
        ext = "ort" if d[4:8] == b"ORTM" else "onnx"
        (a.out / f"{f.stem}.{ext}").write_bytes(d)
        print(f"{f.name} -> {f.stem}.{ext}")


if __name__ == "__main__":
    main()
