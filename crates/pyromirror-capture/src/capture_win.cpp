#if defined(_WIN32)

#define INITGUID
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <dxgi1_5.h>
#include <d3d11_4.h>
#include <wrl/client.h>
#include "../include/pyromirror_capture.h"

using Microsoft::WRL::ComPtr;

struct pyromirror_capture_context {
    ComPtr<IDXGIFactory1> factory;
    ComPtr<IDXGIAdapter> adapter;
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11Device5> device5;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<ID3D11DeviceContext4> context4;
    ComPtr<IDXGIOutput1> output1;
    ComPtr<IDXGIOutputDuplication> duplication;
    ComPtr<ID3D11Fence> fence;
    HANDLE fence_handle;
    uint64_t timeline;
    bool has_frame;
    ComPtr<IDXGIResource> last_frame_resource;
    ComPtr<ID3D11Texture2D> staging_texture;
    uint32_t staging_width;
    uint32_t staging_height;
};

extern "C" pyromirror_capture_context* pyromirror_capture_create(void) {
    auto* ctx = new pyromirror_capture_context();
    ctx->timeline = 0;
    ctx->has_frame = false;

    if (FAILED(CreateDXGIFactory1(IID_PPV_ARGS(&ctx->factory)))) {
        delete ctx;
        return nullptr;
    }

    if (FAILED(ctx->factory->EnumAdapters(0, &ctx->adapter))) {
        delete ctx;
        return nullptr;
    }

    D3D_FEATURE_LEVEL featureLevels[] = { D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0 };
    if (FAILED(D3D11CreateDevice(ctx->adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                 featureLevels, 2, D3D11_SDK_VERSION,
                                 &ctx->device, nullptr, &ctx->context))) {
        delete ctx;
        return nullptr;
    }

    ctx->device.As(&ctx->device5);
    ctx->context.As(&ctx->context4);

    if (FAILED(ctx->device5->CreateFence(0, D3D11_FENCE_FLAG_SHARED, IID_PPV_ARGS(&ctx->fence)))) {
        delete ctx;
        return nullptr;
    }

    if (FAILED(ctx->fence->CreateSharedHandle(nullptr, GENERIC_ALL, nullptr, &ctx->fence_handle))) {
        delete ctx;
        return nullptr;
    }

    ComPtr<IDXGIOutput> output;
    if (FAILED(ctx->adapter->EnumOutputs(0, &output))) {
        delete ctx;
        return nullptr;
    }

    output.As(&ctx->output1);
    if (FAILED(ctx->output1->DuplicateOutput(ctx->device.Get(), &ctx->duplication))) {
        delete ctx;
        return nullptr;
    }

    return ctx;
}

extern "C" bool pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame) {
    if (!ctx || !ctx->duplication) return false;

    if (ctx->has_frame) {
        ctx->duplication->ReleaseFrame();
        ctx->has_frame = false;
    }

    DXGI_OUTDUPL_FRAME_INFO frame_info = {};
    ComPtr<IDXGIResource> resource;
    HRESULT hr = ctx->duplication->AcquireNextFrame(timeout_ms, &frame_info, &resource);

    if (hr == DXGI_ERROR_ACCESS_LOST) {
        // Mode switch or UAC screen, reinvocation required
        return false;
    }

    if (FAILED(hr) || !resource) {
        return false;
    }

    ctx->has_frame = true;
    ctx->last_frame_resource = resource;

    ComPtr<ID3D11Texture2D> tex;
    if (FAILED(resource.As(&tex))) {
        return false;
    }

    D3D11_TEXTURE2D_DESC desc = {};
    tex->GetDesc(&desc);

    ComPtr<IDXGIResource1> res1;
    if (FAILED(resource.As(&res1))) {
        return false;
    }

    HANDLE shared_handle = nullptr;
    if (FAILED(res1->CreateSharedHandle(nullptr, GENERIC_ALL, nullptr, &shared_handle))) {
        return false;
    }

    ctx->timeline++;
    ctx->context4->Signal(ctx->fence.Get(), ctx->timeline);

    out_frame->os_handle = reinterpret_cast<uintptr_t>(shared_handle);
    out_frame->width = desc.Width;
    out_frame->height = desc.Height;
    out_frame->format = static_cast<uint32_t>(desc.Format);
    out_frame->fence_handle = reinterpret_cast<uintptr_t>(ctx->fence_handle);
    out_frame->timeline_value = ctx->timeline;

    return true;
}

extern "C" bool pyromirror_capture_read_pixels(pyromirror_capture_context* ctx, uint8_t* out_pixels, uint32_t pitch) {
    if (!ctx || !ctx->duplication || !ctx->has_frame || !out_pixels || !ctx->last_frame_resource) return false;

    ComPtr<ID3D11Texture2D> tex;
    if (FAILED(ctx->last_frame_resource.As(&tex))) return false;

    D3D11_TEXTURE2D_DESC desc = {};
    tex->GetDesc(&desc);

    if (!ctx->staging_texture || ctx->staging_width != desc.Width || ctx->staging_height != desc.Height) {
        D3D11_TEXTURE2D_DESC staging_desc = desc;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        staging_desc.BindFlags = 0;
        staging_desc.MiscFlags = 0;
        staging_desc.MipLevels = 1;
        staging_desc.ArraySize = 1;
        staging_desc.SampleDesc.Count = 1;
        staging_desc.SampleDesc.Quality = 0;

        if (FAILED(ctx->device->CreateTexture2D(&staging_desc, nullptr, &ctx->staging_texture))) {
            return false;
        }
        ctx->staging_width = desc.Width;
        ctx->staging_height = desc.Height;
    }

    ctx->context->CopyResource(ctx->staging_texture.Get(), tex.Get());

    D3D11_MAPPED_SUBRESOURCE mapped = {};
    if (FAILED(ctx->context->Map(ctx->staging_texture.Get(), 0, D3D11_MAP_READ, 0, &mapped))) {
        return false;
    }

    const uint8_t* src = static_cast<const uint8_t*>(mapped.pData);
    uint32_t copy_width = (desc.Width * 4 < pitch) ? desc.Width * 4 : pitch;

    for (uint32_t y = 0; y < desc.Height; ++y) {
        const uint8_t* src_row = src + y * mapped.RowPitch;
        uint8_t* dst_row = out_pixels + y * pitch;
        if (desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM) {
            for (uint32_t x = 0; x < desc.Width; ++x) {
                uint32_t si = x * 4;
                uint32_t di = x * 4;
                dst_row[di + 0] = src_row[si + 2]; // R <- B
                dst_row[di + 1] = src_row[si + 1]; // G <- G
                dst_row[di + 2] = src_row[si + 0]; // B <- R
                dst_row[di + 3] = 255;             // A
            }
        } else {
            memcpy(dst_row, src_row, copy_width);
        }
    }

    ctx->context->Unmap(ctx->staging_texture.Get(), 0);
    return true;
}

extern "C" void pyromirror_capture_release(pyromirror_capture_context* ctx) {
    if (ctx && ctx->duplication && ctx->has_frame) {
        ctx->duplication->ReleaseFrame();
        ctx->has_frame = false;
        ctx->last_frame_resource.Reset();
    }
}

extern "C" void pyromirror_capture_get_resolution(pyromirror_capture_context* ctx, uint32_t* width, uint32_t* height) {
    if (!ctx || !width || !height) return;
    if (ctx->adapter) {
        ComPtr<IDXGIOutput> output;
        if (SUCCEEDED(ctx->adapter->EnumOutputs(0, &output))) {
            DXGI_OUTPUT_DESC desc = {};
            if (SUCCEEDED(output->GetDesc(&desc))) {
                *width = desc.DesktopCoordinates.right - desc.DesktopCoordinates.left;
                *height = desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top;
                return;
            }
        }
    }
    *width = 1920;
    *height = 1080;
}

extern "C" void pyromirror_capture_destroy(pyromirror_capture_context* ctx) {
    if (ctx) {
        if (ctx->has_frame && ctx->duplication) {
            ctx->duplication->ReleaseFrame();
        }
        delete ctx;
    }
}

#endif // _WIN32
