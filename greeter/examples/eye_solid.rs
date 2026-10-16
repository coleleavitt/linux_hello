//! Render frames of the Windows 11 Hello eye (`hello/eye/solid.json`) to PNG for the docs
//! and for checking the reconstruction by eye:
//!
//!     cargo run -p hello-greeter --example eye_solid -- <solid.json> <out_dir> [px] [frames…]
//!
//! Without frames: the first, middle and last frame of every marker segment, plus
//! `contact-sheet.png`. Each PNG is the 64×64 canvas at `px` pixels (default 128) on the
//! sign-in screen's dark background.

use std::path::Path;

use gtk::cairo;
use hello_greeter::ui::eye_vector::{Composition, Frame, THEME_WHITE};

fn save_png(surface: &cairo::ImageSurface, path: &Path) {
    #[allow(deprecated)] // the replacement, gdk::Texture, cannot be written to a file
    let pixbuf = gtk::gdk::pixbuf_get_from_surface(surface, 0, 0, surface.width(), surface.height()).expect("pixbuf");
    pixbuf.savev(path, "png", &[]).expect("png");
}

fn render(comp: &Composition, frame: f64, px: i32, background: bool) -> cairo::ImageSurface {
    let surface = cairo::ImageSurface::create(cairo::Format::ARgb32, px, px).unwrap();
    let cr = cairo::Context::new(&surface).unwrap();
    if background {
        cr.set_source_rgb(0.1, 0.1, 0.1);
        cr.paint().unwrap();
    }
    cr.scale(f64::from(px) / 64.0, f64::from(px) / 64.0);
    Frame::new(comp, frame, THEME_WHITE).render(&cr).unwrap();
    drop(cr);
    surface
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: eye_solid <solid.json> <out_dir> [px] [frame…]");
        std::process::exit(2);
    }
    let comp = Composition::load(Path::new(&args[1])).expect("solid.json");
    let out = Path::new(&args[2]);
    std::fs::create_dir_all(out).unwrap();
    let px: i32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(128);
    let explicit: Vec<f64> = args.iter().skip(4).filter_map(|s| s.parse().ok()).collect();
    if !explicit.is_empty() {
        for f in explicit {
            save_png(&render(&comp, f, px, false), &out.join(format!("frame-{f:04.0}.png")));
        }
        return;
    }
    // One row per segment: first, middle, last frame.
    let mut segments = vec![];
    for m in &comp.markers {
        if let Some(name) = m.name.strip_suffix("Start") {
            if let Some((start, end)) = comp.segment(name) {
                segments.push((name.to_string(), start, end));
            }
        }
    }
    let cell = px + 8;
    let sheet =
        cairo::ImageSurface::create(cairo::Format::ARgb32, cell * 3 + 140, cell * segments.len() as i32).unwrap();
    let cr = cairo::Context::new(&sheet).unwrap();
    cr.set_source_rgb(0.1, 0.1, 0.1);
    cr.paint().unwrap();
    for (row, (name, start, end)) in segments.iter().enumerate() {
        let frames = [*start, ((start + end) / 2.0).floor() + 0.05, *end];
        for (col, f) in frames.iter().enumerate() {
            let s = render(&comp, *f, px, true);
            save_png(&s, &out.join(format!("eye-solid-{}-{f:04.0}.png", name.to_lowercase())));
            cr.set_source_surface(&s, f64::from(140 + col as i32 * cell), f64::from(row as i32 * cell)).unwrap();
            cr.paint().unwrap();
        }
        cr.set_source_rgb(0.9, 0.9, 0.9);
        cr.set_font_size(13.0);
        cr.move_to(6.0, f64::from(row as i32 * cell) + f64::from(px) / 2.0);
        cr.show_text(&format!("{name} {start:.0}–{end:.0}")).unwrap();
    }
    drop(cr);
    save_png(&sheet, &out.join("contact-sheet.png"));
    println!("{} segments → {}", segments.len(), out.display());
}
