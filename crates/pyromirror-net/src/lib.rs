//! PyroMirror UDP video transport.
//!
//! PyroWave packets are independently decodable, so each one travels in its own datagram behind
//! a [`PayloadHeader`]. Nothing is reassembled or retransmitted: a lost datagram costs a few
//! wavelet blocks of one frame, and the next frame replaces it anyway.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use pyromirror_proto::{
    PayloadHeader, PAYLOAD_HEADER_SIZE, PAYLOAD_PACKET_BEGIN_BIT, PAYLOAD_PACKET_END_BIT,
    PAYLOAD_PACKET_SEQ_BITS, PAYLOAD_PACKET_SEQ_MASK, PAYLOAD_PACKET_SEQ_OFFSET,
    PAYLOAD_STREAM_TYPE_BIT, PAYLOAD_SUBPACKET_SEQ_MASK, PAYLOAD_SUBPACKET_SEQ_OFFSET,
};
use socket2::{Domain, Protocol, Socket, Type};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetError {
    #[error("Socket I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Proto error: {0}")]
    Proto(#[from] pyromirror_proto::ProtoError),
    #[error("Frame has {0} packets, more than the protocol can number")]
    TooManyPackets(usize),
    #[error("Malformed packet")]
    MalformedPacket,
}

/// Datagram sent by the client so the server learns (and keeps learning) its UDP endpoint.
pub const UDP_PUNCH: &[u8] = b"PYROMIRROR_UDP_PUNCH";

/// Largest codec packet that fits a datagram of `mtu` bytes.
pub fn packet_boundary(mtu: usize) -> usize {
    mtu.saturating_sub(PAYLOAD_HEADER_SIZE)
}

/// Token bucket that spreads a frame's datagrams out so switches and socket buffers are not hit
/// with the whole frame at once.
pub struct PacketPacer {
    rate_bytes_per_sec: f64,
    tokens: f64,
    max_tokens: f64,
    last_update: Instant,
}

impl PacketPacer {
    pub fn new(rate_mbps: u32, max_burst_bytes: usize) -> Self {
        let max_tokens = max_burst_bytes as f64;
        Self {
            rate_bytes_per_sec: rate_mbps as f64 * 1_000_000.0 / 8.0,
            tokens: max_tokens,
            max_tokens,
            last_update: Instant::now(),
        }
    }

    /// Accounts for a datagram of `packet_size` bytes. Returns how long to wait before sending it
    /// if the bucket is empty.
    pub fn pace(&mut self, packet_size: usize) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;
        self.tokens = (self.tokens + elapsed * self.rate_bytes_per_sec).min(self.max_tokens);

        // The bucket may go negative: the caller waits out the debt before sending.
        self.tokens -= packet_size as f64;
        if self.tokens >= 0.0 {
            None
        } else {
            Some(Duration::from_secs_f64(-self.tokens / self.rate_bytes_per_sec))
        }
    }
}

/// Sleeps until `deadline` without relying on the OS timer for the last stretch:
/// `thread::sleep` rounds short sleeps up to 1-15 ms on Windows, which would throttle the stream
/// far below the configured bitrate and frame rate.
pub fn sleep_until(deadline: Instant) {
    if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
        if wait > Duration::from_millis(3) {
            std::thread::sleep(wait - Duration::from_millis(2));
        }
    }
    while Instant::now() < deadline {
        std::thread::yield_now();
    }
}

/// Creates a UDP socket with large kernel buffers for high-bitrate streaming.
pub fn create_streaming_socket(bind_addr: SocketAddr, buffer_size: usize) -> Result<std::net::UdpSocket, NetError> {
    let domain = if bind_addr.is_ipv6() { Domain::IPV6 } else { Domain::IPV4 };
    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    // Best effort: the OS clamps these (net.core.rmem_max / wmem_max on Linux).
    let _ = socket.set_recv_buffer_size(buffer_size);
    let _ = socket.set_send_buffer_size(buffer_size);
    socket.bind(&bind_addr.into())?;
    Ok(socket.into())
}

/// Sends encoded frames, one datagram per codec packet.
pub struct FrameSender {
    socket: std::net::UdpSocket,
    target_addr: SocketAddr,
    pacer: PacketPacer,
    frame_seq: u32,
    packet_buf: Vec<u8>,
}

impl FrameSender {
    /// `pace_mbps` is the wire rate datagrams are released at; it should be comfortably above the
    /// video bitrate so a frame is on the wire well before the next one is captured.
    pub fn new(socket: std::net::UdpSocket, target_addr: SocketAddr, pace_mbps: u32) -> Self {
        Self {
            socket,
            target_addr,
            pacer: PacketPacer::new(pace_mbps, 64 * 1024),
            frame_seq: 0,
            packet_buf: Vec::new(),
        }
    }

    pub fn set_target_addr(&mut self, target: SocketAddr) {
        self.target_addr = target;
    }

    pub fn target_addr(&self) -> SocketAddr {
        self.target_addr
    }

    /// Sends the packets of one encoded frame. Returns the number of bytes put on the wire.
    pub fn send_frame<'a, I>(&mut self, packets: I, pts: u64) -> Result<usize, NetError>
    where
        I: ExactSizeIterator<Item = &'a [u8]>,
    {
        let num_packets = packets.len();
        if num_packets > PAYLOAD_SUBPACKET_SEQ_MASK as usize + 1 {
            return Err(NetError::TooManyPackets(num_packets));
        }

        let mut total_sent = 0;
        for (index, packet) in packets.enumerate() {
            let mut flags = (self.frame_seq << PAYLOAD_PACKET_SEQ_OFFSET)
                | ((index as u32) << PAYLOAD_SUBPACKET_SEQ_OFFSET);
            if index == 0 {
                flags |= PAYLOAD_PACKET_BEGIN_BIT;
            }
            if index == num_packets - 1 {
                flags |= PAYLOAD_PACKET_END_BIT;
            }

            let header = PayloadHeader {
                pts,
                dts_delta: 0,
                payload_size: packet.len() as u32,
                num_fec_blocks: 0,
                num_xor_blocks_even: 0,
                num_xor_blocks_odd: 0,
                flags,
            };

            let datagram_len = PAYLOAD_HEADER_SIZE + packet.len();
            if self.packet_buf.len() < datagram_len {
                self.packet_buf.resize(datagram_len, 0);
            }
            header.serialize(&mut self.packet_buf[..PAYLOAD_HEADER_SIZE])?;
            self.packet_buf[PAYLOAD_HEADER_SIZE..datagram_len].copy_from_slice(packet);

            if let Some(wait) = self.pacer.pace(datagram_len) {
                sleep_until(Instant::now() + wait);
            }
            self.socket.send_to(&self.packet_buf[..datagram_len], self.target_addr)?;
            total_sent += datagram_len;
        }

        self.frame_seq = (self.frame_seq + 1) & PAYLOAD_PACKET_SEQ_MASK;
        Ok(total_sent)
    }
}

/// One received video datagram.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPacket<'a> {
    /// Wrapping frame counter, see [`frame_seq_is_newer`].
    pub frame_seq: u32,
    /// Index of this packet within its frame.
    pub index: u32,
    pub pts: u64,
    pub first: bool,
    pub last: bool,
    /// One PyroWave packet, ready for the decoder.
    pub payload: &'a [u8],
}

/// One received audio datagram: interleaved little-endian 16-bit PCM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioPacket<'a> {
    /// Wrapping packet counter, ordered like video frames (see [`frame_seq_is_newer`]).
    pub seq: u32,
    pub pts: u64,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packet<'a> {
    Video(VideoPacket<'a>),
    Audio(AudioPacket<'a>),
}

/// Parses a datagram produced by [`FrameSender`] or [`AudioSender`].
pub fn parse_datagram(data: &[u8]) -> Result<Packet<'_>, NetError> {
    if data.len() < PAYLOAD_HEADER_SIZE {
        return Err(NetError::MalformedPacket);
    }
    let header = PayloadHeader::deserialize(&data[..PAYLOAD_HEADER_SIZE])?;
    let payload = &data[PAYLOAD_HEADER_SIZE..];
    if header.payload_size as usize != payload.len() {
        return Err(NetError::MalformedPacket);
    }
    Ok(if header.is_audio() {
        Packet::Audio(AudioPacket { seq: header.packet_seq(), pts: header.pts, payload })
    } else {
        Packet::Video(VideoPacket {
            frame_seq: header.packet_seq(),
            index: header.subpacket_seq(),
            pts: header.pts,
            first: header.is_packet_begin(),
            last: header.is_packet_end(),
            payload,
        })
    })
}

/// Like [`parse_datagram`], but rejects anything that is not video.
pub fn parse_video_datagram(data: &[u8]) -> Result<VideoPacket<'_>, NetError> {
    match parse_datagram(data)? {
        Packet::Video(packet) => Ok(packet),
        Packet::Audio(_) => Err(NetError::MalformedPacket),
    }
}

/// Sends raw PCM audio. Audio is tiny next to video, so it is neither paced nor compressed.
pub struct AudioSender {
    socket: std::net::UdpSocket,
    target_addr: SocketAddr,
    seq: u32,
    max_samples: usize,
    packet_buf: Vec<u8>,
}

impl AudioSender {
    /// `mtu` is the largest datagram to produce, `channels` keeps frames whole within a packet.
    pub fn new(socket: std::net::UdpSocket, target_addr: SocketAddr, mtu: usize, channels: usize) -> Self {
        let channels = channels.max(1);
        let max_samples = (packet_boundary(mtu) / 2 / channels).max(1) * channels;
        Self { socket, target_addr, seq: 0, max_samples, packet_buf: Vec::new() }
    }

    pub fn set_target_addr(&mut self, target: SocketAddr) {
        self.target_addr = target;
    }

    pub fn send(&mut self, samples: &[i16], pts: u64) -> Result<(), NetError> {
        for chunk in samples.chunks(self.max_samples) {
            let header = PayloadHeader {
                pts,
                dts_delta: 0,
                payload_size: (chunk.len() * 2) as u32,
                num_fec_blocks: 0,
                num_xor_blocks_even: 0,
                num_xor_blocks_odd: 0,
                flags: PAYLOAD_STREAM_TYPE_BIT | (self.seq << PAYLOAD_PACKET_SEQ_OFFSET),
            };
            self.packet_buf.resize(PAYLOAD_HEADER_SIZE, 0);
            header.serialize(&mut self.packet_buf)?;
            self.packet_buf.extend(chunk.iter().flat_map(|s| s.to_le_bytes()));
            self.socket.send_to(&self.packet_buf, self.target_addr)?;
            self.seq = (self.seq + 1) & PAYLOAD_PACKET_SEQ_MASK;
        }
        Ok(())
    }
}

/// Whether frame `a` comes after frame `b`, accounting for wrap-around of the frame counter.
pub fn frame_seq_is_newer(a: u32, b: u32) -> bool {
    let diff = a.wrapping_sub(b) & PAYLOAD_PACKET_SEQ_MASK;
    diff != 0 && diff < (1 << (PAYLOAD_PACKET_SEQ_BITS - 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_allows_burst_then_throttles() {
        let mut pacer = PacketPacer::new(8, 3000); // 1 MB/s, 3000 byte burst
        assert_eq!(pacer.pace(1400), None);
        assert_eq!(pacer.pace(1400), None);
        let wait = pacer.pace(1400).expect("bucket should be empty");
        assert!(wait <= Duration::from_millis(2), "{:?}", wait);
    }

    #[test]
    fn frame_seq_wraps() {
        assert!(frame_seq_is_newer(1, 0));
        assert!(!frame_seq_is_newer(0, 1));
        assert!(!frame_seq_is_newer(5, 5));
        assert!(frame_seq_is_newer(0, PAYLOAD_PACKET_SEQ_MASK));
        assert!(!frame_seq_is_newer(PAYLOAD_PACKET_SEQ_MASK, 0));
    }

    #[test]
    fn audio_is_chunked_on_frame_boundaries() {
        let rx = create_streaming_socket("127.0.0.1:0".parse().unwrap(), 1 << 20).unwrap();
        let tx = create_streaming_socket("127.0.0.1:0".parse().unwrap(), 1 << 20).unwrap();
        rx.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        // 64-byte datagrams leave room for 20 samples = 10 stereo frames.
        let mut sender = AudioSender::new(tx, rx.local_addr().unwrap(), 64, 2);
        let samples: Vec<i16> = (0..50).map(|i| i * 300 - 7000).collect();
        sender.send(&samples, 42).unwrap();

        let mut received = Vec::new();
        let mut buf = [0u8; 128];
        for expected_seq in 0..3 {
            let (len, _) = rx.recv_from(&mut buf).unwrap();
            assert!(len <= 64);
            assert!(parse_video_datagram(&buf[..len]).is_err());
            let Packet::Audio(packet) = parse_datagram(&buf[..len]).unwrap() else { panic!("not audio") };
            assert_eq!((packet.seq, packet.pts), (expected_seq, 42));
            assert_eq!(packet.payload.len() % 4, 0);
            received.extend(packet.payload.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])));
        }
        assert_eq!(received, samples);
    }

    #[test]
    fn rejects_truncated_datagrams() {
        assert!(parse_video_datagram(&[0u8; 10]).is_err());
        assert!(parse_video_datagram(UDP_PUNCH).is_err());
    }
}
