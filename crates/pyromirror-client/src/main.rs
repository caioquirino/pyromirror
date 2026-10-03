//! PyroMirror Client Viewer
//!
//! Receives UDP PyroWave packets, presents via SDL3 Canvas/Texture,
//! and forwards low-latency input.

use clap::Parser;
use log::{error, info, warn};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;
use std::io::{Read, Write};
use byteorder::ByteOrder;

use sdl3::pixels::{Color, PixelFormat};
use sdl3::rect::Rect;

use pyromirror_proto::{
    make_message_type, validate_magic, ClientHello, CodecParameters, InputEvent,
    MSG_TYPE_CLIENT_HELLO,
};
use pyromirror_net::{create_streaming_socket, FrameReceiver};

#[derive(Parser, Debug)]
#[command(name = "pyromirror-client", version, about = "PyroMirror Ultra-Low-Latency Remote Desktop Client")]
struct Args {
    /// Remote host address in format pyro://<ip>:<port> or <ip>:<port>
    uri: String,

    /// Force fragment shader decoding path (optimized for mobile/integrated GPUs)
    #[arg(long, default_value_t = false)]
    force_fragment: bool,

    /// Start in fullscreen mode
    #[arg(short, long, default_value_t = false)]
    fullscreen: bool,

    /// Local port to receive UDP video stream (0 for OS dynamic port)
    #[arg(long, default_value_t = 0)]
    local_port: u16,
}

fn parse_uri(uri: &str) -> anyhow::Result<SocketAddr> {
    let clean = uri.trim_start_matches("pyro://");
    let addr: SocketAddr = clean.parse()?;
    Ok(addr)
}

fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let args = Args::parse();
    let server_addr = parse_uri(&args.uri)?;

    info!("🔥 Connecting to PyroMirror Server at {}", server_addr);

    // 1. Setup UDP receiving socket with 32MB buffer first to get local listening port
    let udp_socket = create_streaming_socket(
        Some(SocketAddr::from(([0, 0, 0, 0], args.local_port))),
        32 * 1024 * 1024,
    )?;
    let std_udp_socket: std::net::UdpSocket = udp_socket.into();
    std_udp_socket.set_read_timeout(Some(std::time::Duration::from_millis(50)))?;
    let local_udp_port = std_udp_socket.local_addr()?.port();
    info!("Local UDP video receiver bound on port {}", local_udp_port);

    // 2. Connect control TCP stream
    let mut tcp_stream = std::net::TcpStream::connect(server_addr)?;
    tcp_stream.set_nodelay(true)?;
    info!("Connected to server control channel");

    // 3. Send ClientHello immediately so server knows our UDP receiving port
    let hello = ClientHello {
        udp_port: local_udp_port,
        flags: 0,
    };
    let mut hello_buf = [0u8; ClientHello::SIZE];
    hello.serialize(&mut hello_buf)?;
    let hello_header = make_message_type(MSG_TYPE_CLIENT_HELLO, ClientHello::SIZE as u32);
    let mut hello_msg = [0u8; 4 + ClientHello::SIZE];
    byteorder::LittleEndian::write_u32(&mut hello_msg[0..4], hello_header);
    hello_msg[4..].copy_from_slice(&hello_buf);
    tcp_stream.write_all(&hello_msg)?;
    info!("Sent ClientHello announcing UDP port {}", local_udp_port);

    // 4. Send UDP punch packets directly to server to open stateful NAT/firewall pinhole
    for _ in 0..3 {
        let _ = std_udp_socket.send_to(b"PYROMIRROR_UDP_PUNCH", server_addr);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    info!("Sent initial UDP punch packets to server at {}", server_addr);

    // 5. Read codec parameters from server
    let mut header_buf = [0u8; 4];
    tcp_stream.read_exact(&mut header_buf)?;
    let magic = byteorder::LittleEndian::read_u32(&header_buf);
    if !validate_magic(magic) {
        anyhow::bail!("Invalid handshake response from server: {:#x}", magic);
    }

    let mut param_buf = [0u8; CodecParameters::SIZE];
    tcp_stream.read_exact(&mut param_buf)?;
    let codec_params = CodecParameters::deserialize(&param_buf)?;
    info!(
        "Stream Negotiated: {}x{} @ {} FPS, Codec: {:?}, Color: {:?}",
        codec_params.width, codec_params.height, codec_params.frame_rate_num,
        codec_params.video_codec, codec_params.video_color_profile
    );

    // 6. Initialize SDL3 Window and Canvas
    let sdl = sdl3::init()?;
    let video = sdl.video()?;

    let mut window_builder = video.window(
        "PyroMirror Viewer",
        codec_params.width as u32,
        codec_params.height as u32,
    );
    window_builder.resizable();
    if args.fullscreen {
        window_builder.fullscreen();
    }

    let window = window_builder.build()?;
    let mut canvas = window.into_canvas();
    info!("SDL3 Window & Canvas initialized successfully");

    // Create streaming texture for video presentation (RGBA32 byte-ordered)
    let texture_creator = canvas.texture_creator();
    let mut texture = texture_creator
        .create_texture_streaming(PixelFormat::RGBA32, codec_params.width as u32, codec_params.height as u32)
        .map_err(|e| anyhow::anyhow!("Failed to create streaming texture: {}", e))?;

    let running = Arc::new(AtomicBool::new(true));
    let running_recv = running.clone();

    // Channel to deliver assembled frames to main render thread
    let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Vec<u8>>(4);

    // Keep sending UDP punch / keepalive in background to maintain firewall pinhole
    let udp_sock_clone = std_udp_socket.try_clone()?;
    std::thread::spawn(move || {
        while running_recv.load(Ordering::Relaxed) {
            let _ = udp_sock_clone.send_to(b"PYROMIRROR_UDP_KEEPALIVE", server_addr);
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    });

    let running_recv2 = running.clone();
    // Calculate maximum frame capacity based on negotiated stream dimensions with generous headroom for 4K/8K
    let uncompressed_frame_size = (codec_params.width as usize) * (codec_params.height as usize) * 4;
    let max_frame_capacity = (uncompressed_frame_size * 2).max(128 * 1024 * 1024);
    info!(
        "Configured FrameReceiver capacity: {} MB (stream frame raw size: {:.2} MB)",
        max_frame_capacity / (1024 * 1024),
        uncompressed_frame_size as f64 / (1024.0 * 1024.0)
    );

    // 7. Spawn video packet receiving and assembly thread
    let recv_handle = std::thread::spawn(move || {
        let mut receiver = FrameReceiver::new(max_frame_capacity);
        let mut packet_buf = [0u8; 65536];
        let mut frames_received: u64 = 0;
        let mut bytes_received: u64 = 0;
        let mut first_frame_logged = false;
        let mut last_fps_report = Instant::now();

        info!("UDP receiver thread listening on port {}...", local_udp_port);

        while running_recv2.load(Ordering::Relaxed) {
            match std_udp_socket.recv_from(&mut packet_buf) {
                Ok((len, from)) => {
                    bytes_received += len as u64;
                    match receiver.push_datagram(&packet_buf[..len]) {
                        Ok(Some((frame_data, _pts))) => {
                            frames_received += 1;
                            if !first_frame_logged {
                                first_frame_logged = true;
                                info!("🎉 First video frame assembled successfully ({} bytes) from {}!", frame_data.len(), from);
                            }
                            let _ = frame_tx.try_send(frame_data);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!("Packet error: {}", e);
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {
                    continue;
                }
                Err(e) => {
                    error!("UDP socket receive error: {}", e);
                    break;
                }
            }

            if last_fps_report.elapsed() >= std::time::Duration::from_secs(1) {
                let mbps = (bytes_received as f64 * 8.0) / 1_000_000.0;
                if frames_received > 0 {
                    info!("Stream Stats: {} FPS, {:.2} Mbps", frames_received, mbps);
                }
                frames_received = 0;
                bytes_received = 0;
                last_fps_report = Instant::now();
            }
        }

        info!("UDP receiver thread terminated");
    });

    // 6. SDL3 Event & Presentation Loop
    let mut event_pump = sdl.event_pump()?;
    let mut grab_active = false;
    let mut relative_mouse = false;
    let mut has_received_frame = false;
    let width = codec_params.width as usize;
    let height = codec_params.height as usize;
    let pitch = width * 4;

    info!("Starting SDL3 viewer event loop. Press Ctrl+Alt+G to grab keyboard/mouse, Ctrl+Alt+M to toggle relative mouse.");

    'main_loop: while running.load(Ordering::Relaxed) {
        // Poll input events
        for event in event_pump.poll_iter() {
            use sdl3::event::Event;
            use sdl3::keyboard::Keycode;

            match event {
                Event::Quit { .. } => break 'main_loop,
                Event::KeyDown {
                    keycode: Some(Keycode::G),
                    keymod,
                    ..
                } if keymod.contains(sdl3::keyboard::Mod::LCTRLMOD) && keymod.contains(sdl3::keyboard::Mod::LALTMOD) => {
                    grab_active = !grab_active;
                    let _ = canvas.window_mut().set_keyboard_grab(grab_active);
                    info!("Keyboard grab: {}", grab_active);
                }
                Event::KeyDown {
                    keycode: Some(Keycode::M),
                    keymod,
                    ..
                } if keymod.contains(sdl3::keyboard::Mod::LCTRLMOD) && keymod.contains(sdl3::keyboard::Mod::LALTMOD) => {
                    relative_mouse = !relative_mouse;
                    let _ = sdl.mouse().set_relative_mouse_mode(canvas.window(), relative_mouse);
                    info!("Relative mouse mode: {}", relative_mouse);
                }
                Event::MouseMotion { xrel, yrel, x, y, .. } => {
                    let input = if relative_mouse {
                        InputEvent::MouseMoveRelative {
                            dx: xrel as i16,
                            dy: yrel as i16,
                        }
                    } else {
                        InputEvent::MouseMoveAbsolute {
                            x: x.max(0.0) as u16,
                            y: y.max(0.0) as u16,
                        }
                    };
                    let mut buf = [0u8; 16];
                    if let Ok(len) = input.serialize(&mut buf) {
                        let header = make_message_type(9, len as u32);
                        let mut msg = [0u8; 20];
                        byteorder::LittleEndian::write_u32(&mut msg[0..4], header);
                        msg[4..4 + len].copy_from_slice(&buf[..len]);
                        let _ = tcp_stream.write_all(&msg[..4 + len]);
                    }
                }
                Event::MouseButtonDown { mouse_btn, .. } => {
                    let input = InputEvent::MouseButton {
                        button: mouse_btn as u8,
                        down: true,
                    };
                    let mut buf = [0u8; 16];
                    if let Ok(len) = input.serialize(&mut buf) {
                        let header = make_message_type(9, len as u32);
                        let mut msg = [0u8; 20];
                        byteorder::LittleEndian::write_u32(&mut msg[0..4], header);
                        msg[4..4 + len].copy_from_slice(&buf[..len]);
                        let _ = tcp_stream.write_all(&msg[..4 + len]);
                    }
                }
                Event::MouseButtonUp { mouse_btn, .. } => {
                    let input = InputEvent::MouseButton {
                        button: mouse_btn as u8,
                        down: false,
                    };
                    let mut buf = [0u8; 16];
                    if let Ok(len) = input.serialize(&mut buf) {
                        let header = make_message_type(9, len as u32);
                        let mut msg = [0u8; 20];
                        byteorder::LittleEndian::write_u32(&mut msg[0..4], header);
                        msg[4..4 + len].copy_from_slice(&buf[..len]);
                        let _ = tcp_stream.write_all(&msg[..4 + len]);
                    }
                }
                _ => {}
            }
        }

        // Check if a new video frame arrived
        if let Ok(frame_data) = frame_rx.try_recv() {
            has_received_frame = true;
            // Update texture buffer if frame matches size, or stride
            if frame_data.len() >= pitch * height {
                let _ = texture.update(None, &frame_data[..pitch * height], pitch);
            } else {
                // If compressed wavelet / synthetic payload, update texture with pattern
                let _ = texture.with_lock(None, |buffer: &mut [u8], _p: usize| {
                    let fill_len = buffer.len().min(frame_data.len());
                    buffer[..fill_len].copy_from_slice(&frame_data[..fill_len]);
                });
            }
        }

        // Render pass
        if has_received_frame {
            let _ = canvas.copy(&texture, None, None);
        } else {
            // Draw initial connected waiting screen so window maps and displays immediately on Wayland/Windows
            canvas.set_draw_color(Color::RGB(18, 24, 38));
            canvas.clear();

            // Draw a subtle animated connection indicator box in center
            canvas.set_draw_color(Color::RGB(59, 130, 246));
            let center_x = (codec_params.width as i32 / 2) - 100;
            let center_y = (codec_params.height as i32 / 2) - 20;
            let _ = canvas.fill_rect(Rect::new(center_x, center_y, 200, 40));
        }

        // Crucial for Wayland/Windows: Present backbuffer to display surface
        canvas.present();

        std::thread::sleep(std::time::Duration::from_millis(4));
    }

    running.store(false, Ordering::Relaxed);
    let _ = recv_handle.join();
    info!("PyroMirror Client shutdown cleanly");
    Ok(())
}
