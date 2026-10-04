//! Safe wrapper around the PyroWave C API.
//!
//! Frames go through PyroWave's CPU-buffer entry points: packed 8-bit RGB is converted to planar
//! BT.709 full-range YCbCr on the CPU, uploaded, and encoded on the GPU. The decoder mirrors
//! that. This costs one colour-conversion pass and one upload/readback per frame; the zero-copy
//! external-memory path (`pyrowave_encoder_encode_gpu_scaled_synchronous`) is the next step.

mod color;

use std::ffi::c_void;
use std::sync::Arc;

use pyrowave_sys as sys;
use thiserror::Error;

pub use color::PixelFormat;

#[derive(Error, Debug)]
pub enum CodecError {
    #[error("no usable Vulkan 1.3 device (PyroWave error {0}); check that GPU drivers are installed")]
    NoDevice(i32),
    #[error("PyroWave {call} failed with error {code}")]
    Call { call: &'static str, code: i32 },
    #[error("invalid dimensions {width}x{height} for {chroma:?}")]
    InvalidDimensions { width: u32, height: u32, chroma: Chroma },
    #[error("buffer too small: need {required} bytes, got {provided}")]
    BufferTooSmall { required: usize, provided: usize },
    #[error("linked PyroWave library is API {major}.{minor}, bindings were written for {}.{}", sys::PYROWAVE_API_VERSION_MAJOR, sys::PYROWAVE_API_VERSION_MINOR)]
    ApiMismatch { major: u32, minor: u32 },
}

fn check(call: &'static str, code: sys::pyrowave_result) -> Result<(), CodecError> {
    if code == sys::PYROWAVE_SUCCESS {
        Ok(())
    } else {
        Err(CodecError::Call { call, code })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chroma {
    /// Half-resolution chroma. Width and height must be even.
    C420,
    /// Full-resolution chroma, keeps coloured text sharp.
    C444,
}

impl Chroma {
    fn raw(self) -> sys::pyrowave_chroma_subsampling {
        match self {
            Chroma::C420 => sys::PYROWAVE_CHROMA_SUBSAMPLING_420,
            Chroma::C444 => sys::PYROWAVE_CHROMA_SUBSAMPLING_444,
        }
    }

    fn cpu_format(self) -> sys::pyrowave_cpu_buffer_format {
        match self {
            Chroma::C420 => sys::PYROWAVE_CPU_BUFFER_FORMAT_YUV420P,
            Chroma::C444 => sys::PYROWAVE_CPU_BUFFER_FORMAT_YUV444P,
        }
    }
}

/// A Vulkan device owned by PyroWave. Encoders and decoders keep it alive.
pub struct Device {
    raw: sys::pyrowave_device,
}

// The device is only ever used through an encoder or decoder, which are `Send` but not `Sync`,
// and PyroWave serialises access to the underlying Vulkan queues itself.
unsafe impl Send for Device {}
unsafe impl Sync for Device {}

impl Device {
    fn check_api() -> Result<(), CodecError> {
        let (mut major, mut minor, mut patch) = (0u32, 0u32, 0u32);
        unsafe { sys::pyrowave_get_api_version(&mut major, &mut minor, &mut patch) };
        if major != sys::PYROWAVE_API_VERSION_MAJOR || minor != sys::PYROWAVE_API_VERSION_MINOR {
            return Err(CodecError::ApiMismatch { major, minor });
        }
        Ok(())
    }

    pub fn new() -> Result<Arc<Self>, CodecError> {
        Self::check_api()?;
        let mut raw = std::ptr::null_mut();
        let code = unsafe { sys::pyrowave_create_default_device(&mut raw) };
        if code != sys::PYROWAVE_SUCCESS || raw.is_null() {
            return Err(CodecError::NoDevice(code));
        }
        Ok(Arc::new(Self { raw }))
    }

    /// A device on the graphics adapter with this LUID (Windows). Textures can only be shared
    /// with whatever else runs on the same adapter, such as desktop capture.
    pub fn on_adapter(luid: [u8; 8]) -> Result<Arc<Self>, CodecError> {
        Self::check_api()?;
        let mut raw = std::ptr::null_mut();
        let code = unsafe { sys::pm_create_device_for_luid(luid.as_ptr(), &mut raw) };
        if code != sys::PYROWAVE_SUCCESS || raw.is_null() {
            return Err(CodecError::NoDevice(code));
        }
        Ok(Arc::new(Self { raw }))
    }

    /// Whether the driver can import textures from other graphics APIs at all.
    pub fn supports_texture_import(&self) -> bool {
        unsafe { sys::pyrowave_device_confirm_interop_support(self.raw) }
    }

    /// True on GPUs with weak compute support (most mobile chips).
    pub fn prefers_fragment_decode(&self) -> bool {
        unsafe { sys::pyrowave_decoder_device_prefers_fragment_path(self.raw) }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        unsafe { sys::pyrowave_device_destroy(self.raw) };
    }
}

/// Planar YCbCr storage matching one encoder/decoder configuration.
struct Planes {
    width: usize,
    height: usize,
    chroma: Chroma,
    data: [Vec<u8>; 3],
}

impl Planes {
    fn new(width: u32, height: u32, chroma: Chroma) -> Result<Self, CodecError> {
        let odd = width % 2 != 0 || height % 2 != 0;
        if width == 0 || height == 0 || (chroma == Chroma::C420 && odd) {
            return Err(CodecError::InvalidDimensions { width, height, chroma });
        }
        let (w, h) = (width as usize, height as usize);
        let (cw, ch) = match chroma {
            Chroma::C420 => (w / 2, h / 2),
            Chroma::C444 => (w, h),
        };
        Ok(Self {
            width: w,
            height: h,
            chroma,
            data: [vec![0; w * h], vec![128; cw * ch], vec![128; cw * ch]],
        })
    }

    fn chroma_width(&self) -> usize {
        match self.chroma {
            Chroma::C420 => self.width / 2,
            Chroma::C444 => self.width,
        }
    }

    fn cpu_buffer(&mut self) -> sys::pyrowave_cpu_buffer {
        let cw = self.chroma_width();
        sys::pyrowave_cpu_buffer {
            data: [
                self.data[0].as_mut_ptr() as *mut c_void,
                self.data[1].as_mut_ptr() as *mut c_void,
                self.data[2].as_mut_ptr() as *mut c_void,
            ],
            row_stride_in_bytes: [self.width, cw, cw],
            plane_size_in_bytes: [self.data[0].len(), self.data[1].len(), self.data[2].len()],
            width: self.width as i32,
            height: self.height as i32,
            format: self.chroma.cpu_format(),
        }
    }
}

fn check_packed(width: usize, height: usize, stride: usize, len: usize) -> Result<(), CodecError> {
    let required = if height == 0 { 0 } else { stride * (height - 1) + width * 4 };
    if stride < width * 4 || len < required {
        return Err(CodecError::BufferTooSmall { required, provided: len });
    }
    Ok(())
}

pub struct Encoder {
    raw: sys::pyrowave_encoder,
    planes: Planes,
    bitstream: Vec<u8>,
    packets: Vec<sys::pyrowave_packet>,
    num_packets: usize,
    /// A texture owned by another graphics API, to encode from without copying.
    texture: Option<Texture>,
    /// Where the most recent frame came from, for `encode_last`.
    last_from_texture: bool,
    device: Arc<Device>,
}

struct Texture {
    raw: *mut sys::pm_gpu_image,
    /// Same size as the encoder, so no scaling is needed.
    exact_size: bool,
}

impl Drop for Texture {
    fn drop(&mut self) {
        unsafe { sys::pm_gpu_image_destroy(self.raw) };
    }
}

/// What kind of OS handle `Encoder::import_texture` is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextureHandle {
    /// The NT handle of a shared `ID3D11Texture2D` (Windows).
    D3d11(usize),
}

unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new(device: Arc<Device>, width: u32, height: u32, chroma: Chroma) -> Result<Self, CodecError> {
        let planes = Planes::new(width, height, chroma)?;
        let info = sys::pyrowave_encoder_create_info {
            device: device.raw,
            width: width as i32,
            height: height as i32,
            chroma: chroma.raw(),
        };
        let mut raw = std::ptr::null_mut();
        check("encoder_create", unsafe { sys::pyrowave_encoder_create(&info, &mut raw) })?;
        Ok(Self {
            raw,
            planes,
            bitstream: Vec::new(),
            packets: Vec::new(),
            num_packets: 0,
            texture: None,
            last_from_texture: false,
            device,
        })
    }

    /// Takes a texture owned by another graphics API as the source for `encode_texture`,
    /// replacing any earlier one. It may be larger than the encoder; it is scaled down on the
    /// GPU. The handle is consumed, whether this succeeds or not.
    pub fn import_texture(&mut self, handle: TextureHandle, width: u32, height: u32, format: PixelFormat) -> Result<(), CodecError> {
        self.texture = None;
        let TextureHandle::D3d11(handle) = handle;
        let format = match format {
            PixelFormat::Bgrx => sys::PM_FORMAT_BGRA8,
            PixelFormat::Rgbx => sys::PM_FORMAT_RGBA8,
        };
        let mut raw = std::ptr::null_mut();
        check("image_create", unsafe {
            sys::pm_gpu_image_import(self.device.raw, handle, sys::PM_HANDLE_D3D11_TEXTURE, width, height, format, &mut raw)
        })?;
        let exact_size = width as usize == self.planes.width && height as usize == self.planes.height;
        self.texture = Some(Texture { raw, exact_size });
        Ok(())
    }

    pub fn has_texture(&self) -> bool {
        self.texture.is_some()
    }

    /// Forgets the imported texture.
    pub fn drop_texture(&mut self) {
        self.texture = None;
        self.last_from_texture = false;
    }

    /// Encodes what the imported texture holds right now. Its owner must have finished writing
    /// it, and must leave it alone until this returns. Scaling and colour conversion happen on
    /// the GPU; no pixels pass through memory.
    pub fn encode_texture(&mut self, max_frame_bytes: usize, packet_boundary: usize) -> Result<Packets<'_>, CodecError> {
        let Some(texture) = &self.texture else {
            return Err(CodecError::Call { call: "encode_texture (no texture imported)", code: sys::PYROWAVE_ERROR_INVALID_ARGUMENT });
        };
        let max_frame_bytes = Self::frame_budget(max_frame_bytes);
        check("encoder_encode_gpu_scaled_synchronous", unsafe {
            sys::pm_gpu_image_encode(self.raw, texture.raw, texture.exact_size, max_frame_bytes)
        })?;
        self.last_from_texture = true;
        self.packetize(max_frame_bytes, packet_boundary)
    }

    /// The packets of the frame encoded last.
    pub fn last_packets(&self) -> Packets<'_> {
        Packets { bitstream: &self.bitstream, packets: &self.packets[..self.num_packets] }
    }

    /// PyroWave rounds the target down to a multiple of 4 and needs some room to work with.
    fn frame_budget(max_frame_bytes: usize) -> usize {
        max_frame_bytes.max(16 * 1024) & !3
    }

    pub fn width(&self) -> u32 {
        self.planes.width as u32
    }

    pub fn height(&self) -> u32 {
        self.planes.height as u32
    }

    /// Encodes one packed 32-bit frame.
    ///
    /// `max_frame_bytes` is a hard cap on the encoded frame (PyroWave's rate control), and
    /// `packet_boundary` the largest packet to produce. Every packet is independently decodable,
    /// so each one can go into its own datagram.
    pub fn encode(
        &mut self,
        pixels: &[u8],
        stride: usize,
        format: PixelFormat,
        max_frame_bytes: usize,
        packet_boundary: usize,
    ) -> Result<Packets<'_>, CodecError> {
        check_packed(self.planes.width, self.planes.height, stride, pixels.len())?;
        let (w, h, chroma) = (self.planes.width, self.planes.height, self.planes.chroma);
        let [y, cb, cr] = &mut self.planes.data;
        color::packed_to_planar(pixels, stride, format, w, h, chroma, y, cb, cr);
        self.last_from_texture = false;
        self.encode_planes(max_frame_bytes, packet_boundary)
    }

    /// Encodes the previously submitted frame again, e.g. to refresh a static desktop so that a
    /// client which lost packets converges to a clean image.
    pub fn encode_last(&mut self, max_frame_bytes: usize, packet_boundary: usize) -> Result<Packets<'_>, CodecError> {
        if self.last_from_texture && self.texture.is_some() {
            // The texture still holds that frame.
            return self.encode_texture(max_frame_bytes, packet_boundary);
        }
        self.encode_planes(max_frame_bytes, packet_boundary)
    }

    fn encode_planes(&mut self, max_frame_bytes: usize, packet_boundary: usize) -> Result<Packets<'_>, CodecError> {
        let max_frame_bytes = Self::frame_budget(max_frame_bytes);
        let buffer = self.planes.cpu_buffer();
        let rate_control = sys::pyrowave_rate_control { maximum_bitstream_size: max_frame_bytes };
        check("encoder_encode_cpu_synchronous", unsafe {
            sys::pyrowave_encoder_encode_cpu_synchronous(self.raw, &buffer, &rate_control)
        })?;
        self.packetize(max_frame_bytes, packet_boundary)
    }

    /// Collects the frame just encoded into packets; waits for the GPU to finish it.
    fn packetize(&mut self, max_frame_bytes: usize, packet_boundary: usize) -> Result<Packets<'_>, CodecError> {
        let mut num_packets = 0usize;
        check("encoder_compute_num_packets", unsafe {
            sys::pyrowave_encoder_compute_num_packets(self.raw, packet_boundary, &mut num_packets)
        })?;
        if self.packets.len() < num_packets {
            self.packets.resize(num_packets, sys::pyrowave_packet::default());
        }
        // Packetizing adds a small header per packet on top of the rate-controlled payload.
        let capacity = max_frame_bytes + num_packets * 64 + 4096;
        if self.bitstream.len() < capacity {
            self.bitstream.resize(capacity, 0);
        }

        let mut out_packets = 0usize;
        check("encoder_packetize", unsafe {
            sys::pyrowave_encoder_packetize(
                self.raw,
                self.packets.as_mut_ptr(),
                packet_boundary,
                &mut out_packets,
                self.bitstream.as_mut_ptr() as *mut c_void,
                self.bitstream.len(),
            )
        })?;
        self.num_packets = out_packets.min(self.packets.len());

        Ok(Packets {
            bitstream: &self.bitstream,
            packets: &self.packets[..self.num_packets],
        })
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // The texture goes first: it belongs to the same device.
        self.texture = None;
        unsafe { sys::pyrowave_encoder_destroy(self.raw) };
    }
}

/// The packets of one encoded frame.
#[derive(Clone, Copy)]
pub struct Packets<'a> {
    bitstream: &'a [u8],
    packets: &'a [sys::pyrowave_packet],
}

impl<'a> Packets<'a> {
    pub fn len(&self) -> usize {
        self.packets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.packets.is_empty()
    }

    pub fn total_bytes(&self) -> usize {
        self.packets.iter().map(|p| p.size).sum()
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = &'a [u8]> + '_ {
        let bitstream = self.bitstream;
        self.packets.iter().map(move |p| &bitstream[p.offset..p.offset + p.size])
    }
}

pub struct Decoder {
    raw: sys::pyrowave_decoder,
    planes: Planes,
    _device: Arc<Device>,
}

unsafe impl Send for Decoder {}

impl Decoder {
    /// `fragment_path` selects the render-pass based iDWT meant for mobile GPUs; pass
    /// `device.prefers_fragment_decode()` unless overriding.
    pub fn new(
        device: Arc<Device>,
        width: u32,
        height: u32,
        chroma: Chroma,
        fragment_path: bool,
    ) -> Result<Self, CodecError> {
        let planes = Planes::new(width, height, chroma)?;
        let info = sys::pyrowave_decoder_create_info {
            device: device.raw,
            width: width as i32,
            height: height as i32,
            chroma: chroma.raw(),
            fragment_path,
        };
        let mut raw = std::ptr::null_mut();
        check("decoder_create", unsafe { sys::pyrowave_decoder_create(&info, &mut raw) })?;
        Ok(Self { raw, planes, _device: device })
    }

    /// Drops all queued packets.
    pub fn clear(&mut self) {
        unsafe { sys::pyrowave_decoder_clear(self.raw) };
    }

    /// Queues one packet. Returns false if PyroWave rejected it (corrupt or stale).
    pub fn push_packet(&mut self, packet: &[u8]) -> bool {
        let code = unsafe {
            sys::pyrowave_decoder_push_packet(self.raw, packet.as_ptr() as *const c_void, packet.len())
        };
        code == sys::PYROWAVE_SUCCESS
    }

    /// Whether the queued packets form a decodable frame. With `allow_partial`, a frame with
    /// missing packets counts too; the missing wavelet blocks decode as blur.
    pub fn is_ready(&self, allow_partial: bool) -> bool {
        unsafe { sys::pyrowave_decoder_decode_is_ready(self.raw, allow_partial) }
    }

    /// Decodes the queued packets into a packed 32-bit buffer of at least `stride * height` bytes.
    pub fn decode(&mut self, out: &mut [u8], stride: usize, format: PixelFormat) -> Result<(), CodecError> {
        check_packed(self.planes.width, self.planes.height, stride, out.len())?;
        let buffer = self.planes.cpu_buffer();
        check("decoder_decode_cpu_buffer_synchronous", unsafe {
            sys::pyrowave_decoder_decode_cpu_buffer_synchronous(self.raw, &buffer)
        })?;
        let (w, h, chroma) = (self.planes.width, self.planes.height, self.planes.chroma);
        let [y, cb, cr] = &self.planes.data;
        color::planar_to_packed(y, cb, cr, w, h, chroma, out, stride, format);
        Ok(())
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { sys::pyrowave_decoder_destroy(self.raw) };
    }
}
