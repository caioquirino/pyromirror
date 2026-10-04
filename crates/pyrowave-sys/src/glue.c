// Thin helpers around the parts of the PyroWave API that take Vulkan types, so that the Rust
// side needs no Vulkan bindings: importing a texture another graphics API owns, and encoding
// straight from it (scaling and RGB -> YCbCr happen on the GPU).

#include <vulkan/vulkan_core.h>
#include "pyrowave.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#ifdef __linux__
#include <dlfcn.h>
#endif

// Keep in sync with the constants in lib.rs.
#define PM_HANDLE_D3D11_TEXTURE 0 // NT handle of a shared ID3D11Texture2D
#define PM_FORMAT_BGRA8 0
#define PM_FORMAT_RGBA8 1
#define PM_FORMAT_RGBA16F 2
#define PM_FORMAT_R8 3

typedef struct pm_gpu_image
{
	pyrowave_image image;
	pyrowave_image_view view;
	// Who the image is taken over from and handed back to around each use: another API of the
	// same driver (EXTERNAL), or whatever made a DMA-BUF (FOREIGN).
	uint32_t queue_family;
} pm_gpu_image;

static bool pm_vk_format(int format, VkFormat *out)
{
	switch (format)
	{
	case PM_FORMAT_BGRA8:
		*out = VK_FORMAT_B8G8R8A8_UNORM;
		return true;
	case PM_FORMAT_RGBA8:
		*out = VK_FORMAT_R8G8B8A8_UNORM;
		return true;
	case PM_FORMAT_RGBA16F:
		*out = VK_FORMAT_R16G16B16A16_SFLOAT;
		return true;
	case PM_FORMAT_R8:
		*out = VK_FORMAT_R8_UNORM;
		return true;
	default:
		return false;
	}
}

// Creates the image `info` describes and the view the encoder or decoder uses.
static pyrowave_result pm_gpu_image_finish(const pyrowave_image_create_info *info, bool writable,
                                           uint32_t queue_family, pm_gpu_image **out)
{
	pm_gpu_image *img = calloc(1, sizeof(*img));
	if (!img)
		return PYROWAVE_ERROR_OUT_OF_HOST_MEMORY;
	img->queue_family = queue_family;

	pyrowave_result result = pyrowave_image_create(info, &img->image);
	if (result == PYROWAVE_SUCCESS)
	{
		result = pyrowave_image_get_image_view(img->image, VK_IMAGE_ASPECT_COLOR_BIT,
		                                       writable ? VK_IMAGE_USAGE_STORAGE_BIT : VK_IMAGE_USAGE_SAMPLED_BIT,
		                                       &img->view);
		if (result != PYROWAVE_SUCCESS)
			pyrowave_image_destroy(img->image);
	}
	if (result != PYROWAVE_SUCCESS)
	{
		free(img);
		return result;
	}

	*out = img;
	return PYROWAVE_SUCCESS;
}

// A device on the adapter with this LUID (Windows), which is where another API's textures live.
pyrowave_result pm_create_device_for_luid(const uint8_t *luid, pyrowave_device *device)
{
	pyrowave_luid id;
	memcpy(id.luid, luid, VK_LUID_SIZE);
	return pyrowave_create_device_by_compat(0, 0, NULL, NULL, &id, device);
}

// PyroWave takes ownership of `handle` when the import succeeds. Callers treat it as consumed
// either way and never use it again.
pyrowave_result pm_gpu_image_import(pyrowave_device device, uintptr_t handle, int handle_kind,
                                    uint32_t width, uint32_t height, int format, bool writable,
                                    pm_gpu_image **out)
{
	VkImageCreateInfo image_info;
	memset(&image_info, 0, sizeof(image_info));
	image_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
	image_info.imageType = VK_IMAGE_TYPE_2D;
	image_info.extent.width = width;
	image_info.extent.height = height;
	image_info.extent.depth = 1;
	image_info.mipLevels = 1;
	image_info.arrayLayers = 1;
	image_info.samples = VK_SAMPLE_COUNT_1_BIT;
	image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	image_info.tiling = VK_IMAGE_TILING_OPTIMAL;
	// As in PyroWave's own D3D11 interop test; the usage flags matter little for an import.
	image_info.flags = VK_IMAGE_CREATE_MUTABLE_FORMAT_BIT;
	image_info.usage = VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_TRANSFER_SRC_BIT | VK_IMAGE_USAGE_TRANSFER_DST_BIT;
	// The decoder writes its planes as storage images.
	if (writable)
		image_info.usage |= VK_IMAGE_USAGE_STORAGE_BIT;

	if (!pm_vk_format(format, &image_info.format))
		return PYROWAVE_ERROR_INVALID_ARGUMENT;

	pyrowave_image_create_info info;
	memset(&info, 0, sizeof(info));
	info.device = device;
	info.external_handle = (pyrowave_os_handle)handle;
	info.image_create_info = &image_info;
	switch (handle_kind)
	{
	case PM_HANDLE_D3D11_TEXTURE:
		info.handle_type = VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT;
		break;
	default:
		return PYROWAVE_ERROR_INVALID_ARGUMENT;
	}

	return pm_gpu_image_finish(&info, writable, VK_QUEUE_FAMILY_EXTERNAL, out);
}

#ifdef __linux__
// The glue links no Vulkan loader; the few functions needed here come from the one PyroWave
// already loaded.
static PFN_vkGetInstanceProcAddr pm_instance_proc_addr(void)
{
	static PFN_vkGetInstanceProcAddr proc;
	if (!proc)
	{
		void *loader = dlopen("libvulkan.so.1", RTLD_NOW | RTLD_LOCAL);
		if (loader)
			proc = (PFN_vkGetInstanceProcAddr)dlsym(loader, "vkGetInstanceProcAddr");
	}
	return proc;
}
#endif

#define PM_MAX_MODIFIERS 64

// What an imported DMA-BUF is used as: something to encode from, or (`writable`) a plane to
// decode into.
static VkImageUsageFlags pm_dmabuf_usage(bool writable)
{
	return writable ? (VK_IMAGE_USAGE_SAMPLED_BIT | VK_IMAGE_USAGE_STORAGE_BIT) : VK_IMAGE_USAGE_SAMPLED_BIT;
}

// The DRM format modifiers with which a DMA-BUF of this format can be imported as something to
// encode from, or with `writable` as a plane to decode into. Writes at most `capacity` of them to
// `out` and returns how many; 0 where there are no DMA-BUFs.
size_t pm_dmabuf_modifiers(pyrowave_device device, int format, bool writable, uint64_t *out, size_t capacity)
{
#ifdef __linux__
	VkFormat vk_format;
	if (!pm_vk_format(format, &vk_format))
		return 0;
	PFN_vkGetInstanceProcAddr proc = pm_instance_proc_addr();
	if (!proc)
		return 0;

	VkInstance instance = VK_NULL_HANDLE;
	VkPhysicalDevice gpu = VK_NULL_HANDLE;
	pyrowave_device_get_vk_device_handles(device, &instance, &gpu, NULL);
	if (!instance || !gpu)
		return 0;

	PFN_vkGetPhysicalDeviceFormatProperties2 format_properties =
		(PFN_vkGetPhysicalDeviceFormatProperties2)proc(instance, "vkGetPhysicalDeviceFormatProperties2");
	PFN_vkGetPhysicalDeviceImageFormatProperties2 image_format_properties =
		(PFN_vkGetPhysicalDeviceImageFormatProperties2)proc(instance, "vkGetPhysicalDeviceImageFormatProperties2");
	if (!format_properties || !image_format_properties)
		return 0;

	VkDrmFormatModifierPropertiesEXT modifiers[PM_MAX_MODIFIERS];
	VkDrmFormatModifierPropertiesListEXT list;
	memset(&list, 0, sizeof(list));
	list.sType = VK_STRUCTURE_TYPE_DRM_FORMAT_MODIFIER_PROPERTIES_LIST_EXT;
	VkFormatProperties2 properties;
	memset(&properties, 0, sizeof(properties));
	properties.sType = VK_STRUCTURE_TYPE_FORMAT_PROPERTIES_2;
	properties.pNext = &list;
	// First the count, then the list itself.
	format_properties(gpu, vk_format, &properties);
	if (list.drmFormatModifierCount > PM_MAX_MODIFIERS)
		list.drmFormatModifierCount = PM_MAX_MODIFIERS;
	list.pDrmFormatModifierProperties = modifiers;
	format_properties(gpu, vk_format, &properties);

	size_t count = 0;
	for (uint32_t i = 0; i < list.drmFormatModifierCount && count < capacity; i++)
	{
		// A plane to decode into is one plain surface, with nothing on the side.
		if (modifiers[i].drmFormatModifierPlaneCount > (writable ? 1u : 4u))
			continue;

		VkPhysicalDeviceImageDrmFormatModifierInfoEXT modifier_info;
		memset(&modifier_info, 0, sizeof(modifier_info));
		modifier_info.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_DRM_FORMAT_MODIFIER_INFO_EXT;
		modifier_info.drmFormatModifier = modifiers[i].drmFormatModifier;
		modifier_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;

		VkPhysicalDeviceExternalImageFormatInfo external_info;
		memset(&external_info, 0, sizeof(external_info));
		external_info.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_EXTERNAL_IMAGE_FORMAT_INFO;
		external_info.pNext = &modifier_info;
		external_info.handleType = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;

		// Has to describe the image exactly as pm_gpu_image_import_dmabuf creates it.
		VkPhysicalDeviceImageFormatInfo2 image_info;
		memset(&image_info, 0, sizeof(image_info));
		image_info.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_IMAGE_FORMAT_INFO_2;
		image_info.pNext = &external_info;
		image_info.format = vk_format;
		image_info.type = VK_IMAGE_TYPE_2D;
		image_info.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
		image_info.usage = pm_dmabuf_usage(writable);

		VkExternalImageFormatProperties external_properties;
		memset(&external_properties, 0, sizeof(external_properties));
		external_properties.sType = VK_STRUCTURE_TYPE_EXTERNAL_IMAGE_FORMAT_PROPERTIES;
		VkImageFormatProperties2 image_properties;
		memset(&image_properties, 0, sizeof(image_properties));
		image_properties.sType = VK_STRUCTURE_TYPE_IMAGE_FORMAT_PROPERTIES_2;
		image_properties.pNext = &external_properties;

		if (image_format_properties(gpu, &image_info, &image_properties) != VK_SUCCESS)
			continue;
		if (!(external_properties.externalMemoryProperties.externalMemoryFeatures &
		      VK_EXTERNAL_MEMORY_FEATURE_IMPORTABLE_BIT))
			continue;
		out[count++] = modifiers[i].drmFormatModifier;
	}
	return count;
#else
	(void)device;
	(void)format;
	(void)writable;
	(void)out;
	(void)capacity;
	return 0;
#endif
}

// The DRM render node of the GPU the device runs on, to tell whether something else (a
// compositor, a window's renderer) uses the same one. False where that is not known.
bool pm_device_drm_render_node(pyrowave_device device, int64_t *major, int64_t *minor)
{
#ifdef __linux__
	PFN_vkGetInstanceProcAddr proc = pm_instance_proc_addr();
	if (!proc)
		return false;
	VkInstance instance = VK_NULL_HANDLE;
	VkPhysicalDevice gpu = VK_NULL_HANDLE;
	pyrowave_device_get_vk_device_handles(device, &instance, &gpu, NULL);
	if (!instance || !gpu)
		return false;
	PFN_vkGetPhysicalDeviceProperties2 get_properties =
		(PFN_vkGetPhysicalDeviceProperties2)proc(instance, "vkGetPhysicalDeviceProperties2");
	if (!get_properties)
		return false;

	// Left untouched (all zero) by a driver without VK_EXT_physical_device_drm.
	VkPhysicalDeviceDrmPropertiesEXT drm;
	memset(&drm, 0, sizeof(drm));
	drm.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_DRM_PROPERTIES_EXT;
	VkPhysicalDeviceProperties2 properties;
	memset(&properties, 0, sizeof(properties));
	properties.sType = VK_STRUCTURE_TYPE_PHYSICAL_DEVICE_PROPERTIES_2;
	properties.pNext = &drm;
	get_properties(gpu, &properties);
	if (!drm.hasRender)
		return false;
	*major = drm.renderMajor;
	*minor = drm.renderMinor;
	return true;
#else
	(void)device;
	(void)major;
	(void)minor;
	return false;
#endif
}

// Imports a DMA-BUF (Linux) as something to encode from, or with `writable` as a plane to decode
// into. `offsets` and `strides` describe its `planes` memory planes (1 to 4), which all live in
// the one buffer `fd` names. PyroWave takes ownership of `fd`; callers treat it as consumed
// either way.
pyrowave_result pm_gpu_image_import_dmabuf(pyrowave_device device, int fd, uint32_t width, uint32_t height,
                                           int format, uint64_t modifier, uint32_t planes,
                                           const uint32_t *offsets, const uint32_t *strides, bool writable,
                                           pm_gpu_image **out)
{
	if (planes < 1 || planes > 4)
		return PYROWAVE_ERROR_INVALID_ARGUMENT;

	VkSubresourceLayout layouts[4];
	memset(layouts, 0, sizeof(layouts));
	for (uint32_t i = 0; i < planes; i++)
	{
		layouts[i].offset = offsets[i];
		layouts[i].rowPitch = strides[i];
	}

	VkImageDrmFormatModifierExplicitCreateInfoEXT explicit_info;
	memset(&explicit_info, 0, sizeof(explicit_info));
	explicit_info.sType = VK_STRUCTURE_TYPE_IMAGE_DRM_FORMAT_MODIFIER_EXPLICIT_CREATE_INFO_EXT;
	explicit_info.drmFormatModifier = modifier;
	explicit_info.drmFormatModifierPlaneCount = planes;
	explicit_info.pPlaneLayouts = layouts;

	VkImageCreateInfo image_info;
	memset(&image_info, 0, sizeof(image_info));
	image_info.sType = VK_STRUCTURE_TYPE_IMAGE_CREATE_INFO;
	image_info.pNext = &explicit_info;
	image_info.imageType = VK_IMAGE_TYPE_2D;
	image_info.extent.width = width;
	image_info.extent.height = height;
	image_info.extent.depth = 1;
	image_info.mipLevels = 1;
	image_info.arrayLayers = 1;
	image_info.samples = VK_SAMPLE_COUNT_1_BIT;
	image_info.sharingMode = VK_SHARING_MODE_EXCLUSIVE;
	image_info.tiling = VK_IMAGE_TILING_DRM_FORMAT_MODIFIER_EXT;
	// Exactly what pm_dmabuf_modifiers asked the driver about.
	image_info.usage = pm_dmabuf_usage(writable);
	if (!pm_vk_format(format, &image_info.format))
		return PYROWAVE_ERROR_INVALID_ARGUMENT;

	pyrowave_image_create_info info;
	memset(&info, 0, sizeof(info));
	info.device = device;
	info.external_handle = (pyrowave_os_handle)fd;
	info.handle_type = VK_EXTERNAL_MEMORY_HANDLE_TYPE_DMA_BUF_BIT_EXT;
	info.image_create_info = &image_info;

	return pm_gpu_image_finish(&info, writable, VK_QUEUE_FAMILY_FOREIGN_EXT, out);
}

// Encodes the image's current contents, scaled to the encoder's size. The owner of the image
// must have finished writing it (its own GPU work completed) before this is called, and must not
// write it again until the frame has been packetized.
pyrowave_result pm_gpu_image_encode(pyrowave_encoder encoder, pm_gpu_image *image, bool exact_size,
                                    size_t maximum_bitstream_size)
{
	pyrowave_gpu_external_reference ref;
	ref.image = image->image;
	ref.queue_family_index = image->queue_family;

	// No semaphores: the two sides take turns, as described above.
	pyrowave_gpu_sync_operation acquire, release;
	memset(&acquire, 0, sizeof(acquire));
	memset(&release, 0, sizeof(release));
	acquire.images = &ref;
	acquire.num_images = 1;
	release.images = &ref;
	release.num_images = 1;

	pyrowave_scaled_encode_info scaling;
	memset(&scaling, 0, sizeof(scaling));
	scaling.view = image->view;
	scaling.input_color_space = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR;
	scaling.output_color_space = VK_COLOR_SPACE_SRGB_NONLINEAR_KHR;
	scaling.intermediate_plane_format = VK_FORMAT_R8_UNORM;
	scaling.ycbcr_chroma_midpoint = 128.0f / 255.0f;
	// At the same size there is nothing to filter; linear sampling at texel centres is exact.
	scaling.force_linear_filtering = exact_size;
	// A desktop is mostly flat colour and text; dither noise would only cost bits.
	scaling.skip_dither = true;

	pyrowave_rate_control rate_control;
	rate_control.maximum_bitstream_size = maximum_bitstream_size;

	return pyrowave_encoder_encode_gpu_scaled_synchronous(encoder, &acquire, &release, &scaling, &rate_control);
}

void pm_gpu_image_destroy(pm_gpu_image *image)
{
	if (!image)
		return;
	pyrowave_image_destroy(image->image);
	free(image);
}

// A fence of another graphics API (Windows: the NT handle of a shared ID3D11Fence or
// ID3D12Fence), which PyroWave signals when it has finished writing textures of that API.
typedef struct pm_gpu_fence
{
	pyrowave_sync_object sync;
} pm_gpu_fence;

// PyroWave takes ownership of `handle` when the import succeeds.
pyrowave_result pm_gpu_fence_import(pyrowave_device device, uintptr_t handle, pm_gpu_fence **out)
{
	pyrowave_sync_object_create_info info;
	memset(&info, 0, sizeof(info));
	info.device = device;
	info.external_handle = (pyrowave_os_handle)handle;
	// A D3D11 fence is the same thing as a D3D12 fence on Windows 10 and later.
	info.handle_type = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_D3D12_FENCE_BIT;
	info.semaphore_type = VK_SEMAPHORE_TYPE_TIMELINE;

	pm_gpu_fence *fence = calloc(1, sizeof(*fence));
	if (!fence)
		return PYROWAVE_ERROR_OUT_OF_HOST_MEMORY;
	pyrowave_result result = pyrowave_sync_object_create(&info, &fence->sync);
	if (result != PYROWAVE_SUCCESS)
	{
		free(fence);
		return result;
	}
	*out = fence;
	return PYROWAVE_SUCCESS;
}

// A fence of PyroWave's own, for where the other API has none to share (Linux): the decode is
// simply waited for before the planes' owner is told about it.
pyrowave_result pm_gpu_fence_create(pyrowave_device device, pm_gpu_fence **out)
{
	pyrowave_sync_object_create_info info;
	memset(&info, 0, sizeof(info));
	info.device = device;
	// No handle to import: PyroWave creates the object itself.
	info.external_handle = (pyrowave_os_handle)(intptr_t)-1;
	info.handle_type = VK_EXTERNAL_SEMAPHORE_HANDLE_TYPE_OPAQUE_FD_BIT;
	info.semaphore_type = VK_SEMAPHORE_TYPE_TIMELINE;
	// Means nothing without an import, but PyroWave insists on it then.
	info.import_flags = VK_SEMAPHORE_IMPORT_TEMPORARY_BIT;

	pm_gpu_fence *fence = calloc(1, sizeof(*fence));
	if (!fence)
		return PYROWAVE_ERROR_OUT_OF_HOST_MEMORY;
	pyrowave_result result = pyrowave_sync_object_create(&info, &fence->sync);
	if (result != PYROWAVE_SUCCESS)
	{
		free(fence);
		return result;
	}
	*out = fence;
	return PYROWAVE_SUCCESS;
}

void pm_gpu_fence_destroy(pm_gpu_fence *fence)
{
	if (!fence)
		return;
	pyrowave_sync_object_destroy(fence->sync);
	free(fence);
}

// Decodes the queued frame into three imported planes (Y, Cb, Cr) and waits until the GPU has
// written them, by way of `fence` reaching `value` (which must grow with every call). After
// that the planes' owner may read them.
pyrowave_result pm_gpu_decode(pyrowave_decoder decoder, pm_gpu_image *const *planes, pm_gpu_fence *fence,
                              uint64_t value, uint64_t timeout_ns)
{
	pyrowave_gpu_external_reference acquire_refs[3], release_refs[3];
	pyrowave_gpu_buffers buffers;
	memset(&buffers, 0, sizeof(buffers));
	for (int i = 0; i < 3; i++)
	{
		// What the planes held before does not matter.
		acquire_refs[i].image = planes[i]->image;
		acquire_refs[i].queue_family_index = VK_QUEUE_FAMILY_IGNORED;
		release_refs[i].image = planes[i]->image;
		release_refs[i].queue_family_index = planes[i]->queue_family;
		buffers.planes[i] = planes[i]->view;
	}

	pyrowave_gpu_sync_operation acquire, release;
	memset(&acquire, 0, sizeof(acquire));
	memset(&release, 0, sizeof(release));
	acquire.images = acquire_refs;
	acquire.num_images = 3;
	release.images = release_refs;
	release.num_images = 3;
	release.sync.semaphore = pyrowave_sync_object_get_semaphore(fence->sync);
	release.sync.value = value;

	pyrowave_result result = pyrowave_decoder_decode_gpu_buffer(decoder, &acquire, &release, &buffers);
	if (result != PYROWAVE_SUCCESS)
		return result;
	return pyrowave_sync_object_cpu_wait(fence->sync, value, timeout_ns);
}
