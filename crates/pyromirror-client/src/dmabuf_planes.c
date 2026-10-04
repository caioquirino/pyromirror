// DMA-BUF planes the viewer's picture is decoded into (Linux).
//
// They are allocated with GBM on the GPU SDL's OpenGL renderer draws with, and shared two ways:
// with Vulkan, so that PyroWave writes the decoded planes into them, and with OpenGL (through
// EGL images), so that SDL draws from them; no pixels pass through memory.
//
// EGL and GBM are loaded at run time and declared here by hand, so that neither building nor
// starting the viewer depends on them: where they are missing, the picture is converted on the
// CPU as before.

#if defined(__linux__)

#include <dlfcn.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/sysmacros.h>
#include <unistd.h>

typedef void *EGLDisplay;
typedef void *EGLContext;
typedef void *EGLImage;
typedef void *EGLDevice;
typedef intptr_t EGLAttrib;
typedef int32_t EGLint;
typedef unsigned int EGLBoolean;
typedef unsigned int EGLenum;
typedef uint64_t EGLuint64KHR;

#define EGL_HEIGHT 0x3056
#define EGL_WIDTH 0x3057
#define EGL_NONE 0x3038
#define EGL_DEVICE_EXT 0x322C
#define EGL_LINUX_DMA_BUF_EXT 0x3270
#define EGL_LINUX_DRM_FOURCC_EXT 0x3271
#define EGL_DMA_BUF_PLANE0_FD_EXT 0x3272
#define EGL_DMA_BUF_PLANE0_OFFSET_EXT 0x3273
#define EGL_DMA_BUF_PLANE0_PITCH_EXT 0x3274
#define EGL_DRM_RENDER_NODE_FILE_EXT 0x3377
#define EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT 0x3443
#define EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT 0x3444

#define GL_TEXTURE_2D 0x0DE1
#define GL_TEXTURE_BINDING_2D 0x8069

// 'R', '8', ' ', ' ': one 8-bit channel. GBM and DRM use the same codes.
#define FOURCC_R8 0x20203852u
#define GBM_BO_USE_RENDERING (1 << 2)

struct gbm_device;
struct gbm_bo;

typedef struct pmv_display
{
	void *egl_library;
	void *gbm_library;
	EGLDisplay display;
	int drm_fd;
	struct gbm_device *gbm;

	EGLImage (*create_image)(EGLDisplay, EGLContext, EGLenum, void *, const EGLint *);
	EGLBoolean (*destroy_image)(EGLDisplay, EGLImage);
	EGLBoolean (*query_modifiers)(EGLDisplay, EGLint, EGLint, EGLuint64KHR *, EGLBoolean *, EGLint *);
	void (*image_target_texture)(unsigned int, void *);
	void (*gen_textures)(int, unsigned int *);
	void (*delete_textures)(int, const unsigned int *);
	void (*bind_texture)(unsigned int, unsigned int);
	void (*get_integer)(unsigned int, int *);
	unsigned int (*get_error)(void);

	void (*gbm_device_destroy)(struct gbm_device *);
	struct gbm_bo *(*gbm_bo_create_with_modifiers)(struct gbm_device *, uint32_t, uint32_t, uint32_t,
	                                               const uint64_t *, unsigned int);
	int (*gbm_bo_get_fd)(struct gbm_bo *);
	uint32_t (*gbm_bo_get_stride)(struct gbm_bo *);
	uint32_t (*gbm_bo_get_offset)(struct gbm_bo *, int);
	uint64_t (*gbm_bo_get_modifier)(struct gbm_bo *);
	int (*gbm_bo_get_plane_count)(struct gbm_bo *);
	void (*gbm_bo_destroy)(struct gbm_bo *);
} pmv_display;

// One plane: the buffer, and what each side knows it by.
typedef struct pmv_plane
{
	struct gbm_bo *bo;
	EGLImage image;
	uint32_t texture; // OpenGL texture name
	int32_t fd;       // The DMA-BUF; closed by pmv_plane_destroy
	uint32_t stride;
	uint32_t offset;
	uint64_t modifier;
} pmv_plane;

void pmv_display_close(pmv_display *d)
{
	if (!d)
		return;
	if (d->gbm)
		d->gbm_device_destroy(d->gbm);
	if (d->drm_fd >= 0)
		close(d->drm_fd);
	// The libraries stay loaded: SDL uses EGL too, and unloading a graphics driver's library
	// under it gains nothing.
	free(d);
}

// What SDL's OpenGL renderer draws with, if that is an EGL context on a GPU with a DRM render
// node. Must be called on the thread that renders, like everything else here except the fields
// of a finished plane. NULL where any of it is missing.
pmv_display *pmv_display_open(void)
{
	pmv_display *d = calloc(1, sizeof(*d));
	if (!d)
		return NULL;
	d->drm_fd = -1;

	d->egl_library = dlopen("libEGL.so.1", RTLD_NOW | RTLD_GLOBAL);
	d->gbm_library = dlopen("libgbm.so.1", RTLD_NOW | RTLD_LOCAL);
	if (!d->egl_library || !d->gbm_library)
		goto fail;

	EGLDisplay (*get_current_display)(void) = dlsym(d->egl_library, "eglGetCurrentDisplay");
	EGLContext (*get_current_context)(void) = dlsym(d->egl_library, "eglGetCurrentContext");
	void *(*get_proc)(const char *) = dlsym(d->egl_library, "eglGetProcAddress");
	if (!get_current_display || !get_current_context || !get_proc)
		goto fail;
	// With GLX (SDL on X11 by default) there is no EGL display, and no way to import a DMA-BUF.
	d->display = get_current_display();
	if (!d->display || !get_current_context())
		goto fail;

	EGLBoolean (*query_display_attrib)(EGLDisplay, EGLint, EGLAttrib *) = get_proc("eglQueryDisplayAttribEXT");
	const char *(*query_device_string)(EGLDevice, EGLint) = get_proc("eglQueryDeviceStringEXT");
	d->create_image = get_proc("eglCreateImageKHR");
	d->destroy_image = get_proc("eglDestroyImageKHR");
	d->query_modifiers = get_proc("eglQueryDmaBufModifiersEXT");
	d->image_target_texture = get_proc("glEGLImageTargetTexture2DOES");
	d->gen_textures = get_proc("glGenTextures");
	d->delete_textures = get_proc("glDeleteTextures");
	d->bind_texture = get_proc("glBindTexture");
	d->get_integer = get_proc("glGetIntegerv");
	d->get_error = get_proc("glGetError");
	if (!query_display_attrib || !query_device_string || !d->create_image || !d->destroy_image ||
	    !d->query_modifiers || !d->image_target_texture || !d->gen_textures || !d->delete_textures ||
	    !d->bind_texture || !d->get_integer || !d->get_error)
		goto fail;

	struct gbm_device *(*gbm_create_device)(int) = dlsym(d->gbm_library, "gbm_create_device");
	d->gbm_device_destroy = dlsym(d->gbm_library, "gbm_device_destroy");
	d->gbm_bo_create_with_modifiers = dlsym(d->gbm_library, "gbm_bo_create_with_modifiers");
	d->gbm_bo_get_fd = dlsym(d->gbm_library, "gbm_bo_get_fd");
	d->gbm_bo_get_stride = dlsym(d->gbm_library, "gbm_bo_get_stride");
	d->gbm_bo_get_offset = dlsym(d->gbm_library, "gbm_bo_get_offset");
	d->gbm_bo_get_modifier = dlsym(d->gbm_library, "gbm_bo_get_modifier");
	d->gbm_bo_get_plane_count = dlsym(d->gbm_library, "gbm_bo_get_plane_count");
	d->gbm_bo_destroy = dlsym(d->gbm_library, "gbm_bo_destroy");
	if (!gbm_create_device || !d->gbm_device_destroy || !d->gbm_bo_create_with_modifiers || !d->gbm_bo_get_fd ||
	    !d->gbm_bo_get_stride || !d->gbm_bo_get_offset || !d->gbm_bo_get_modifier || !d->gbm_bo_get_plane_count ||
	    !d->gbm_bo_destroy)
		goto fail;

	// The GPU the context renders on, as the device file GBM allocates through.
	EGLAttrib device = 0;
	if (!query_display_attrib(d->display, EGL_DEVICE_EXT, &device) || !device)
		goto fail;
	const char *node = query_device_string((EGLDevice)device, EGL_DRM_RENDER_NODE_FILE_EXT);
	if (!node)
		goto fail;
	d->drm_fd = open(node, O_RDWR | O_CLOEXEC);
	if (d->drm_fd < 0)
		goto fail;
	d->gbm = gbm_create_device(d->drm_fd);
	if (!d->gbm)
		goto fail;
	return d;

fail:
	pmv_display_close(d);
	return NULL;
}

// The DRM render node of the GPU the window is drawn on.
bool pmv_display_render_node(pmv_display *d, int64_t *major_out, int64_t *minor_out)
{
	struct stat st;
	if (fstat(d->drm_fd, &st) != 0 || !S_ISCHR(st.st_mode))
		return false;
	*major_out = major(st.st_rdev);
	*minor_out = minor(st.st_rdev);
	return true;
}

// The DRM format modifiers with which OpenGL can sample a single-channel DMA-BUF as an ordinary
// texture. Writes at most `capacity` and returns how many.
size_t pmv_display_modifiers(pmv_display *d, uint64_t *out, size_t capacity)
{
	EGLuint64KHR modifiers[64];
	EGLBoolean external_only[64];
	EGLint count = 0;
	if (!d->query_modifiers(d->display, (EGLint)FOURCC_R8, 64, modifiers, external_only, &count))
		return 0;
	size_t n = 0;
	for (EGLint i = 0; i < count && i < 64 && n < capacity; i++)
	{
		// "External only" textures need a different sampler type than SDL's shaders use.
		if (!external_only[i])
			out[n++] = modifiers[i];
	}
	return n;
}

void pmv_plane_destroy(pmv_display *d, pmv_plane *plane)
{
	if (plane->texture)
		d->delete_textures(1, &plane->texture);
	if (plane->image)
		d->destroy_image(d->display, plane->image);
	if (plane->fd >= 0)
		close(plane->fd);
	if (plane->bo)
		d->gbm_bo_destroy(plane->bo);
	memset(plane, 0, sizeof(*plane));
	plane->fd = -1;
}

// Allocates a single-channel 8-bit plane with one of `modifiers` and names an OpenGL texture for
// it. The texture has no contents until pmv_plane_attach.
bool pmv_plane_create(pmv_display *d, uint32_t width, uint32_t height, const uint64_t *modifiers, size_t count,
                      pmv_plane *plane)
{
	memset(plane, 0, sizeof(*plane));
	plane->fd = -1;

	plane->bo = d->gbm_bo_create_with_modifiers(d->gbm, width, height, FOURCC_R8, modifiers, (unsigned int)count);
	if (!plane->bo)
		return false;
	// Vulkan is told about one memory plane (see pm_dmabuf_modifiers).
	if (d->gbm_bo_get_plane_count(plane->bo) != 1)
		goto fail;
	plane->fd = d->gbm_bo_get_fd(plane->bo);
	if (plane->fd < 0)
		goto fail;
	plane->stride = d->gbm_bo_get_stride(plane->bo);
	plane->offset = d->gbm_bo_get_offset(plane->bo, 0);
	plane->modifier = d->gbm_bo_get_modifier(plane->bo);

	const EGLint attributes[] = {
		EGL_WIDTH, (EGLint)width,
		EGL_HEIGHT, (EGLint)height,
		EGL_LINUX_DRM_FOURCC_EXT, (EGLint)FOURCC_R8,
		EGL_DMA_BUF_PLANE0_FD_EXT, plane->fd,
		EGL_DMA_BUF_PLANE0_OFFSET_EXT, (EGLint)plane->offset,
		EGL_DMA_BUF_PLANE0_PITCH_EXT, (EGLint)plane->stride,
		EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(plane->modifier & 0xffffffffu),
		EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(plane->modifier >> 32),
		EGL_NONE,
	};
	plane->image = d->create_image(d->display, NULL, EGL_LINUX_DMA_BUF_EXT, NULL, attributes);
	if (!plane->image)
		goto fail;

	d->gen_textures(1, &plane->texture);
	if (!plane->texture)
		goto fail;
	return true;

fail:
	pmv_plane_destroy(d, plane);
	return false;
}

// Makes the plane's texture show the DMA-BUF. To be called after SDL has wrapped the texture:
// SDL gives every texture it is handed storage of its own, which this replaces.
bool pmv_plane_attach(pmv_display *d, pmv_plane *plane)
{
	// SDL keeps track of what is bound; leave it as it was.
	int bound = 0;
	d->get_integer(GL_TEXTURE_BINDING_2D, &bound);
	while (d->get_error() != 0)
	{
	}
	d->bind_texture(GL_TEXTURE_2D, plane->texture);
	d->image_target_texture(GL_TEXTURE_2D, plane->image);
	bool ok = d->get_error() == 0;
	d->bind_texture(GL_TEXTURE_2D, (unsigned int)bound);
	return ok;
}

#endif // __linux__
