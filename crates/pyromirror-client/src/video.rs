//! UDP receive thread: decodes video, passes audio on.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, TrySendError};
use log::{debug, error, info, warn};

use pyromirror_codec::{Decoder, PixelFormat};
use pyromirror_net::{frame_seq_is_newer, parse_datagram, Packet};

/// A decoded picture, on its way to the window.
pub enum Frame {
    /// RGBA pixels, to upload.
    Pixels(Vec<u8>),
    /// The picture is in this set of GPU textures already (see `gpu_present`).
    Target(usize),
}

#[derive(Default)]
struct Stats {
    frames: u32,
    partial: u32,
    skipped: u32,
    audio_packets: u32,
    bytes: usize,
    decode: Duration,
    /// Frames that never left the GPU.
    on_gpu: u32,
    /// The largest video datagram that arrived, in bytes.
    largest: usize,
}

/// Incomplete frames within one report (two seconds) without a single complete one, from which
/// on the picture counts as not getting through. An idle desktop still sends four a second.
const STALLED_FRAMES: u32 = 4;

/// Feeds datagrams to the decoder and sends finished frames to the render thread.
///
/// A frame is decoded as soon as PyroWave reports it complete. If the next frame starts arriving
/// first, packets were lost: the incomplete frame is still decoded if enough of it is there
/// (missing blocks come out blurred), otherwise it is skipped.
pub fn receive_loop(
    socket: UdpSocket,
    mut decoder: Decoder,
    width: u32,
    height: u32,
    frames: Sender<Frame>,
    // Called after a frame was handed over, to wake the thread that shows it.
    wake: impl Fn(),
    recycled: Receiver<Vec<u8>>,
    audio: Option<Sender<Vec<i16>>>,
    summary: Arc<Mutex<String>>,
    // Set to the size of the largest datagram that arrived when no frame could be put together
    // for a while; the main thread passes that on to the server and resets it to 0.
    stalled: Arc<AtomicU32>,
    // Datagrams larger than this are dropped, to stand in for a network that does (testing).
    largest_carried: usize,
    running: Arc<AtomicBool>,
) {
    let stride = width as usize * 4;
    let frame_len = stride * height as usize;
    let mut datagram = vec![0u8; 65536];

    let mut current_seq: Option<u32> = None;
    let mut packets_in_frame = 0u32;
    let mut frame_done = false;
    let mut audio_seq: Option<u32> = None;

    let mut stats = Stats::default();
    let mut last_report = Instant::now();
    let mut first_frame = true;
    // Which set of GPU textures the next frame goes into, while there are any.
    let mut next_target = 0;

    let mut decode = |decoder: &mut Decoder, stats: &mut Stats, partial: bool| {
        let start = Instant::now();
        let mut frame = None;
        if decoder.gpu_targets() > 0 {
            match decoder.decode_to_target(next_target) {
                Ok(()) => {
                    frame = Some(Frame::Target(next_target));
                    next_target = (next_target + 1) % decoder.gpu_targets();
                }
                Err(e) => {
                    warn!("Zero-copy display failed ({}); converting frames on the CPU instead", e);
                    decoder.drop_gpu_targets();
                }
            }
        }
        let frame = match frame {
            Some(frame) => frame,
            None => {
                let mut buffer = recycled.try_recv().unwrap_or_default();
                buffer.resize(frame_len, 0);
                if let Err(e) = decoder.decode(&mut buffer, stride, PixelFormat::Rgbx) {
                    warn!("Decode failed: {}", e);
                    return;
                }
                Frame::Pixels(buffer)
            }
        };
        stats.on_gpu += matches!(frame, Frame::Target(_)) as u32;
        stats.decode += start.elapsed();
        stats.frames += 1;
        stats.partial += partial as u32;
        if first_frame {
            first_frame = false;
            info!("First frame decoded in {:.1} ms", start.elapsed().as_secs_f64() * 1000.0);
        }

        match frames.try_send(frame) {
            Ok(()) => wake(),
            Err(TrySendError::Full(_)) => {}
            Err(TrySendError::Disconnected(_)) => running.store(false, Ordering::Relaxed),
        }
    };

    while running.load(Ordering::Relaxed) {
        match socket.recv_from(&mut datagram) {
            Ok((len, _)) if len > largest_carried => {}
            Ok((len, _)) => {
                // Anything that does not parse is not ours (or is corrupt); ignore it.
                let packet = match parse_datagram(&datagram[..len]) {
                    Ok(Packet::Video(packet)) => packet,
                    Ok(Packet::Audio(packet)) => {
                        stats.audio_packets += 1;
                        // Late or duplicated audio would play out of order; drop it.
                        let fresh = audio_seq.map_or(true, |last| frame_seq_is_newer(packet.seq, last));
                        if let (true, Some(audio)) = (fresh, &audio) {
                            audio_seq = Some(packet.seq);
                            let pcm = packet.payload.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]));
                            let _ = audio.try_send(pcm.collect());
                        }
                        continue;
                    }
                    Err(_) => continue,
                };
                stats.bytes += len;
                stats.largest = stats.largest.max(len);

                match current_seq {
                    Some(seq) if packet.frame_seq == seq => {}
                    Some(seq) if !frame_seq_is_newer(packet.frame_seq, seq) => continue, // late packet
                    _ => {
                        if current_seq.is_some() && !frame_done && packets_in_frame > 0 {
                            if decoder.is_ready(true) {
                                decode(&mut decoder, &mut stats, true);
                            } else {
                                stats.skipped += 1;
                            }
                        }
                        decoder.clear();
                        current_seq = Some(packet.frame_seq);
                        packets_in_frame = 0;
                        frame_done = false;
                    }
                }

                if frame_done {
                    continue;
                }
                if decoder.push_packet(packet.payload) {
                    packets_in_frame += 1;
                }
                if decoder.is_ready(false) {
                    decode(&mut decoder, &mut stats, false);
                    frame_done = true;
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock) => {}
            // Windows reports ICMP "port unreachable" from our own keepalives on the receiving
            // socket while the server is not listening yet; that is not a receive failure.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => {
                error!("UDP receive failed: {}", e);
                running.store(false, Ordering::Relaxed);
            }
        }

        let elapsed = last_report.elapsed();
        if elapsed >= Duration::from_secs(2) {
            // Datagrams arrive but never a whole frame's worth: most likely the big ones are
            // being dropped on the way.
            let is_stalled = stats.frames == 0 && stats.skipped >= STALLED_FRAMES;
            if is_stalled {
                stalled.store(stats.largest.max(1) as u32, Ordering::Relaxed);
            }
            // Shown in the viewer's toolbar.
            *summary.lock().unwrap() = if is_stalled {
                "no picture: packets lost".to_owned()
            } else {
                format!(
                    "{:.0} fps {:.0} Mbps pkt {}",
                    stats.frames as f64 / elapsed.as_secs_f64(),
                    stats.bytes as f64 * 8.0 / 1e6 / elapsed.as_secs_f64(),
                    stats.largest
                )
            };
            if stats.frames > 0 || stats.skipped > 0 {
                let log_degraded = stats.partial > 0 || stats.skipped > 0;
                let message = format!(
                    "{:.1} fps, {:.1} Mbps, {:.2} ms decode per frame ({}), {} partial, {} skipped, {} audio packets, datagrams up to {} bytes",
                    stats.frames as f64 / elapsed.as_secs_f64(),
                    stats.bytes as f64 * 8.0 / 1e6 / elapsed.as_secs_f64(),
                    stats.decode.as_secs_f64() * 1000.0 / stats.frames.max(1) as f64,
                    if stats.on_gpu > 0 { "zero-copy" } else { "read back, converted on the CPU" },
                    stats.partial,
                    stats.skipped,
                    stats.audio_packets,
                    stats.largest
                );
                if log_degraded {
                    info!("{}", message);
                } else {
                    debug!("{}", message);
                }
            }
            stats = Stats::default();
            last_report = Instant::now();
        }
    }
}
