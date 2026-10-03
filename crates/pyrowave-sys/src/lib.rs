//! Pyrowave FFI bindings

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::os::raw::{c_int, c_void};

pub type pyrowave_result = i32;
pub const PYROWAVE_SUCCESS: pyrowave_result = 0;
pub const PYROWAVE_TIMEOUT: pyrowave_result = 1;
pub const PYROWAVE_ERROR_GENERIC: pyrowave_result = -1;
pub const PYROWAVE_ERROR_INVALID_ARGUMENT: pyrowave_result = -2;

pub type pyrowave_chroma_subsampling = u32;
pub const PYROWAVE_CHROMA_SUBSAMPLING_420: pyrowave_chroma_subsampling = 0;
pub const PYROWAVE_CHROMA_SUBSAMPLING_444: pyrowave_chroma_subsampling = 1;

pub type pyrowave_encoder = *mut c_void;
pub type pyrowave_decoder = *mut c_void;
pub type pyrowave_device = *mut c_void;
pub type pyrowave_sync_object = *mut c_void;
pub type pyrowave_image = *mut c_void;
pub type pyrowave_os_handle = usize;

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
#[derive(Debug, Copy, Clone)]
pub struct pyrowave_packet {
    pub offset: usize,
    pub size: usize,
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct pyrowave_rate_control {
    pub maximum_bitstream_size: usize,
}

extern "C" {
    pub fn pyrowave_get_api_version(major: *mut u32, minor: *mut u32, patch: *mut u32);
    pub fn pyrowave_create_default_device(device: *mut pyrowave_device) -> pyrowave_result;
    pub fn pyrowave_device_destroy(device: pyrowave_device);

    pub fn pyrowave_encoder_create(
        info: *const pyrowave_encoder_create_info,
        encoder: *mut pyrowave_encoder,
    ) -> pyrowave_result;
    pub fn pyrowave_encoder_destroy(encoder: pyrowave_encoder);

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

    pub fn pyrowave_decoder_create(
        info: *const pyrowave_decoder_create_info,
        decoder: *mut pyrowave_decoder,
    ) -> pyrowave_result;
    pub fn pyrowave_decoder_destroy(decoder: pyrowave_decoder);

    pub fn pyrowave_decoder_push_packet(
        decoder: pyrowave_decoder,
        data: *const c_void,
        size: usize,
    ) -> pyrowave_result;

    pub fn pyrowave_decoder_decode_is_ready(
        decoder: pyrowave_decoder,
        allow_partial_frame: bool,
    ) -> bool;

    pub fn pyrowave_decoder_device_prefers_fragment_path(device: pyrowave_device) -> bool;
}
