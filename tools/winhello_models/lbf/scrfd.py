"""Minimal SCRFD (insightface det_10g.onnx) face detector used only to build validation inputs."""
from __future__ import annotations

import numpy as np
import onnxruntime as ort


class Scrfd:
    def __init__(self, path: str, size: int = 640):
        self.sess = ort.InferenceSession(path, providers=["CPUExecutionProvider"])
        self.size = size
        self.input = self.sess.get_inputs()[0].name

    def detect(self, img: np.ndarray, thresh: float = 0.5, nms: float = 0.4):
        """img: HxW gray or HxWx3 RGB uint8. Returns list of (box[x1,y1,x2,y2], score, kps[5,2])."""
        if img.ndim == 2:
            img = np.repeat(img[:, :, None], 3, 2)
        h, w = img.shape[:2]
        s = self.size / max(h, w)
        nh, nw = int(round(h * s)), int(round(w * s))
        from PIL import Image
        rs = np.asarray(Image.fromarray(img).resize((nw, nh), Image.BILINEAR))
        canvas = np.zeros((self.size, self.size, 3), np.uint8)
        canvas[:nh, :nw] = rs
        blob = ((canvas.astype(np.float32) - 127.5) / 128.0).transpose(2, 0, 1)[None]
        outs = self.sess.run(None, {self.input: blob})
        boxes, scores, kpss = [], [], []
        for i, stride in enumerate((8, 16, 32)):
            sc, bb, kp = outs[i].reshape(-1), outs[i + 3].reshape(-1, 4) * stride, outs[i + 6].reshape(-1, 10) * stride
            fh = fw = self.size // stride
            yy, xx = np.mgrid[:fh, :fw]
            centers = np.stack([xx, yy], -1).reshape(-1, 2).astype(np.float32) * stride
            centers = np.repeat(centers, 2, 0)
            keep = sc >= thresh
            c = centers[keep]
            b = np.concatenate([c - bb[keep][:, :2], c + bb[keep][:, 2:]], 1)
            k = kp[keep].reshape(-1, 5, 2) + c[:, None]
            boxes.append(b); scores.append(sc[keep]); kpss.append(k)
        boxes = np.concatenate(boxes) / s; scores = np.concatenate(scores); kpss = np.concatenate(kpss) / s
        order = scores.argsort()[::-1]
        out = []
        while len(order):
            i = order[0]
            out.append((boxes[i], float(scores[i]), kpss[i]))
            xx1 = np.maximum(boxes[i, 0], boxes[order[1:], 0]); yy1 = np.maximum(boxes[i, 1], boxes[order[1:], 1])
            xx2 = np.minimum(boxes[i, 2], boxes[order[1:], 2]); yy2 = np.minimum(boxes[i, 3], boxes[order[1:], 3])
            inter = np.clip(xx2 - xx1, 0, None) * np.clip(yy2 - yy1, 0, None)
            area = lambda bx: (bx[..., 2] - bx[..., 0]) * (bx[..., 3] - bx[..., 1])
            iou = inter / (area(boxes[i]) + area(boxes[order[1:]]) - inter)
            order = order[1:][iou <= nms]
        return out
