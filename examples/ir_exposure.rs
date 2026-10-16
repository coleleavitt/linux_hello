//! Exposure measurements on the real IR camera: mean lit-frame brightness per frame from stream
//! start (warm-up), and after each exposure region change.
//!
//! Usage: `ir_exposure [--frames N] [x1,y1,x2,y2 ...]`, with the regions as face boxes in frame
//! pixels (`full` = the whole frame). Prints one line per frame: index, time since the stream
//! started, lit flag, mean brightness of the whole frame and of the last region set.
//! Devices: `LINUX_HELLO_IR` (default /dev/video2) and `LINUX_HELLO_IR_META` (default /dev/video3).

use anyhow::Result;
use image::GrayImage;
use linux_hello::IrCamera;
use std::time::Instant;

fn mean(image: &GrayImage, bbox: [f32; 4]) -> f32 {
    let [x1, y1, x2, y2] = bbox.map(|v| v as u32);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in y1.min(image.height())..y2.min(image.height()) {
        for x in x1.min(image.width())..x2.min(image.width()) {
            sum += image.get_pixel(x, y).0[0] as u64;
            n += 1;
        }
    }
    sum as f32 / n.max(1) as f32
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("debug").init();
    let mut args = std::env::args().skip(1).peekable();
    let mut frames = 30;
    if args.peek().map(String::as_str) == Some("--frames") {
        args.next();
        frames = args.next().expect("--frames N").parse()?;
    }
    let device = std::env::var("LINUX_HELLO_IR").unwrap_or_else(|_| "/dev/video2".into());
    let meta = std::env::var("LINUX_HELLO_IR_META").unwrap_or_else(|_| "/dev/video3".into());

    let t0 = Instant::now();
    let mut camera = IrCamera::open_with(&device, Some(&meta), 0)?;
    println!("# opened in {:.0} ms", t0.elapsed().as_secs_f32() * 1e3);
    let full = [0.0, 0.0, 640.0, 360.0];
    let mut region = full;
    let mut index = 0;
    let step = |camera: &mut IrCamera, label: &str, region: [f32; 4], index: &mut usize| -> Result<()> {
        println!("# {label}");
        for _ in 0..frames {
            let frame = camera.capture_frame()?;
            println!(
                "{index:3} t={:6.0}ms lit={:<5} frame={:6.1} region={:6.1}",
                t0.elapsed().as_secs_f32() * 1e3,
                frame.lit.map_or("?".to_string(), |l| l.to_string()),
                mean(&frame.image, full),
                mean(&frame.image, region),
            );
            *index += 1;
        }
        Ok(())
    };
    step(&mut camera, "stream start, default region", region, &mut index)?;
    for arg in args {
        region = if arg == "full" {
            full
        } else {
            let v: Vec<f32> = arg.split(',').map(|s| s.parse()).collect::<Result<_, _>>()?;
            [v[0], v[1], v[2], v[3]]
        };
        camera.expose_on(region);
        step(&mut camera, &format!("region {region:?}"), region, &mut index)?;
    }
    Ok(())
}
