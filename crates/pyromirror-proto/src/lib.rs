//! PyroMirror Wire Protocol
//!
//! Binary network protocol designed for ultra-low latency remote desktop streaming.
//! All multi-byte integers are serialized in little-endian byte order.

pub mod auth;

use byteorder::{ByteOrder, LittleEndian};
use thiserror::Error;

/// Magic mask and version tags for message identification
pub const PYRO_VERSION_MASK: u32 = 0xaa02 << 16;
pub const PYRO_MESSAGE_MAGIC_MASK: u32 = !0u32 << 14;

pub const MSG_TYPE_CLIENT_HELLO: u32 = 1;
pub const MSG_TYPE_CODEC_PARAMS: u32 = 7;
pub const MSG_TYPE_INPUT_EVENT: u32 = 9;

#[inline]
pub const fn make_message_type(t: u32, size: u32) -> u32 {
    (((b'P' as u32) << 26)
        | ((b'Y' as u32) << 20)
        | ((b'R' as u32) << 14)
        | t
        | (size << 6))
        ^ PYRO_VERSION_MASK
}

#[inline]
pub fn validate_magic(v: u32) -> bool {
    make_message_type(0, 0) == (v & PYRO_MESSAGE_MAGIC_MASK)
}

#[inline]
pub fn message_get_type(v: u32) -> u32 {
    (v ^ PYRO_VERSION_MASK) & 0x3f
}

#[inline]
pub fn message_get_length(v: u32) -> usize {
    (((v ^ PYRO_VERSION_MASK) >> 6) & 0xff) as usize
}

/// Largest payload a control message can carry (the length field is 8 bits).
pub const MAX_MESSAGE_PAYLOAD: usize = 255;

/// Writes one framed control message (4-byte header followed by the payload).
pub fn write_message<W: std::io::Write>(w: &mut W, msg_type: u32, payload: &[u8]) -> std::io::Result<()> {
    debug_assert!(payload.len() <= MAX_MESSAGE_PAYLOAD);
    let mut msg = [0u8; 4 + MAX_MESSAGE_PAYLOAD];
    LittleEndian::write_u32(&mut msg[0..4], make_message_type(msg_type, payload.len() as u32));
    msg[4..4 + payload.len()].copy_from_slice(payload);
    w.write_all(&msg[..4 + payload.len()])
}

/// Reads one framed control message into `payload`, returning its type and length.
pub fn read_message<R: std::io::Read>(
    r: &mut R,
    payload: &mut [u8; MAX_MESSAGE_PAYLOAD],
) -> std::io::Result<(u32, usize)> {
    let mut header = [0u8; 4];
    r.read_exact(&mut header)?;
    let magic = LittleEndian::read_u32(&header);
    if !validate_magic(magic) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            ProtoError::InvalidMagic(magic),
        ));
    }
    let len = message_get_length(magic);
    r.read_exact(&mut payload[..len])?;
    Ok((message_get_type(magic), len))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientHello {
    pub udp_port: u16,
    pub flags: u16,
}

impl ClientHello {
    pub const SIZE: usize = 4;

    pub fn serialize(&self, buf: &mut [u8]) -> Result<(), ProtoError> {
        if buf.len() < Self::SIZE {
            return Err(ProtoError::BufferTooSmall {
                required: Self::SIZE,
                provided: buf.len(),
            });
        }
        LittleEndian::write_u16(&mut buf[0..2], self.udp_port);
        LittleEndian::write_u16(&mut buf[2..4], self.flags);
        Ok(())
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < Self::SIZE {
            return Err(ProtoError::BufferTooSmall {
                required: Self::SIZE,
                provided: buf.len(),
            });
        }
        Ok(Self {
            udp_port: LittleEndian::read_u16(&buf[0..2]),
            flags: LittleEndian::read_u16(&buf[2..4]),
        })
    }
}


#[derive(Error, Debug, PartialEq, Eq)]
pub enum ProtoError {
    #[error("Buffer too small: required {required}, provided {provided}")]
    BufferTooSmall { required: usize, provided: usize },
    #[error("Invalid magic number: {0:#x}")]
    InvalidMagic(u32),
    #[error("Unknown or invalid message type: {0:#x}")]
    UnknownMessageType(u32),
    #[error("Payload length mismatch: expected {expected}, actual {actual}")]
    LengthMismatch { expected: usize, actual: usize },
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecType {
    None = 0,
    H264 = 1,
    H265 = 2,
    AV1 = 3,
    PyroWave = 4,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodecType {
    None = 0,
    Opus = 1,
    Aac = 2,
    RawS16LE = 3,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoColorProfile {
    Bt709LimitedLeftChroma420 = 0,
    Bt709FullCenterChroma420 = 1,
    Bt2020NclPqLimitedLeftChroma420 = 2,
    Bt2020NclPqFullCenterChroma420 = 3,
    Bt709FullChroma444 = 4,
    Bt2020NclPqFullChroma444 = 5,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecParameters {
    pub video_codec: VideoCodecType,
    pub video_color_profile: VideoColorProfile,
    pub audio_codec: AudioCodecType,
    pub frame_rate_num: u16,
    pub frame_rate_den: u16,
    pub width: u16,
    pub height: u16,
    pub audio_channels: u32,
    pub audio_sample_rate: u32,
}

impl CodecParameters {
    pub const SIZE: usize = 28;

    pub fn serialize(&self, buf: &mut [u8]) -> Result<(), ProtoError> {
        if buf.len() < Self::SIZE {
            return Err(ProtoError::BufferTooSmall {
                required: Self::SIZE,
                provided: buf.len(),
            });
        }
        LittleEndian::write_u32(&mut buf[0..4], self.video_codec as u32);
        LittleEndian::write_u32(&mut buf[4..8], self.video_color_profile as u32);
        LittleEndian::write_u32(&mut buf[8..12], self.audio_codec as u32);
        LittleEndian::write_u16(&mut buf[12..14], self.frame_rate_num);
        LittleEndian::write_u16(&mut buf[14..16], self.frame_rate_den);
        LittleEndian::write_u16(&mut buf[16..18], self.width);
        LittleEndian::write_u16(&mut buf[18..20], self.height);
        LittleEndian::write_u32(&mut buf[20..24], self.audio_channels);
        LittleEndian::write_u32(&mut buf[24..28], self.audio_sample_rate);
        Ok(())
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < Self::SIZE {
            return Err(ProtoError::BufferTooSmall {
                required: Self::SIZE,
                provided: buf.len(),
            });
        }
        let v_codec = match LittleEndian::read_u32(&buf[0..4]) {
            0 => VideoCodecType::None,
            1 => VideoCodecType::H264,
            2 => VideoCodecType::H265,
            3 => VideoCodecType::AV1,
            4 => VideoCodecType::PyroWave,
            _ => VideoCodecType::None,
        };
        let color_profile = match LittleEndian::read_u32(&buf[4..8]) {
            0 => VideoColorProfile::Bt709LimitedLeftChroma420,
            1 => VideoColorProfile::Bt709FullCenterChroma420,
            2 => VideoColorProfile::Bt2020NclPqLimitedLeftChroma420,
            3 => VideoColorProfile::Bt2020NclPqFullCenterChroma420,
            4 => VideoColorProfile::Bt709FullChroma444,
            5 => VideoColorProfile::Bt2020NclPqFullChroma444,
            _ => VideoColorProfile::Bt709FullCenterChroma420,
        };
        let a_codec = match LittleEndian::read_u32(&buf[8..12]) {
            0 => AudioCodecType::None,
            1 => AudioCodecType::Opus,
            2 => AudioCodecType::Aac,
            3 => AudioCodecType::RawS16LE,
            _ => AudioCodecType::None,
        };
        Ok(Self {
            video_codec: v_codec,
            video_color_profile: color_profile,
            audio_codec: a_codec,
            frame_rate_num: LittleEndian::read_u16(&buf[12..14]),
            frame_rate_den: LittleEndian::read_u16(&buf[14..16]),
            width: LittleEndian::read_u16(&buf[16..18]),
            height: LittleEndian::read_u16(&buf[18..20]),
            audio_channels: LittleEndian::read_u32(&buf[20..24]),
            audio_sample_rate: LittleEndian::read_u32(&buf[24..28]),
        })
    }
}

/// Input Events (Mouse, Keyboard, Gamepad)
#[derive(Debug, Clone, PartialEq)]
pub enum InputEvent {
    MouseMoveAbsolute {
        x: u16,
        y: u16,
    },
    MouseMoveRelative {
        dx: i16,
        dy: i16,
    },
    MouseButton {
        button: u8,
        down: bool,
    },
    MouseWheel {
        dx: i16,
        dy: i16,
    },
    KeyboardKey {
        scancode: u16,
        down: bool,
        modifiers: u16,
    },
    Gamepad {
        seq: u16,
        buttons: u16,
        axis_lx: i16,
        axis_ly: i16,
        axis_rx: i16,
        axis_ry: i16,
        trigger_l: u8,
        trigger_r: u8,
        hat_x: i8,
        hat_y: i8,
    },
}

impl InputEvent {
    pub fn serialize(&self, buf: &mut [u8]) -> Result<usize, ProtoError> {
        match self {
            InputEvent::MouseMoveAbsolute { x, y } => {
                if buf.len() < 5 {
                    return Err(ProtoError::BufferTooSmall { required: 5, provided: buf.len() });
                }
                buf[0] = 1; // type
                LittleEndian::write_u16(&mut buf[1..3], *x);
                LittleEndian::write_u16(&mut buf[3..5], *y);
                Ok(5)
            }
            InputEvent::MouseMoveRelative { dx, dy } => {
                if buf.len() < 5 {
                    return Err(ProtoError::BufferTooSmall { required: 5, provided: buf.len() });
                }
                buf[0] = 2; // type
                LittleEndian::write_i16(&mut buf[1..3], *dx);
                LittleEndian::write_i16(&mut buf[3..5], *dy);
                Ok(5)
            }
            InputEvent::MouseButton { button, down } => {
                if buf.len() < 3 {
                    return Err(ProtoError::BufferTooSmall { required: 3, provided: buf.len() });
                }
                buf[0] = 3; // type
                buf[1] = *button;
                buf[2] = if *down { 1 } else { 0 };
                Ok(3)
            }
            InputEvent::MouseWheel { dx, dy } => {
                if buf.len() < 5 {
                    return Err(ProtoError::BufferTooSmall { required: 5, provided: buf.len() });
                }
                buf[0] = 4; // type
                LittleEndian::write_i16(&mut buf[1..3], *dx);
                LittleEndian::write_i16(&mut buf[3..5], *dy);
                Ok(5)
            }
            InputEvent::KeyboardKey { scancode, down, modifiers } => {
                if buf.len() < 6 {
                    return Err(ProtoError::BufferTooSmall { required: 6, provided: buf.len() });
                }
                buf[0] = 5; // type
                LittleEndian::write_u16(&mut buf[1..3], *scancode);
                buf[3] = if *down { 1 } else { 0 };
                LittleEndian::write_u16(&mut buf[4..6], *modifiers);
                Ok(6)
            }
            InputEvent::Gamepad {
                seq,
                buttons,
                axis_lx,
                axis_ly,
                axis_rx,
                axis_ry,
                trigger_l,
                trigger_r,
                hat_x,
                hat_y,
            } => {
                if buf.len() < 19 {
                    return Err(ProtoError::BufferTooSmall { required: 19, provided: buf.len() });
                }
                buf[0] = 6; // type
                LittleEndian::write_u16(&mut buf[1..3], *seq);
                LittleEndian::write_u16(&mut buf[3..5], *buttons);
                LittleEndian::write_i16(&mut buf[5..7], *axis_lx);
                LittleEndian::write_i16(&mut buf[7..9], *axis_ly);
                LittleEndian::write_i16(&mut buf[9..11], *axis_rx);
                LittleEndian::write_i16(&mut buf[11..13], *axis_ry);
                buf[13] = *trigger_l;
                buf[14] = *trigger_r;
                buf[15] = *hat_x as u8;
                buf[16] = *hat_y as u8;
                Ok(17)
            }
        }
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.is_empty() {
            return Err(ProtoError::BufferTooSmall { required: 1, provided: 0 });
        }
        match buf[0] {
            1 => {
                if buf.len() < 5 {
                    return Err(ProtoError::BufferTooSmall { required: 5, provided: buf.len() });
                }
                Ok(InputEvent::MouseMoveAbsolute {
                    x: LittleEndian::read_u16(&buf[1..3]),
                    y: LittleEndian::read_u16(&buf[3..5]),
                })
            }
            2 => {
                if buf.len() < 5 {
                    return Err(ProtoError::BufferTooSmall { required: 5, provided: buf.len() });
                }
                Ok(InputEvent::MouseMoveRelative {
                    dx: LittleEndian::read_i16(&buf[1..3]),
                    dy: LittleEndian::read_i16(&buf[3..5]),
                })
            }
            3 => {
                if buf.len() < 3 {
                    return Err(ProtoError::BufferTooSmall { required: 3, provided: buf.len() });
                }
                Ok(InputEvent::MouseButton {
                    button: buf[1],
                    down: buf[2] != 0,
                })
            }
            4 => {
                if buf.len() < 5 {
                    return Err(ProtoError::BufferTooSmall { required: 5, provided: buf.len() });
                }
                Ok(InputEvent::MouseWheel {
                    dx: LittleEndian::read_i16(&buf[1..3]),
                    dy: LittleEndian::read_i16(&buf[3..5]),
                })
            }
            5 => {
                if buf.len() < 6 {
                    return Err(ProtoError::BufferTooSmall { required: 6, provided: buf.len() });
                }
                Ok(InputEvent::KeyboardKey {
                    scancode: LittleEndian::read_u16(&buf[1..3]),
                    down: buf[3] != 0,
                    modifiers: LittleEndian::read_u16(&buf[4..6]),
                })
            }
            6 => {
                if buf.len() < 17 {
                    return Err(ProtoError::BufferTooSmall { required: 17, provided: buf.len() });
                }
                Ok(InputEvent::Gamepad {
                    seq: LittleEndian::read_u16(&buf[1..3]),
                    buttons: LittleEndian::read_u16(&buf[3..5]),
                    axis_lx: LittleEndian::read_i16(&buf[5..7]),
                    axis_ly: LittleEndian::read_i16(&buf[7..9]),
                    axis_rx: LittleEndian::read_i16(&buf[9..11]),
                    axis_ry: LittleEndian::read_i16(&buf[11..13]),
                    trigger_l: buf[13],
                    trigger_r: buf[14],
                    hat_x: buf[15] as i8,
                    hat_y: buf[16] as i8,
                })
            }
            _ => Err(ProtoError::UnknownMessageType(buf[0] as u32)),
        }
    }
}

/// Datagram Payload Header (UDP packet prefix)
pub const PAYLOAD_HEADER_SIZE: usize = 24;

pub const PAYLOAD_KEY_FRAME_BIT: u32 = 1 << 0;
pub const PAYLOAD_STREAM_TYPE_BIT: u32 = 1 << 1; // 0 = Video, 1 = Audio
pub const PAYLOAD_PACKET_FEC_BIT: u32 = 1 << 2;
pub const PAYLOAD_PACKET_BEGIN_BIT: u32 = 1 << 3;
pub const PAYLOAD_PACKET_END_BIT: u32 = 1 << 4;

pub const PAYLOAD_PACKET_SEQ_OFFSET: u32 = 5;
pub const PAYLOAD_PACKET_SEQ_BITS: u32 = 13;
pub const PAYLOAD_SUBPACKET_SEQ_OFFSET: u32 = 18;
pub const PAYLOAD_SUBPACKET_SEQ_BITS: u32 = 14;

pub const PAYLOAD_PACKET_SEQ_MASK: u32 = (1 << PAYLOAD_PACKET_SEQ_BITS) - 1;
pub const PAYLOAD_SUBPACKET_SEQ_MASK: u32 = (1 << PAYLOAD_SUBPACKET_SEQ_BITS) - 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadHeader {
    pub pts: u64,
    pub dts_delta: u32,
    pub payload_size: u32,
    pub num_fec_blocks: u16,
    pub num_xor_blocks_even: u8,
    pub num_xor_blocks_odd: u8,
    pub flags: u32,
}

impl PayloadHeader {
    #[inline]
    pub fn packet_seq(&self) -> u32 {
        (self.flags >> PAYLOAD_PACKET_SEQ_OFFSET) & PAYLOAD_PACKET_SEQ_MASK
    }

    #[inline]
    pub fn subpacket_seq(&self) -> u32 {
        (self.flags >> PAYLOAD_SUBPACKET_SEQ_OFFSET) & PAYLOAD_SUBPACKET_SEQ_MASK
    }

    #[inline]
    pub fn is_key_frame(&self) -> bool {
        (self.flags & PAYLOAD_KEY_FRAME_BIT) != 0
    }

    #[inline]
    pub fn is_audio(&self) -> bool {
        (self.flags & PAYLOAD_STREAM_TYPE_BIT) != 0
    }

    #[inline]
    pub fn is_packet_begin(&self) -> bool {
        (self.flags & PAYLOAD_PACKET_BEGIN_BIT) != 0
    }

    #[inline]
    pub fn is_packet_end(&self) -> bool {
        (self.flags & PAYLOAD_PACKET_END_BIT) != 0
    }

    #[inline]
    pub fn is_fec(&self) -> bool {
        (self.flags & PAYLOAD_PACKET_FEC_BIT) != 0
    }

    pub fn serialize(&self, buf: &mut [u8]) -> Result<(), ProtoError> {
        if buf.len() < PAYLOAD_HEADER_SIZE {
            return Err(ProtoError::BufferTooSmall {
                required: PAYLOAD_HEADER_SIZE,
                provided: buf.len(),
            });
        }
        LittleEndian::write_u64(&mut buf[0..8], self.pts);
        LittleEndian::write_u32(&mut buf[8..12], self.dts_delta);
        LittleEndian::write_u32(&mut buf[12..16], self.payload_size);
        LittleEndian::write_u16(&mut buf[16..18], self.num_fec_blocks);
        buf[18] = self.num_xor_blocks_even;
        buf[19] = self.num_xor_blocks_odd;
        LittleEndian::write_u32(&mut buf[20..24], self.flags);
        Ok(())
    }

    pub fn deserialize(buf: &[u8]) -> Result<Self, ProtoError> {
        if buf.len() < PAYLOAD_HEADER_SIZE {
            return Err(ProtoError::BufferTooSmall {
                required: PAYLOAD_HEADER_SIZE,
                provided: buf.len(),
            });
        }
        Ok(Self {
            pts: LittleEndian::read_u64(&buf[0..8]),
            dts_delta: LittleEndian::read_u32(&buf[8..12]),
            payload_size: LittleEndian::read_u32(&buf[12..16]),
            num_fec_blocks: LittleEndian::read_u16(&buf[16..18]),
            num_xor_blocks_even: buf[18],
            num_xor_blocks_odd: buf[19],
            flags: LittleEndian::read_u32(&buf[20..24]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codec_params_roundtrip() {
        let params = CodecParameters {
            video_codec: VideoCodecType::PyroWave,
            video_color_profile: VideoColorProfile::Bt709FullChroma444,
            audio_codec: AudioCodecType::Opus,
            frame_rate_num: 60,
            frame_rate_den: 1,
            width: 1920,
            height: 1080,
            audio_channels: 2,
            audio_sample_rate: 48000,
        };

        let mut buf = [0u8; CodecParameters::SIZE];
        params.serialize(&mut buf).unwrap();

        let decoded = CodecParameters::deserialize(&buf).unwrap();
        assert_eq!(params, decoded);
    }

    #[test]
    fn test_payload_header_roundtrip() {
        let flags = PAYLOAD_KEY_FRAME_BIT
            | PAYLOAD_PACKET_BEGIN_BIT
            | (42 << PAYLOAD_PACKET_SEQ_OFFSET)
            | (5 << PAYLOAD_SUBPACKET_SEQ_OFFSET);

        let header = PayloadHeader {
            pts: 1234567890123,
            dts_delta: 16666,
            payload_size: 1400,
            num_fec_blocks: 0,
            num_xor_blocks_even: 0,
            num_xor_blocks_odd: 0,
            flags,
        };

        let mut buf = [0u8; PAYLOAD_HEADER_SIZE];
        header.serialize(&mut buf).unwrap();

        let decoded = PayloadHeader::deserialize(&buf).unwrap();
        assert_eq!(header, decoded);
        assert!(decoded.is_key_frame());
        assert!(decoded.is_packet_begin());
        assert!(!decoded.is_audio());
        assert_eq!(decoded.packet_seq(), 42);
        assert_eq!(decoded.subpacket_seq(), 5);
    }

    #[test]
    fn test_input_events_roundtrip() {
        let event = InputEvent::MouseMoveRelative { dx: -15, dy: 30 };
        let mut buf = [0u8; 32];
        let len = event.serialize(&mut buf).unwrap();
        let decoded = InputEvent::deserialize(&buf[..len]).unwrap();
        assert_eq!(event, decoded);

        let key_event = InputEvent::KeyboardKey {
            scancode: 42,
            down: true,
            modifiers: 0x01,
        };
        let len = key_event.serialize(&mut buf).unwrap();
        let decoded_key = InputEvent::deserialize(&buf[..len]).unwrap();
        assert_eq!(key_event, decoded_key);
    }
}
