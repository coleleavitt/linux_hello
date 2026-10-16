//! The Hello face icon: Windows' own frames or image when the assets have them, a glyph of
//! its icon font, or our Cairo drawing. It animates while looking and smiles when recognised.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gdk_pixbuf::Pixbuf;
use gtk::prelude::*;
use gtk::{glib, pango};

use crate::assets::Hello;
use crate::state::Face;

/// Logical size in pixels; frames and images are scaled to it.
pub const SIZE: i32 = 56;
const FRAME_RATE: f64 = 30.0;

pub struct HelloIcon {
    pub widget: gtk::Widget,
    shared: Rc<RefCell<Shared>>,
}

struct Shared {
    face: Face,
    frames: Vec<Pixbuf>,
    /// Microseconds of the frame clock when the current animation began.
    started: Option<i64>,
}

impl HelloIcon {
    pub fn new(source: &Hello) -> HelloIcon {
        let shared = Rc::new(RefCell::new(Shared { face: Face::Idle, frames: vec![], started: None }));
        let widget: gtk::Widget = match source {
            Hello::Glyph { family, ch } => {
                let label = gtk::Label::new(Some(&ch.to_string()));
                let attrs = pango::AttrList::new();
                attrs.insert(pango::AttrString::new_family(family));
                attrs.insert(pango::AttrSize::new_size_absolute(SIZE * pango::SCALE * 3 / 4));
                label.set_attributes(Some(&attrs));
                label.upcast()
            }
            Hello::Frames(paths) => {
                shared.borrow_mut().frames =
                    paths.iter().filter_map(|p| Pixbuf::from_file_at_scale(p, SIZE, SIZE, true).ok()).collect();
                drawing_area(&shared).upcast()
            }
            Hello::Image(path) => {
                shared.borrow_mut().frames =
                    Pixbuf::from_file_at_scale(path, SIZE, SIZE, true).ok().into_iter().collect();
                drawing_area(&shared).upcast()
            }
            Hello::Vector => drawing_area(&shared).upcast(),
        };
        widget.set_size_request(SIZE, SIZE);
        widget.add_css_class("hello-icon");
        HelloIcon { widget, shared }
    }

    pub fn set_face(&self, face: &Face) {
        let mut shared = self.shared.borrow_mut();
        if shared.face == *face {
            return;
        }
        shared.face = face.clone();
        shared.started = None;
        drop(shared);
        self.widget.set_opacity(if matches!(face, Face::Failed(_)) { 0.55 } else { 1.0 });
        self.widget.queue_draw();
        if matches!(face, Face::Looking) {
            let shared = Rc::clone(&self.shared);
            self.widget.add_tick_callback(move |widget, clock| {
                let mut s = shared.borrow_mut();
                if s.face != Face::Looking {
                    return glib::ControlFlow::Break;
                }
                s.started.get_or_insert(clock.frame_time());
                widget.queue_draw();
                glib::ControlFlow::Continue
            });
        }
    }
}

fn drawing_area(shared: &Rc<RefCell<Shared>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    let shared = Rc::clone(shared);
    area.set_draw_func(move |area, cr, width, height| {
        let s = shared.borrow();
        let elapsed = s.started.map_or(0.0, |t0| (area.frame_clock().map_or(t0, |c| c.frame_time()) - t0) as f64 / 1e6);
        if s.frames.is_empty() {
            draw_vector(cr, width as f64, height as f64, &s.face, elapsed);
        } else {
            let index = if s.face == Face::Looking { (elapsed * FRAME_RATE) as usize % s.frames.len() } else { 0 };
            let frame = &s.frames[index];
            cr.set_source_pixbuf(frame, (width - frame.width()) as f64 / 2.0, (height - frame.height()) as f64 / 2.0);
            let _ = cr.paint();
        }
    });
    area
}

/// A smiling face outline in the Windows Hello style: while looking the eyes glance from
/// side to side; recognised, the eyes close happily and the smile widens.
fn draw_vector(cr: &gtk::cairo::Context, width: f64, height: f64, face: &Face, elapsed: f64) {
    let radius = width.min(height) / 2.0 - 2.0;
    cr.translate(width / 2.0, height / 2.0);
    cr.scale(radius, radius);
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(2.4 / radius);
    cr.set_line_cap(gtk::cairo::LineCap::Round);

    cr.arc(0.0, 0.0, 1.0, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();

    let glance = if *face == Face::Looking { 0.12 * (elapsed * 1.6).sin() } else { 0.0 };
    for x in [-0.36, 0.36] {
        if *face == Face::Welcome {
            cr.arc(x, -0.1, 0.16, 1.15 * std::f64::consts::PI, 1.85 * std::f64::consts::PI);
            let _ = cr.stroke();
        } else {
            cr.arc(x + glance, -0.18, 0.09, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
        }
    }

    let (from, to) = if *face == Face::Welcome { (0.12, 0.88) } else { (0.2, 0.8) };
    cr.arc(0.0, 0.08, 0.55, from * std::f64::consts::PI, to * std::f64::consts::PI);
    let _ = cr.stroke();
}
