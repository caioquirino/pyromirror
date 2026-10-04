//! Drawing the picture without it passing through memory (Linux).
//!
//! PyroWave decodes on the GPU. Normally the planes are read back, converted to RGB on the CPU
//! and uploaded again for SDL to draw. Here they are instead decoded into DMA-BUFs that SDL's
//! OpenGL renderer samples as textures, and its shader does the YCbCr to RGB conversion.

use std::ffi::{c_void, CStr};

use anyhow::{bail, Context};
use sdl3::render::{Texture, TextureCreator, WindowCanvas};
use sdl3::sys;
use sdl3::video::WindowContext;

use pyromirror_codec::{Decoder, DmaBufPlanes, TextureHandle};

/// Mirrors `pmv_plane` in dmabuf_planes.c.
#[repr(C)]
struct Plane {
    bo: *mut c_void,
    image: *mut c_void,
    texture: u32,
    fd: i32,
    stride: u32,
    offset: u32,
    modifier: u64,
}

extern "C" {
    fn pmv_display_open() -> *mut c_void;
    fn pmv_display_close(display: *mut c_void);
    fn pmv_display_render_node(display: *mut c_void, major: *mut i64, minor: *mut i64) -> bool;
    fn pmv_display_modifiers(display: *mut c_void, out: *mut u64, capacity: usize) -> usize;
    fn pmv_plane_create(display: *mut c_void, width: u32, height: u32, modifiers: *const u64, count: usize, plane: *mut Plane) -> bool;
    fn pmv_plane_attach(display: *mut c_void, plane: *mut Plane) -> bool;
    fn pmv_plane_destroy(display: *mut c_void, plane: *mut Plane);
}

/// Sets of planes to rotate through, so that the one on screen and the one just decoded are
/// never the one being written.
const SETS: usize = 3;

/// The planes and what they were allocated through. Dropped on the thread that renders, while
/// the renderer still exists.
struct Planes {
    display: *mut c_void,
    planes: Vec<Plane>,
}

impl Drop for Planes {
    fn drop(&mut self) {
        unsafe {
            for plane in &mut self.planes {
                pmv_plane_destroy(self.display, plane);
            }
            pmv_display_close(self.display);
        }
    }
}

/// What the main thread draws from: one texture per set of planes.
pub struct Targets<'r> {
    // Before the planes they wrap.
    pub textures: Vec<Texture<'r>>,
    _planes: Planes,
}

/// Which of SDL's OpenGL renderers draws the window, as the properties its textures are wrapped
/// with (Y, U, V); `None` for any other renderer.
fn texture_properties(canvas: &WindowCanvas) -> Option<[*const std::ffi::c_char; 3]> {
    use sys::render::*;
    let name = unsafe { SDL_GetRendererName(canvas.raw()) };
    if name.is_null() {
        return None;
    }
    match unsafe { CStr::from_ptr(name) }.to_bytes() {
        b"opengl" => Some([
            SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_NUMBER,
            SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_U_NUMBER,
            SDL_PROP_TEXTURE_CREATE_OPENGL_TEXTURE_V_NUMBER,
        ]),
        b"opengles2" => Some([
            SDL_PROP_TEXTURE_CREATE_OPENGLES2_TEXTURE_NUMBER,
            SDL_PROP_TEXTURE_CREATE_OPENGLES2_TEXTURE_U_NUMBER,
            SDL_PROP_TEXTURE_CREATE_OPENGLES2_TEXTURE_V_NUMBER,
        ]),
        _ => None,
    }
}

/// Creates the planes, hands them to the decoder to decode into, and wraps each set in an SDL
/// texture to draw. `Ok(None)` where the window is not drawn in a way that allows it (no OpenGL
/// on EGL, as with X11's GLX or the software renderer), which is nothing to warn about.
pub fn create<'r>(canvas: &WindowCanvas, creator: &'r TextureCreator<WindowContext>, decoder: &mut Decoder) -> anyhow::Result<Option<Targets<'r>>> {
    let Some(properties) = texture_properties(canvas) else { return Ok(None) };
    let display = unsafe { pmv_display_open() };
    if display.is_null() {
        return Ok(None);
    }
    let mut planes = Planes { display, planes: Vec::with_capacity(SETS * 3) };

    // The planes live on the GPU that draws the window; the decoder has to run on the same one.
    let (mut major, mut minor) = (0i64, 0i64);
    let window_gpu = unsafe { pmv_display_render_node(display, &mut major, &mut minor) }.then_some((major, minor));
    if let (Some(window), Some(decoder)) = (window_gpu, decoder.device().drm_render_node()) {
        if window != decoder {
            bail!("the window is drawn on another GPU than the one that decodes");
        }
    }

    // What both sides can do with a single-channel plane, in OpenGL's order of preference.
    let writable = decoder.device().dmabuf_plane_modifiers();
    let mut modifiers = vec![0u64; 64];
    let count = unsafe { pmv_display_modifiers(display, modifiers.as_mut_ptr(), modifiers.len()) };
    modifiers.truncate(count);
    modifiers.retain(|m| writable.contains(m));
    if modifiers.is_empty() {
        bail!("OpenGL and Vulkan have no way in common to share a plane");
    }

    decoder.create_gpu_fence().context("PyroWave could not create a fence")?;

    let sizes = decoder.plane_sizes();
    let mut textures = Vec::new();
    for _ in 0..SETS {
        let first = planes.planes.len();
        for (width, height) in sizes {
            let mut plane = Plane { bo: std::ptr::null_mut(), image: std::ptr::null_mut(), texture: 0, fd: -1, stride: 0, offset: 0, modifier: 0 };
            if !unsafe { pmv_plane_create(display, width, height, modifiers.as_ptr(), modifiers.len(), &mut plane) } {
                bail!("could not allocate a shared {}x{} plane", width, height);
            }
            planes.planes.push(plane);
        }
        let set = &mut planes.planes[first..];
        let handle = |plane: &Plane| TextureHandle::DmaBuf {
            fd: plane.fd,
            layout: DmaBufPlanes { modifier: plane.modifier, planes: 1, offsets: [plane.offset, 0, 0, 0], strides: [plane.stride, 0, 0, 0] },
        };
        decoder
            .add_gpu_target([handle(&set[0]), handle(&set[1]), handle(&set[2])])
            .context("PyroWave could not import the planes")?;

        // SDL knows planar YCbCr as "IYUV". It samples the three planes by position, so chroma
        // planes at full size (4:4:4) work as well as half-size ones.
        let texture = unsafe {
            use sys::properties::SDL_SetNumberProperty;
            use sys::render::*;
            let props = sys::properties::SDL_CreateProperties();
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_FORMAT_NUMBER, sys::pixels::SDL_PIXELFORMAT_IYUV.0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_ACCESS_NUMBER, SDL_TEXTUREACCESS_STATIC.0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_WIDTH_NUMBER, sizes[0].0 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_HEIGHT_NUMBER, sizes[0].1 as i64);
            SDL_SetNumberProperty(props, SDL_PROP_TEXTURE_CREATE_COLORSPACE_NUMBER, sys::pixels::SDL_COLORSPACE_BT709_FULL.0 as i64);
            for (property, plane) in properties.iter().zip(set.iter()) {
                SDL_SetNumberProperty(props, *property, plane.texture as i64);
            }
            let raw = SDL_CreateTextureWithProperties(canvas.raw(), props);
            sys::properties::SDL_DestroyProperties(props);
            if raw.is_null() {
                bail!("SDL could not wrap the OpenGL textures: {}", sdl3::get_error());
            }
            creator.raw_create_texture(raw)
        };
        textures.push(texture);

        // Only now: SDL gave the textures storage of its own when it wrapped them.
        for plane in set.iter_mut() {
            if !unsafe { pmv_plane_attach(display, plane) } {
                bail!("OpenGL could not use a shared plane as a texture");
            }
        }
    }
    Ok(Some(Targets { textures, _planes: planes }))
}
