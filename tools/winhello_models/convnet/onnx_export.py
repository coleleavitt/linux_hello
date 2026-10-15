"""Convert a parsed DNNTestLib ``conv_net_v1.0`` net to ONNX (NCHW, float32).

Graph input  ``image``: float32 [1, C, H, W] holding raw 0..255 pixel values
(the u8 crop the engine feeds in); the ImageDataLayer normalisation is part of
the graph.  Outputs:

* ``output``  - last layer of the net (512-d feature for slots 11/12, 2 logits otherwise)
* ``logits``  - only for nets with the 2x512 cosine head: W @ l2norm(output)
* ``prob``    - softmax over the 2 logits, as the engine computes it (sub_18003C0B0)
"""
from __future__ import annotations

import numpy as np
import onnx
from onnx import TensorProto, helper, numpy_helper

from .format import ConvNet

OPSET = 17


def to_onnx(net: ConvNet, *, engine_head: bool = True, expose_all: bool = False,
            doc: str = "") -> onnx.ModelProto:
    nodes, inits = [], []
    names: dict[str, str] = {}  # layer name -> tensor name
    c, h, w = net.input_shape
    uid = [0]

    reserved = {"image", "output", "logits", "prob", "feature_l2"}

    def tname(layer_name: str) -> str:
        return layer_name + "__layer" if layer_name in reserved else layer_name

    def const(name, arr):
        inits.append(numpy_helper.from_array(np.asarray(arr), name))
        return name

    def fresh(base):
        uid[0] += 1
        return f"{base}__{uid[0]}"

    for L in net.layers:
        out = tname(L.name)
        ins = [names[n] for n in L.inputs]
        t = L.type
        if t == "image_data":
            if L.mean is not None and L.mean.data is not None:
                mean = L.mean.as_bacn()[..., 0].transpose(2, 0, 1)[None]  # (1, C, H, W)
                nodes.append(helper.make_node("Sub", ["image", const(fresh("mean"), mean.astype(np.float32))], [out]))
            else:
                tmp = fresh("scaled")
                nodes.append(helper.make_node("Div", ["image", const(fresh("c127_5"), np.array(127.5, np.float32))], [tmp]))
                nodes.append(helper.make_node("Sub", [tmp, const(fresh("one"), np.array(1.0, np.float32))], [out]))
        elif t == "conv":
            wt = L.weight.as_bacn().transpose(3, 2, 0, 1)  # (kh,kw,cin,cout) -> OIHW
            pa, pb = L.conv_pad
            sa, sb = L.conv_stride
            cin = [ins[0], const(L.name + "/W", np.ascontiguousarray(wt, np.float32))]
            if L.bias is not None and L.bias.data is not None:
                cin.append(const(L.name + "/b", L.bias.as_bacn().reshape(-1).astype(np.float32)))
            nodes.append(helper.make_node("Conv", cin, [out], kernel_shape=list(wt.shape[2:]),
                                          pads=[pb, pa, pb, pa], strides=[sb, sa], name=L.name))
        elif t == "neuron":
            nodes.append(helper.make_node("Relu" if L.subtype == "relu" else "Sigmoid", ins, [out], name=L.name))
        elif t == "pool":
            sa, sb = L.pool_stride
            ka, kb = L.pool_kernel
            if ka != kb:
                raise NotImplementedError("non-square pooling (engine uses kA for both output dims)")
            attrs = dict(kernel_shape=[kb, ka], strides=[sb, sa], ceil_mode=1)
            if L.subtype == "max":
                nodes.append(helper.make_node("MaxPool", ins, [out], name=L.name, **attrs))
            else:
                nodes.append(helper.make_node("AveragePool", ins, [out], name=L.name, count_include_pad=0, **attrs))
        elif t == "padding":
            pa, pb = L.ints
            nodes.append(helper.make_node("Pad", [ins[0], const(fresh("pads"), np.array([0, 0, pb, pa, 0, 0, pb, pa], np.int64))],
                                          [out], name=L.name, mode="constant"))
        elif t == "concat":
            nodes.append(helper.make_node("Concat", ins, [out], name=L.name, axis=1))
        elif t == "flatten":
            nodes.append(helper.make_node("Flatten", ins, [out], name=L.name, axis=1))
        elif t == "fc":
            wt = L.weight.as_bacn().reshape(-1, L.weight.dims[0]).astype(np.float32)  # (in, out)
            src = ins[0]
            prev = next(p for p in net.layers if p.name == L.inputs[0])
            if prev.type != "flatten":
                # engine reads the raw HWC buffer; reproduce that order
                tr = fresh("hwc")
                nodes.append(helper.make_node("Transpose", [src], [tr], perm=[0, 2, 3, 1]))
                src = fresh("flat")
                nodes.append(helper.make_node("Flatten", [tr], [src], axis=1))
            nodes.append(helper.make_node("Gemm", [src, const(L.name + "/W", wt),
                                                   const(L.name + "/b", L.bias.as_bacn().reshape(-1).astype(np.float32))],
                                          [out], name=L.name))
        elif t == "dropout":
            nodes.append(helper.make_node("Identity", ins, [out], name=L.name))
        elif t == "softmax":
            nodes.append(helper.make_node("Softmax", ins, [out], name=L.name, axis=1))
        elif t == "eltwise_max":
            nodes.append(helper.make_node("Max", ins, [out], name=L.name))
        else:
            raise NotImplementedError(f"layer type {t!r} has no ONNX mapping")
        names[L.name] = out

    last = names[net.layers[-1].name]
    nodes.append(helper.make_node("Flatten", [last], ["output"], axis=1))
    outputs = [helper.make_tensor_value_info("output", TensorProto.FLOAT, [1, None])]
    if engine_head:
        head = net.head_weights()
        logits = "output"
        if head is not None:
            nodes.append(helper.make_node("LpNormalization", ["output"], ["feature_l2"], axis=1, p=2))
            nodes.append(helper.make_node("MatMul", ["feature_l2", const("cos_head/W", np.ascontiguousarray(head.T, np.float32))], ["logits"]))
            outputs.append(helper.make_tensor_value_info("logits", TensorProto.FLOAT, [1, 2]))
            logits = "logits"
        nodes.append(helper.make_node("Softmax", [logits], ["prob"], axis=1))
        outputs.append(helper.make_tensor_value_info("prob", TensorProto.FLOAT, [1, None]))
    graph = helper.make_graph(nodes, "conv_net_v1", [helper.make_tensor_value_info("image", TensorProto.FLOAT, [1, c, h, w])],
                              outputs, initializer=inits)
    model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", OPSET)],
                              producer_name="linux_hello winhello_models.convnet")
    if expose_all:  # debugging: every layer output becomes a graph output
        inferred = onnx.shape_inference.infer_shapes(model)
        vi = {v.name: v for v in inferred.graph.value_info}
        for L in net.layers:
            if names[L.name] in vi and names[L.name] != "output":
                model.graph.output.append(vi[names[L.name]])
    model.ir_version = 8
    if doc:
        model.doc_string = doc
    onnx.checker.check_model(model)
    return model
