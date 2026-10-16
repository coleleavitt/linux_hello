//! The Hello face icon: Windows' own still and animation when the assets have them, a glyph
//! of its icon font, or our Cairo drawing. The animation plays while looking.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk::prelude::GdkCairoContextExt;
use gtk::gdk_pixbuf::{Pixbuf, PixbufAnimation, PixbufAnimationIter};
use gtk::prelude::*;
use gtk::{glib, pango};

use crate::assets::Hello;
use crate::state::Face;

/// Logical size in pixels; Windows' images are scaled to it.
pub const SIZE: i32 = 56;

pub struct HelloIcon {
    pub widget: gtk::Widget,
    shared: Rc<RefCell<Shared>>,
}

struct Shared {
    face: Face,
    still: Option<Pixbuf>,
    animation: Option<PixbufAnimation>,
    /// The running animation (looking, recognised).
    playing: Option<PixbufAnimationIter>,
    /// Seconds since the looking animation of the Cairo face began.
    started: Option<i64>,
}

impl HelloIcon {
    pub fn new(source: &Hello) -> HelloIcon {
        let mut shared = Shared { face: Face::Idle, still: None, animation: None, playing: None, started: None };
        let widget: gtk::Widget = match source {
            Hello::Glyph { family, ch } => {
                let label = gtk::Label::new(Some(&ch.to_string()));
                let attrs = pango::AttrList::new();
                attrs.insert(pango::AttrString::new_family(family));
                attrs.insert(pango::AttrSize::new_size_absolute(SIZE * pango::SCALE * 3 / 4));
                label.set_attributes(Some(&attrs));
                label.upcast()
            }
            Hello::Windows { still, animation } => {
                shared.still = still.as_ref().and_then(|p| Pixbuf::from_file_at_scale(p, SIZE, SIZE, true).ok());
                shared.animation = animation.as_ref().and_then(|p| PixbufAnimation::from_file(p).ok());
                gtk::DrawingArea::new().upcast()
            }
            Hello::Vector => gtk::DrawingArea::new().upcast(),
        };
        let shared = Rc::new(RefCell::new(shared));
        if let Some(area) = widget.downcast_ref::<gtk::DrawingArea>() {
            let draw = Rc::clone(&shared);
            area.set_draw_func(move |area, cr, width, height| draw.borrow().draw(area, cr, width, height));
        }
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
        shared.playing = None;
        // Windows' GIF plays once and ends on the smile: looped while looking, once when
        // recognised.
        let animate = matches!(face, Face::Starting | Face::Looking | Face::Verifying | Face::Welcome);
        if animate {
            shared.playing = shared.animation.as_ref().map(|a| a.iter(Some(std::time::SystemTime::now())));
        }
        drop(shared);
        self.widget.set_opacity(if matches!(face, Face::Failed(_)) { 0.55 } else { 1.0 });
        self.widget.queue_draw();
        if animate {
            let shared = Rc::clone(&self.shared);
            let face = face.clone();
            self.widget.add_tick_callback(move |widget, clock| {
                let mut s = shared.borrow_mut();
                if s.face != face {
                    return glib::ControlFlow::Break;
                }
                s.started.get_or_insert(clock.frame_time());
                if let Some(playing) = &s.playing {
                    let now = std::time::SystemTime::now();
                    playing.advance(now);
                    // No delay once a non-looping animation has shown its last frame.
                    if playing.delay_time().is_none() {
                        if face == Face::Welcome {
                            return glib::ControlFlow::Break;
                        }
                        s.playing = s.animation.as_ref().map(|a| a.iter(Some(now)));
                    }
                }
                widget.queue_draw();
                glib::ControlFlow::Continue
            });
        }
    }
}

impl Shared {
    fn draw(&self, area: &gtk::DrawingArea, cr: &gtk::cairo::Context, width: i32, height: i32) {
        let frame = match (&self.playing, &self.still) {
            (Some(playing), _) => Some(playing.pixbuf()),
            (None, Some(still)) => Some(still.clone()),
            (None, None) => None,
        };
        match frame {
            Some(frame) => {
                let scale = f64::from(width.min(height)) / f64::from(frame.width().max(frame.height()));
                cr.translate(
                    (f64::from(width) - scale * f64::from(frame.width())) / 2.0,
                    (f64::from(height) - scale * f64::from(frame.height())) / 2.0,
                );
                cr.scale(scale, scale);
                cr.set_source_pixbuf(&frame, 0.0, 0.0);
                let _ = cr.paint();
            }
            None => {
                let elapsed = self
                    .started
                    .map_or(0.0, |t0| (area.frame_clock().map_or(t0, |c| c.frame_time()) - t0) as f64 / 1e6);
                draw_vector(cr, f64::from(width), f64::from(height), &self.face, elapsed);
            }
        }
    }
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
