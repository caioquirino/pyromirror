//! Drawing the picture without it passing through memory (Windows).
//!
//! PyroWave decodes on the GPU. Normally the planes are read back, converted to RGB on the CPU
//! and uploaded again for SDL to draw. Here they are instead decoded into Direct3D textures that
//! belong to SDL's renderer, which also does the YCbCr to RGB conversion in its shader.

use std::ffi::{c_void, CStr};

use anyhow::{bail, Context};
use sdl3::render::{Texture, TextureCreator, WindowCanvas};
use sdl3::sys;
use sdl3::video::WindowContext;

use pyromirror_codec::{Decoder, TextureHandle};

extern "C" {
    fn pmv_adapter_luid(device: *mut c_void, luid: *mut u8) -> bool;
    fn pmv_create_plane(device: *mut c_void, width: u32, height: u32, handle: *mut usize) -> *mut c_void;
    fn pmv_create_fence(device: *mut c_void, handle: *mut usize) -> *mut c_void;
    fn pmv_release(unknown: *mut c_void);
}

/// Sets of planes to rotate through, so that the one on screen and the one just decoded are
/// never the one being written.
const SETS: usize = 3;

/// A Direct3D object kept alive for as long as this exists.
struct Com(*mut c_void);

impl Drop for Com {
    fn drop(&mut self) {
        unsafe { pmv_release(self.0) };
    }
}

/// The Direct3D 11 device SDL draws with, if that is how it draws.
fn renderer_device(canvas: &WindowCanvas) -> Option<*mut c_void> {
    unsafe {
        let name = sys::render::SDL_GetRendererName(canvas.raw());
        if name.is_null() || CStr::from_ptr(name).to_bytes() != b"direct3d11" {
            return None;
        }
        let props = sys::render::SDL_GetRendererProperties(canvas.raw());
        let device = sys::properties::SDL_GetPointerProperty(props, sys::render::SDL_PROP_RENDERER_D3D11_DEVICE_POINTER, std::ptr::null_mut());
        (!device.is_null()).then_some(device)
    }
}

/// The graphics adapter the window is drawn on; the decoder has to run on the same one.
pub fn adapter(canvas: &WindowCanvas) -> Option<[u8; 8]> {
    let device = renderer_device(canvas)?;
    let mut luid = [0u8; 8];
    unsafe { pmv_adapter_luid(device, luid.as_mut_ptr()) }.then_some(luid)
}

/// What the main thread draws from: one texture per set of planes.
pub struct Targets<'r> {
    pub textures: Vec<Texture<'r>>,
    /// The planes and the fence; SDL and PyroWave hold their own references too.
    _objects: Vec<Com>,
}

/// Creates the plane textures, hands them to the decoder to decode into, and wraps each set in
/// an SDL texture to draw.
pub fn create<'r>(canvas: &WindowCanvas, creator: &'r TextureCreator<WindowContext>, decoder: &mut Decoder) -> anyhow::Result<Targets<'r>> {
    let Some(device) = renderer_device(canvas) else { bail!("SDL is not drawing with Direct3D 11") };
    let mut objects = Vec::new();

    let mut fence_handle = 0usize;
    let fence = unsafe { pmv_create_fence(device, &mut fence_handle) };
    if fence.is_null() {
        bail!("could not create a shared Direct3D fence");
    }
    objects.push(Com(fence));
    decoder.set_gpu_fence(fence_handle).context("PyroWave could not import the Direct3D fence")?;

    let sizes = decoder.plane_sizes();
    let mut textures = Vec::new();
    for _ in 0..SETS {
        let mut planes = [std::ptr::null_mut::<c_void>(); 3];
        let mut handles = [0usize; 3];
        for (i, (width, height)) in sizes.into_iter().enumerate() {
            planes[i] = unsafe { pmv_create_plane(device, width, height, &mut handles[i]) };
            if planes[i].is_null() {
                bail!("could not create a shared {}x{} Direct3D texture", width, height);
            }
            objects.push(Com(planes[i]));
        }
        decoder
            .add_gpu_target(handles.map(TextureHandle::D3d11))
            .context("PyroWave could not import the Direct3D textures")?;

        // SDL knows planar YCbCr as "IYUV". It samples the three planes by position, so chroma
        // planes at full size (4:4:4) work as well as half-size ones.
        let texture = unsafe {
            use sys::properties::{SDL_SetNumberProperty, SDL_SetPointerProperty};
            use sys::render::*;
            let props = sys::properties::SDL_CreateProperties();
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, sys::pixels::SDL_PIXELFORMAT_IYUV.0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, SDL_TEXTUREACCESS_STATIC.0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, sizes[0].0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, sizes[0].1 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, sys::pixels::SDL_COLORSPACE_BT709_FULL.0 as i64);
            SDL_SetPointerProperty(props, SDL_PROP_TEXTURE_CREATE_D3D11_TEXTURE_POINTER, planes[0]);
            SDL_SetPointerProperty(props, SDL_PROP_TEXTURE_CREATE_D3D11_TEXTURE_U_POINTER, planes[1]);
            SDL_SetPointerProperty(props, SDL_PROP_TEXTURE_CREATE_D3D11_TEXTURE_V_POINTER, planes[2]);
            let raw = SDL_CreateTextureWithProperties(canvas.raw(), props);
            sys::properties::SDL_DestroyProperties(props);
            if raw.is_null() {
                bail!("SDL could not wrap the Direct3D textures: {}", sdl3::get_error());
            }
            creator.raw_create_texture(raw)
        };
        textures.push(texture);
    }
    Ok(Targets { textures, _objects: objects })
}
