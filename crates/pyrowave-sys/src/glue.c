// Thin helpers around the parts of the PyroWave API that take Vulkan types, so that the Rust
// side needs no Vulkan bindings: importing a texture another graphics API owns, and encoding
// straight from it (scaling and RGB -> YCbCr happen on the GPU).

#include <vulkan/vulkan_core.h>
#include "pyrowave.h"

#include <stdint.h>
#include <stdlib.h>
#include <string.h>

// Keep in sync with the constants in lib.rs.
#define PM_HANDLE_D3D11_TEXTURE 0 // NT handle of a shared ID3D11Texture2D
#define PM_FORMAT_BGRA8 0
#define PM_FORMAT_RGBA8 1
#define PM_FORMAT_RGBA16F 2

typedef struct pm_gpu_image
{
	pyrowave_image image;
	pyrowave_image_view view;
} pm_gpu_image;

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
                                    uint32_t width, uint32_t height, int format, pm_gpu_image **out)
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

	switch (format)
	{
	case PM_FORMAT_BGRA8:
		image_info.format = VK_FORMAT_B8G8R8A8_UNORM;
		break;
	case PM_FORMAT_RGBA8:
		image_info.format = VK_FORMAT_R8G8B8A8_UNORM;
		break;
	case PM_FORMAT_RGBA16F:
		image_info.format = VK_FORMAT_R16G16B16A16_SFLOAT;
		break;
	default:
		return PYROWAVE_ERROR_INVALID_ARGUMENT;
	}

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

	pm_gpu_image *img = calloc(1, sizeof(*img));
	if (!img)
		return PYROWAVE_ERROR_OUT_OF_HOST_MEMORY;

	pyrowave_result result = pyrowave_image_create(&info, &img->image);
	if (result == PYROWAVE_SUCCESS)
	{
		result = pyrowave_image_get_image_view(img->image, VK_IMAGE_ASPECT_COLOR_BIT,
		                                       VK_IMAGE_USAGE_SAMPLED_BIT, &img->view);
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

// Encodes the image's current contents, scaled to the encoder's size. The owner of the image
// must have finished writing it (its own GPU work completed) before this is called, and must not
// write it again until the frame has been packetized.
pyrowave_result pm_gpu_image_encode(pyrowave_encoder encoder, pm_gpu_image *image, bool exact_size,
                                    size_t maximum_bitstream_size)
{
	pyrowave_gpu_external_reference ref;
	ref.image = image->image;
	ref.queue_family_index = VK_QUEUE_FAMILY_EXTERNAL;

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
