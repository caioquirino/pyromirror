//! PyroMirror server.
//!
//! Captures the desktop, encodes it with PyroWave and streams the packets over UDP. A TCP
//! connection per client carries the handshake and input events.

mod source;

use std::io::Write;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use log::{debug, error, info, trace, warn};

use pyromirror_capture::{CaptureOptions, Capturer};
use pyromirror_codec::{Chroma, Device, Encoder, Packets};
use pyromirror_net::{create_streaming_socket, packet_boundary, sleep_until, FrameSender, UDP_PUNCH};
use pyromirror_proto::{
    read_message, write_message, AudioCodecType, ClientHello, CodecParameters, InputEvent,
    VideoCodecType, VideoColorProfile, MAX_MESSAGE_PAYLOAD, MSG_TYPE_CLIENT_HELLO,
    MSG_TYPE_CODEC_PARAMS, MSG_TYPE_INPUT_EVENT,
};
use source::{Source, SourceFrame, TestPattern};

/// How often an unchanged desktop is re-sent, so a client that lost packets converges to a clean
/// image and a freshly connected one gets a picture immediately.
const IDLE_REFRESH: Duration = Duration::from_millis(250);

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum ChromaArg {
    /// Full-resolution chroma, sharp coloured text
    #[value(name = "444")]
    C444,
    /// Half-resolution chroma, less bandwidth
    #[value(name = "420")]
    C420,
}

#[derive(Parser, Debug)]
#[command(name = "pyromirror-server", version, about = "PyroMirror low-latency remote desktop host")]
struct Args {
    /// Address to listen on (TCP control and UDP video)
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,

    /// Port for TCP control and UDP video
    #[arg(short, long, default_value_t = 9000)]
    port: u16,

    /// Video bitrate in Mbps; every frame is capped to bitrate / fps
    #[arg(short, long, default_value_t = 250, value_parser = clap::value_parser!(u32).range(1..=20000))]
    bitrate_mbps: u32,

    /// Maximum frames per second
    #[arg(short, long, default_value_t = 60, value_parser = clap::value_parser!(u32).range(1..=1000))]
    fps: u32,

    /// UDP datagram size in bytes (1400 for standard Ethernet, 8900 for jumbo frames)
    #[arg(short, long, default_value_t = 1400, value_parser = clap::value_parser!(u32).range(256..=65000))]
    mtu: u32,

    /// Chroma subsampling
    #[arg(long, value_enum, default_value = "444")]
    chroma: ChromaArg,

    /// Windows: index of the monitor to capture (default: primary)
    #[arg(long)]
    monitor: Option<u32>,

    /// Stream a generated WIDTHxHEIGHT test pattern instead of the desktop, e.g. 1920x1080
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    test_pattern: Option<(u32, u32)>,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
    let parse = |v: &str| v.parse::<u32>().ok().filter(|n| (16..=16384).contains(n));
    match (parse(w), parse(h)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err("width and height must be between 16 and 16384".into()),
    }
}

/// Everything a streaming session needs; lives across client connections.
struct Pipeline {
    source: Source,
    encoder: Encoder,
    max_frame_bytes: usize,
    packet_boundary: usize,
    frame_interval: Duration,
}

/// Encodes a captured frame. Fails if the desktop no longer matches the encoder's size.
fn encode_frame<'e>(
    encoder: &'e mut Encoder,
    frame: &SourceFrame<'_>,
    max_frame_bytes: usize,
    packet_boundary: usize,
) -> anyhow::Result<Packets<'e>> {
    // The encoder may be one pixel smaller than the desktop (4:2:0 cropping), never larger.
    if frame.width & !1 != encoder.width() & !1 || frame.height & !1 != encoder.height() & !1 {
        bail!(
            "desktop resolution changed to {}x{}; restart the server to stream it",
            frame.width,
            frame.height
        );
    }
    Ok(encoder.encode(frame.data, frame.stride as usize, frame.format, max_frame_bytes, packet_boundary)?)
}

fn main() -> anyhow::Result<()> {
    env_logger::init_from_env(env_logger::Env::default().default_filter_or("info"));
    let args = Args::parse();

    let chroma = match args.chroma {
        ChromaArg::C444 => Chroma::C444,
        ChromaArg::C420 => Chroma::C420,
    };

    let mut source = match args.test_pattern {
        Some((w, h)) => {
            info!("Streaming a {}x{} test pattern instead of the desktop", w, h);
            Source::pattern(TestPattern::new(w, h))
        }
        None => Source::capture(
            Capturer::new(&CaptureOptions { output: args.monitor }).context("could not start desktop capture")?,
        ),
    };

    let device = Device::new().context("could not initialise PyroWave")?;

    let max_frame_bytes = (args.bitrate_mbps as usize * 1_000_000 / 8) / args.fps as usize;
    let boundary = packet_boundary(args.mtu as usize);

    // The first frame tells us the desktop resolution. It is also encoded once, which leaves it
    // in the encoder so an idle desktop can be sent to the first client right away.
    let encoder = source.with_first_frame(Duration::from_secs(10), |first| {
        // 4:2:0 needs even dimensions; crop the odd row/column if there is one.
        let (w, h) = match chroma {
            Chroma::C444 => (first.width, first.height),
            Chroma::C420 => (first.width & !1, first.height & !1),
        };
        if w > u16::MAX as u32 || h > u16::MAX as u32 {
            bail!("desktop of {}x{} is larger than the protocol supports", w, h);
        }
        let mut encoder = Encoder::new(device, w, h, chroma).context("could not create the PyroWave encoder")?;
        encode_frame(&mut encoder, first, max_frame_bytes, boundary)?;
        Ok(encoder)
    })?;
    let (width, height) = (encoder.width(), encoder.height());

    info!(
        "Streaming {}x{} at up to {} fps, {} Mbps ({} KiB per frame), {} byte datagrams, chroma {:?}",
        width,
        height,
        args.fps,
        args.bitrate_mbps,
        max_frame_bytes / 1024,
        args.mtu,
        chroma
    );

    let params = CodecParameters {
        video_codec: VideoCodecType::PyroWave,
        video_color_profile: match chroma {
            Chroma::C444 => VideoColorProfile::Bt709FullChroma444,
            Chroma::C420 => VideoColorProfile::Bt709FullCenterChroma420,
        },
        audio_codec: AudioCodecType::None,
        frame_rate_num: args.fps as u16,
        frame_rate_den: 1,
        width: width as u16,
        height: height as u16,
        audio_channels: 0,
        audio_sample_rate: 0,
    };

    let bind_addr = SocketAddr::new(args.bind, args.port);
    let listener = TcpListener::bind(bind_addr).with_context(|| format!("could not listen on TCP {}", bind_addr))?;
    let udp = create_streaming_socket(bind_addr, 8 * 1024 * 1024)
        .with_context(|| format!("could not bind UDP {}", bind_addr))?;
    udp.set_read_timeout(Some(Duration::from_millis(100)))?;

    let mut pipeline = Pipeline {
        source,
        encoder,
        max_frame_bytes,
        packet_boundary: boundary,
        frame_interval: Duration::from_secs_f64(1.0 / args.fps as f64),
    };

    info!("Listening on {} (TCP control + UDP video); waiting for a client", bind_addr);
    loop {
        let (tcp, client_addr) = listener.accept()?;
        info!("Client connected from {}", client_addr);
        match serve_client(tcp, client_addr, &udp, &params, &mut pipeline, args.bitrate_mbps) {
            Ok(()) => info!("Client {} disconnected", client_addr),
            Err(e) => {
                error!("Session with {} ended: {:#}", client_addr, e);
                if pipeline.source.is_lost() {
                    bail!("desktop capture stopped; restart the server");
                }
            }
        }
    }
}

fn serve_client(
    mut tcp: TcpStream,
    client_addr: SocketAddr,
    udp: &UdpSocket,
    params: &CodecParameters,
    pipeline: &mut Pipeline,
    bitrate_mbps: u32,
) -> anyhow::Result<()> {
    tcp.set_nodelay(true)?;

    // Handshake: the client announces its UDP port, we answer with the stream description.
    tcp.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
    let (msg_type, len) = read_message(&mut tcp, &mut payload).context("no ClientHello received")?;
    if msg_type != MSG_TYPE_CLIENT_HELLO {
        bail!("expected ClientHello, got message type {}", msg_type);
    }
    let hello = ClientHello::deserialize(&payload[..len])?;
    tcp.set_read_timeout(None)?;

    let mut param_buf = [0u8; CodecParameters::SIZE];
    params.serialize(&mut param_buf)?;
    write_message(&mut tcp, MSG_TYPE_CODEC_PARAMS, &param_buf)?;
    tcp.flush()?;

    let target = Mutex::new(SocketAddr::new(client_addr.ip(), hello.udp_port));
    info!("Sending video to {}", target.lock().unwrap());

    let running = AtomicBool::new(true);
    let control = tcp.try_clone()?;
    let punch_socket = udp.try_clone()?;
    let send_socket = udp.try_clone()?;

    std::thread::scope(|scope| {
        // Control channel: input events, and the signal that the client went away.
        scope.spawn(|| {
            let mut control = control;
            let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
            while running.load(Ordering::Relaxed) {
                match read_message(&mut control, &mut payload) {
                    Ok((MSG_TYPE_INPUT_EVENT, len)) => match InputEvent::deserialize(&payload[..len]) {
                        // TODO: inject into the desktop (SendInput on Windows, RemoteDesktop
                        // portal / uinput on Linux).
                        Ok(event) => trace!("Input event: {:?}", event),
                        Err(e) => debug!("Undecodable input event: {}", e),
                    },
                    Ok((other, _)) => debug!("Ignoring control message type {}", other),
                    Err(_) => break,
                }
            }
            running.store(false, Ordering::Relaxed);
        });

        // The client keeps sending small datagrams from its video socket. If it sits behind NAT,
        // their source address is where video has to go, rather than the port it announced.
        scope.spawn(|| {
            let mut buf = [0u8; 64];
            while running.load(Ordering::Relaxed) {
                if let Ok((len, from)) = punch_socket.recv_from(&mut buf) {
                    if from.ip() == client_addr.ip() && &buf[..len] == UDP_PUNCH {
                        let mut target = target.lock().unwrap();
                        if *target != from {
                            info!("Client video endpoint is {} (was {})", from, *target);
                            *target = from;
                        }
                    }
                }
            }
        });

        let result = stream_video(pipeline, send_socket, &target, &running, bitrate_mbps);
        running.store(false, Ordering::Relaxed);
        let _ = tcp.shutdown(Shutdown::Both);
        result
    })
}

fn stream_video(
    pipeline: &mut Pipeline,
    socket: UdpSocket,
    target: &Mutex<SocketAddr>,
    running: &AtomicBool,
    bitrate_mbps: u32,
) -> anyhow::Result<()> {
    let Pipeline { source, encoder, max_frame_bytes, packet_boundary, frame_interval } = pipeline;
    let (max_frame_bytes, packet_boundary, frame_interval) = (*max_frame_bytes, *packet_boundary, *frame_interval);

    // Release datagrams at twice the video bitrate: a frame is fully on the wire within half a
    // frame interval instead of being spread across all of it.
    let mut sender = FrameSender::new(socket, *target.lock().unwrap(), bitrate_mbps.saturating_mul(2));

    let start = Instant::now();
    let mut last_sent: Option<Instant> = None;
    let mut stats = Stats::default();

    let mut next_frame_at = Instant::now();

    while running.load(Ordering::Relaxed) {
        sender.set_target_addr(*target.lock().unwrap());

        let frame = source.next_frame(frame_interval)?;
        let encode_start = Instant::now();
        let packets = match frame {
            Some(frame) => Some(encode_frame(encoder, &frame, max_frame_bytes, packet_boundary)?),
            None if last_sent.map_or(true, |t| t.elapsed() >= IDLE_REFRESH) => {
                Some(encoder.encode_last(max_frame_bytes, packet_boundary)?)
            }
            None => None,
        };

        if let Some(packets) = packets {
            let encode_time = encode_start.elapsed();
            let pts = start.elapsed().as_micros() as u64;
            match sender.send_frame(packets.iter(), pts) {
                Ok(bytes) => stats.frame(bytes, encode_time),
                // A full send buffer or a transient ICMP error only costs this frame.
                Err(e) => warn!("Dropped a frame: {}", e),
            }
            last_sent = Some(Instant::now());
        }
        stats.report();

        // Cap the frame rate; the capture backend may deliver faster than requested. If we are
        // running behind, do not try to catch up with a burst.
        next_frame_at = (next_frame_at + frame_interval).max(Instant::now());
        sleep_until(next_frame_at);
    }
    Ok(())
}

#[derive(Default)]
struct Stats {
    since: Option<Instant>,
    frames: u32,
    bytes: usize,
    encode: Duration,
}

impl Stats {
    fn frame(&mut self, bytes: usize, encode: Duration) {
        self.frames += 1;
        self.bytes += bytes;
        self.encode += encode;
    }

    fn report(&mut self) {
        let since = *self.since.get_or_insert_with(Instant::now);
        let elapsed = since.elapsed();
        if elapsed < Duration::from_secs(2) {
            return;
        }
        if self.frames > 0 {
            debug!(
                "{:.1} fps, {:.1} Mbps, {:.2} ms convert+encode per frame",
                self.frames as f64 / elapsed.as_secs_f64(),
                self.bytes as f64 * 8.0 / 1e6 / elapsed.as_secs_f64(),
                self.encode.as_secs_f64() * 1000.0 / self.frames as f64
            );
        }
        *self = Stats { since: Some(Instant::now()), ..Default::default() };
    }
}
