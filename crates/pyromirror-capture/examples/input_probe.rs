//! Checks capture and input injection by hand on a real desktop.
//!
//! Starts capture, reports what the frames and the pointer look like, then moves the pointer to
//! each position given on the command line as `x,y` fractions of the monitor (for example
//! `0.5,0.5 0,0 1,1`). After each move the command in `PROBE_READ`, if set, is run; make it
//! print where the desktop says the pointer is, to compare. This moves the real pointer.
//!
//! cargo run -p pyromirror-capture --example input_probe -- 0.5,0.5

use std::time::{Duration, Instant};

use pyromirror_capture::{CaptureOptions, Capturer};

fn main() {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("warn"));
    let mut capturer = Capturer::new(&CaptureOptions::default()).expect("capture did not start");

    // The pointer's shape arrives with the frames.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut frames = 0;
    let mut size = None;
    while Instant::now() < deadline && frames < 30 {
        if let Ok(Some(frame)) = capturer.next_frame(Duration::from_millis(100)) {
            frames += 1;
            size = Some((frame.width, frame.height, frame.format, frame.texture.is_some()));
        }
    }
    println!("frames: {frames}, last: {size:?}");
    match capturer.cursor(None) {
        Some(c) => println!(
            "cursor: serial {} in_video {} visible {} {}x{} hot {},{} ({} bytes, {} opaque pixels)",
            c.serial,
            c.in_video,
            c.visible,
            c.width,
            c.height,
            c.hot_x,
            c.hot_y,
            c.rgba.len(),
            c.rgba.chunks_exact(4).filter(|p| p[3] == 255).count()
        ),
        None => println!("cursor: nothing reported"),
    }

    let Some(injector) = capturer.input_injector() else {
        println!("input: this desktop does not allow injection");
        return;
    };
    let read = std::env::var("PROBE_READ").ok();
    for arg in std::env::args().skip(1) {
        let Some((x, y)) = arg.split_once(',').and_then(|(x, y)| Some((x.parse::<f64>().ok()?, y.parse::<f64>().ok()?))) else {
            eprintln!("skipping `{arg}`: expected x,y");
            continue;
        };
        injector.pointer_absolute(x, y);
        std::thread::sleep(Duration::from_millis(300));
        let seen = read.as_ref().and_then(|command| std::process::Command::new("sh").arg("-c").arg(command).output().ok());
        let seen = seen.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).unwrap_or_default();
        println!("moved to {x},{y}: {seen}");
    }
}
