"""Rebuild a standard ONNX model from an ONNX Runtime ORT-format (flatbuffer) model.

The legacy recognition containers ship ORT-format models. ORT format keeps the
(level-1 optimised) graph, so this is a lossless re-serialisation of nodes,
initialisers and value infos. ORT-specific fused ops are unfused into standard
ONNX ops (com.microsoft.FusedGemm -> Gemm + activation) so the result runs in any
ONNX runtime.
"""
from __future__ import annotations

import onnx
from onnx import TensorProto, helper, numpy_helper
from onnxruntime.tools.ort_format_model.ort_flatbuffers_py import fbs

import numpy as np

_ACT = {"Relu": "Relu", "Sigmoid": "Sigmoid", "Tanh": "Tanh", "LeakyRelu": "LeakyRelu"}


def _s(b):
    return b.decode() if isinstance(b, bytes) else (b or "")


def _tensor(t) -> TensorProto:
    dims = [int(d) for d in (t.DimsAsNumpy() if t.DimsLength() else [])]
    dtype = t.DataType()
    if t.ExternalDataOffset() not in (-1, 0) and t.RawDataLength() == 0:
        raise NotImplementedError("external data not supported")
    np_dtype = helper.tensor_dtype_to_np_dtype(dtype)
    if dtype == TensorProto.STRING:
        arr = np.array([t.StringData(i) for i in range(t.StringDataLength())], dtype=object).reshape(dims)
    else:
        raw = t.RawDataAsNumpy().tobytes() if t.RawDataLength() else b""
        arr = np.frombuffer(raw, dtype=np_dtype).reshape(dims)
    return numpy_helper.from_array(arr, _s(t.Name()))


def _attr(a):
    AT = fbs.AttributeType.AttributeType
    name, typ = _s(a.Name()), a.Type()
    if typ == AT.FLOAT:
        return helper.make_attribute(name, a.F())
    if typ == AT.INT:
        return helper.make_attribute(name, a.I())
    if typ == AT.STRING:
        return helper.make_attribute(name, a.S())
    if typ == AT.TENSOR:
        return helper.make_attribute(name, _tensor(a.T()))
    if typ == AT.FLOATS:
        return helper.make_attribute(name, [float(x) for x in a.FloatsAsNumpy()], attr_type=onnx.AttributeProto.FLOATS)
    if typ == AT.INTS:
        return helper.make_attribute(name, [int(x) for x in a.IntsAsNumpy()], attr_type=onnx.AttributeProto.INTS)
    if typ == AT.STRINGS:
        return helper.make_attribute(name, [a.Strings(i) for i in range(a.StringsLength())], attr_type=onnx.AttributeProto.STRINGS)
    raise NotImplementedError(f"attribute type {typ} ({name})")


def _value_info(v):
    ti = v.Type()
    name = _s(v.Name())
    if ti is None or ti.ValueType() != fbs.TypeInfoValue.TypeInfoValue.tensor_type:
        return helper.make_empty_tensor_value_info(name)
    tt = fbs.TensorTypeAndShape.TensorTypeAndShape()
    tab = ti.Value()
    tt.Init(tab.Bytes, tab.Pos)
    shape = None
    if tt.Shape() is not None:
        shape = []
        for i in range(tt.Shape().DimLength()):
            dv = tt.Shape().Dim(i).Value()
            if dv is None:
                shape.append(None)
            elif dv.DimType() == fbs.DimensionValueType.DimensionValueType.VALUE:
                shape.append(int(dv.DimValue()))
            else:
                shape.append(_s(dv.DimParam()) or None)
    return helper.make_tensor_value_info(name, tt.ElemType(), shape)


def _unfuse(node: onnx.NodeProto) -> list[onnx.NodeProto]:
    if node.domain != "com.microsoft" or node.op_type != "FusedGemm":
        return [node]
    attrs = {a.name: helper.get_attribute_value(a) for a in node.attribute}
    act = attrs.pop("activation").decode()
    act_attrs = {k[len("activation_"):]: v for k, v in list(attrs.items()) if k.startswith("activation_")}
    for k in list(attrs):
        if k.startswith("activation_"):
            attrs.pop(k)
    mid = node.output[0] + "__pre_act"
    gemm = helper.make_node("Gemm", list(node.input), [mid], name=node.name + "/Gemm", **attrs)
    act_node = helper.make_node(_ACT[act], [mid], list(node.output), name=node.name + "/" + act, **act_attrs)
    return [gemm, act_node]


def _toposort(nodes, graph_inputs, inits):
    avail = set(graph_inputs) | set(inits) | {""}
    out, pending = [], list(nodes)
    while pending:
        progressed = False
        for n in list(pending):
            if all(i in avail for i in n.input):
                out.append(n)
                avail.update(n.output)
                pending.remove(n)
                progressed = True
        if not progressed:
            raise ValueError("graph is not a DAG / missing inputs")
    return out


def convert(ort_bytes: bytes) -> onnx.ModelProto:
    buf = bytearray(ort_bytes)
    if not fbs.InferenceSession.InferenceSession.InferenceSessionBufferHasIdentifier(buf, 0):
        raise ValueError("not an ORT format model")
    model = fbs.InferenceSession.InferenceSession.GetRootAsInferenceSession(buf, 0).Model()
    g = model.Graph()
    inits = [_tensor(g.Initializers(i)) for i in range(g.InitializersLength())]
    if g.SparseInitializersLength():
        raise NotImplementedError("sparse initialisers")
    vinfo = {}
    for i in range(g.NodeArgsLength()):
        v = g.NodeArgs(i)
        vinfo[_s(v.Name())] = _value_info(v)
    nodes = []
    for i in range(g.NodesLength()):
        n = g.Nodes(i)
        if n.Type() != fbs.NodeType.NodeType.Primitive:
            raise NotImplementedError("fused (compiled) node in ORT model")
        node = helper.make_node(
            _s(n.OpType()),
            [_s(n.Inputs(j)) for j in range(n.InputsLength())],
            [_s(n.Outputs(j)) for j in range(n.OutputsLength())],
            name=_s(n.Name()),
            domain=_s(n.Domain()),
        )
        node.attribute.extend(_attr(n.Attributes(j)) for j in range(n.AttributesLength()))
        nodes.extend(_unfuse(node))
    init_names = {t.name for t in inits}
    inputs = [_s(g.Inputs(i)) for i in range(g.InputsLength())]
    outputs = [_s(g.Outputs(i)) for i in range(g.OutputsLength())]
    nodes = _toposort(nodes, inputs, init_names)
    graph = helper.make_graph(
        nodes,
        "graph",
        [vinfo[n] for n in inputs if n not in init_names],
        [vinfo[n] for n in outputs],
        initializer=inits,
    )
    opsets = []
    for i in range(model.OpsetImportLength()):
        o = model.OpsetImport(i)
        opsets.append(helper.make_opsetid(_s(o.Domain()), o.Version()))
    used = {n.domain for n in nodes}
    opsets = [o for o in opsets if o.domain in used or o.domain in ("", "ai.onnx")]
    m = helper.make_model(graph, opset_imports=opsets, producer_name=_s(model.ProducerName()),
                          producer_version=_s(model.ProducerVersion()))
    m.ir_version = model.IrVersion()
    for i in range(model.MetadataPropsLength()):
        p = model.MetadataProps(i)
        m.metadata_props.add(key=_s(p.Key()), value=_s(p.Value()))
    return m
