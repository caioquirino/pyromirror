//! Raw FFI bindings to the PyroWave C API (`submodules/pyrowave/pyrowave.h`, API 0.6).
//!
//! The device, encoder and decoder entry points that work on CPU buffers are bound directly. The
//! external-memory (zero-copy GPU) entry points take Vulkan types; they are reached through the
//! small C helpers in `glue.c` (`pm_*`), which need none on this side.

#![allow(non_camel_case_types)]

use std::os::raw::{c_int, c_void};

pub const PYROWAVE_API_VERSION_MAJOR: u32 = 0;
pub const PYROWAVE_API_VERSION_MINOR: u32 = 6;

pub type pyrowave_result = c_int;
pub const PYROWAVE_SUCCESS: pyrowave_result = 0;
pub const PYROWAVE_TIMEOUT: pyrowave_result = 1;
pub const PYROWAVE_ERROR_GENERIC: pyrowave_result = -1;
pub const PYROWAVE_ERROR_INVALID_ARGUMENT: pyrowave_result = -2;
pub const PYROWAVE_ERROR_OUT_OF_HOST_MEMORY: pyrowave_result = -3;
pub const PYROWAVE_ERROR_OUT_OF_DEVICE_MEMORY: pyrowave_result = -4;
pub const PYROWAVE_ERROR_NO_VULKAN: pyrowave_result = -5;
pub const PYROWAVE_ERROR_NOT_IMPLEMENTED: pyrowave_result = -6;
pub const PYROWAVE_ERROR_UNSUPPORTED_EXTERNAL_HANDLE: pyrowave_result = -7;
pub const PYROWAVE_ERROR_FAILED_EXTERNAL_HANDLE: pyrowave_result = -8;

pub type pyrowave_chroma_subsampling = c_int;
pub const PYROWAVE_CHROMA_SUBSAMPLING_420: pyrowave_chroma_subsampling = 0;
pub const PYROWAVE_CHROMA_SUBSAMPLING_444: pyrowave_chroma_subsampling = 1;

pub type pyrowave_cpu_buffer_format = c_int;
/// 2 planes, encode only.
pub const PYROWAVE_CPU_BUFFER_FORMAT_NV12: pyrowave_cpu_buffer_format = 0;
pub const PYROWAVE_CPU_BUFFER_FORMAT_YUV420P: pyrowave_cpu_buffer_format = 1;
pub const PYROWAVE_CPU_BUFFER_FORMAT_YUV444P: pyrowave_cpu_buffer_format = 2;

#[repr(C)]
pub struct pyrowave_encoder_opaque {
    _private: [u8; 0],
}
#[repr(C)]
pub struct pyrowave_decoder_opaque {
    _private: [u8; 0],
}
#[repr(C)]
pub struct pyrowave_device_opaque {
    _private: [u8; 0],
}

pub type pyrowave_encoder = *mut pyrowave_encoder_opaque;
pub type pyrowave_decoder = *mut pyrowave_decoder_opaque;
pub type pyrowave_device = *mut pyrowave_device_opaque;

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct pyrowave_encoder_create_info {
    pub device: pyrowave_device,
    pub width: c_int,
    pub height: c_int,
    pub chroma: pyrowave_chroma_subsampling,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct pyrowave_decoder_create_info {
    pub device: pyrowave_device,
    pub width: c_int,
    pub height: c_int,
    pub chroma: pyrowave_chroma_subsampling,
    pub fragment_path: bool,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, Default)]
pub struct pyrowave_packet {
    pub offset: usize,
    pub size: usize,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct pyrowave_rate_control {
    pub maximum_bitstream_size: usize,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct pyrowave_cpu_buffer {
    pub data: [*mut c_void; 3],
    pub row_stride_in_bytes: [usize; 3],
    pub plane_size_in_bytes: [usize; 3],
    pub width: c_int,
    pub height: c_int,
    pub format: pyrowave_cpu_buffer_format,
}

/// An imported GPU texture, ready to encode from (see `glue.c`).
#[repr(C)]
pub struct pm_gpu_image {
    _private: [u8; 0],
}

/// `handle_kind`: the NT handle of a shared `ID3D11Texture2D`.
pub const PM_HANDLE_D3D11_TEXTURE: c_int = 0;
pub const PM_FORMAT_BGRA8: c_int = 0;
pub const PM_FORMAT_RGBA8: c_int = 1;
pub const PM_FORMAT_RGBA16F: c_int = 2;
pub const PM_FORMAT_R8: c_int = 3;

/// An imported fence of another graphics API (see `glue.c`).
#[repr(C)]
pub struct pm_gpu_fence {
    _private: [u8; 0],
}

extern "C" {
    pub fn pm_create_device_for_luid(luid: *const u8, device: *mut pyrowave_device) -> pyrowave_result;
    pub fn pm_gpu_image_import(
        device: pyrowave_device,
        handle: usize,
        handle_kind: c_int,
        width: u32,
        height: u32,
        format: c_int,
        writable: bool,
        out: *mut *mut pm_gpu_image,
    ) -> pyrowave_result;
    pub fn pm_dmabuf_modifiers(device: pyrowave_device, format: c_int, writable: bool, out: *mut u64, capacity: usize) -> usize;
    pub fn pm_device_drm_render_node(device: pyrowave_device, major: *mut i64, minor: *mut i64) -> bool;
    pub fn pm_gpu_fence_create(device: pyrowave_device, out: *mut *mut pm_gpu_fence) -> pyrowave_result;
    pub fn pm_gpu_image_import_dmabuf(
        device: pyrowave_device,
        fd: c_int,
        width: u32,
        height: u32,
        format: c_int,
        modifier: u64,
        planes: u32,
        offsets: *const u32,
        strides: *const u32,
        writable: bool,
        out: *mut *mut pm_gpu_image,
    ) -> pyrowave_result;
    pub fn pm_gpu_fence_import(device: pyrowave_device, handle: usize, out: *mut *mut pm_gpu_fence) -> pyrowave_result;
    pub fn pm_gpu_fence_destroy(fence: *mut pm_gpu_fence);
    pub fn pm_gpu_decode(
        decoder: pyrowave_decoder,
        planes: *const *mut pm_gpu_image,
        fence: *mut pm_gpu_fence,
        value: u64,
        timeout_ns: u64,
    ) -> pyrowave_result;
    pub fn pm_gpu_image_encode(
        encoder: pyrowave_encoder,
        image: *mut pm_gpu_image,
        exact_size: bool,
        maximum_bitstream_size: usize,
    ) -> pyrowave_result;
    pub fn pm_gpu_image_destroy(image: *mut pm_gpu_image);

    pub fn pyrowave_device_confirm_interop_support(device: pyrowave_device) -> bool;
}

extern "C" {
    pub fn pyrowave_get_api_version(major: *mut u32, minor: *mut u32, patch: *mut u32);

    pub fn pyrowave_create_default_device(device: *mut pyrowave_device) -> pyrowave_result;
    pub fn pyrowave_device_destroy(device: pyrowave_device);

    pub fn pyrowave_encoder_create(
        info: *const pyrowave_encoder_create_info,
        encoder: *mut pyrowave_encoder,
    ) -> pyrowave_result;
    pub fn pyrowave_encoder_encode_cpu_synchronous(
        encoder: pyrowave_encoder,
        buffers: *const pyrowave_cpu_buffer,
        rate_control: *const pyrowave_rate_control,
    ) -> pyrowave_result;
    pub fn pyrowave_encoder_compute_num_packets(
        encoder: pyrowave_encoder,
        packet_boundary: usize,
        num_packets: *mut usize,
    ) -> pyrowave_result;
    pub fn pyrowave_encoder_packetize(
        encoder: pyrowave_encoder,
        packets: *mut pyrowave_packet,
        packet_boundary: usize,
        out_packets: *mut usize,
        bitstream: *mut c_void,
        size: usize,
    ) -> pyrowave_result;
    pub fn pyrowave_encoder_destroy(encoder: pyrowave_encoder);

    pub fn pyrowave_decoder_device_prefers_fragment_path(device: pyrowave_device) -> bool;
    pub fn pyrowave_decoder_create(
        info: *const pyrowave_decoder_create_info,
        decoder: *mut pyrowave_decoder,
    ) -> pyrowave_result;
    pub fn pyrowave_decoder_clear(decoder: pyrowave_decoder);
    pub fn pyrowave_decoder_push_packet(
        decoder: pyrowave_decoder,
        data: *const c_void,
        size: usize,
    ) -> pyrowave_result;
    pub fn pyrowave_decoder_decode_is_ready(decoder: pyrowave_decoder, allow_partial_frame: bool) -> bool;
    pub fn pyrowave_decoder_decode_cpu_buffer_synchronous(
        decoder: pyrowave_decoder,
        buffers: *const pyrowave_cpu_buffer,
    ) -> pyrowave_result;
    pub fn pyrowave_decoder_destroy(decoder: pyrowave_decoder);
}
