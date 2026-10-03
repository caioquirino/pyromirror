//! PyroMirror Server Daemon
//!
//! Captures desktop frames zero-copy, encodes via PyroWave Vulkan compute,
//! and streams UDP packets with token-bucket pacing.

use clap::Parser;
use log::{error, info, warn};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use byteorder::ByteOrder;
use tokio::net::TcpListener;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use pyromirror_proto::{
    make_message_type, message_get_length, message_get_type, validate_magic, AudioCodecType,
    ClientHello, CodecParameters, InputEvent, VideoCodecType, VideoColorProfile,
    MSG_TYPE_CLIENT_HELLO, MSG_TYPE_CODEC_PARAMS,
};
use pyromirror_net::{create_streaming_socket, FrameSender};
use pyromirror_capture::{create_default_capturer, ScreenCapturer};

#[derive(Parser, Debug)]
#[command(name = "pyromirror-server", version, about = "PyroMirror Ultra-Low-Latency Remote Desktop Host")]
struct Args {
    /// Port for TCP control and UDP video streaming
    #[arg(short, long, default_value_t = 9000)]
    port: u16,

    /// Target streaming bitrate in Mbps
    #[arg(short, long, default_value_t = 500)]
    bitrate_mbps: u32,

    /// Target streaming framerate
    #[arg(short, long, default_value_t = 60)]
    fps: u32,

    /// Stream width
    #[arg(long, default_value_t = 1280)]
    width: u16,

    /// Stream height
    #[arg(long, default_value_t = 720)]
    height: u16,

    /// Packet MTU in bytes (1400 for standard Ethernet, 8900 for Jumbo / Thunderbolt)
    #[arg(short, long, default_value_t = 1400)]
    mtu: usize,

    /// Chroma subsampling (444 for sharp desktop text, 420 for bandwidth saving)
    #[arg(long, default_value = "444")]
    chroma: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));

    let args = Args::parse();
    info!("🔥 PyroMirror Server starting on port {}", args.port);
    info!(
        "Configuration: {}x{} @ {} FPS, {} Mbps, MTU: {} bytes, Chroma: {}",
        args.width, args.height, args.fps, args.bitrate_mbps, args.mtu, args.chroma
    );

    let color_profile = if args.chroma == "420" {
        VideoColorProfile::Bt709FullCenterChroma420
    } else {
        VideoColorProfile::Bt709FullChroma444
    };

    let shared_capturer: Option<Arc<std::sync::Mutex<Box<dyn ScreenCapturer>>>> = match create_default_capturer() {
        Ok(c) => Some(Arc::new(std::sync::Mutex::new(c))),
        Err(e) => {
            warn!("Native capturer unavailable ({}), using fallback test pattern generator", e);
            None
        }
    };

    let (detected_w, detected_h) = shared_capturer
        .as_ref()
        .map(|c| c.lock().unwrap().resolution())
        .unwrap_or((0, 0));

    let (stream_w, stream_h) = if detected_w > 0 && detected_h > 0 && args.width == 1280 && args.height == 720 {
        info!("🖥️ Detected native display resolution: {}x{}", detected_w, detected_h);
        (detected_w as u16, detected_h as u16)
    } else {
        (args.width, args.height)
    };

    let params = CodecParameters {
        video_codec: VideoCodecType::PyroWave,
        video_color_profile: color_profile,
        audio_codec: AudioCodecType::Opus,
        frame_rate_num: args.fps as u16,
        frame_rate_den: 1,
        width: stream_w,
        height: stream_h,
        audio_channels: 2,
        audio_sample_rate: 48000,
    };

    let tcp_addr: SocketAddr = format!("0.0.0.0:{}", args.port).parse()?;
    let listener = TcpListener::bind(tcp_addr).await?;
    info!("TCP control listener bound to {}", tcp_addr);

    // Prepare UDP socket with 32MB socket buffers for burst streaming
    let udp_socket = create_streaming_socket(Some(format!("0.0.0.0:{}", args.port).parse()?), 32 * 1024 * 1024)?;
    let std_udp_socket: std::net::UdpSocket = udp_socket.into();
    std_udp_socket.set_read_timeout(Some(std::time::Duration::from_millis(50)))?;

    info!("Waiting for incoming PyroMirror client connection...");

    loop {
        let (mut socket, client_addr) = listener.accept().await?;
        info!("Client connected via TCP from {}", client_addr);

        let running = Arc::new(AtomicBool::new(true));

        // 1. Read optional ClientHello to discover client's UDP port immediately
        let mut initial_target_port = args.port;
        let mut first_hdr = [0u8; 4];
        if let Ok(Ok(_)) = tokio::time::timeout(std::time::Duration::from_millis(1500), socket.read_exact(&mut first_hdr)).await {
            let magic = byteorder::LittleEndian::read_u32(&first_hdr);
            if validate_magic(magic) {
                let msg_type = message_get_type(magic);
                let msg_len = message_get_length(magic);
                if msg_type == MSG_TYPE_CLIENT_HELLO && msg_len == ClientHello::SIZE {
                    let mut hello_buf = [0u8; ClientHello::SIZE];
                    if socket.read_exact(&mut hello_buf).await.is_ok() {
                        if let Ok(hello) = ClientHello::deserialize(&hello_buf) {
                            initial_target_port = hello.udp_port;
                            info!("✅ Received ClientHello: Client bound UDP on port {}", hello.udp_port);
                        }
                    }
                }
            }
        }

        // 2. Send codec parameters to client
        let mut param_buf = [0u8; CodecParameters::SIZE];
        params.serialize(&mut param_buf)?;
        let msg_header = make_message_type(MSG_TYPE_CODEC_PARAMS, CodecParameters::SIZE as u32);

        let mut out_msg = [0u8; 4 + CodecParameters::SIZE];
        byteorder::LittleEndian::write_u32(&mut out_msg[0..4], msg_header);
        out_msg[4..].copy_from_slice(&param_buf);

        if let Err(e) = socket.write_all(&out_msg).await {
            error!("Failed to send codec parameters to client: {}", e);
            continue;
        }

        let initial_target = SocketAddr::new(client_addr.ip(), initial_target_port);
        info!("Initial UDP video destination set to: {}", initial_target);
        let target_udp_addr = Arc::new(RwLock::new(initial_target));

        // 3. Spawn UDP punch / keepalive listener to dynamically track client's NAT / firewall endpoint
        let udp_listen_sock = std_udp_socket.try_clone()?;
        let running_listener = running.clone();
        let target_addr_listener = target_udp_addr.clone();

        std::thread::spawn(move || {
            let mut punch_buf = [0u8; 256];
            let _ = udp_listen_sock.set_read_timeout(Some(std::time::Duration::from_millis(100)));
            while running_listener.load(Ordering::Relaxed) {
                match udp_listen_sock.recv_from(&mut punch_buf) {
                    Ok((len, from_addr)) => {
                        let is_punch = &punch_buf[..len] == b"PYROMIRROR_UDP_PUNCH" || &punch_buf[..len] == b"PYROMIRROR_UDP_KEEPALIVE";
                        if from_addr.ip() == client_addr.ip() || is_punch {
                            let mut target = target_addr_listener.write().unwrap();
                            if *target != from_addr {
                                info!("🎯 UDP punch detected! Active client UDP endpoint updated to: {}", from_addr);
                                *target = from_addr;
                            }
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock => {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });

        // 4. Spawn video capture and streaming thread
        let running_clone = running.clone();
        let udp_send_sock = std_udp_socket.try_clone()?;
        let bitrate_mbps = args.bitrate_mbps;
        let mtu = args.mtu;
        let fps = args.fps;
        let width = stream_w as usize;
        let height = stream_h as usize;
        let target_addr_sender = target_udp_addr.clone();
        let capturer_clone = shared_capturer.clone();

        let streaming_handle = std::thread::spawn(move || {
            let initial_dest = *target_addr_sender.read().unwrap();
            let mut sender = FrameSender::new(udp_send_sock, initial_dest, bitrate_mbps, mtu);

            let frame_duration = std::time::Duration::from_secs_f64(1.0 / fps as f64);
            let start_time = std::time::Instant::now();
            let mut frame_count: u64 = 0;
            let mut native_logged = false;
            let mut synthetic_logged = false;

            info!("Starting video streaming loop to {}...", initial_dest);

            while running_clone.load(Ordering::Relaxed) {
                let loop_start = std::time::Instant::now();
                let pts = start_time.elapsed().as_micros() as u64;

                // Continuously track active discovered client endpoint
                let current_target = *target_addr_sender.read().unwrap();
                sender.set_target_addr(current_target);

                let mut captured_screen = false;
                let timeout_ms = if frame_count < 10 { 100 } else { 16 };

                if let Some(ref capturer_arc) = capturer_clone {
                    let mut capturer = capturer_arc.lock().unwrap();
                    if let Ok(frame) = capturer.acquire_frame(timeout_ms) {
                        let f_width = if frame.width > 0 { frame.width as usize } else { width };
                        let f_height = if frame.height > 0 { frame.height as usize } else { height };
                        let mut frame_bytes = vec![0u8; f_width * f_height * 4];
                        if capturer.copy_pixels(&mut frame_bytes, (f_width * 4) as u32) {
                            captured_screen = true;
                            if !native_logged {
                                native_logged = true;
                                info!("🎥 Live screen capture active: streaming {}x{} desktop", f_width, f_height);
                            }
                            let _ = sender.send_frame(&frame_bytes, pts, frame_count % 60 == 0);
                        }
                        capturer.release_frame();
                    }
                }

                if !captured_screen {
                    if !synthetic_logged && frame_count > 30 {
                        synthetic_logged = true;
                        info!("🎨 Native capture returned no frames (headless/WSL2); streaming animated test pattern");
                    }
                    let frame_bytes = generate_vibrant_frame(width, height, frame_count);
                    let _ = sender.send_frame(&frame_bytes, pts, frame_count % 60 == 0);
                }

                frame_count += 1;
                let elapsed = loop_start.elapsed();
                if elapsed < frame_duration {
                    std::thread::sleep(frame_duration - elapsed);
                }
            }

            info!("Video streaming loop terminated");
        });

        // Handle TCP input events from client
        let mut msg_header_buf = [0u8; 4];
        let mut payload_buf = [0u8; 256];

        while running.load(Ordering::Relaxed) {
            match socket.read_exact(&mut msg_header_buf).await {
                Ok(_) => {
                    let msg_magic = byteorder::LittleEndian::read_u32(&msg_header_buf);
                    if !validate_magic(msg_magic) {
                        warn!("Received invalid message magic from client, disconnecting");
                        break;
                    }

                    let len = message_get_length(msg_magic);
                    if len > payload_buf.len() {
                        warn!("Message length {} exceeds buffer, disconnecting", len);
                        break;
                    }

                    if let Err(e) = socket.read_exact(&mut payload_buf[..len]).await {
                        error!("Failed to read message payload: {}", e);
                        break;
                    }

                    // Process input event
                    if let Ok(event) = InputEvent::deserialize(&payload_buf[..len]) {
                        match event {
                            InputEvent::MouseMoveRelative { dx, dy } => {
                                log::trace!("Input: Mouse Move Relative dx: {}, dy: {}", dx, dy);
                            }
                            InputEvent::MouseMoveAbsolute { x, y } => {
                                log::trace!("Input: Mouse Move Absolute x: {}, y: {}", x, y);
                            }
                            InputEvent::MouseButton { button, down } => {
                                log::debug!("Input: Mouse Button {} down={}", button, down);
                            }
                            InputEvent::KeyboardKey { scancode, down, modifiers } => {
                                log::debug!("Input: Key scancode: {} down={} mod={}", scancode, down, modifiers);
                            }
                            InputEvent::Gamepad { .. } => {}
                            _ => {}
                        }
                    }
                }
                Err(_) => {
                    info!("Client disconnected: {}", client_addr);
                    break;
                }
            }
        }

        running.store(false, Ordering::Relaxed);
        let _ = streaming_handle.join();
        info!("Ready for next client connection");
    }
}

/// Generates an animated, vivid RGBA test pattern (color gradient with moving bar and timestamp)
fn generate_vibrant_frame(width: usize, height: usize, frame_num: u64) -> Vec<u8> {
    let mut buf = vec![0u8; width * height * 4];
    let offset = ((frame_num * 8) % width as u64) as usize;

    for y in 0..height {
        let y_ratio = (y * 255) / height;
        for x in 0..width {
            let idx = (y * width + x) * 4;
            let x_ratio = (x * 255) / width;

            // Draw animated vertical scan bar
            if (x >= offset && x < offset + 40) || (x < 40 && offset + 40 > width && x < (offset + 40) % width) {
                buf[idx] = 255;     // R
                buf[idx + 1] = 255; // G
                buf[idx + 2] = 255; // B
                buf[idx + 3] = 255; // A (fully opaque)
            } else {
                buf[idx] = x_ratio as u8;          // R: gradient across X
                buf[idx + 1] = y_ratio as u8;      // G: gradient across Y
                buf[idx + 2] = 180;                // B: pleasant blue tone
                buf[idx + 3] = 255;                // A: fully opaque
            }
        }
    }
    buf
}


