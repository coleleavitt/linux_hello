#!/bin/sh
# Download the four ONNX models linux_hello uses into models/ and verify their SHA-256.
set -eu
cd "$(dirname "$0")/../models"
mkdir -p antispoof
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# InsightFace buffalo_l v0.7: SCRFD-10G detector and ArcFace R50 (WebFace600K) recognizer
curl -fL -o "$tmp/buffalo_l.zip" https://github.com/deepinsight/insightface/releases/download/v0.7/buffalo_l.zip
unzip -q -o -j "$tmp/buffalo_l.zip" det_10g.onnx w600k_r50.onnx -d .

# FaceNox MiniFASNetV2-SE liveness model (98.20 checkpoint)
curl -fL -o antispoof/facenox_antispoof.onnx \
  https://raw.githubusercontent.com/facenox/face-antispoof-onnx/2f8dd93dd0fd9d120f08e83d9020719f67ffdb68/models/best/98.20/best_model.onnx

# Silent-Face MiniFASNetV2 2.7_80x80, converted to ONNX
curl -fL -o antispoof/MiniFASNetV2.onnx \
  https://huggingface.co/garciafido/minifasnet-v2-anti-spoofing-onnx/resolve/main/minifasnet_v2.onnx

sha256sum -c <<'EOF'
5838f7fe053675b1c7a08b633df49e7af5495cee0493c7dcf6697200b85b5b91  det_10g.onnx
4c06341c33c2ca1f86781dab0e829f88ad5b64be9fba56e56bc9ebdefc619e43  w600k_r50.onnx
af2381b88f38769222ed93379e12444e2a50814575de1c46170de570c55a42b6  antispoof/facenox_antispoof.onnx
d7b3cd9ba8a7ceb13baa8c4720902e27ca3112eff52f926c08804af6b6eecc7b  antispoof/MiniFASNetV2.onnx
EOF
