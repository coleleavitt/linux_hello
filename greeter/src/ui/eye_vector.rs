//! Windows 11's Hello eye: the LottieGen composition `AnimatedVisuals::HelloSolidAnimations`
//! compiled into Windows.UI.Logon.dll, reconstructed as data (`hello/eye/solid.json`, written
//! by tools/winhello_ui/lottie_extract.py; docs/windows-hello-ui.md §1.3, §6) and played with
//! Cairo, frame-exact: 64×64 units, 2270 frames at 60 fps, the segment per `BioFeedbackState`
//! from the markers (`LottieAnimatorModel::UpdateMarker`: FaceLoop, TooFar, TooClose, Spinner
//! and Sync loop, the others play once and hold their last frame).
//!
//! The document is Windows.UI.Composition's object graph as the generated code builds it
//! (visuals, shapes, geometries, brushes, key-frame animations, easing functions, the few
//! expression bindings), so this module evaluates it the way the compositor does: every
//! animated property is a function of the root `Progress` (frame / 2270); between two key
//! frames the value is interpolated with the target key frame's easing function (a cubic
//! Bézier in progress space, or a step: `StepThenHold` jumps at once, `HoldThenStep` at the
//! end); path key frames interpolate control points; track mattes are `CompositeEffect`s
//! (DestinationIn / DestinationOut) over off-screen surfaces, Cairo's `DestIn` / `DestOut`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

use anyhow::{bail, Context, Result};
use gtk::cairo;
use gtk::glib;
use gtk::prelude::*;
use serde::Deserialize;
use serde_json::Value;

use crate::state::Face;

/// `BioFeedbackState` → the marker pair played and whether it loops (§1.2, `UpdateMarker`).
fn segment_for(face: &Face) -> Option<(&'static str, bool)> {
    match face {
        Face::Idle => None,
        Face::Starting | Face::Looking => Some(("Face", false)),
        Face::Verifying | Face::Blink => Some(("FaceLoop", true)),
        Face::Welcome => Some(("FaceSuccess", false)),
        Face::Failed(_) => Some(("FaceFail", false)),
    }
}

// ------------------------------------------------------------------ the document

#[derive(Deserialize)]
pub struct Composition {
    pub frame_count: u32,
    pub frame_rate: u32,
    pub markers: Vec<Marker>,
    root: String,
    root_properties: Option<PropertySet>,
    objects: HashMap<String, Object>,
}

/// `Markers::get`: name → progress (the marker's frame + 0.05, over 2270, as the compiled
/// table has it; `AnimatedVisualPlayer.PlayAsync(from, to)` takes these progress values).
#[derive(Deserialize, Clone)]
pub struct Marker {
    pub name: String,
    pub progress: f64,
}

#[derive(Deserialize, Default)]
struct Object {
    kind: String,
    #[serde(default)]
    props: HashMap<String, Value>,
    #[serde(default)]
    children: Vec<String>,
    #[serde(default)]
    shapes: Vec<String>,
    #[serde(default)]
    geometries: Vec<String>,
    #[serde(default)]
    color_stops: Vec<String>,
    #[serde(default)]
    animations: Vec<Binding>,
    #[serde(default)]
    keyframes: Vec<KeyFrame>,
    #[serde(default)]
    figures: Vec<Figure>,
    fill_mode: Option<String>,
    effect: Option<Effect>,
    #[serde(default)]
    sources: HashMap<String, String>,
    property_set: Option<PropertySet>,
}

/// `StartAnimation(property, animation)` or an expression animation bound to `property`.
#[derive(Deserialize, Clone)]
struct Binding {
    property: String,
    animation: Option<String>,
    expression: Option<String>,
}

#[derive(Deserialize)]
struct KeyFrame {
    progress: f64,
    value: Option<Value>,
    spatial_bezier: Option<SpatialBezier>,
    easing: Option<String>,
}

/// A Lottie spatial Bézier between two position key frames: the compiled code writes it as an
/// expression key frame in a helper scalar `t` (root property `t0`…`t4`, itself animated).
#[derive(Deserialize)]
struct SpatialBezier {
    t: String,
    points: [[f64; 2]; 4],
}

/// One figure of a Direct2D path: `start`, then cubic Béziers `["C", x1, y1, x2, y2, x, y]`
/// and lines `["L", x, y]`.
#[derive(Deserialize, Clone, Debug)]
struct Figure {
    start: [f64; 2],
    closed: bool,
    segments: Vec<Vec<Value>>,
}

#[derive(Deserialize)]
struct Effect {
    #[serde(rename = "type")]
    kind: String,
    mode: Option<String>,
    blur_amount: Option<f64>,
}

#[derive(Deserialize, Default)]
struct PropertySet {
    #[serde(default)]
    scalars: HashMap<String, f64>,
    #[serde(default)]
    animations: Vec<Binding>,
}

impl Composition {
    pub fn load(path: &Path) -> Result<Composition> {
        let text = std::fs::read_to_string(path).with_context(|| path.display().to_string())?;
        let comp: Composition = serde_json::from_str(&text).with_context(|| path.display().to_string())?;
        if comp.frame_count == 0 || comp.frame_rate == 0 || !comp.objects.contains_key(&comp.root) {
            bail!("{}: not a hello-eye composition", path.display());
        }
        Ok(comp)
    }

    /// The frame range `[start, end]` of a marker pair (`<name>Start`…`<name>End`), in frames
    /// (fractional: the markers sit 0.05 frame after the key frames that switch the layers).
    pub fn segment(&self, name: &str) -> Option<(f64, f64)> {
        let find = |suffix: &str| {
            self.markers
                .iter()
                .find(|m| m.name == format!("{name}{suffix}"))
                .map(|m| m.progress * f64::from(self.frame_count))
        };
        Some((find("Start")?, find("End")?))
    }

    fn obj(&self, id: &str) -> &Object {
        self.objects.get(id).unwrap_or_else(|| panic!("solid.json: missing object {id}"))
    }
}

// ------------------------------------------------------------------ evaluation

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color {
    pub a: f64,
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Color {
    fn parse(s: &str) -> Option<Color> {
        let v = u32::from_str_radix(s.strip_prefix('#')?, 16).ok()?;
        let c = |x: u32| f64::from((v >> x) & 0xff) / 255.0;
        Some(Color { a: c(24), r: c(16), g: c(8), b: c(0) })
    }

    fn lerp(self, o: Color, t: f64) -> Color {
        let l = |a: f64, b: f64| a + (b - a) * t;
        Color { a: l(self.a, o.a), r: l(self.r, o.r), g: l(self.g, o.g), b: l(self.b, o.b) }
    }
}

/// A property value at some progress.
#[derive(Clone, Debug)]
enum Val {
    Scalar(f64),
    Vec2([f64; 2]),
    Vec3([f64; 3]),
    Bool(bool),
    Color(Color),
    /// A path: `CompositionPath` figures (the geometry group flattened to its figures).
    Path(Vec<Figure>),
}

impl Val {
    fn from_json(v: &Value) -> Option<Val> {
        match v {
            Value::Number(n) => Some(Val::Scalar(n.as_f64()?)),
            Value::Bool(b) => Some(Val::Bool(*b)),
            Value::String(s) => Color::parse(s).map(Val::Color),
            Value::Array(a) => {
                let f: Vec<f64> = a.iter().filter_map(Value::as_f64).collect();
                match f.len() {
                    2 => Some(Val::Vec2([f[0], f[1]])),
                    3 => Some(Val::Vec3([f[0], f[1], f[2]])),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn scalar(&self) -> f64 {
        match self {
            Val::Scalar(s) => *s,
            Val::Bool(b) => f64::from(u8::from(*b)),
            _ => 0.0,
        }
    }

    fn vec2(&self) -> [f64; 2] {
        match self {
            Val::Vec2(v) => *v,
            Val::Vec3(v) => [v[0], v[1]],
            Val::Scalar(s) => [*s, *s],
            _ => [0.0, 0.0],
        }
    }

    fn lerp(&self, o: &Val, t: f64) -> Val {
        let l = |a: f64, b: f64| a + (b - a) * t;
        match (self, o) {
            (Val::Scalar(a), Val::Scalar(b)) => Val::Scalar(l(*a, *b)),
            (Val::Vec2(a), Val::Vec2(b)) => Val::Vec2([l(a[0], b[0]), l(a[1], b[1])]),
            (Val::Vec3(a), Val::Vec3(b)) => Val::Vec3([l(a[0], b[0]), l(a[1], b[1]), l(a[2], b[2])]),
            (Val::Color(a), Val::Color(b)) => Val::Color(a.lerp(*b, t)),
            (Val::Path(a), Val::Path(b)) => Val::Path(lerp_figures(a, b, t)),
            _ => {
                if t < 1.0 {
                    self.clone()
                } else {
                    o.clone()
                }
            }
        }
    }
}

/// Point-wise interpolation of two paths with the same structure (Composition's
/// `PathKeyFrameAnimation`).
fn lerp_figures(a: &[Figure], b: &[Figure], t: f64) -> Vec<Figure> {
    let l = |x: f64, y: f64| x + (y - x) * t;
    a.iter()
        .zip(b)
        .map(|(fa, fb)| Figure {
            start: [l(fa.start[0], fb.start[0]), l(fa.start[1], fb.start[1])],
            closed: fa.closed,
            segments: fa
                .segments
                .iter()
                .zip(&fb.segments)
                .map(|(sa, sb)| {
                    sa.iter()
                        .zip(sb)
                        .map(|(x, y)| match (x.as_f64(), y.as_f64()) {
                            (Some(x), Some(y)) => Value::from(l(x, y)),
                            _ => x.clone(),
                        })
                        .collect()
                })
                .collect(),
        })
        .collect()
}

/// The unit cubic Bézier easing of Windows Composition (`CubicBezierEasingFunction`): y at the
/// parameter whose x equals `x`.
pub fn cubic_bezier(cp1: [f64; 2], cp2: [f64; 2], x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    let bez = |p1: f64, p2: f64, t: f64| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    // Newton's method on x(t) = x, with bisection as the safety net.
    let mut t = x;
    for _ in 0..8 {
        let u = 1.0 - t;
        let dx = 3.0 * u * u * cp1[0] + 6.0 * u * t * (cp2[0] - cp1[0]) + 3.0 * t * t * (1.0 - cp2[0]);
        let err = bez(cp1[0], cp2[0], t) - x;
        if err.abs() < 1e-7 || dx.abs() < 1e-9 {
            break;
        }
        t = (t - err / dx).clamp(0.0, 1.0);
    }
    if (bez(cp1[0], cp2[0], t) - x).abs() > 1e-5 {
        let (mut lo, mut hi) = (0.0, 1.0);
        for _ in 0..40 {
            t = 0.5 * (lo + hi);
            if bez(cp1[0], cp2[0], t) < x {
                lo = t;
            } else {
                hi = t;
            }
        }
    }
    bez(cp1[1], cp2[1], t)
}

/// What the composition looks like at one progress: the evaluator.
pub struct Frame<'a> {
    comp: &'a Composition,
    progress: f64,
    /// `_theme.SolidColor`: the one theme colour, white on the dark sign-in screen.
    theme: Color,
}

impl<'a> Frame<'a> {
    pub fn new(comp: &'a Composition, frame: f64, theme: Color) -> Frame<'a> {
        Frame { comp, progress: frame / f64::from(comp.frame_count), theme }
    }

    /// The target key frame's easing applied to the normalised time within the interval.
    fn ease(&self, easing: Option<&String>, t: f64) -> f64 {
        let Some(id) = easing else { return t };
        let e = self.comp.obj(id);
        match e.kind.as_str() {
            "CubicBezierEasingFunction" => {
                let cp = |k: &str| Val::from_json(&e.props[k]).map_or([0.0, 0.0], |v| v.vec2());
                cubic_bezier(cp("ControlPoint1"), cp("ControlPoint2"), t)
            }
            // StepThenHold: jump to the key frame's value at once; HoldThenStep: at the end.
            "StepEasingFunction" => {
                let initial = e.props.get("IsInitialStepSingleFrame").and_then(Value::as_bool).unwrap_or(false);
                if initial || t >= 1.0 {
                    1.0
                } else {
                    0.0
                }
            }
            _ => t,
        }
    }

    fn keyframe_value(&self, kf: &KeyFrame) -> Val {
        if let Some(sb) = &kf.spatial_bezier {
            let t = self.root_scalar(&sb.t);
            let u = 1.0 - t;
            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
            let mut p = [0.0, 0.0];
            for (k, pt) in sb.points.iter().enumerate() {
                p[0] += w[k] * pt[0];
                p[1] += w[k] * pt[1];
            }
            return Val::Vec2(p);
        }
        match &kf.value {
            Some(Value::String(s)) if self.comp.objects.contains_key(s) => Val::Path(self.path_figures(s)),
            Some(v) => Val::from_json(v).unwrap_or(Val::Scalar(0.0)),
            None => Val::Scalar(0.0),
        }
    }

    /// A key-frame animation at the current progress.
    fn animation(&self, id: &str) -> Val {
        let a = self.comp.obj(id);
        let kfs = &a.keyframes;
        let p = self.progress;
        let Some(first) = kfs.first() else { return Val::Scalar(0.0) };
        if a.kind == "BooleanKeyFrameAnimation" {
            // Discrete: the last key frame reached.
            let kf = kfs.iter().take_while(|k| k.progress <= p).last().unwrap_or(first);
            return self.keyframe_value(kf);
        }
        if p <= first.progress {
            return self.keyframe_value(first);
        }
        for w in kfs.windows(2) {
            let (k0, k1) = (&w[0], &w[1]);
            if p <= k1.progress {
                let span = k1.progress - k0.progress;
                let t = if span <= 0.0 { 1.0 } else { (p - k0.progress) / span };
                let v1 = self.keyframe_value(k1);
                if k1.spatial_bezier.is_some() {
                    return v1;
                }
                return self.keyframe_value(k0).lerp(&v1, self.ease(k1.easing.as_ref(), t));
            }
        }
        self.keyframe_value(kfs.last().unwrap_or(first))
    }

    /// A root property (`Progress`, `t0`…`t4`).
    fn root_scalar(&self, name: &str) -> f64 {
        if name == "Progress" {
            return self.progress;
        }
        let Some(ps) = &self.comp.root_properties else { return 0.0 };
        self.property_set_scalar(ps, name)
    }

    fn property_set_scalar(&self, ps: &PropertySet, name: &str) -> f64 {
        if let Some(b) = ps.animations.iter().find(|b| b.property == name) {
            if let Some(a) = &b.animation {
                return self.animation(a).scalar();
            }
        }
        ps.scalars.get(name).copied().unwrap_or(0.0)
    }

    /// Property `name` of object `id`: the static value overridden by what animates it
    /// (whole property, or its `.X` / `.Y` components, or an expression).
    fn prop(&self, id: &str, name: &str) -> Option<Val> {
        let o = self.comp.obj(id);
        let mut value = match o.props.get(name) {
            Some(Value::String(s)) if self.comp.objects.contains_key(s) => Some(Val::Path(self.path_figures(s))),
            Some(v) => Val::from_json(v),
            None => None,
        };
        if let Some(b) = o.animations.iter().find(|b| b.property == name) {
            value = match (&b.animation, &b.expression) {
                (Some(a), _) => Some(self.animation(a)),
                (None, Some(e)) => self.expression(id, e).or(value),
                _ => value,
            };
        }
        for (suffix, k) in [(".X", 0), (".Y", 1), (".Z", 2)] {
            let Some(b) = o.animations.iter().find(|b| b.property == format!("{name}{suffix}")) else { continue };
            let Some(a) = &b.animation else { continue };
            let s = self.animation(a).scalar();
            value = Some(match value {
                Some(Val::Vec2(mut v)) if k < 2 => {
                    v[k] = s;
                    Val::Vec2(v)
                }
                Some(Val::Vec3(mut v)) => {
                    v[k] = s;
                    Val::Vec3(v)
                }
                _ if name == "Scale" => {
                    let mut v = [1.0, 1.0, 1.0];
                    v[k] = s;
                    Val::Vec3(v)
                }
                _ => {
                    let mut v = [0.0, 0.0, 0.0];
                    v[k] = s;
                    Val::Vec3(v)
                }
            });
        }
        value
    }

    /// The expression animations the generated code uses (docs/windows-hello-ui.md §6.2).
    fn expression(&self, id: &str, expr: &str) -> Option<Val> {
        let my = |name: &str| -> f64 {
            // `my.<scalar>`: the object's property set, else the object's own property.
            let o = self.comp.obj(id);
            if let Some(ps) = &o.property_set {
                if ps.scalars.contains_key(name) || ps.animations.iter().any(|a| a.property == name) {
                    return self.property_set_scalar(ps, name);
                }
            }
            self.prop(id, name).map_or(0.0, |v| v.scalar())
        };
        if let Some(rest) = expr.strip_prefix("ColorRGB(_theme.SolidColor.W") {
            let factor = if let Some(f) = rest.strip_prefix("*my.") {
                my(f.split(',').next().unwrap_or(""))
            } else if let Some(f) = rest.strip_prefix('*') {
                f.split(',').next().and_then(|s| s.parse().ok()).unwrap_or(1.0)
            } else {
                1.0
            };
            return Some(Val::Color(Color { a: self.theme.a * factor, ..self.theme }));
        }
        if expr == "Min(my.TStart,my.TEnd)" {
            return Some(Val::Scalar(my("TStart").min(my("TEnd"))));
        }
        if expr == "Max(my.TStart,my.TEnd)" {
            return Some(Val::Scalar(my("TStart").max(my("TEnd"))));
        }
        if expr == "Vector2(0,0)-(my.Size/Vector2(2,2))" {
            let s = self.prop(id, "Size").map_or([0.0, 0.0], |v| v.vec2());
            return Some(Val::Vec2([-s[0] / 2.0, -s[1] / 2.0]));
        }
        if let Some(rest) = expr.strip_prefix("Vector2(Min(") {
            // Vector2(Min(K,(Min(my.Size.X,my.Size.Y)/2)),…): corner radius capped at half the size.
            let k: f64 = rest.split(',').next()?.parse().ok()?;
            let s = self.prop(id, "Size").map_or([0.0, 0.0], |v| v.vec2());
            let r = k.min(s[0].min(s[1]) / 2.0);
            return Some(Val::Vec2([r, r]));
        }
        None
    }

    fn scalar(&self, id: &str, name: &str, default: f64) -> f64 {
        self.prop(id, name).map_or(default, |v| v.scalar())
    }

    fn vec2(&self, id: &str, name: &str, default: [f64; 2]) -> [f64; 2] {
        self.prop(id, name).map_or(default, |v| v.vec2())
    }

    fn visible(&self, id: &str) -> bool {
        match self.prop(id, "IsVisible") {
            Some(Val::Bool(b)) => b,
            Some(v) => v.scalar() != 0.0,
            None => true,
        }
    }

    /// The figures of a `CompositionPath` (through its D2D geometry or geometry group).
    fn path_figures(&self, id: &str) -> Vec<Figure> {
        let o = self.comp.obj(id);
        match o.kind.as_str() {
            "CompositionPath" => {
                o.props.get("Geometry").and_then(Value::as_str).map_or_else(Vec::new, |g| self.path_figures(g))
            }
            "D2DGeometryGroup" => o.geometries.iter().flat_map(|g| self.path_figures(g)).collect(),
            "D2DPathGeometry" => o.figures.clone(),
            _ => Vec::new(),
        }
    }

    /// The fill rule of a path (D2D fill mode), through groups.
    fn fill_rule(&self, id: &str) -> cairo::FillRule {
        let o = self.comp.obj(id);
        match o.kind.as_str() {
            "CompositionPath" => {
                o.props.get("Geometry").and_then(Value::as_str).map_or(cairo::FillRule::Winding, |g| self.fill_rule(g))
            }
            _ if o.fill_mode.as_deref() == Some("even_odd") => cairo::FillRule::EvenOdd,
            _ => cairo::FillRule::Winding,
        }
    }

    fn color_of_brush(&self, id: &str) -> Option<Color> {
        match self.prop(id, "Color") {
            Some(Val::Color(c)) => Some(c),
            _ => None,
        }
    }

    // ------------------------------------------------------------ rendering

    /// Draw the composition into the 64×64 unit square at the context's origin.
    pub fn render(&self, cr: &cairo::Context) -> Result<()> {
        self.visual(cr, &self.comp.root)
    }

    /// A visual's or shape's transform: Scale and Rotation about CenterPoint, then Offset, then
    /// TransformMatrix (the generated code relies on this order: a `ContainerShape` with
    /// `Offset` (448, 452) and `TransformMatrix` scale 0.27 + (79, 78) lands at the centre).
    fn transform(&self, cr: &cairo::Context, id: &str) {
        if let Some(Value::Array(m)) = self.comp.obj(id).props.get("TransformMatrix") {
            let f: Vec<f64> = m.iter().filter_map(Value::as_f64).collect();
            let mat = match f.len() {
                16 => cairo::Matrix::new(f[0], f[1], f[4], f[5], f[12], f[13]),
                6 => cairo::Matrix::new(f[0], f[1], f[2], f[3], f[4], f[5]),
                _ => cairo::Matrix::identity(),
            };
            cr.transform(mat);
        }
        let offset = self.vec2(id, "Offset", [0.0, 0.0]);
        cr.translate(offset[0], offset[1]);
        let center = self.vec2(id, "CenterPoint", [0.0, 0.0]);
        let scale = self.vec2(id, "Scale", [1.0, 1.0]);
        let rotation = self.scalar(id, "RotationAngleInDegrees", 0.0);
        if scale != [1.0, 1.0] || rotation != 0.0 {
            cr.translate(center[0], center[1]);
            cr.rotate(rotation.to_radians());
            cr.scale(scale[0], scale[1]);
            cr.translate(-center[0], -center[1]);
        }
    }

    fn visual(&self, cr: &cairo::Context, id: &str) -> Result<()> {
        if !self.visible(id) {
            return Ok(());
        }
        let o = self.comp.obj(id);
        cr.save()?;
        self.transform(cr, id);
        let size = self.vec2(id, "Size", [0.0, 0.0]);
        if o.props.contains_key("Clip") || o.kind == "ShapeVisual" {
            // The only clip in the composition is an InsetClip without insets: the visual's bounds.
            cr.rectangle(0.0, 0.0, size[0], size[1]);
            cr.clip();
        }
        let opacity = self.scalar(id, "Opacity", 1.0);
        if opacity < 1.0 {
            cr.push_group();
        }
        match o.kind.as_str() {
            "ContainerVisual" => {
                for c in &o.children {
                    self.visual(cr, c)?;
                }
            }
            "ShapeVisual" => {
                for s in &o.shapes {
                    self.shape(cr, s)?;
                }
            }
            "SpriteVisual" => {
                if let Some(brush) = o.props.get("Brush").and_then(Value::as_str) {
                    self.sprite(cr, brush, size)?;
                }
            }
            _ => {}
        }
        if opacity < 1.0 {
            cr.pop_group_to_source()?;
            cr.paint_with_alpha(opacity)?;
        }
        cr.restore()?;
        Ok(())
    }

    /// A `SpriteVisual`: its brush over its bounds.
    fn sprite(&self, cr: &cairo::Context, brush: &str, size: [f64; 2]) -> Result<()> {
        let b = self.comp.obj(brush);
        match b.kind.as_str() {
            "ColorBrush" => {
                if let Some(c) = self.color_of_brush(brush) {
                    cr.set_source_rgba(c.r, c.g, c.b, c.a);
                    cr.rectangle(0.0, 0.0, size[0], size[1]);
                    cr.fill()?;
                }
            }
            "EffectBrush" | "SurfaceBrush" => {
                let (surface, x0, y0) = self.brush_surface(cr, brush, size)?;
                cr.save()?;
                cr.rectangle(0.0, 0.0, size[0], size[1]);
                cr.clip();
                cr.identity_matrix();
                cr.set_source_surface(&surface, x0, y0)?;
                cr.paint()?;
                cr.restore()?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Render a surface or effect brush, covering the sprite's bounds `(0, 0, size)` under the
    /// current transform, into an image surface in device pixels; returns it with the device
    /// position of its top-left corner.
    fn brush_surface(
        &self,
        cr: &cairo::Context,
        brush: &str,
        size: [f64; 2],
    ) -> Result<(cairo::ImageSurface, f64, f64)> {
        let corners = [(0.0, 0.0), (size[0], 0.0), (0.0, size[1]), (size[0], size[1])];
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for (x, y) in corners {
            let (dx, dy) = cr.user_to_device(x, y);
            x0 = x0.min(dx);
            y0 = y0.min(dy);
            x1 = x1.max(dx);
            y1 = y1.max(dy);
        }
        let (x0, y0) = (x0.floor(), y0.floor());
        let (w, h) = (((x1 - x0).ceil() as i32).clamp(1, 4096), ((y1 - y0).ceil() as i32).clamp(1, 4096));
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h)?;
        let b = self.comp.obj(brush);
        match b.kind.as_str() {
            "SurfaceBrush" => {
                let vs = b.props.get("Surface").and_then(Value::as_str).context("SurfaceBrush without a surface")?;
                let source = self
                    .comp
                    .obj(vs)
                    .props
                    .get("SourceVisual")
                    .and_then(Value::as_str)
                    .context("VisualSurface without a visual")?;
                let source_size = self.vec2(vs, "SourceSize", size);
                let ctx = cairo::Context::new(&surface)?;
                let mut m = cr.matrix();
                m.set_x0(m.x0() - x0);
                m.set_y0(m.y0() - y0);
                ctx.set_matrix(m);
                // The surface's SourceSize is stretched over the sprite.
                ctx.scale(size[0] / source_size[0], size[1] / source_size[1]);
                self.visual(&ctx, source)?;
            }
            "EffectBrush" => {
                let effect = b.effect.as_ref().context("EffectBrush without an effect")?;
                let input = |name: &str| -> Result<cairo::ImageSurface> {
                    let id = b.sources.get(name).with_context(|| format!("effect input {name}"))?;
                    Ok(self.brush_surface(cr, id, size)?.0)
                };
                match effect.kind.as_str() {
                    "composite" => {
                        let dest = input("destination")?;
                        let src = input("source")?;
                        let ctx = cairo::Context::new(&dest)?;
                        ctx.set_source_surface(&src, 0.0, 0.0)?;
                        ctx.set_operator(match effect.mode.as_deref() {
                            Some("destination_out") => cairo::Operator::DestOut,
                            Some("source_over") => cairo::Operator::Over,
                            _ => cairo::Operator::DestIn,
                        });
                        ctx.paint()?;
                        drop(ctx);
                        return Ok((dest, x0, y0));
                    }
                    "gaussian_blur" => {
                        let mut src = input("source")?;
                        let m = cr.matrix();
                        let device_scale = (m.xx() * m.yy() - m.xy() * m.yx()).abs().sqrt();
                        gaussian_blur(&mut src, effect.blur_amount.unwrap_or(0.0) * device_scale)?;
                        return Ok((src, x0, y0));
                    }
                    other => bail!("effect {other}"),
                }
            }
            "ColorBrush" => {
                if let Some(c) = self.color_of_brush(brush) {
                    let ctx = cairo::Context::new(&surface)?;
                    ctx.set_source_rgba(c.r, c.g, c.b, c.a);
                    ctx.paint()?;
                }
            }
            other => bail!("brush {other} on a sprite"),
        }
        Ok((surface, x0, y0))
    }

    fn shape(&self, cr: &cairo::Context, id: &str) -> Result<()> {
        let o = self.comp.obj(id);
        let scale = self.vec2(id, "Scale", [1.0, 1.0]);
        if scale[0] == 0.0 || scale[1] == 0.0 {
            return Ok(());
        }
        cr.save()?;
        self.transform(cr, id);
        match o.kind.as_str() {
            "ContainerShape" => {
                for s in &o.shapes {
                    self.shape(cr, s)?;
                }
            }
            "SpriteShape" => self.sprite_shape(cr, id)?,
            _ => {}
        }
        cr.restore()?;
        Ok(())
    }

    fn set_brush(&self, cr: &cairo::Context, brush: &str) -> Result<bool> {
        let b = self.comp.obj(brush);
        match b.kind.as_str() {
            "ColorBrush" => match self.color_of_brush(brush) {
                Some(c) if c.a > 0.0 => {
                    cr.set_source_rgba(c.r, c.g, c.b, c.a);
                    Ok(true)
                }
                _ => Ok(false),
            },
            "LinearGradientBrush" => {
                let s = self.vec2(brush, "StartPoint", [0.0, 0.0]);
                let e = self.vec2(brush, "EndPoint", [0.0, 0.0]);
                let g = cairo::LinearGradient::new(s[0], s[1], e[0], e[1]);
                for stop in &b.color_stops {
                    let c = self.color_of_brush(stop).unwrap_or(self.theme);
                    g.add_color_stop_rgba(self.scalar(stop, "Offset", 0.0), c.r, c.g, c.b, c.a);
                }
                cr.set_source(&g)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn sprite_shape(&self, cr: &cairo::Context, id: &str) -> Result<()> {
        let o = self.comp.obj(id);
        let Some(geometry) = o.props.get("Geometry").and_then(Value::as_str) else { return Ok(()) };
        let fill_rule = self.geometry_path(cr, geometry);
        cr.set_fill_rule(fill_rule);
        if let Some(fill) = o.props.get("FillBrush").and_then(Value::as_str) {
            if self.set_brush(cr, fill)? {
                cr.fill_preserve()?;
            }
        }
        if let Some(stroke) = o.props.get("StrokeBrush").and_then(Value::as_str) {
            let thickness = self.scalar(id, "StrokeThickness", 1.0);
            if thickness > 0.0 && self.set_brush(cr, stroke)? {
                cr.set_line_width(thickness);
                let cap = |name: &str| match o.props.get(name).and_then(Value::as_str) {
                    Some("round") => cairo::LineCap::Round,
                    Some("square") => cairo::LineCap::Square,
                    _ => cairo::LineCap::Butt,
                };
                cr.set_line_cap(cap("StrokeStartCap"));
                cr.set_line_join(match o.props.get("StrokeLineJoin").and_then(Value::as_str) {
                    Some("round") => cairo::LineJoin::Round,
                    Some("bevel") => cairo::LineJoin::Bevel,
                    _ => cairo::LineJoin::Miter,
                });
                cr.set_miter_limit(self.scalar(id, "StrokeMiterLimit", 1.0));
                cr.stroke_preserve()?;
            }
        }
        cr.new_path();
        Ok(())
    }

    /// Build the geometry's path (trimmed if it is), returning its fill rule.
    fn geometry_path(&self, cr: &cairo::Context, id: &str) -> cairo::FillRule {
        let g = self.comp.obj(id);
        let trim_start = self.scalar(id, "TrimStart", 0.0);
        let trim_end = self.scalar(id, "TrimEnd", 1.0);
        let trim_offset = self.scalar(id, "TrimOffset", 0.0);
        let trimmed = trim_start != 0.0 || trim_end != 1.0 || trim_offset != 0.0;
        let mut rule = cairo::FillRule::Winding;
        let figures: Vec<Figure> = match g.kind.as_str() {
            "PathGeometry" => match self.prop(id, "Path") {
                Some(Val::Path(f)) => {
                    if let Some(p) = g.props.get("Path").and_then(Value::as_str) {
                        rule = self.fill_rule(p);
                    }
                    f
                }
                _ => Vec::new(),
            },
            "EllipseGeometry" => {
                let c = self.vec2(id, "Center", [0.0, 0.0]);
                let r = self.vec2(id, "Radius", [0.0, 0.0]);
                ellipse_figure(c, r)
            }
            "RectangleGeometry" => {
                let o = self.vec2(id, "Offset", [0.0, 0.0]);
                let s = self.vec2(id, "Size", [0.0, 0.0]);
                rounded_rect_figure(o, s, [0.0, 0.0])
            }
            "RoundedRectangleGeometry" => {
                let o = self.vec2(id, "Offset", [0.0, 0.0]);
                let s = self.vec2(id, "Size", [0.0, 0.0]);
                let r = self.vec2(id, "CornerRadius", [0.0, 0.0]);
                rounded_rect_figure(o, s, r)
            }
            _ => Vec::new(),
        };
        if trimmed {
            trimmed_path(cr, &figures, trim_start + trim_offset, trim_end + trim_offset);
        } else {
            for f in &figures {
                figure_path(cr, f);
            }
        }
        rule
    }
}

fn seg_floats(seg: &[Value]) -> (char, Vec<f64>) {
    let kind = seg.first().and_then(Value::as_str).and_then(|s| s.chars().next()).unwrap_or('L');
    (kind, seg.iter().skip(1).filter_map(Value::as_f64).collect())
}

fn figure_path(cr: &cairo::Context, f: &Figure) {
    cr.move_to(f.start[0], f.start[1]);
    for seg in &f.segments {
        match seg_floats(seg) {
            ('C', v) if v.len() == 6 => cr.curve_to(v[0], v[1], v[2], v[3], v[4], v[5]),
            ('L', v) if v.len() == 2 => cr.line_to(v[0], v[1]),
            _ => {}
        }
    }
    if f.closed {
        cr.close_path();
    }
}

/// An ellipse as four cubic Béziers, starting at 3 o'clock and going clockwise (Direct2D's
/// ellipse geometry, which `CompositionEllipseGeometry` draws and trims).
fn ellipse_figure(c: [f64; 2], r: [f64; 2]) -> Vec<Figure> {
    const K: f64 = 0.552_284_749_830_793_4;
    let (cx, cy, rx, ry) = (c[0], c[1], r[0], r[1]);
    let seg = |x1: f64, y1: f64, x2: f64, y2: f64, x: f64, y: f64| -> Vec<Value> {
        vec![Value::from("C"), x1.into(), y1.into(), x2.into(), y2.into(), x.into(), y.into()]
    };
    vec![Figure {
        start: [cx + rx, cy],
        closed: true,
        segments: vec![
            seg(cx + rx, cy + K * ry, cx + K * rx, cy + ry, cx, cy + ry),
            seg(cx - K * rx, cy + ry, cx - rx, cy + K * ry, cx - rx, cy),
            seg(cx - rx, cy - K * ry, cx - K * rx, cy - ry, cx, cy - ry),
            seg(cx + K * rx, cy - ry, cx + rx, cy - K * ry, cx + rx, cy),
        ],
    }]
}

/// A (rounded) rectangle, clockwise from the top-left corner's end of its arc.
fn rounded_rect_figure(o: [f64; 2], s: [f64; 2], r: [f64; 2]) -> Vec<Figure> {
    const K: f64 = 0.552_284_749_830_793_4;
    let (x, y, w, h) = (o[0], o[1], s[0], s[1]);
    let rx = r[0].min(w / 2.0).max(0.0);
    let ry = r[1].min(h / 2.0).max(0.0);
    let line = |px: f64, py: f64| -> Vec<Value> { vec![Value::from("L"), px.into(), py.into()] };
    let arc = |x1: f64, y1: f64, x2: f64, y2: f64, px: f64, py: f64| -> Vec<Value> {
        vec![Value::from("C"), x1.into(), y1.into(), x2.into(), y2.into(), px.into(), py.into()]
    };
    let mut segments = vec![line(x + w - rx, y)];
    if rx > 0.0 || ry > 0.0 {
        segments.push(arc(x + w - rx + K * rx, y, x + w, y + ry - K * ry, x + w, y + ry));
    }
    segments.push(line(x + w, y + h - ry));
    if rx > 0.0 || ry > 0.0 {
        segments.push(arc(x + w, y + h - ry + K * ry, x + w - rx + K * rx, y + h, x + w - rx, y + h));
    }
    segments.push(line(x + rx, y + h));
    if rx > 0.0 || ry > 0.0 {
        segments.push(arc(x + rx - K * rx, y + h, x, y + h - ry + K * ry, x, y + h - ry));
    }
    segments.push(line(x, y + ry));
    if rx > 0.0 || ry > 0.0 {
        segments.push(arc(x, y + ry - K * ry, x + rx - K * rx, y, x + rx, y));
    }
    vec![Figure { start: [x + rx, y], closed: true, segments }]
}

/// Flatten the figures to polylines (closed figures repeat their start) for trimming.
fn flatten(figures: &[Figure]) -> Vec<Vec<[f64; 2]>> {
    const STEPS: usize = 24;
    figures
        .iter()
        .map(|f| {
            let mut pts = vec![f.start];
            for seg in &f.segments {
                let p0 = *pts.last().unwrap_or(&f.start);
                match seg_floats(seg) {
                    ('C', v) if v.len() == 6 => {
                        for i in 1..=STEPS {
                            let t = i as f64 / STEPS as f64;
                            let u = 1.0 - t;
                            let w = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                            pts.push([
                                w[0] * p0[0] + w[1] * v[0] + w[2] * v[2] + w[3] * v[4],
                                w[0] * p0[1] + w[1] * v[1] + w[2] * v[3] + w[3] * v[5],
                            ]);
                        }
                    }
                    ('L', v) if v.len() == 2 => pts.push([v[0], v[1]]),
                    _ => {}
                }
            }
            if f.closed {
                pts.push(f.start);
            }
            pts
        })
        .collect()
}

/// The part of the path between the fractions `start` and `end` of its total length
/// (`TrimStart` / `TrimEnd` with `TrimOffset` added; the range wraps around 1).
fn trimmed_path(cr: &cairo::Context, figures: &[Figure], start: f64, end: f64) {
    let polylines = flatten(figures);
    let total: f64 = polylines.iter().map(|p| p.windows(2).map(|w| dist(w[0], w[1])).sum::<f64>()).sum();
    if total <= 0.0 {
        return;
    }
    let (mut s, mut e) = (start.min(end), start.max(end));
    if e - s < 1e-9 {
        return; // nothing of the path: not even a cap
    }
    if e - s >= 1.0 {
        for f in figures {
            figure_path(cr, f);
        }
        return;
    }
    s = s.rem_euclid(1.0);
    e = s + (end - start).abs();
    let ranges: Vec<(f64, f64)> =
        if e > 1.0 { vec![(s * total, total), (0.0, (e - 1.0) * total)] } else { vec![(s * total, e * total)] };
    for (from, to) in ranges {
        let mut at = 0.0;
        let mut drawing = false;
        for p in &polylines {
            for w in p.windows(2) {
                let d = dist(w[0], w[1]);
                if d <= 0.0 {
                    continue;
                }
                let (a, b) = (at, at + d);
                at = b;
                if b < from || a > to {
                    continue;
                }
                let ta = ((from - a) / d).clamp(0.0, 1.0);
                let tb = ((to - a) / d).clamp(0.0, 1.0);
                let pa = lerp_pt(w[0], w[1], ta);
                let pb = lerp_pt(w[0], w[1], tb);
                if !drawing {
                    cr.move_to(pa[0], pa[1]);
                    drawing = true;
                }
                cr.line_to(pb[0], pb[1]);
            }
            drawing = false;
        }
    }
}

fn dist(a: [f64; 2], b: [f64; 2]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn lerp_pt(a: [f64; 2], b: [f64; 2], t: f64) -> [f64; 2] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]
}

/// Win2D's `GaussianBlurEffect` (`BlurAmount` = σ in pixels) as three box blurs.
fn gaussian_blur(surface: &mut cairo::ImageSurface, sigma: f64) -> Result<()> {
    if sigma < 0.5 {
        return Ok(());
    }
    let (w, h, stride) = (surface.width() as usize, surface.height() as usize, surface.stride() as usize);
    // Box sizes whose triple approximates a Gaussian of this σ (Kovesi's "Fast almost-Gaussian").
    let ideal = (12.0 * sigma * sigma / 3.0 + 1.0).sqrt();
    let mut wl = ideal.floor() as usize;
    if wl % 2 == 0 {
        wl -= 1;
    }
    let wu = wl + 2;
    let m = ((12.0 * sigma * sigma - 3.0 * (wl * wl) as f64 - 12.0 * wl as f64 - 9.0) / (-4.0 * wl as f64 - 4.0))
        .round() as usize;
    let sizes = [if m > 0 { wl } else { wu }, if m > 1 { wl } else { wu }, if m > 2 { wl } else { wu }];
    surface.flush();
    let mut data = surface.data()?;
    let mut px: Vec<[f64; 4]> = vec![[0.0; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * 4;
            px[y * w + x] =
                [f64::from(data[i]), f64::from(data[i + 1]), f64::from(data[i + 2]), f64::from(data[i + 3])];
        }
    }
    let mut tmp = px.clone();
    for size in sizes {
        let r = (size / 2) as isize;
        box_blur_pass(&px, &mut tmp, w, h, r, true);
        box_blur_pass(&tmp, &mut px, w, h, r, false);
    }
    for y in 0..h {
        for x in 0..w {
            let i = y * stride + x * 4;
            for k in 0..4 {
                data[i + k] = px[y * w + x][k].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    drop(data);
    surface.mark_dirty();
    Ok(())
}

fn box_blur_pass(src: &[[f64; 4]], dst: &mut [[f64; 4]], w: usize, h: usize, r: isize, horizontal: bool) {
    let (n, m) = if horizontal { (h, w) } else { (w, h) };
    let idx = |line: usize, i: isize| if horizontal { line * w + i as usize } else { i as usize * w + line };
    let norm = 1.0 / (2 * r + 1) as f64;
    for line in 0..n {
        let mut acc = [0.0; 4];
        for i in -r..=r {
            let j = i.clamp(0, m as isize - 1);
            for k in 0..4 {
                acc[k] += src[idx(line, j)][k];
            }
        }
        for i in 0..m as isize {
            for k in 0..4 {
                dst[idx(line, i)][k] = acc[k] * norm;
            }
            let out = (i - r).clamp(0, m as isize - 1);
            let inn = (i + r + 1).clamp(0, m as isize - 1);
            for k in 0..4 {
                acc[k] += src[idx(line, inn)][k] - src[idx(line, out)][k];
            }
        }
    }
}

// ------------------------------------------------------------------ the player

/// The segment playing: frames `[start, end]` from `started` (frame-clock µs), looped or held
/// at `end`.
struct Playing {
    start: f64,
    end: f64,
    looped: bool,
    started: i64,
}

pub struct Player {
    comp: Rc<Composition>,
    playing: Option<Playing>,
    face: Face,
}

impl Player {
    pub fn new(comp: Rc<Composition>) -> Player {
        Player { comp, playing: None, face: Face::Idle }
    }

    /// `UpdateMarker`: a state change stops the current segment and starts the new one at
    /// its first frame.
    pub fn set_face(&mut self, face: &Face, now_us: i64) {
        if *face == self.face {
            return;
        }
        self.face = face.clone();
        self.playing = segment_for(face).and_then(|(name, looped)| {
            let (start, end) = self.comp.segment(name)?;
            Some(Playing { start, end, looped, started: now_us })
        });
    }

    /// The frame to show at `now_us`, `None` when nothing plays.
    pub fn frame_at(&self, now_us: i64) -> Option<f64> {
        let p = self.playing.as_ref()?;
        let elapsed = (now_us - p.started).max(0) as f64 / 1e6 * f64::from(self.comp.frame_rate);
        let len = p.end - p.start;
        Some(if p.looped && len > 0.0 { p.start + elapsed.rem_euclid(len) } else { (p.start + elapsed).min(p.end) })
    }

    pub fn is_playing(&self) -> bool {
        self.playing.is_some()
    }
}

/// The eye as a widget: `size` is its logical size (Windows: 64 px).
pub struct SolidEye {
    pub widget: gtk::DrawingArea,
    player: Rc<RefCell<Player>>,
}

/// White, the theme colour on the dark sign-in screen (`HelloSolidAnimations::SolidColor`).
pub const THEME_WHITE: Color = Color { a: 1.0, r: 1.0, g: 1.0, b: 1.0 };

impl SolidEye {
    pub fn new(path: &Path, size: i32) -> Result<SolidEye> {
        let comp = Rc::new(Composition::load(path)?);
        let player = Rc::new(RefCell::new(Player::new(Rc::clone(&comp))));
        let widget = gtk::DrawingArea::new();
        widget.set_size_request(size, size);
        let draw = Rc::clone(&player);
        widget.set_draw_func(move |area, cr, width, height| {
            let now = area.frame_clock().map_or(0, |c| c.frame_time());
            let Some(frame) = draw.borrow().frame_at(now) else { return };
            let side = f64::from(width.min(height));
            cr.translate((f64::from(width) - side) / 2.0, (f64::from(height) - side) / 2.0);
            cr.rectangle(0.0, 0.0, side, side);
            cr.clip();
            cr.scale(side / 64.0, side / 64.0);
            if let Err(e) = Frame::new(&comp, frame, THEME_WHITE).render(cr) {
                tracing::warn!("eye: {e:#}");
            }
        });
        let tick = Rc::clone(&player);
        widget.add_tick_callback(move |widget, _| {
            if tick.borrow().is_playing() {
                widget.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        Ok(SolidEye { widget, player })
    }

    pub fn set_face(&self, face: &Face) {
        let now = self.widget.frame_clock().map_or(0, |c| c.frame_time());
        self.player.borrow_mut().set_face(face, now);
        self.widget.queue_draw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_bezier_matches_hand_computed_points() {
        // Control points on the diagonal: the identity.
        assert!((cubic_bezier([0.167, 0.167], [0.833, 0.833], 0.3) - 0.3).abs() < 1e-6);
        // Ease-in-out (0.42, 0)–(0.58, 1) at t = 0.25: x = 3·0.75²·0.25·0.42 + 3·0.75·0.25²·0.58 + 0.25³
        // = 0.274375, y = 3·0.75·0.25² + 0.25³ = 0.15625.
        assert!((cubic_bezier([0.42, 0.0], [0.58, 1.0], 0.274_375) - 0.156_25).abs() < 1e-5);
        // Ease-out (0, 0)–(0.58, 1) at t = 0.5: x = 3·0.5·0.25·0.58 + 0.125 = 0.3425, y = 0.375 + 0.125 = 0.5.
        assert!((cubic_bezier([0.0, 0.0], [0.58, 1.0], 0.342_5) - 0.5).abs() < 1e-5);
        // The entrance's pop (CubicBezierEasingFunction_01, (0.14, 0.98)–(0.292, 1)) at t = 0.5:
        // x = 0.0525 + 0.1095 + 0.125 = 0.287, y = 0.3675 + 0.375 + 0.125 = 0.8675.
        assert!((cubic_bezier([0.14, 0.98], [0.292, 1.0], 0.287) - 0.867_5).abs() < 1e-5);
        assert_eq!(cubic_bezier([0.0, 0.0], [0.58, 1.0], 0.0), 0.0);
        assert_eq!(cubic_bezier([0.0, 0.0], [0.58, 1.0], 1.0), 1.0);
    }

    fn composition(json: &str) -> Composition {
        serde_json::from_str(json).unwrap()
    }

    /// A scalar animation in the file's shape: 0 → hold → 0 → ease to 10, as `ScalarAnimation_0_to_10`.
    const ANIM: &str = r#"{
        "frame_count": 2270, "frame_rate": 60, "markers": [], "root": "v",
        "objects": {
            "v": {"kind": "ShapeVisual", "props": {"Size": [1, 1]}, "animations": [{"property": "Opacity", "animation": "a"}]},
            "hold": {"kind": "StepEasingFunction", "props": {"IsFinalStepSingleFrame": true}},
            "step": {"kind": "StepEasingFunction", "props": {"IsInitialStepSingleFrame": true}},
            "ease": {"kind": "CubicBezierEasingFunction", "props": {"ControlPoint1": [0.167, 0.167], "ControlPoint2": [0.833, 0.833]}},
            "a": {"kind": "ScalarKeyFrameAnimation", "keyframes": [
                {"progress": 0.0, "value": 0.0, "easing": "step"},
                {"progress": 0.5, "value": 0.0, "easing": "hold"},
                {"progress": 0.6, "value": 10.0, "easing": "ease"},
                {"progress": 0.8, "value": 20.0, "easing": "step"}]}
        }}"#;

    #[test]
    fn keyframes_interpolate_with_the_target_easing_and_hold_after_the_last() {
        let comp = composition(ANIM);
        let at = |frame: f64| Frame::new(&comp, frame, THEME_WHITE).scalar("v", "Opacity", 1.0);
        assert_eq!(at(0.0), 0.0);
        assert_eq!(at(0.3 * 2270.0), 0.0);
        // Half-way through the eased interval: the symmetric ease gives exactly half.
        assert!((at(0.55 * 2270.0) - 5.0).abs() < 1e-6);
        assert!((at(0.6 * 2270.0) - 10.0).abs() < 1e-9);
        // StepThenHold into the 20 key frame: 20 as soon as the interval starts.
        assert_eq!(at(0.61 * 2270.0), 20.0);
        assert_eq!(at(2270.0), 20.0);
    }

    #[test]
    fn hold_then_step_keeps_the_previous_value_until_the_key_frame() {
        let comp = composition(
            ANIM.replace(r#""value": 0.0, "easing": "hold""#, r#""value": 7.0, "easing": "hold""#).as_str(),
        );
        let at = |p: f64| Frame::new(&comp, p * 2270.0, THEME_WHITE).scalar("v", "Opacity", 1.0);
        assert_eq!(at(0.25), 0.0);
        assert_eq!(at(0.499), 0.0);
        assert_eq!(at(0.5), 7.0);
    }

    #[test]
    fn boolean_animations_are_discrete() {
        let comp = composition(
            r#"{"frame_count": 2270, "frame_rate": 60, "markers": [], "root": "v", "objects": {
                "v": {"kind": "ContainerVisual", "animations": [{"property": "IsVisible", "animation": "b"}]},
                "b": {"kind": "BooleanKeyFrameAnimation", "keyframes": [
                    {"progress": 0.0, "value": false}, {"progress": 0.5, "value": true}, {"progress": 0.75, "value": false}]}}}"#,
        );
        let vis = |p: f64| Frame::new(&comp, p * 2270.0, THEME_WHITE).visible("v");
        assert!(!vis(0.0));
        assert!(!vis(0.49));
        assert!(vis(0.5));
        assert!(vis(0.74));
        assert!(!vis(0.75));
    }

    #[test]
    fn trim_cuts_a_line_by_length() {
        let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 10, 10).unwrap();
        let cr = cairo::Context::new(&surface).unwrap();
        let line = vec![Figure {
            start: [0.0, 0.0],
            closed: false,
            segments: vec![vec!["L".into(), 100.0.into(), 0.0.into()]],
        }];
        trimmed_path(&cr, &line, 0.25, 0.75);
        let (x0, _, x1, _) = cr.path_extents().unwrap();
        assert!((x0 - 25.0).abs() < 1e-9 && (x1 - 75.0).abs() < 1e-9);
        // Wrapping past 1: two pieces.
        cr.new_path();
        trimmed_path(&cr, &line, 0.9, 1.2);
        let (x0, _, x1, _) = cr.path_extents().unwrap();
        assert!((x0 - 0.0).abs() < 1e-9 && (x1 - 100.0).abs() < 1e-9);
    }

    #[test]
    fn segments_loop_or_hold_at_60_fps() {
        let comp = composition(
            r#"{"frame_count": 2270, "frame_rate": 60, "root": "v", "objects": {"v": {"kind": "ContainerVisual"}},
                "markers": [{"name": "FaceStart", "progress": 0.0}, {"name": "FaceEnd", "progress": 0.026},
                            {"name": "FaceLoopStart", "progress": 0.0264537444933921},
                            {"name": "FaceLoopEnd", "progress": 0.127775330396476}]}"#,
        );
        let mut p = Player::new(Rc::new(comp));
        p.set_face(&Face::Looking, 0);
        assert_eq!(p.frame_at(0), Some(0.0));
        assert_eq!(p.frame_at(500_000), Some(30.0));
        assert!((p.frame_at(2_000_000).unwrap() - 59.02).abs() < 1e-9); // held at FaceEnd
        p.set_face(&Face::Verifying, 2_000_000);
        let start = p.frame_at(2_000_000).unwrap();
        assert!((start - 60.05).abs() < 1e-9, "{start}");
        // 3.8333 s = 230 frames = one lap; 10 ms later the loop is 0.6 frame in again.
        let lap = p.frame_at(2_000_000 + 3_843_333).unwrap();
        assert!((lap - (start + 0.6)).abs() < 1e-3, "{lap}");
        assert!((p.frame_at(2_000_000 + 1_000_000).unwrap() - 120.05).abs() < 1e-9);
        p.set_face(&Face::Idle, 0);
        assert_eq!(p.frame_at(1), None);
    }

    /// The real file, when the git-ignored assets are around (`$LINUX_HELLO_ASSETS`,
    /// `$LINUX_HELLO_MODELS/windows_hello/assets`, the repository's models directory).
    fn real_composition() -> Option<Composition> {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../models/windows_hello/assets");
        let dirs = [
            std::env::var_os("LINUX_HELLO_ASSETS").map(std::path::PathBuf::from),
            std::env::var_os("LINUX_HELLO_MODELS").map(|m| Path::new(&m).join("windows_hello/assets")),
            Some(repo),
        ];
        let file = dirs.into_iter().flatten().map(|d| d.join("hello/eye/solid.json")).find(|f| f.is_file())?;
        Some(Composition::load(&file).expect("solid.json"))
    }

    #[test]
    fn the_compiled_composition_has_windows_timing_and_markers() {
        let Some(comp) = real_composition() else {
            eprintln!("solid.json not found: skipped");
            return;
        };
        assert_eq!((comp.frame_count, comp.frame_rate), (2270, 60));
        // 2270 frames at 60 fps = 37.833 s = the composition's Duration of 378,333,333 ticks.
        let seconds = f64::from(comp.frame_count) / f64::from(comp.frame_rate);
        assert!((seconds - 378_333_333e-7).abs() < 1e-3);
        for (name, start, end) in [
            ("Face", 0, 59),
            ("FaceLoop", 60, 290),
            ("FaceSuccess", 291, 408),
            ("FaceFail", 409, 477),
            ("TooFar", 1528, 1758),
            ("TooClose", 1759, 1989),
            ("Spinner", 1990, 2109),
        ] {
            let (s, e) = comp.segment(name).unwrap();
            assert_eq!((s.floor() as i64, e.floor() as i64), (start, end), "{name}");
        }
        // Frame 0 is blank, the face has appeared by the end of the entrance.
        let coverage = |frame: f64| {
            let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, 64, 64).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            Frame::new(&comp, frame, THEME_WHITE).render(&cr).unwrap();
            drop(cr);
            let data = surface.take_data().unwrap();
            data.chunks(4).filter(|px| px[3] > 128).count()
        };
        assert_eq!(coverage(0.0), 0);
        assert!(coverage(59.05) > 100);
    }
}
