//! PyroMirror server.
//!
//! Captures the desktop, encodes it with PyroWave and streams the packets over UDP. A TCP
//! connection per client carries the handshake and input events.

mod permissions;
mod source;

use std::io::Write;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use log::{debug, error, info, trace, warn};

use pyromirror_audio::AudioCapture;
use pyromirror_capture::{CaptureOptions, Capturer, DmaBuf, DmaBufModifiers, InputInjector};
use pyromirror_codec::{Chroma, Device, DmaBufPlanes, Encoder, Packets, TextureHandle};
use pyromirror_net::{create_streaming_socket, packet_boundary, sleep_until, AudioSender, FrameQueue, FrameSender, UDP_PUNCH};
use pyromirror_proto::auth::{self, TokenStore};
use pyromirror_proto::control::{Listener, Side, Toggles};
use pyromirror_proto::{
    read_message, write_message, AudioCodecType, ClientHello, CodecParameters, InputEvent,
    CursorHeader, VideoCodecType, VideoColorProfile, CURSOR_IN_VIDEO, CURSOR_VISIBLE, MAX_CURSOR_SIDE,
    MAX_MESSAGE_PAYLOAD, MSG_TYPE_CLIENT_HELLO, MSG_TYPE_CODEC_PARAMS, MSG_TYPE_CURSOR, MSG_TYPE_DATAGRAMS_LOST, MSG_TYPE_INPUT_EVENT,
    MSG_TYPE_SESSION_ACTION, MSG_TYPE_SESSION_STATE,
};
use source::{Source, SourceFrame, TestPattern};

/// No toggle state has arrived from the client; not a valid `Toggles` bit pattern.
const TOGGLES_UNKNOWN: u8 = u8::MAX;

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

    /// Shrink the picture by this integer factor before encoding (2 turns 4K into 1080p)
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..=8))]
    scale: u32,

    /// How fast datagrams are released, as a multiple of the bitrate. Lower values (down to 1.1)
    /// smooth out bursts for Wi-Fi at the cost of a few milliseconds of latency
    #[arg(long, default_value_t = 2.0, value_parser = parse_pace)]
    pace_factor: f64,

    /// Windows: index of the monitor to capture (default: primary)
    #[arg(long)]
    monitor: Option<u32>,

    /// Let anyone who can reach this computer connect, without pairing
    #[arg(long)]
    no_pairing: bool,

    /// Do not capture or send audio
    #[arg(long)]
    no_audio: bool,

    /// Read every frame back from the GPU and convert it on the CPU, as older versions did,
    /// instead of encoding straight from the captured texture
    #[arg(long)]
    no_zero_copy: bool,

    /// Ignore the client's mouse and keyboard (view-only)
    #[arg(long)]
    no_input: bool,

    /// Check that sharing can start unattended (asking for any permission now), then exit
    #[arg(long)]
    check_permissions: bool,

    /// Stream a generated WIDTHxHEIGHT test pattern instead of the desktop, e.g. 1920x1080
    #[arg(long, value_name = "WxH", value_parser = parse_size)]
    test_pattern: Option<(u32, u32)>,
}

fn parse_pace(s: &str) -> Result<f64, String> {
    match s.parse::<f64>() {
        Ok(v) if (1.1..=100.0).contains(&v) => Ok(v),
        _ => Err("expected a number between 1.1 and 100".into()),
    }
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once(['x', 'X']).ok_or("expected WIDTHxHEIGHT")?;
    let parse = |v: &str| v.parse::<u32>().ok().filter(|n| (16..=16384).contains(n));
    match (parse(w), parse(h)) {
        (Some(w), Some(h)) => Ok((w, h)),
        _ => Err("width and height must be between 16 and 16384".into()),
    }
}

/// The next datagram size to try when the viewer reports that nothing gets through whole:
/// 1900 bytes passes most Wi-Fi (whose own frames may carry up to 2304), 1400 passes anything
/// that carries ordinary Ethernet. `None` when there is nothing smaller left to try.
fn smaller_datagrams(mtu: usize) -> Option<usize> {
    [1900, 1400].into_iter().find(|&smaller| smaller < mtu)
}

/// Everything a streaming session needs; lives across client connections.
struct Pipeline {
    source: Source,
    encoder: Encoder,
    /// The capture texture the encoder imported last, while frames stay on the GPU.
    imported_texture: Option<u64>,
    max_frame_bytes: usize,
    packet_boundary: usize,
    frame_interval: Duration,
    mtu: usize,
    pace_mbps: u32,
    injector: Option<InputInjector>,
    audio: Option<Audio>,
    pairing: Pairing,
}

/// Who may connect: computers paired earlier, and new ones after a one-time code is typed.
struct Pairing {
    /// False lets everyone in.
    required: bool,
    server_id: auth::Id,
    paired: TokenStore,
}

/// Loopback audio capture. It runs for the lifetime of the server; samples are only queued while
/// a client is being served.
struct Audio {
    _capture: AudioCapture,
    samples: crossbeam_channel::Receiver<Vec<i16>>,
    wanted: Arc<AtomicBool>,
    sample_rate: u32,
}

impl Audio {
    fn start() -> anyhow::Result<Self> {
        // Roughly a second of audio; if the sender falls that far behind, newer audio is dropped.
        let (tx, samples) = crossbeam_channel::bounded::<Vec<i16>>(256);
        let wanted = Arc::new(AtomicBool::new(false));
        let capture = {
            let wanted = wanted.clone();
            AudioCapture::start(move |pcm| {
                if wanted.load(Ordering::Relaxed) {
                    let _ = tx.try_send(pcm.to_vec());
                }
            })?
        };
        let sample_rate = capture.sample_rate();
        Ok(Self { _capture: capture, samples, wanted, sample_rate })
    }
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

/// A frame that stayed on the GPU, as far as encoding needs to know it.
#[derive(Clone, Copy)]
struct TextureFrame {
    id: u64,
    width: u32,
    height: u32,
    format: pyromirror_codec::PixelFormat,
    /// Linux: the DMA-BUF the frame is in.
    dmabuf: Option<DmaBuf>,
}

impl TextureFrame {
    /// What `frame` needs for encoding, if it stayed on the GPU.
    fn of(frame: &SourceFrame<'_>) -> Option<Self> {
        let id = frame.texture?;
        Some(Self { id, width: frame.width, height: frame.height, format: frame.format, dmabuf: frame.dmabuf })
    }
}

/// Encodes straight from the capture texture. `None` means that did not work: zero-copy has
/// been switched off, and the same image comes again as pixels with the next frame.
fn encode_texture<'e>(
    source: &mut Source,
    encoder: &'e mut Encoder,
    imported: &mut Option<u64>,
    frame: TextureFrame,
    max_frame_bytes: usize,
    packet_boundary: usize,
) -> anyhow::Result<Option<Packets<'e>>> {
    // The texture is the whole desktop; the encoder shrinks it by the --scale factor.
    let scale = source.scale().max(1);
    let (width, height) = (frame.width / scale, frame.height / scale);
    if width & !1 != encoder.width() & !1 || height & !1 != encoder.height() & !1 {
        bail!("desktop resolution changed to {}x{}; restart the server to stream it", frame.width, frame.height);
    }

    let mut attempt = || -> anyhow::Result<()> {
        if !encoder.has_texture(frame.id) {
            let handle = match frame.dmabuf {
                // One of the few buffers the compositor cycles through; each is imported the
                // first time it comes by.
                Some(d) => {
                    let layout = DmaBufPlanes { modifier: d.modifier, planes: d.planes, offsets: d.offsets, strides: d.strides };
                    TextureHandle::DmaBuf { fd: d.fd, layout }
                }
                // One shared texture, which was replaced as a whole.
                None => {
                    *imported = None;
                    encoder.drop_textures();
                    TextureHandle::D3d11(source.export_texture().context("the capture texture could not be shared")?)
                }
            };
            encoder.import_texture(frame.id, handle, frame.width, frame.height, frame.format)?;
            if imported.replace(frame.id).is_none() {
                info!("Zero-copy capture: encoding straight from the {}x{} desktop texture", frame.width, frame.height);
            }
        }
        encoder.encode_texture(frame.id, max_frame_bytes, packet_boundary)?;
        Ok(())
    };
    match attempt() {
        Ok(()) => Ok(Some(encoder.last_packets())),
        Err(e) => {
            warn!("Zero-copy capture failed ({:#}); reading frames back from the GPU instead", e);
            *imported = None;
            encoder.drop_textures();
            source.set_gpu_frames(false);
            Ok(None)
        }
    }
}

fn main() -> anyhow::Result<()> {
    // The D-Bus library logs its own workings at info level, and warns about portal objects
    // that are gone by the time it looks; neither is ours to report.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .filter_module("zbus", log::LevelFilter::Error)
        .filter_module("tracing", log::LevelFilter::Error)
        .init();
    let args = Args::parse();
    pyromirror_capture::init_process();

    if args.check_permissions {
        return permissions::check(SocketAddr::new(args.bind, args.port), args.monitor, !args.no_input);
    }

    let chroma = match args.chroma {
        ChromaArg::C444 => Chroma::C444,
        ChromaArg::C420 => Chroma::C420,
    };

    // Linux: the device has to say which DMA-BUFs it can import before capture is negotiated
    // with the compositor. (Windows goes the other way round: the device follows the adapter
    // that captures.)
    #[cfg(target_os = "linux")]
    let (early_device, dmabuf) = {
        let device = Device::new().context("could not initialise PyroWave")?;
        let wanted = !args.no_zero_copy && args.test_pattern.is_none() && device.supports_texture_import();
        let modifiers = wanted
            .then(|| DmaBufModifiers {
                bgrx: device.dmabuf_modifiers(pyromirror_codec::PixelFormat::Bgrx),
                rgbx: device.dmabuf_modifiers(pyromirror_codec::PixelFormat::Rgbx),
            })
            .filter(|m| !m.bgrx.is_empty() || !m.rgbx.is_empty());
        (Some(device), modifiers)
    };
    #[cfg(not(target_os = "linux"))]
    let (early_device, dmabuf): (Option<Arc<Device>>, Option<DmaBufModifiers>) = (None, None);

    let mut injector = None;
    let mut capture_adapter = None;
    let mut source = match args.test_pattern {
        Some((w, h)) => {
            info!("Streaming a {}x{} test pattern instead of the desktop", w, h);
            Source::pattern(TestPattern::new(w, h))
        }
        None => {
            let capturer =
                Capturer::new(&CaptureOptions { output: args.monitor, dmabuf }).context("could not start desktop capture")?;
            capture_adapter = capturer.adapter_luid();
            if !args.no_input {
                injector = capturer.input_injector();
                if injector.is_none() {
                    warn!("This desktop does not allow input injection; clients will be view-only");
                }
            }
            Source::capture(capturer)
        }
    }
    .with_scale(args.scale);

    // Frames can stay on the GPU only if the encoder runs on the adapter that captures them.
    let (device, mut zero_copy) = match (early_device, capture_adapter.filter(|_| !args.no_zero_copy).map(Device::on_adapter)) {
        // Whether frames stay on the GPU was settled when capture started; asked below.
        (Some(device), _) => (device, !args.no_zero_copy),
        (None, Some(Ok(device))) => {
            let usable = device.supports_texture_import();
            if !usable {
                info!("The graphics driver cannot share textures; frames will be read back from the GPU");
            }
            (device, usable)
        }
        (None, Some(Err(e))) => {
            debug!("No PyroWave device on the capture adapter ({}); using the default one", e);
            (Device::new().context("could not initialise PyroWave")?, false)
        }
        (None, None) => (Device::new().context("could not initialise PyroWave")?, false),
    };

    let max_frame_bytes = (args.bitrate_mbps as usize * 1_000_000 / 8) / args.fps as usize;
    let boundary = packet_boundary(args.mtu as usize);

    // The first frame tells us the desktop resolution. It is also encoded once, which leaves it
    // in the encoder so an idle desktop can be sent to the first client right away.
    let mut imported_texture = None;
    let deadline = Instant::now() + Duration::from_secs(10);
    let encoder = loop {
        let Some(first) = source.next_frame(Duration::from_millis(100))? else {
            if Instant::now() >= deadline {
                bail!("no frame arrived from the capture backend within 10 seconds");
            }
            continue;
        };
        // A frame that stayed on the GPU is the whole desktop; the encoder shrinks it.
        let texture = TextureFrame::of(&first);
        let shrink = if texture.is_some() { args.scale } else { 1 };
        // 4:2:0 needs even dimensions; crop the odd row/column if there is one.
        let (w, h) = match chroma {
            Chroma::C444 => (first.width / shrink, first.height / shrink),
            Chroma::C420 => (first.width / shrink & !1, first.height / shrink & !1),
        };
        if w < 16 || h < 16 {
            bail!("--scale {} leaves only {}x{} pixels", args.scale, w, h);
        }
        if w > u16::MAX as u32 || h > u16::MAX as u32 {
            bail!("desktop of {}x{} is larger than the protocol supports", w, h);
        }
        let mut encoder = Encoder::new(device.clone(), w, h, chroma).context("could not create the PyroWave encoder")?;
        match texture {
            None => {
                encode_frame(&mut encoder, &first, max_frame_bytes, boundary)?;
                break encoder;
            }
            // Linux, where frames are on the GPU from the start. If the encoder cannot take
            // them, capture switches to pixels and the next frame is the first one.
            Some(texture) => {
                if encode_texture(&mut source, &mut encoder, &mut imported_texture, texture, max_frame_bytes, boundary)?.is_some() {
                    break encoder;
                }
            }
        }
    };
    let (width, height) = (encoder.width(), encoder.height());

    // From here on frames stay on the GPU where that is possible. On Windows the first one above
    // came as pixels either way, which is what a fallback has to start from.
    zero_copy = zero_copy && source.set_gpu_frames(true);
    if !zero_copy && !args.no_zero_copy {
        debug!("Zero-copy capture is not available here; frames are read back and converted on the CPU");
    }

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

    let audio = if args.no_audio {
        None
    } else {
        match Audio::start() {
            Ok(audio) => {
                info!("Capturing audio at {} Hz", audio.sample_rate);
                Some(audio)
            }
            Err(e) => {
                warn!("{}; streaming without audio", e);
                None
            }
        }
    };

    let config_dir = auth::config_dir().unwrap_or_else(std::env::temp_dir);
    let pairing = Pairing {
        required: !args.no_pairing,
        server_id: auth::local_id(&config_dir.join("server-id")),
        paired: TokenStore::load(config_dir.join("paired-clients")),
    };
    if pairing.required {
        info!(
            "{} paired computer(s); a new one will be asked for a one-time code shown here",
            pairing.paired.len()
        );
    } else {
        warn!("Pairing is off: anyone who can reach this computer can connect");
    }

    let params = CodecParameters {
        video_codec: VideoCodecType::PyroWave,
        video_color_profile: match chroma {
            Chroma::C444 => VideoColorProfile::Bt709FullChroma444,
            Chroma::C420 => VideoColorProfile::Bt709FullCenterChroma420,
        },
        audio_codec: if audio.is_some() { AudioCodecType::RawS16LE } else { AudioCodecType::None },
        frame_rate_num: args.fps as u16,
        frame_rate_den: 1,
        width: width as u16,
        height: height as u16,
        audio_channels: if audio.is_some() { pyromirror_audio::CHANNELS as u32 } else { 0 },
        audio_sample_rate: audio.as_ref().map_or(0, |a| a.sample_rate),
    };

    let bind_addr = SocketAddr::new(args.bind, args.port);
    let listener = TcpListener::bind(bind_addr).with_context(|| format!("could not listen on TCP {}", bind_addr))?;
    let udp = create_streaming_socket(bind_addr, 8 * 1024 * 1024)
        .with_context(|| format!("could not bind UDP {}", bind_addr))?;
    udp.set_read_timeout(Some(Duration::from_millis(100)))?;

    // By default a frame is fully on the wire within half a frame interval instead of being
    // spread across all of it.
    let pace_mbps = ((args.bitrate_mbps as f64 * args.pace_factor).ceil() as u32).max(1);

    let mut pipeline = Pipeline {
        source,
        encoder,
        imported_texture,
        max_frame_bytes,
        packet_boundary: boundary,
        frame_interval: Duration::from_secs_f64(1.0 / args.fps as f64),
        mtu: args.mtu as usize,
        pace_mbps,
        injector,
        audio,
        pairing,
    };

    info!("Listening on {} (TCP control + UDP video); waiting for a client", bind_addr);
    loop {
        let (tcp, client_addr) = listener.accept()?;
        info!("Client connected from {}", client_addr);
        match serve_client(tcp, client_addr, &udp, &params, &mut pipeline) {
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

    // Leaves time for a person to read the code here and type it on the other computer.
    tcp.set_read_timeout(Some(Duration::from_secs(120)))?;
    let pairing = &mut pipeline.pairing;
    // The launcher may have removed computers since the last connection.
    pairing.paired.reload();
    let server_name = auth::device_name();
    let accepted = auth::serve(&mut tcp, pairing.required, pairing.server_id, &server_name, &mut pairing.paired, |name, code| {
        // The launcher reads this line to show the request.
        info!("Pairing request from {}: code {}", name, code);
    })
    .map_err(|e| {
        info!("Pairing ended: {}", e);
        e
    })?;
    if hello.flags & pyromirror_proto::HELLO_PAIR_ONLY != 0 {
        // The other side is only setting up or checking its pairing; no session follows, so
        // this must not look like someone connecting.
        info!("{} checked its pairing", accepted.name);
        return Ok(());
    }
    info!("Accepted {}", accepted.name);
    // Only paired clients can have their pairing taken away.
    let revocable = pairing.required.then_some(accepted.client_id);
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

    let (stream_w, stream_h) = (params.width as f64, params.height as f64);
    let kick = tcp.try_clone()?;
    let Pipeline { injector, audio, mtu, pace_mbps, pairing, source, encoder, imported_texture, max_frame_bytes, packet_boundary, frame_interval } = pipeline;
    let paired = &mut pairing.paired;
    // Only the channel and the flag cross into the audio thread; the capture itself stays put.
    let audio = audio.as_ref().map(|a| (&a.samples, &*a.wanted));
    let (injector, pace_mbps) = (injector.as_ref(), *pace_mbps);
    // Audio datagrams are small either way; they follow the size the session starts with.
    let audio_mtu = *mtu;
    // How often the client has said that no frame arrives whole.
    let datagrams_lost = &AtomicU32::new(0);
    let audio_socket = udp.try_clone()?;
    let (running, target) = (&running, &target);
    // What the client's session has switched on, once it has said so (older clients never do).
    let client_toggles = &AtomicU8::new(TOGGLES_UNKNOWN);

    std::thread::scope(|scope| {
        // Control channel: input events, and the signal that the client went away.
        scope.spawn(|| {
            let mut control = control;
            let mut payload = [0u8; MAX_MESSAGE_PAYLOAD];
            while running.load(Ordering::Relaxed) {
                match read_message(&mut control, &mut payload) {
                    Ok((MSG_TYPE_INPUT_EVENT, len)) => match InputEvent::deserialize(&payload[..len]) {
                        Ok(event) => {
                            match event {
                                // Motion is far too frequent to log above trace level.
                                InputEvent::MouseMoveAbsolute { .. } | InputEvent::MouseMoveRelative { .. } => {
                                    trace!("Input event: {:?}", event)
                                }
                                _ => debug!("Input event: {:?}", event),
                            }
                            if let Some(injector) = injector {
                                inject(injector, event, stream_w, stream_h);
                            }
                        }
                        Err(e) => debug!("Undecodable input event: {}", e),
                    },
                    Ok((MSG_TYPE_SESSION_STATE, 1)) => client_toggles.store(payload[0], Ordering::Relaxed),
                    Ok((MSG_TYPE_DATAGRAMS_LOST, 4)) => {
                        let largest = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
                        debug!("The client gets no whole frame; the largest datagram it received had {} bytes", largest);
                        datagrams_lost.fetch_add(1, Ordering::Relaxed);
                    }
                    Ok((other, _)) => debug!("Ignoring control message type {}", other),
                    Err(_) => break,
                }
            }
            // Do not leave keys or buttons stuck down on the host.
            if let Some(injector) = injector {
                injector.release_all();
            }
            running.store(false, Ordering::Relaxed);
        });

        if let Some((samples, wanted)) = audio {
            scope.spawn(move || {
                let mut sender =
                    AudioSender::new(audio_socket, *target.lock().unwrap(), audio_mtu, pyromirror_audio::CHANNELS as usize);
                let start = Instant::now();
                // Anything still queued belongs to the previous client.
                while samples.try_recv().is_ok() {}
                wanted.store(true, Ordering::Relaxed);
                while running.load(Ordering::Relaxed) {
                    if let Ok(pcm) = samples.recv_timeout(Duration::from_millis(100)) {
                        sender.set_target_addr(*target.lock().unwrap());
                        // A failed send only costs a few milliseconds of sound.
                        let _ = sender.send(&pcm, start.elapsed().as_micros() as u64);
                    }
                }
                wanted.store(false, Ordering::Relaxed);
            });
        }

        // The client keeps sending small datagrams from its video socket. If it sits behind NAT,
        // their source address is where video has to go, rather than the port it announced.
        scope.spawn(|| {
            let mut buf = [0u8; 64];
            let mut last_check = Instant::now();
            while running.load(Ordering::Relaxed) {
                // Removing a computer from the paired list (in the launcher) ends its session.
                if let (Some(client_id), true) = (revocable, last_check.elapsed() >= Duration::from_secs(2)) {
                    last_check = Instant::now();
                    paired.reload();
                    if paired.get(&client_id).is_none() {
                        info!("{} was removed from the paired computers; disconnecting it", accepted.name);
                        running.store(false, Ordering::Relaxed);
                        let _ = kick.shutdown(Shutdown::Both);
                        break;
                    }
                }
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

        let result = stream_video(
            source,
            encoder,
            imported_texture,
            *max_frame_bytes,
            mtu,
            packet_boundary,
            datagrams_lost,
            *frame_interval,
            send_socket,
            &tcp,
            target,
            running,
            pace_mbps,
            client_toggles,
        );
        running.store(false, Ordering::Relaxed);
        let _ = tcp.shutdown(Shutdown::Both);
        result
    })
}

/// Tells the viewer what the pointer looks like.
fn send_cursor(control: &mut &TcpStream, cursor: &pyromirror_capture::Cursor, scale: u32) -> std::io::Result<()> {
    let fits = cursor.width <= MAX_CURSOR_SIDE as u32 && cursor.height <= MAX_CURSOR_SIDE as u32;
    let has_image = fits && !cursor.in_video && !cursor.rgba.is_empty();
    let mut flags = 0;
    if cursor.visible {
        flags |= CURSOR_VISIBLE;
    }
    if cursor.in_video {
        flags |= CURSOR_IN_VIDEO;
    }
    let header = CursorHeader {
        width: if has_image { cursor.width as u16 } else { 0 },
        height: if has_image { cursor.height as u16 } else { 0 },
        hot_x: cursor.hot_x.min(u16::MAX as u32) as u16,
        hot_y: cursor.hot_y.min(u16::MAX as u32) as u16,
        flags,
        scale: scale.clamp(1, u16::MAX as u32) as u16,
    };
    write_message(control, MSG_TYPE_CURSOR, &header.serialize())?;
    if has_image {
        control.write_all(&cursor.rgba)?;
    }
    control.flush()
}

/// Applies one client input event to the host desktop.
fn inject(injector: &InputInjector, event: InputEvent, stream_w: f64, stream_h: f64) {
    match event {
        // Absolute positions arrive in stream pixels.
        InputEvent::MouseMoveAbsolute { x, y } => {
            injector.pointer_absolute((x as f64 + 0.5) / stream_w, (y as f64 + 0.5) / stream_h)
        }
        InputEvent::MouseMoveRelative { dx, dy } => injector.pointer_relative(dx as f64, dy as f64),
        InputEvent::MouseButton { button, down } => injector.button(button, down),
        InputEvent::MouseWheel { dx, dy } => injector.wheel(dx as i32, dy as i32),
        InputEvent::KeyboardKey { scancode, down, .. } => injector.key(scancode, down),
        InputEvent::Gamepad { .. } => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn stream_video(
    source: &mut Source,
    encoder: &mut Encoder,
    imported_texture: &mut Option<u64>,
    max_frame_bytes: usize,
    // The datagram size and the packet size that follows from it. Both shrink, for as long as the
    // server runs, when the client reports that datagrams this big do not reach it.
    mtu: &mut usize,
    packet_boundary_now: &mut usize,
    datagrams_lost: &AtomicU32,
    frame_interval: Duration,
    socket: UdpSocket,
    mut control: &TcpStream,
    target: &Mutex<SocketAddr>,
    running: &AtomicBool,
    pace_mbps: u32,
    client_toggles: &AtomicU8,
) -> anyhow::Result<()> {
    // The tray menu on this computer can steer the client's session; that is the way out for
    // someone who is stuck inside it, since this desktop is what they can still reach.
    let mut session = Listener::new(Side::Host);
    let mut last_session_poll = Instant::now();
    // The pointer is not part of the picture: its shape goes to the viewer, which draws it
    // itself. `None` makes the first check send whatever is known.
    let mut cursor_serial: Option<u64> = None;
    // Frames go out from a thread of their own, while this one gets on with the next.
    let mut sender = FrameQueue::new(FrameSender::new(socket, *target.lock().unwrap(), pace_mbps))?;

    let start = Instant::now();
    let mut last_sent: Option<Instant> = None;
    let mut stats = Stats::default();

    // Frames are sent as the desktop delivers them. Waiting for a tick of our own instead would
    // send each one up to a frame late, and repeat or skip one whenever the two clocks drift
    // past each other. This only holds back a desktop that is faster than the stream is meant
    // to be; the slack keeps one that runs at just about the same rate from being held back.
    let min_interval = frame_interval - frame_interval / 8;
    let mut next_frame_from = Instant::now();

    // Time for the client to notice that a change helped before it is asked again.
    let mut shrunk_at: Option<Instant> = None;

    while running.load(Ordering::Relaxed) {
        if datagrams_lost.swap(0, Ordering::Relaxed) > 0 && shrunk_at.map_or(true, |t| t.elapsed() >= Duration::from_secs(3)) {
            match smaller_datagrams(*mtu) {
                Some(smaller) => {
                    warn!(
                        "Datagrams of {} bytes do not reach the client (a jumbo-frame setting on a network \
                         that cannot carry them?); sending {} byte datagrams from now on",
                        *mtu, smaller
                    );
                    *mtu = smaller;
                    *packet_boundary_now = packet_boundary(smaller);
                    shrunk_at = Some(Instant::now());
                }
                None => debug!("The client gets no whole frame, and {} byte datagrams are the smallest tried", *mtu),
            }
        }
        let packet_boundary = *packet_boundary_now;

        if source.sets_its_own_rate() {
            sleep_until(next_frame_from);
        }
        let frame = source.next_frame(frame_interval)?;
        if let Some(frame) = &frame {
            // Counted from when the desktop delivered it, not from when it was copied.
            next_frame_from = Instant::now() - frame.prepare + min_interval;
        }
        let encode_start = Instant::now();
        // Borrow of `source` by the frame ends with the encode below; the pointer is checked
        // after it.
        let (mut prepare, mut on_gpu) = (Duration::ZERO, false);
        let packets = match frame {
            Some(frame) => {
                prepare = frame.prepare;
                match TextureFrame::of(&frame) {
                    Some(texture) => {
                        on_gpu = true;
                        encode_texture(source, encoder, imported_texture, texture, max_frame_bytes, packet_boundary)?
                    }
                    None => Some(encode_frame(encoder, &frame, max_frame_bytes, packet_boundary)?),
                }
            }
            None if last_sent.map_or(true, |t| t.elapsed() >= IDLE_REFRESH) => {
                Some(encoder.encode_last(max_frame_bytes, packet_boundary)?)
            }
            None => None,
        };

        if let Some(packets) = packets {
            let encode_time = encode_start.elapsed();
            let pts = start.elapsed().as_micros() as u64;
            let bytes = sender.send_frame(packets.iter(), pts, *target.lock().unwrap());
            stats.frame(bytes, prepare, encode_time, on_gpu);
            last_sent = Some(Instant::now());
        }
        if let Some(cursor) = source.cursor(cursor_serial) {
            cursor_serial = Some(cursor.serial);
            if let Err(e) = send_cursor(&mut control, &cursor, source.scale()) {
                debug!("Could not send the pointer shape: {}", e);
            }
        }
        match client_toggles.load(Ordering::Relaxed) {
            TOGGLES_UNKNOWN => {}
            bits => session.publish(Toggles::from_bits(bits)),
        }
        if last_session_poll.elapsed() >= Duration::from_millis(100) {
            last_session_poll = Instant::now();
            for action in session.poll() {
                info!("Tray menu: {} for the client", action.label());
                if let Err(e) = write_message(&mut control, MSG_TYPE_SESSION_ACTION, &[action.to_byte()]) {
                    debug!("Could not send a session action: {}", e);
                }
            }
        }
        stats.report();
    }
    Ok(())
}

#[derive(Default)]
struct Stats {
    since: Option<Instant>,
    frames: u32,
    bytes: usize,
    /// Getting frames to the encoder: readback and shrinking, or the copy on the GPU.
    prepare: Duration,
    /// Colour conversion (when done on the CPU) and encoding.
    encode: Duration,
    /// Frames that never left the GPU.
    on_gpu: u32,
    reported: bool,
}

impl Stats {
    fn frame(&mut self, bytes: usize, prepare: Duration, encode: Duration, on_gpu: bool) {
        self.frames += 1;
        self.bytes += bytes;
        self.prepare += prepare;
        self.encode += encode;
        self.on_gpu += on_gpu as u32;
    }

    fn report(&mut self) {
        let since = *self.since.get_or_insert_with(Instant::now);
        let elapsed = since.elapsed();
        if elapsed < Duration::from_secs(2) {
            return;
        }
        if self.frames > 0 {
            let per_frame = |total: Duration| total.as_secs_f64() * 1000.0 / self.frames as f64;
            let path = match self.on_gpu {
                0 => "read back, converted on the CPU",
                n if n == self.frames => "zero-copy",
                _ => "mixed",
            };
            let line = format!(
                "{:.1} fps, {:.1} Mbps; per frame: {:.2} ms capture + {:.2} ms encode ({})",
                self.frames as f64 / elapsed.as_secs_f64(),
                self.bytes as f64 * 8.0 / 1e6 / elapsed.as_secs_f64(),
                per_frame(self.prepare),
                per_frame(self.encode),
                path
            );
            // Once per session where everyone sees it, then only for those who ask.
            if self.reported {
                debug!("{}", line);
            } else {
                info!("Frame cost: {}", line);
            }
            self.reported = true;
        }
        *self = Stats { since: Some(Instant::now()), reported: self.reported, ..Default::default() };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn datagrams_step_down_to_wifi_then_ethernet_size() {
        assert_eq!(smaller_datagrams(8900), Some(1900));
        assert_eq!(smaller_datagrams(1900), Some(1400));
        assert_eq!(smaller_datagrams(1500), Some(1400));
        assert_eq!(smaller_datagrams(1400), None);
        assert_eq!(smaller_datagrams(1200), None);
    }
}
