"""NumPy re-implementation of DNNTestLib inference (FaceRecognitionEngineAdapter.dll).

Activations are kept in DNNTestLib's own memory order (storage type 0:
[y][x][c], i.e. HWC; index fn sub_180025188) so this is an independent check
of the NCHW ONNX graph.  Every layer follows the decompiled forward routine:

  image_data  sub_180017AA0  u8 -> x/127.5 - 1  (or u8 - mean if a mean blob exists)
  conv        sub_180018A30 / sub_1800BEE00   cross-correlation, zero padding, + bias
  relu        sub_180019170   max(x, 0)
  pool        sub_180019470 (max) / sub_180024D50 (avg); output size
              ceil((in - k) / s) + 1 (sub_180015AE0); windows clipped at the
              far border, avg divides by the clipped window area
  padding     sub_180024BB0   zero padding (pA on x, pB on y)
  concat      sub_180017FE0   channel concat in input order
  flatten     sub_180024410   output index c*H*W + y*W + x (CHW order)
  fc          sub_1800246A0   out = bias + sum_i x[i] * W[i, :]
"""
from __future__ import annotations

import math

import numpy as np

from .format import ConvNet, Layer


def _conv(x, L: Layer):
    w = L.weight.as_bacn()  # (kh, kw, cin, cout)
    kh, kw, cin, cout = w.shape
    pa, pb = L.conv_pad
    sa, sb = L.conv_stride
    H, W, C = x.shape
    assert C == cin, (L.name, C, cin)
    xp = np.zeros((H + 2 * pb, W + 2 * pa, C), np.float32)
    xp[pb : pb + H, pa : pa + W] = x
    oh = (H + 2 * pb - kh) // sb + 1
    ow = (W + 2 * pa - kw) // sa + 1
    out = np.zeros((oh, ow, cout), np.float64)
    for dy in range(kh):
        for dx in range(kw):
            patch = xp[dy : dy + sb * (oh - 1) + 1 : sb, dx : dx + sa * (ow - 1) + 1 : sa]
            out += patch.astype(np.float64) @ w[dy, dx].astype(np.float64)
    b = L.bias.as_bacn().reshape(-1) if L.bias is not None and L.bias.data is not None else 0.0
    return (out + b).astype(np.float32)


def _pool(x, L: Layer):
    sa, sb = L.pool_stride
    ka, kb = L.pool_kernel
    H, W, C = x.shape
    # sub_180015AE0 uses the x kernel size for both dims; all shipped kernels are square.
    oh = int(math.ceil((H - ka) / sb)) + 1
    ow = int(math.ceil((W - ka) / sa)) + 1
    out = np.empty((oh, ow, C), np.float32)
    for oy in range(oh):
        y0 = oy * sb
        y1 = min(y0 + kb, H)
        for ox in range(ow):
            x0 = ox * sa
            x1 = min(x0 + ka, W)
            win = x[y0:y1, x0:x1]
            if L.subtype == "max":
                out[oy, ox] = win.max(axis=(0, 1))
            else:
                out[oy, ox] = win.sum(axis=(0, 1), dtype=np.float64) / ((y1 - y0) * (x1 - x0))
    return out


def preprocess(net: ConvNet, image_u8: np.ndarray) -> np.ndarray:
    """image_u8: (H, W) or (H, W, C) uint8 -> (H, W, C) float32 as ImageDataLayer does."""
    img = np.asarray(image_u8)
    if img.ndim == 2:
        img = img[:, :, None]
    L = net.input_layer
    if L.mean is not None and L.mean.data is not None:
        m = L.mean.as_bacn()[..., 0]  # (H, W, C)
        return img.astype(np.float32) - m
    return img.astype(np.float32) / np.float32(127.5) - np.float32(1.0)


def forward(net: ConvNet, image_u8: np.ndarray, keep_all: bool = False):
    """Run the net; returns the last layer output (flattened) or a dict of all layer outputs."""
    acts: dict[str, np.ndarray] = {}
    for L in net.layers:
        ins = [acts[n] for n in L.inputs]
        t = L.type
        if t == "image_data":
            y = preprocess(net, image_u8)
        elif t == "conv":
            y = _conv(ins[0], L)
        elif t == "neuron":
            y = np.maximum(ins[0], 0) if L.subtype == "relu" else 1 / (1 + np.exp(-ins[0]))
        elif t == "pool":
            y = _pool(ins[0], L)
        elif t == "padding":
            pa, pb = L.ints
            y = np.pad(ins[0], ((pb, pb), (pa, pa), (0, 0)))
        elif t == "concat":
            y = np.concatenate(ins, axis=2)
        elif t == "flatten":
            y = ins[0].transpose(2, 0, 1).reshape(1, 1, -1)
        elif t == "fc":
            w = L.weight.as_bacn().reshape(-1, L.weight.dims[0])  # (in, out)
            v = ins[0].reshape(-1).astype(np.float64)
            y = (L.bias.as_bacn().reshape(-1) + v @ w).astype(np.float32).reshape(1, 1, -1)
        elif t == "dropout":
            y = ins[0]
        elif t == "softmax":
            e = np.exp(ins[0] - ins[0].max(axis=-1, keepdims=True))
            y = e / e.sum(axis=-1, keepdims=True)
        elif t == "eltwise_max":
            y = np.maximum.reduce(ins)
        else:
            raise NotImplementedError(t)
        acts[L.name] = y
    return acts if keep_all else acts[net.layers[-1].name].reshape(-1)


def l2_normalize(v: np.ndarray) -> np.ndarray:
    """sub_18004178C: divide by the L2 norm unless it is zero."""
    s = float(np.sum(v.astype(np.float32) ** 2))
    return v / np.float32(math.sqrt(s)) if s != 0.0 else v


def softmax(v: np.ndarray) -> np.ndarray:
    """sub_18003C0B0: exp(x - max) / sum."""
    e = np.exp(v - v.max())
    return e / e.sum()


def classify(net: ConvNet, image_u8: np.ndarray) -> np.ndarray:
    """Full engine path for one anti-spoofing net: returns [p0, p1].

    Slots 11/12 (RunDualIRAntiSpoofingClassifier 0x18004a6d0): 512-d net output,
    L2-normalised, times the 2x512 trailer matrix (sub_18003F8E8, no bias), softmax.
    Slots 13/14/15: the net already ends in an fc with 2 outputs; softmax.
    """
    out = forward(net, image_u8)
    head = net.head_weights()
    if head is not None:
        out = head.astype(np.float32) @ l2_normalize(out.astype(np.float32))
    return softmax(out.astype(np.float64))
