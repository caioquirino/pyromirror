// Direct3D 11 textures the viewer's picture is decoded into (Windows).
//
// They are created on the device SDL renders with and shared with Vulkan, so that PyroWave writes
// the decoded planes straight into what SDL then draws; no pixels pass through memory.

#define COBJMACROS
#define WIN32_LEAN_AND_MEAN
// Makes the interface ids used below real definitions, so no extra library is needed for them.
#include <initguid.h>
#include <windows.h>
#include <d3d11_4.h>
#include <dxgi1_2.h>

#include <stdbool.h>
#include <stdint.h>
#include <string.h>

// The graphics adapter `device` (an ID3D11Device) runs on, as its 8-byte LUID.
bool pmv_adapter_luid(void *device, uint8_t *luid)
{
	IDXGIDevice *dxgi = NULL;
	IDXGIAdapter *adapter = NULL;
	DXGI_ADAPTER_DESC desc;
	bool ok = false;
	if (SUCCEEDED(ID3D11Device_QueryInterface((ID3D11Device *)device, &IID_IDXGIDevice, (void **)&dxgi)))
	{
		if (SUCCEEDED(IDXGIDevice_GetAdapter(dxgi, &adapter)))
		{
			if (SUCCEEDED(IDXGIAdapter_GetDesc(adapter, &desc)))
			{
				memcpy(luid, &desc.AdapterLuid, 8);
				ok = true;
			}
			IDXGIAdapter_Release(adapter);
		}
		IDXGIDevice_Release(dxgi);
	}
	return ok;
}

// A single-channel 8-bit texture that Vulkan can write and Direct3D can sample. Returns the
// ID3D11Texture2D (release with pmv_release) and, in `handle`, an NT handle to share it with;
// NULL on failure.
void *pmv_create_plane(void *device, uint32_t width, uint32_t height, uintptr_t *handle)
{
	D3D11_TEXTURE2D_DESC desc;
	memset(&desc, 0, sizeof(desc));
	desc.Width = width;
	desc.Height = height;
	desc.MipLevels = 1;
	desc.ArraySize = 1;
	desc.Format = DXGI_FORMAT_R8_UNORM;
	desc.SampleDesc.Count = 1;
	desc.Usage = D3D11_USAGE_DEFAULT;
	desc.BindFlags = D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_RENDER_TARGET | D3D11_BIND_UNORDERED_ACCESS;
	desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;

	ID3D11Texture2D *texture = NULL;
	if (FAILED(ID3D11Device_CreateTexture2D((ID3D11Device *)device, &desc, NULL, &texture)))
		return NULL;

	IDXGIResource1 *resource = NULL;
	HANDLE shared = NULL;
	if (FAILED(ID3D11Texture2D_QueryInterface(texture, &IID_IDXGIResource1, (void **)&resource)) ||
	    FAILED(IDXGIResource1_CreateSharedHandle(resource, NULL, GENERIC_ALL, NULL, &shared)))
	{
		if (resource)
			IDXGIResource1_Release(resource);
		ID3D11Texture2D_Release(texture);
		return NULL;
	}
	IDXGIResource1_Release(resource);
	*handle = (uintptr_t)shared;
	return texture;
}

// A fence Vulkan signals when it has finished writing the planes. Returns the ID3D11Fence
// (release with pmv_release) and an NT handle to share it with; NULL on failure.
void *pmv_create_fence(void *device, uintptr_t *handle)
{
	ID3D11Device5 *device5 = NULL;
	ID3D11Fence *fence = NULL;
	HANDLE shared = NULL;
	if (FAILED(ID3D11Device_QueryInterface((ID3D11Device *)device, &IID_ID3D11Device5, (void **)&device5)))
		return NULL;
	HRESULT hr = ID3D11Device5_CreateFence(device5, 0, D3D11_FENCE_FLAG_SHARED, &IID_ID3D11Fence, (void **)&fence);
	ID3D11Device5_Release(device5);
	if (FAILED(hr))
		return NULL;
	if (FAILED(ID3D11Fence_CreateSharedHandle(fence, NULL, GENERIC_ALL, NULL, &shared)))
	{
		ID3D11Fence_Release(fence);
		return NULL;
	}
	*handle = (uintptr_t)shared;
	return fence;
}

void pmv_release(void *unknown)
{
	if (unknown)
		IUnknown_Release((IUnknown *)unknown);
}
