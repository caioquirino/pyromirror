//! PyroMirror client.
//!
//! Receives PyroWave packets over UDP, decodes them on the GPU and shows the result in an SDL3
//! window. Mouse and keyboard events go back to the server over the TCP control connection.

mod pointer;
mod toolbar;
mod video;

use std::io::Read;
use std::net::{Shutdown, SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::Parser;
use log::{info, warn};
use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::{Keycode, Mod};
use sdl3::pixels::{Color, PixelFormat};
use sdl3::render::FRect;

use pyromirror_codec::{Chroma, Decoder, Device};
use pyromirror_net::{create_streaming_socket, UDP_PUNCH};
use pyromirror_proto::auth::{self, TokenStore};
use pyromirror_proto::{
    read_message, write_message, AudioCodecType, ClientHello, CodecParameters, CursorHeader, InputEvent, VideoCodecType,
    MSG_TYPE_CURSOR,
    VideoColorProfile, MAX_MESSAGE_PAYLOAD, MSG_TYPE_CLIENT_HELLO, MSG_TYPE_CODEC_PARAMS,
    MSG_TYPE_INPUT_EVENT,
};

const DEFAULT_PORT: u16 = 9000;
const AUDIO_CUSHION_MS: usize = 40;
const AUDIO_MAX_QUEUED_MS: usize = 200;

#[derive(Parser, Debug)]
#[command(name = "pyromirror-client", version, about = "PyroMirror low-latency remote desktop viewer")]
struct Args {
    /// Server address: pyro://<host>[:<port>] or <host>[:<port>]
    uri: String,

    /// Decode with fragment shaders instead of compute (meant for mobile / weak integrated GPUs)
    #[arg(long)]
    force_fragment: bool,

    /// Start in fullscreen mode
    #[arg(short, long)]
    fullscreen: bool,

    /// Only pair with the host (asking for its code if needed), then exit without opening a window
    #[arg(long)]
    pair_only: bool,

    /// Keep the mouse pointer inside the viewer window (toggle with Ctrl+Alt+L)
    #[arg(long)]
    lock_mouse: bool,

    /// Local UDP port to receive video on (0 lets the OS pick)
    #[arg(long, default_value_t = 0)]
    local_port: u16,

    /// Do not play the host's audio
    #[arg(long)]
    no_audio: bool,

    /// Exit after this many seconds (for automated testing)
    #[arg(long, hide = true)]
    exit_after: Option<f64>,

    /// Write what the window shows to this BMP file on exit (for automated testing)
    #[arg(long, hide = true)]
    dump_window: Option<PathBuf>,

    /// Write the most recent frame to this file as a PPM image on exit (for automated testing)
    #[arg(long, hide = true)]
    dump_frame: Option<PathBuf>,
}

fn resolve(uri: &str) -> anyhow::Result<SocketAddr> {
    let host = uri.trim_start_matches("pyro://").trim_end_matches('/');
    let with_port = if host.parse::<SocketAddr>().is_ok() || host.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
        host.to_string()
    } else {
        format!("{}:{}", host, DEFAULT_PORT)
    };
    with_port
        .to_socket_addrs()
        .with_context(|| format!("could not resolve `{}`", with_port))?
        .next()
        .with_context(|| format!("`{}` has no address", with_port))
}

fn send_input(tcp: &mut TcpStream, event: InputEvent) -> bool {
    let mut buf = [0u8; 32];
    match event.serialize(&mut buf) {
        Ok(len) => write_message(tcp, MSG_TYPE_INPUT_EVENT, &buf[..len]).is_ok(),
        Err(_) => true,
    }
}

/// Largest rectangle with the stream's aspect ratio that fits the window, centred.
fn letterbox(window: (u32, u32), stream: (u32, u32)) -> FRect {
    let (ww, wh) = (window.0.max(1) as f32, window.1.max(1) as f32);
    let scale = (ww / stream.0 as f32).min(wh / stream.1 as f32);
    let (w, h) = (stream.0 as f32 * scale, stream.1 as f32 * scale);
    FRect::new((ww - w) / 2.0, (wh - h) / 2.0, w, h)
}

fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    let args = Args::parse();
    let server_addr = resolve(&args.uri)?;

    // 1. Bind the video socket first so its port can be announced in the handshake.
    let unspecified: std::net::IpAddr = if server_addr.is_ipv6() {
        std::net::Ipv6Addr::UNSPECIFIED.into()
    } else {
        std::net::Ipv4Addr::UNSPECIFIED.into()
    };
    let udp = create_streaming_socket(SocketAddr::new(unspecified, args.local_port), 32 * 1024 * 1024)
        .context("could not bind the UDP video socket")?;
    udp.set_read_timeout(Some(Duration::from_millis(50)))?;
    let local_udp_port = udp.local_addr()?.port();

    // 2. Handshake over TCP: hello, pairing, stream description.
    let config_dir = auth::config_dir().unwrap_or_else(std::env::temp_dir);
    let client_id = auth::local_id(&config_dir.join("client-id"));
    let mut hosts = TokenStore::load(config_dir.join("paired-hosts"));

    info!("Connecting to {}", server_addr);
    let mut tcp = TcpStream::connect_timeout(&server_addr, Duration::from_secs(5))
        .with_context(|| format!("could not connect to {}", server_addr))?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(10)))?;

    let mut hello = [0u8; ClientHello::SIZE];
    let flags = if args.pair_only { pyromirror_proto::HELLO_PAIR_ONLY } else { 0 };
    ClientHello { udp_port: local_udp_port, flags }.serialize(&mut hello)?;
    write_message(&mut tcp, MSG_TYPE_CLIENT_HELLO, &hello)?;

    // A host that does not know this computer shows a one-time code, which the person types
    // here: on the terminal, or into the launcher, which passes it on through our stdin.
    let host = auth::connect(&mut tcp, client_id, &auth::device_name(), &mut hosts, |prompt| {
        if prompt.wrong_attempts > 0 {
            warn!("Wrong pairing code, try again");
        } else if prompt.pairing_revoked {
            // The launcher reads this line too.
            warn!("This computer is no longer paired with the host; it has to be paired again");
        }
        // The launcher reads this line to show its pairing prompt.
        info!("Pairing code needed: enter the code shown on the host");
        let mut line = String::new();
        match std::io::stdin().read_line(&mut line) {
            Ok(n) if n > 0 && !line.trim().is_empty() => Some(line),
            _ => None,
        }
    })
    .context("could not pair with the host")?;
    if host.newly_paired {
        info!("Paired with this host; no code will be needed next time");
    }
    // The launcher reads this line to remember the computer.
    info!("Host: {} {}", auth::to_hex(&host.server_id), host.server_name);
    if args.pair_only {
        return Ok(());
    }

    let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
    let (msg_type, len) = read_message(&mut tcp, &mut payload).context("no stream description from the server")?;
    if msg_type != MSG_TYPE_CODEC_PARAMS {
        bail!("unexpected handshake reply (message type {})", msg_type);
    }
    let params = CodecParameters::deserialize(&payload[..len])?;
    tcp.set_read_timeout(None)?;

    if params.video_codec != VideoCodecType::PyroWave {
        bail!("server offers {:?}, this client only decodes PyroWave", params.video_codec);
    }
    let chroma = match params.video_color_profile {
        VideoColorProfile::Bt709FullChroma444 => Chroma::C444,
        VideoColorProfile::Bt709FullCenterChroma420 => Chroma::C420,
        other => bail!("unsupported colour profile {:?}", other),
    };
    let (width, height) = (params.width as u32, params.height as u32);
    info!("Stream: {}x{} @ {} fps, chroma {:?}; receiving on UDP port {}", width, height, params.frame_rate_num, chroma, local_udp_port);

    // 3. Decoder.
    let device = Device::new().context("could not initialise PyroWave")?;
    let fragment_path = args.force_fragment || device.prefers_fragment_decode();
    let decoder = Decoder::new(device, width, height, chroma, fragment_path)
        .context("could not create the PyroWave decoder")?;

    // 4. Window.
    let sdl = sdl3::init()?;
    let video_subsystem = sdl.video()?;

    // Start at the stream's size unless that does not fit the screen.
    let (mut win_w, mut win_h) = (width, height);
    if let Ok(bounds) = video_subsystem.get_primary_display().and_then(|d| d.get_usable_bounds()) {
        let (max_w, max_h) = (bounds.width() * 9 / 10, bounds.height() * 9 / 10);
        if win_w > max_w || win_h > max_h {
            let scale = (max_w as f32 / win_w as f32).min(max_h as f32 / win_h as f32);
            win_w = ((win_w as f32 * scale) as u32).max(1);
            win_h = ((win_h as f32 * scale) as u32).max(1);
        }
    }

    let mut window_builder = video_subsystem.window("PyroMirror", win_w, win_h);
    window_builder.resizable().position_centered();
    if args.fullscreen {
        window_builder.fullscreen();
    }
    let mut canvas = window_builder.build()?.into_canvas();
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormat::RGBA32, width, height)
        .context("could not create the video texture")?;

    // Audio: packets are queued into an SDL stream, which resamples to whatever the device wants.
    let audio_rate = params.audio_sample_rate as usize;
    let audio_channels = params.audio_channels as usize;
    let audio_stream = if args.no_audio || params.audio_codec != AudioCodecType::RawS16LE || audio_rate == 0 || audio_channels == 0 {
        None
    } else {
        let spec = sdl3::audio::AudioSpec {
            freq: Some(audio_rate as i32),
            channels: Some(audio_channels as i32),
            format: Some(sdl3::audio::AudioFormat::S16LE),
        };
        let opened = sdl.audio().and_then(|audio| {
            let stream = audio.default_playback_device().open_device_stream(Some(&spec))?;
            stream.resume()?;
            Ok(stream)
        });
        match opened {
            Ok(stream) => {
                info!("Audio: {} Hz, {} channels", audio_rate, audio_channels);
                Some(stream)
            }
            Err(e) => {
                warn!("No audio playback: {}", e);
                None
            }
        }
    };
    let (audio_tx, audio_rx) = crossbeam_channel::bounded::<Vec<i16>>(256);
    let audio_tx = audio_stream.is_some().then_some(audio_tx);
    // Queue this much before playback so network jitter does not cause dropouts, and start over
    // if the queue grows well past it (the two clocks drift apart slowly).
    let bytes_per_ms = audio_rate * audio_channels * 2 / 1000;
    let audio_cushion = vec![0i16; audio_rate * audio_channels * AUDIO_CUSHION_MS / 1000];
    let audio_max_queued = (bytes_per_ms * AUDIO_MAX_QUEUED_MS) as i32;

    // 5. Background threads: video receive/decode, UDP keepalive, control-channel watchdog.
    let running = Arc::new(AtomicBool::new(true));
    let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Vec<u8>>(2);
    let (recycle_tx, recycle_rx) = crossbeam_channel::bounded::<Vec<u8>>(4);

    let summary = Arc::new(Mutex::new(String::new()));
    let video_thread = {
        let (udp, running, summary) = (udp.try_clone()?, running.clone(), summary.clone());
        std::thread::Builder::new()
            .name("video".into())
            .spawn(move || video::receive_loop(udp, decoder, width, height, frame_tx, recycle_rx, audio_tx, summary, running))?
    };

    {
        // Tells the server where to send video and keeps NAT / firewall state alive.
        let running = running.clone();
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let _ = udp.send_to(UDP_PUNCH, server_addr);
                std::thread::sleep(Duration::from_millis(500));
            }
        });
    }

    let (cursor_tx, cursor_rx) = crossbeam_channel::unbounded::<(CursorHeader, Vec<u8>)>();
    {
        // After the handshake the server only sends the pointer's shape; a failed read means it
        // went away.
        let (mut tcp, running) = (tcp.try_clone()?, running.clone());
        std::thread::spawn(move || {
            let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
            loop {
                match read_message(&mut tcp, &mut payload) {
                    Ok((MSG_TYPE_CURSOR, len)) => {
                        let Ok(header) = CursorHeader::deserialize(&payload[..len]) else { break };
                        let mut image = vec![0u8; header.image_len()];
                        if tcp.read_exact(&mut image).is_err() {
                            break;
                        }
                        let _ = cursor_tx.send((header, image));
                    }
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if running.swap(false, Ordering::Relaxed) {
                warn!("Server closed the connection");
            }
        });
    }

    // 6. Event and presentation loop.
    info!("Move the pointer to the top edge of the window for the toolbar");
    info!("Ctrl+Alt+G: grab keyboard | Ctrl+Alt+L: lock mouse | Ctrl+Alt+M: relative mouse | Ctrl+Alt+F: fullscreen | Ctrl+Alt+Q: quit");
    let mut event_pump = sdl.event_pump()?;
    let started = Instant::now();
    let mut grab = false;
    let mut relative_mouse = false;
    let mut fullscreen = args.fullscreen;
    let mut have_frame = false;
    let mut last_frame: Option<Vec<u8>> = None;
    let mut redraw = true;
    let mut remote_pointer = pointer::Pointer::new();
    let mut muted = false;
    // Confines the pointer to the window so it cannot slip onto another monitor or the local
    // taskbar; takes effect while the window has focus.
    let mut mouse_lock = args.lock_mouse;
    if mouse_lock {
        canvas.window_mut().set_mouse_grab(true);
    }
    let mut toolbar = toolbar::Toolbar::new();
    let mut pointer: Option<(f32, f32)> = None;
    // A press that landed on the toolbar; its release must not reach the remote desktop either.
    let mut toolbar_press = false;

    'main: while running.load(Ordering::Relaxed) {
        if args.exit_after.is_some_and(|secs| started.elapsed().as_secs_f64() >= secs) {
            break;
        }

        let dst = letterbox(canvas.window().size(), (width, height));
        // Window coordinates -> stream pixels.
        let to_stream = |x: f32, y: f32| {
            let sx = ((x - dst.x) / dst.w * width as f32).clamp(0.0, width as f32 - 1.0);
            let sy = ((y - dst.y) / dst.h * height as f32).clamp(0.0, height as f32 - 1.0);
            (sx as u16, sy as u16)
        };

        let window_width = canvas.window().size().0 as f32;
        let stats = summary.lock().unwrap().clone();

        for event in event_pump.poll_iter() {
            // The toolbar gets first pick of pointer events (not in relative mode, where there
            // is no pointer position to speak of).
            let mut action = None;
            match &event {
                Event::MouseMotion { x, y, .. } => pointer = Some((*x, *y)),
                Event::Window { win_event: WindowEvent::MouseLeave, .. } => pointer = None,
                _ => {}
            }
            if !relative_mouse {
                match &event {
                    Event::MouseMotion { x, y, .. } if toolbar.captures((*x, *y), window_width, &stats) => continue,
                    Event::MouseButtonDown { x, y, .. } if toolbar.captures((*x, *y), window_width, &stats) => {
                        toolbar_press = true;
                        action = toolbar.click((*x, *y), window_width, &stats);
                    }
                    Event::MouseButtonUp { .. } if toolbar_press => {
                        toolbar_press = false;
                        continue;
                    }
                    Event::MouseWheel { .. } if pointer.is_some_and(|p| toolbar.captures(p, window_width, &stats)) => continue,
                    _ => {}
                }
            }
            if toolbar_press || action.is_some() {
                match action {
                    Some(toolbar::Action::Fullscreen) => {
                        fullscreen = !fullscreen;
                        let _ = canvas.window_mut().set_fullscreen(fullscreen);
                    }
                    Some(toolbar::Action::KeyboardGrab) => {
                        grab = !grab;
                        canvas.window_mut().set_keyboard_grab(grab);
                    }
                    Some(toolbar::Action::MouseLock) => {
                        mouse_lock = !mouse_lock;
                        canvas.window_mut().set_mouse_grab(mouse_lock);
                    }
                    Some(toolbar::Action::Mute) => muted = !muted,
                    Some(toolbar::Action::Disconnect) => break 'main,
                    None => {}
                }
                redraw = true;
                continue;
            }

            let input = match event {
                Event::Quit { .. } => break 'main,
                Event::Window { win_event: WindowEvent::Exposed | WindowEvent::Resized(..) | WindowEvent::PixelSizeChanged(..), .. } => {
                    redraw = true;
                    None
                }
                Event::KeyDown { keycode: Some(key), keymod, repeat: false, .. }
                    if keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD)
                        && keymod.intersects(Mod::LALTMOD | Mod::RALTMOD)
                        && matches!(key, Keycode::G | Keycode::M | Keycode::F | Keycode::L | Keycode::Q) =>
                {
                    match key {
                        Keycode::G => {
                            grab = !grab;
                            canvas.window_mut().set_keyboard_grab(grab);
                            info!("Keyboard grab: {}", grab);
                        }
                        Keycode::M => {
                            relative_mouse = !relative_mouse;
                            sdl.mouse().set_relative_mouse_mode(canvas.window(), relative_mouse);
                            info!("Relative mouse: {}", relative_mouse);
                        }
                        Keycode::F => {
                            fullscreen = !fullscreen;
                            let _ = canvas.window_mut().set_fullscreen(fullscreen);
                        }
                        Keycode::L => {
                            mouse_lock = !mouse_lock;
                            canvas.window_mut().set_mouse_grab(mouse_lock);
                            info!("Mouse lock: {}", mouse_lock);
                            redraw = true;
                        }
                        _ => break 'main,
                    }
                    None
                }
                Event::KeyDown { scancode: Some(scancode), keymod, repeat: false, .. } => Some(InputEvent::KeyboardKey {
                    scancode: scancode.to_i32() as u16,
                    down: true,
                    modifiers: keymod.bits(),
                }),
                Event::KeyUp { scancode: Some(scancode), keymod, .. } => Some(InputEvent::KeyboardKey {
                    scancode: scancode.to_i32() as u16,
                    down: false,
                    modifiers: keymod.bits(),
                }),
                Event::MouseMotion { x, y, xrel, yrel, .. } => Some(if relative_mouse {
                    InputEvent::MouseMoveRelative { dx: xrel as i16, dy: yrel as i16 }
                } else {
                    let (x, y) = to_stream(x, y);
                    InputEvent::MouseMoveAbsolute { x, y }
                }),
                Event::MouseButtonDown { mouse_btn, .. } => Some(InputEvent::MouseButton { button: mouse_btn as u8, down: true }),
                Event::MouseButtonUp { mouse_btn, .. } => Some(InputEvent::MouseButton { button: mouse_btn as u8, down: false }),
                Event::MouseWheel { x, y, .. } => Some(InputEvent::MouseWheel { dx: (x * 120.0) as i16, dy: (y * 120.0) as i16 }),
                _ => None,
            };
            if let Some(input) = input {
                if !send_input(&mut tcp, input) {
                    warn!("Lost the control connection");
                    break 'main;
                }
            }
        }

        if let Some(stream) = &audio_stream {
            for pcm in audio_rx.try_iter() {
                if muted {
                    let _ = stream.clear();
                    continue;
                }
                let queued = stream.queued_bytes().unwrap_or(0);
                if queued > audio_max_queued {
                    let _ = stream.clear();
                }
                if queued == 0 || queued > audio_max_queued {
                    let _ = stream.put_data_i16(&audio_cushion);
                }
                let _ = stream.put_data_i16(&pcm);
            }
        }

        // Wait briefly for the next frame; this also paces the loop while the stream is idle.
        if let Ok(mut frame) = frame_rx.recv_timeout(Duration::from_millis(4)) {
            // Only the newest frame is worth showing.
            while let Ok(newer) = frame_rx.try_recv() {
                let _ = recycle_tx.try_send(std::mem::replace(&mut frame, newer));
            }
            texture.update(None, &frame, width as usize * 4).context("texture upload failed")?;
            have_frame = true;
            redraw = true;
            if let Some(old) = last_frame.replace(frame) {
                let _ = recycle_tx.try_send(old);
            }
        }

        if toolbar.update(if relative_mouse { None } else { pointer }, window_width, &stats) {
            redraw = true;
        }

        // One pointer: the remote one's shape, drawn by this window. (Relative mode hides the
        // pointer altogether, which SDL does by itself.)
        for (header, image) in cursor_rx.try_iter() {
            remote_pointer.set_shape(header, image);
        }
        if !relative_mouse {
            let over_toolbar = pointer.is_some_and(|p| toolbar.captures(p, window_width, &stats));
            remote_pointer.apply(&sdl.mouse(), dst.w / width as f32, over_toolbar);
        }

        if redraw {
            canvas.set_draw_color(if have_frame { Color::RGB(0, 0, 0) } else { Color::RGB(18, 24, 38) });
            canvas.clear();
            if have_frame {
                canvas.copy(&texture, None, Some(dst))?;
            }
            toolbar.draw(&mut canvas, window_width, &stats, |action| match action {
                toolbar::Action::Fullscreen => fullscreen,
                toolbar::Action::KeyboardGrab => grab,
                toolbar::Action::MouseLock => mouse_lock,
                toolbar::Action::Mute => muted,
                toolbar::Action::Disconnect => false,
            });
            canvas.present();
            redraw = false;
        }
    }

    if let Some(path) = &args.dump_window {
        // Redraw into the back buffer and read it before it is presented.
        canvas.set_draw_color(Color::RGB(0, 0, 0));
        canvas.clear();
        let dst = letterbox(canvas.window().size(), (width, height));
        if have_frame {
            canvas.copy(&texture, None, Some(dst))?;
        }
        let window_width = canvas.window().size().0 as f32;
        let stats = summary.lock().unwrap().clone();
        toolbar.draw(&mut canvas, window_width, &stats, |action| match action {
            toolbar::Action::MouseLock => mouse_lock,
            toolbar::Action::Mute => muted,
            _ => false,
        });
        canvas.read_pixels(None)?.save_bmp(path)?;
    }

    running.store(false, Ordering::Relaxed);
    let _ = tcp.shutdown(Shutdown::Both);
    let _ = video_thread.join();

    if let Some(path) = &args.dump_frame {
        match &last_frame {
            Some(frame) => {
                let mut ppm = format!("P6\n{} {}\n255\n", width, height).into_bytes();
                ppm.extend(frame.chunks_exact(4).flat_map(|px| [px[0], px[1], px[2]]));
                std::fs::write(path, ppm).with_context(|| format!("could not write {}", path.display()))?;
                info!("Wrote the last frame to {}", path.display());
            }
            None => bail!("no frame was received, nothing to dump"),
        }
    }
    Ok(())
}
