//! The Hello icon under the user's name: Windows' eye animation when the assets have it
//! ([`super::eye`]), a glyph of its icon font, or our Cairo face (the shape of
//! `hello-face.svg`: two eyes and a smile).

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{glib, pango};
use tracing::warn;

use super::eye::Eye;
use crate::assets::Hello;
use crate::state::Face;

/// Windows: a 64×64 Lottie canvas / a 60 px glyph.
pub const SIZE: i32 = 64;

pub struct HelloIcon {
    pub widget: gtk::Widget,
    kind: Kind,
}

enum Kind {
    Eye(Eye),
    Glyph,
    Vector(Rc<RefCell<VectorState>>),
}

struct VectorState {
    face: Face,
    /// Frame-clock time when the looking animation began.
    started: Option<i64>,
}

impl HelloIcon {
    pub fn new(source: &Hello) -> HelloIcon {
        let (widget, kind): (gtk::Widget, Kind) = match source {
            Hello::Eye(dir) => match Eye::new(dir, SIZE) {
                Ok(eye) => (eye.widget.clone().upcast(), Kind::Eye(eye)),
                Err(e) => {
                    warn!("Windows eye animation in {}: {e:#}; using the built-in icon", dir.display());
                    vector()
                }
            },
            Hello::Glyph { family, ch } => {
                let label = gtk::Label::new(Some(&ch.to_string()));
                let attrs = pango::AttrList::new();
                attrs.insert(pango::AttrString::new_family(family));
                attrs.insert(pango::AttrSize::new_size_absolute(SIZE * pango::SCALE * 3 / 4));
                label.set_attributes(Some(&attrs));
                (label.upcast(), Kind::Glyph)
            }
            Hello::Vector => vector(),
        };
        widget.set_size_request(SIZE, SIZE);
        widget.add_css_class("hello-icon");
        HelloIcon { widget, kind }
    }

    pub fn set_face(&self, face: &Face) {
        match &self.kind {
            Kind::Eye(eye) => eye.set_face(face),
            Kind::Glyph => self.widget.set_opacity(if matches!(face, Face::Failed(_)) { 0.55 } else { 1.0 }),
            Kind::Vector(state) => {
                let mut s = state.borrow_mut();
                if s.face == *face {
                    return;
                }
                s.face = face.clone();
                s.started = None;
                drop(s);
                self.widget.set_opacity(if matches!(face, Face::Failed(_)) { 0.55 } else { 1.0 });
                self.widget.queue_draw();
                if matches!(face, Face::Looking | Face::Verifying) {
                    let state = Rc::clone(state);
                    let face = face.clone();
                    self.widget.add_tick_callback(move |widget, clock| {
                        let mut s = state.borrow_mut();
                        if s.face != face {
                            return glib::ControlFlow::Break;
                        }
                        s.started.get_or_insert(clock.frame_time());
                        widget.queue_draw();
                        glib::ControlFlow::Continue
                    });
                }
            }
        }
    }
}

fn vector() -> (gtk::Widget, Kind) {
    let state = Rc::new(RefCell::new(VectorState { face: Face::Idle, started: None }));
    let area = gtk::DrawingArea::new();
    let draw = Rc::clone(&state);
    area.set_draw_func(move |area, cr, width, height| {
        let s = draw.borrow();
        let elapsed = s.started.map_or(0.0, |t0| (area.frame_clock().map_or(t0, |c| c.frame_time()) - t0) as f64 / 1e6);
        draw_vector(cr, f64::from(width), f64::from(height), &s.face, elapsed);
    });
    (area.upcast(), Kind::Vector(state))
}

/// The Windows Hello face: two eyes and a smile (as `hello-face.svg`, no outline). While
/// looking the eyes glance from side to side; recognised, they close happily.
fn draw_vector(cr: &gtk::cairo::Context, width: f64, height: f64, face: &Face, elapsed: f64) {
    let radius = width.min(height) / 2.0;
    cr.translate(width / 2.0, height / 2.0);
    cr.scale(radius, radius);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(0.17);
    cr.set_line_cap(gtk::cairo::LineCap::Round);

    let glance = if matches!(face, Face::Looking | Face::Verifying) { 0.1 * (elapsed * 1.6).sin() } else { 0.0 };
    for x in [-0.42, 0.42] {
        if *face == Face::Welcome {
            cr.arc(x, -0.3, 0.16, 1.15 * std::f64::consts::PI, 1.85 * std::f64::consts::PI);
            let _ = cr.stroke();
        } else {
            cr.arc(x + glance, -0.42, 0.13, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
    }
    // The smile: an arc through (-0.78, 0.1) and (0.78, 0.1), bottoming out at y = 0.62.
    cr.arc(0.0, -0.45, 1.07, 0.26 * std::f64::consts::PI, 0.74 * std::f64::consts::PI);
    let _ = cr.stroke();
}
