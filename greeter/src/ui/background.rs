//! The full-screen background: Windows' lock-screen image (or a solid colour), blurred and
//! dimmed while the sign-in screen is up, as Windows does when the lock screen slides away.

use std::path::Path;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

mod imp {
    use std::cell::Cell;

    use super::*;

    #[derive(Default)]
    pub struct Background {
        pub picture: gtk::Picture,
        /// 0 = the lock screen's sharp image, 1 = fully blurred and dimmed.
        pub amount: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Background {
        const NAME: &'static str = "HelloBackground";
        type Type = super::Background;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }
    }

    impl ObjectImpl for Background {
        fn constructed(&self) {
            self.parent_constructed();
            self.picture.set_content_fit(gtk::ContentFit::Cover);
            self.picture.set_parent(&*self.obj());
        }

        fn dispose(&self) {
            self.picture.unparent();
        }
    }

    impl WidgetImpl for Background {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let bounds = gtk::graphene::Rect::new(0.0, 0.0, widget.width() as f32, widget.height() as f32);
            let amount = self.amount.get();
            // The solid colour under everything also fills the blur's soft edges.
            snapshot.append_color(&gtk::gdk::RGBA::new(0.09, 0.11, 0.16, 1.0), &bounds);
            if self.picture.paintable().is_some() {
                if amount > 0.0 {
                    snapshot.push_blur(40.0 * amount);
                }
                widget.snapshot_child(&self.picture, snapshot);
                if amount > 0.0 {
                    snapshot.pop();
                }
            }
            if amount > 0.0 {
                snapshot.append_color(&gtk::gdk::RGBA::new(0.0, 0.0, 0.0, 0.45 * amount as f32), &bounds);
            }
        }
    }
}

glib::wrapper! {
    pub struct Background(ObjectSubclass<imp::Background>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Background {
    pub fn new(image: Option<&Path>) -> Background {
        let background: Background = glib::Object::new();
        if let Some(path) = image {
            background.imp().picture.set_filename(Some(path));
        }
        background
    }

    /// Blur and dim (`true`) or show the image sharp, over 250 ms.
    pub fn set_blurred(&self, blurred: bool) {
        let target = if blurred { 1.0 } else { 0.0 };
        let start = self.imp().amount.get();
        if start == target {
            return;
        }
        let began = std::cell::Cell::new(None);
        self.add_tick_callback(move |widget, clock| {
            let now = clock.frame_time();
            let t0 = began.get().unwrap_or(now);
            began.set(Some(t0));
            let progress = ((now - t0) as f64 / 250_000.0).min(1.0);
            widget.imp().amount.set(start + (target - start) * progress);
            widget.queue_draw();
            if progress < 1.0 {
                glib::ControlFlow::Continue
            } else {
                glib::ControlFlow::Break
            }
        });
    }
}
