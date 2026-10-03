//! PyroMirror High-Speed Network Engine
//!
//! Handles UDP packet pacing, socket configuration, frame reassembly, and token-bucket rate limiting.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use pyromirror_proto::{
    PayloadHeader, PAYLOAD_HEADER_SIZE, PAYLOAD_KEY_FRAME_BIT,
    PAYLOAD_PACKET_BEGIN_BIT, PAYLOAD_PACKET_SEQ_OFFSET, PAYLOAD_SUBPACKET_SEQ_OFFSET,
};
use socket2::{Domain, Protocol, Socket, Type};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum NetError {
    #[error("Socket I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Proto error: {0}")]
    Proto(#[from] pyromirror_proto::ProtoError),
    #[error("Buffer overflow: packet exceeds capacity")]
    BufferOverflow,
    #[error("Malformed packet")]
    MalformedPacket,
}

/// Token bucket packet pacer to prevent switch buffer exhaustion during micro-bursts
pub struct PacketPacer {
    rate_bytes_per_sec: u64,
    tokens: f64,
    max_tokens: f64,
    last_update: Instant,
}

impl PacketPacer {
    pub fn new(bitrate_mbps: u32, max_burst_ms: u32) -> Self {
        let rate_bytes_per_sec = (bitrate_mbps as u64 * 1_000_000) / 8;
        let max_tokens = (rate_bytes_per_sec as f64) * (max_burst_ms as f64 / 1000.0);
        Self {
            rate_bytes_per_sec,
            tokens: max_tokens,
            max_tokens,
            last_update: Instant::now(),
        }
    }

    pub fn set_bitrate(&mut self, bitrate_mbps: u32) {
        self.rate_bytes_per_sec = (bitrate_mbps as u64 * 1_000_000) / 8;
    }

    /// Checks if a packet of `packet_size` bytes can be sent immediately.
    /// If not, returns the duration to wait before sending.
    pub fn pace(&mut self, packet_size: usize) -> Option<Duration> {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_update).as_secs_f64();
        self.last_update = now;

        self.tokens = (self.tokens + elapsed * (self.rate_bytes_per_sec as f64)).min(self.max_tokens);

        let cost = packet_size as f64;
        if self.tokens >= cost {
            self.tokens -= cost;
            None
        } else {
            let deficit = cost - self.tokens;
            let wait_secs = deficit / (self.rate_bytes_per_sec as f64);
            Some(Duration::from_secs_f64(wait_secs))
        }
    }
}

/// Creates a tuned, non-blocking UDP socket optimized for high-bandwidth streaming
pub fn create_streaming_socket(bind_addr: Option<SocketAddr>, buffer_size: usize) -> Result<Socket, NetError> {
    let domain = if bind_addr.map_or(false, |a| a.is_ipv6()) {
        Domain::IPV6
    } else {
        Domain::IPV4
    };

    let socket = Socket::new(domain, Type::DGRAM, Some(Protocol::UDP))?;
    let _ = socket.set_recv_buffer_size(buffer_size);
    let _ = socket.set_send_buffer_size(buffer_size);

    if let Some(addr) = bind_addr {
        socket.bind(&addr.into())?;
    }

    Ok(socket)
}

/// UDP Packet Chunk Sender
pub struct FrameSender {
    socket: std::net::UdpSocket,
    target_addr: SocketAddr,
    pacer: PacketPacer,
    mtu: usize,
    frame_seq: u32,
    packet_buf: Vec<u8>,
}

impl FrameSender {
    pub fn new(
        socket: std::net::UdpSocket,
        target_addr: SocketAddr,
        bitrate_mbps: u32,
        mtu: usize,
    ) -> Self {
        Self {
            socket,
            target_addr,
            pacer: PacketPacer::new(bitrate_mbps, 5),
            mtu,
            frame_seq: 0,
            packet_buf: vec![0u8; mtu],
        }
    }

    pub fn socket(&self) -> &std::net::UdpSocket {
        &self.socket
    }

    pub fn set_target_addr(&mut self, target: SocketAddr) {
        self.target_addr = target;
    }

    pub fn target_addr(&self) -> SocketAddr {
        self.target_addr
    }


    /// Chunks an encoded frame bitstream into UDP datagrams and sends them with pacing
    pub fn send_frame(&mut self, bitstream: &[u8], pts: u64, is_key: bool) -> Result<usize, NetError> {
        let max_payload = self.mtu.saturating_sub(PAYLOAD_HEADER_SIZE);
        if max_payload == 0 {
            return Err(NetError::BufferOverflow);
        }

        let num_packets = (bitstream.len() + max_payload - 1) / max_payload;
        let mut total_sent = 0;

        for subpacket_idx in 0..num_packets {
            let start = subpacket_idx * max_payload;
            let end = (start + max_payload).min(bitstream.len());
            let chunk = &bitstream[start..end];

            let mut flags = (self.frame_seq << PAYLOAD_PACKET_SEQ_OFFSET)
                | ((subpacket_idx as u32) << PAYLOAD_SUBPACKET_SEQ_OFFSET);

            if is_key {
                flags |= PAYLOAD_KEY_FRAME_BIT;
            }
            if subpacket_idx == 0 {
                flags |= PAYLOAD_PACKET_BEGIN_BIT;
            }
            if subpacket_idx == num_packets - 1 {
                flags |= pyromirror_proto::PAYLOAD_PACKET_END_BIT;
            }

            let header = PayloadHeader {
                pts,
                dts_delta: 0,
                payload_size: chunk.len() as u32,
                num_fec_blocks: 0,
                num_xor_blocks_even: 0,
                num_xor_blocks_odd: 0,
                flags,
            };

            header.serialize(&mut self.packet_buf[..PAYLOAD_HEADER_SIZE])?;
            self.packet_buf[PAYLOAD_HEADER_SIZE..PAYLOAD_HEADER_SIZE + chunk.len()]
                .copy_from_slice(chunk);

            let datagram_len = PAYLOAD_HEADER_SIZE + chunk.len();

            // Apply token-bucket rate limiting pacing
            if let Some(wait) = self.pacer.pace(datagram_len) {
                std::thread::sleep(wait);
            }

            self.socket.send_to(&self.packet_buf[..datagram_len], self.target_addr)?;
            total_sent += datagram_len;
        }

        self.frame_seq = (self.frame_seq + 1) & pyromirror_proto::PAYLOAD_PACKET_SEQ_MASK;
        Ok(total_sent)
    }
}

/// Assembles incoming UDP datagrams into complete frames
pub struct FrameReceiver {
    frames: HashMap<u32, AssemblingFrame>,
    max_frame_bytes: usize,
}

struct AssemblingFrame {
    pts: u64,
    chunks: Vec<Option<Vec<u8>>>,
    received_bytes: usize,
    total_subpackets: Option<usize>,
    first_seen: Instant,
}

impl FrameReceiver {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self {
            frames: HashMap::new(),
            max_frame_bytes,
        }
    }

    /// Feeds a received UDP datagram into the frame assembler.
    /// Returns `Some((frame_bytes, pts))` if the frame is completely assembled.
    pub fn push_datagram(&mut self, data: &[u8]) -> Result<Option<(Vec<u8>, u64)>, NetError> {
        if data.len() < PAYLOAD_HEADER_SIZE {
            return Err(NetError::MalformedPacket);
        }

        let header = PayloadHeader::deserialize(&data[..PAYLOAD_HEADER_SIZE])?;
        let payload = &data[PAYLOAD_HEADER_SIZE..PAYLOAD_HEADER_SIZE + (header.payload_size as usize).min(data.len() - PAYLOAD_HEADER_SIZE)];

        let frame_seq = header.packet_seq();
        let subpacket_seq = header.subpacket_seq() as usize;

        // Clean up stale frames older than 1000ms (1 second) to allow full frames to reassemble even with pacing
        self.frames.retain(|_, f| f.first_seen.elapsed() < Duration::from_millis(1000));

        let frame = self.frames.entry(frame_seq).or_insert_with(|| AssemblingFrame {
            pts: header.pts,
            chunks: Vec::new(),
            received_bytes: 0,
            total_subpackets: None,
            first_seen: Instant::now(),
        });

        if header.is_packet_end() {
            frame.total_subpackets = Some(subpacket_seq + 1);
        }

        if subpacket_seq >= frame.chunks.len() {
            frame.chunks.resize(subpacket_seq + 1, None);
        }

        if frame.chunks[subpacket_seq].is_none() {
            frame.received_bytes += payload.len();
            if frame.received_bytes > self.max_frame_bytes {
                self.frames.remove(&frame_seq);
                return Err(NetError::BufferOverflow);
            }
            frame.chunks[subpacket_seq] = Some(payload.to_vec());
        }

        // Check if all chunks up to total_subpackets have arrived
        if let Some(total) = frame.total_subpackets {
            if frame.chunks.len() == total && frame.chunks.iter().all(|c| c.is_some()) {
                let mut assembled = Vec::with_capacity(frame.received_bytes);
                for chunk in frame.chunks.drain(..) {
                    if let Some(c) = chunk {
                        assembled.extend_from_slice(&c);
                    }
                }
                let pts = frame.pts;
                self.frames.remove(&frame_seq);
                return Ok(Some((assembled, pts)));
            }
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_packet_pacer() {
        let mut pacer = PacketPacer::new(200, 5); // 200 Mbps, 5ms burst
        assert_eq!(pacer.pace(1400), None);
    }

    #[test]
    fn test_frame_sender_receiver_roundtrip() {
        let sender_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let recv_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let target_addr = recv_sock.local_addr().unwrap();

        let mut sender = FrameSender::new(sender_sock, target_addr, 500, 1400);
        let mut receiver = FrameReceiver::new(2_000_000);

        let test_payload = vec![0x42u8; 3500]; // Multi-packet frame
        sender.send_frame(&test_payload, 99999, true).unwrap();

        let mut assembled_frame = None;
        let mut buf = [0u8; 2048];

        for _ in 0..10 {
            if let Ok((len, _)) = recv_sock.recv_from(&mut buf) {
                if let Ok(Some((data, pts))) = receiver.push_datagram(&buf[..len]) {
                    assembled_frame = Some((data, pts));
                    break;
                }
            }
        }

        assert!(assembled_frame.is_some());
        let (data, pts) = assembled_frame.unwrap();
        assert_eq!(pts, 99999);
        assert_eq!(data, test_payload);
    }
}
