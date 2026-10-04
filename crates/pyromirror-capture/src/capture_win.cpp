#if defined(_WIN32)

// DXGI Desktop Duplication capture.
//
// Each changed desktop image is copied into a CPU-readable staging texture and handed out as a
// mapped BGRA pointer. In GPU mode it is instead copied into a shared texture that the encoder
// reads directly through Vulkan, which skips the readback (the expensive part at 4K).
//
// A monitor in HDR mode is duplicated as linear scRGB floats and converted to SDR by a small
// shader here, with the SDR brightness the user chose in Windows as white. (The plain
// duplication API hands out Windows' own conversion instead, which looks washed out.)

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <dxgi1_6.h>
#include <d3d11.h>
#include <d3d11_4.h>
#include <d3dcompiler.h>
#include <wrl/client.h>
#include <stdio.h>
#include <string.h>
#include <string>
#include <vector>
#include "../include/pyromirror_capture.h"

using Microsoft::WRL::ComPtr;

struct pyromirror_capture_context {
    ComPtr<ID3D11Device> device;
    ComPtr<ID3D11DeviceContext> context;
    ComPtr<IDXGIOutput1> output;
    ComPtr<IDXGIOutputDuplication> duplication;
    ComPtr<ID3D11Texture2D> staging;
    D3D11_TEXTURE2D_DESC staging_desc = {};
    bool mapped = false;
    std::string last_error;
    LUID adapter_luid = {};

    // GPU mode.
    bool gpu = false;
    ComPtr<ID3D11Texture2D> shared;
    D3D11_TEXTURE2D_DESC shared_desc = {};
    uint64_t shared_id = 0;
    // The shared texture holds an image nobody has received as pixels (set when GPU mode is
    // switched off).
    bool shared_pending = false;
    // Either of these tells when the GPU has finished copying into the shared texture.
    ComPtr<ID3D11DeviceContext4> context4;
    ComPtr<ID3D11Fence> fence;
    uint64_t fence_value = 0;
    ComPtr<ID3D11Query> copy_done;

    // HDR desktops: scRGB in, SDR out, into `shared`.
    bool hdr_failed = false; // Fall back to the duplication API that converts by itself.
    bool hdr_frames = false; // The last frame came as scRGB.
    ComPtr<ID3D11VertexShader> hdr_vs;
    ComPtr<ID3D11PixelShader> hdr_ps;
    ComPtr<ID3D11Buffer> hdr_constants;
    ComPtr<ID3D11Texture2D> hdr_source;
    ComPtr<ID3D11ShaderResourceView> hdr_source_view;
    ComPtr<ID3D11RenderTargetView> shared_target;
    uint64_t shared_target_id = 0;
    float sdr_white_nits = 0.0f;
    uint32_t frames_since_white_check = 0;

    // Desktop Duplication never draws the pointer into the image; it reports it on the side.
    uint64_t cursor_serial = 0;
    bool cursor_visible = true;
    uint32_t cursor_width = 0, cursor_height = 0, cursor_hot_x = 0, cursor_hot_y = 0;
    std::vector<uint8_t> cursor_rgba;
    std::vector<uint8_t> cursor_raw;
};

// Converts the pointer shape Windows reports into straight-alpha RGBA.
static void convert_cursor(pyromirror_capture_context* ctx, const DXGI_OUTDUPL_POINTER_SHAPE_INFO& info) {
    const uint8_t* src = ctx->cursor_raw.data();
    uint32_t w = info.Width;
    uint32_t h = info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME ? info.Height / 2 : info.Height;
    if (w == 0 || h == 0 || w > 512 || h > 512) return;

    std::vector<uint8_t> rgba(static_cast<size_t>(w) * h * 4, 0);
    // Pixels that Windows would invert against what is under them. There is no "invert" on the
    // viewer's side, so they become black with a white outline, which shows on any background
    // (this is what makes the text I-beam visible).
    std::vector<bool> inverted(static_cast<size_t>(w) * h, false);

    for (uint32_t y = 0; y < h; ++y) {
        for (uint32_t x = 0; x < w; ++x) {
            uint8_t* out = &rgba[(static_cast<size_t>(y) * w + x) * 4];
            if (info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_MONOCHROME) {
                // 1 bit per pixel: an AND mask followed by an XOR mask.
                uint8_t bit = 0x80 >> (x % 8);
                bool and_bit = src[y * info.Pitch + x / 8] & bit;
                bool xor_bit = src[(y + h) * info.Pitch + x / 8] & bit;
                if (!and_bit) {
                    out[0] = out[1] = out[2] = xor_bit ? 255 : 0;
                    out[3] = 255;
                } else if (xor_bit) {
                    inverted[static_cast<size_t>(y) * w + x] = true;
                }
            } else {
                const uint8_t* in = src + y * info.Pitch + x * 4; // B, G, R, A
                if (info.Type == DXGI_OUTDUPL_POINTER_SHAPE_TYPE_COLOR) {
                    out[0] = in[2]; out[1] = in[1]; out[2] = in[0]; out[3] = in[3];
                } else if (in[3] == 0) { // masked colour: opaque pixel
                    out[0] = in[2]; out[1] = in[1]; out[2] = in[0]; out[3] = 255;
                } else if (in[0] || in[1] || in[2]) { // masked colour: XOR with the screen
                    inverted[static_cast<size_t>(y) * w + x] = true;
                }
            }
        }
    }

    for (uint32_t y = 0; y < h; ++y) {
        for (uint32_t x = 0; x < w; ++x) {
            size_t i = static_cast<size_t>(y) * w + x;
            if (inverted[i]) {
                rgba[i * 4 + 0] = rgba[i * 4 + 1] = rgba[i * 4 + 2] = 0;
                rgba[i * 4 + 3] = 255;
            } else if (rgba[i * 4 + 3] == 0) {
                bool next_to_inverted = (x > 0 && inverted[i - 1]) || (x + 1 < w && inverted[i + 1]) ||
                                        (y > 0 && inverted[i - w]) || (y + 1 < h && inverted[i + w]);
                if (next_to_inverted) {
                    rgba[i * 4 + 0] = rgba[i * 4 + 1] = rgba[i * 4 + 2] = 255;
                    rgba[i * 4 + 3] = 255;
                }
            }
        }
    }

    ctx->cursor_rgba.swap(rgba);
    ctx->cursor_width = w;
    ctx->cursor_height = h;
    ctx->cursor_hot_x = info.HotSpot.x > 0 ? static_cast<uint32_t>(info.HotSpot.x) : 0;
    ctx->cursor_hot_y = info.HotSpot.y > 0 ? static_cast<uint32_t>(info.HotSpot.y) : 0;
    ctx->cursor_serial++;
}

// Picks up pointer changes that came with a duplication frame.
static void update_cursor(pyromirror_capture_context* ctx, const DXGI_OUTDUPL_FRAME_INFO& info) {
    if (info.LastMouseUpdateTime.QuadPart != 0) {
        bool visible = info.PointerPosition.Visible != 0;
        if (visible != ctx->cursor_visible) {
            ctx->cursor_visible = visible;
            ctx->cursor_serial++;
        }
    }
    if (info.PointerShapeBufferSize > 0) {
        ctx->cursor_raw.resize(info.PointerShapeBufferSize);
        DXGI_OUTDUPL_POINTER_SHAPE_INFO shape = {};
        UINT needed = 0;
        if (SUCCEEDED(ctx->duplication->GetFramePointerShape(info.PointerShapeBufferSize, ctx->cursor_raw.data(), &needed, &shape))) {
            convert_cursor(ctx, shape);
        }
    }
}

static std::string hr_message(const char* what, HRESULT hr) {
    char buf[256];
    snprintf(buf, sizeof(buf), "%s failed (HRESULT 0x%08lx)", what, static_cast<unsigned long>(hr));
    return buf;
}

// Finds the requested output and the adapter driving it. On hybrid-GPU laptops the desktop is
// usually attached to the integrated adapter, which need not be adapter 0.
static bool find_output(int32_t wanted, ComPtr<IDXGIAdapter1>& out_adapter, ComPtr<IDXGIOutput1>& out_output, std::string& error) {
    ComPtr<IDXGIFactory1> factory;
    HRESULT hr = CreateDXGIFactory1(IID_PPV_ARGS(&factory));
    if (FAILED(hr)) {
        error = hr_message("CreateDXGIFactory1", hr);
        return false;
    }

    int32_t index = 0;
    ComPtr<IDXGIAdapter1> first_adapter;
    ComPtr<IDXGIOutput1> first_output;

    ComPtr<IDXGIAdapter1> adapter;
    for (UINT a = 0; factory->EnumAdapters1(a, &adapter) != DXGI_ERROR_NOT_FOUND; ++a) {
        ComPtr<IDXGIOutput> output;
        for (UINT o = 0; adapter->EnumOutputs(o, &output) != DXGI_ERROR_NOT_FOUND; ++o) {
            DXGI_OUTPUT_DESC desc = {};
            ComPtr<IDXGIOutput1> output1;
            if (FAILED(output->GetDesc(&desc)) || !desc.AttachedToDesktop || FAILED(output.As(&output1))) {
                continue;
            }

            bool primary = desc.DesktopCoordinates.left == 0 && desc.DesktopCoordinates.top == 0;
            if ((wanted < 0 && primary) || wanted == index) {
                out_adapter = adapter;
                out_output = output1;
                return true;
            }
            if (!first_output) {
                first_adapter = adapter;
                first_output = output1;
            }
            ++index;
        }
    }

    if (wanted < 0 && first_output) {
        out_adapter = first_adapter;
        out_output = first_output;
        return true;
    }

    char buf[160];
    snprintf(buf, sizeof(buf), "no monitor with index %d attached to the desktop (%d found); "
             "desktop duplication is unavailable in RDP sessions and for services", wanted, index);
    error = buf;
    return false;
}

// Copies `src` into the staging texture, from where it can be read (see map_staging).
static bool copy_to_staging(pyromirror_capture_context* ctx, ID3D11Texture2D* src) {
    D3D11_TEXTURE2D_DESC desc = {};
    src->GetDesc(&desc);
    if (!ctx->staging || ctx->staging_desc.Width != desc.Width || ctx->staging_desc.Height != desc.Height ||
        ctx->staging_desc.Format != desc.Format) {
        D3D11_TEXTURE2D_DESC staging_desc = {};
        staging_desc.Width = desc.Width;
        staging_desc.Height = desc.Height;
        staging_desc.MipLevels = 1;
        staging_desc.ArraySize = 1;
        staging_desc.Format = desc.Format;
        staging_desc.SampleDesc.Count = 1;
        staging_desc.Usage = D3D11_USAGE_STAGING;
        staging_desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;

        ctx->staging.Reset();
        HRESULT hr = ctx->device->CreateTexture2D(&staging_desc, nullptr, &ctx->staging);
        if (FAILED(hr)) {
            ctx->last_error = hr_message("CreateTexture2D(staging)", hr);
            return false;
        }
        ctx->staging_desc = staging_desc;
    }

    ctx->context->CopyResource(ctx->staging.Get(), src);
    return true;
}

// Hands out the staging texture's pixels.
static int map_staging(pyromirror_capture_context* ctx, pyromirror_capture_frame* out_frame) {
    D3D11_MAPPED_SUBRESOURCE mapped = {};
    HRESULT hr = ctx->context->Map(ctx->staging.Get(), 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        ctx->last_error = hr_message("ID3D11DeviceContext::Map", hr);
        return PYROMIRROR_CAPTURE_ERROR;
    }
    ctx->mapped = true;

    // Desktop duplication always delivers DXGI_FORMAT_B8G8R8A8_UNORM.
    out_frame->data = static_cast<const uint8_t*>(mapped.pData);
    out_frame->width = ctx->staging_desc.Width;
    out_frame->height = ctx->staging_desc.Height;
    out_frame->stride = mapped.RowPitch;
    out_frame->format = PYROMIRROR_CAPTURE_FORMAT_BGRX;
    out_frame->gpu_texture = 0;
    out_frame->prepare_us = 0;
    return PYROMIRROR_CAPTURE_FRAME;
}

// Makes sure there is a shared texture matching `desc`. A new one gets a new id.
static bool ensure_shared(pyromirror_capture_context* ctx, const D3D11_TEXTURE2D_DESC& desc) {
    if (ctx->shared && ctx->shared_desc.Width == desc.Width && ctx->shared_desc.Height == desc.Height &&
        ctx->shared_desc.Format == desc.Format) {
        return true;
    }
    D3D11_TEXTURE2D_DESC shared_desc = {};
    shared_desc.Width = desc.Width;
    shared_desc.Height = desc.Height;
    shared_desc.MipLevels = 1;
    shared_desc.ArraySize = 1;
    shared_desc.Format = desc.Format;
    shared_desc.SampleDesc.Count = 1;
    shared_desc.Usage = D3D11_USAGE_DEFAULT;
    shared_desc.BindFlags = D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE;
    // An NT handle is what Vulkan imports (VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT).
    shared_desc.MiscFlags = D3D11_RESOURCE_MISC_SHARED | D3D11_RESOURCE_MISC_SHARED_NTHANDLE;

    ctx->shared.Reset();
    if (FAILED(ctx->device->CreateTexture2D(&shared_desc, nullptr, &ctx->shared))) {
        return false;
    }
    ctx->shared_desc = shared_desc;
    ctx->shared_id++;
    return true;
}

// Blocks until the GPU has executed everything issued so far, so that another API can read the
// shared texture without any further synchronisation.
static void wait_for_gpu(pyromirror_capture_context* ctx) {
    if (ctx->fence && ctx->context4) {
        if (SUCCEEDED(ctx->context4->Signal(ctx->fence.Get(), ++ctx->fence_value))) {
            ctx->context->Flush();
            // A null event makes this call wait.
            if (SUCCEEDED(ctx->fence->SetEventOnCompletion(ctx->fence_value, nullptr))) {
                return;
            }
        }
    }
    if (ctx->copy_done) {
        ctx->context->End(ctx->copy_done.Get());
        ctx->context->Flush();
        BOOL done = FALSE;
        // A copy takes well under a millisecond; give up after a while rather than hang.
        for (int i = 0; i < 2000; ++i) {
            if (ctx->context->GetData(ctx->copy_done.Get(), &done, sizeof(done), 0) == S_OK && done) {
                return;
            }
            Sleep(0);
        }
    }
    ctx->context->Flush();
}

// How bright Windows shows SDR white on this monitor while it is in HDR mode (the "SDR content
// brightness" slider), in nits.
static float query_sdr_white_nits(pyromirror_capture_context* ctx) {
    const float fallback = 240.0f; // The slider's usual default.
    DXGI_OUTPUT_DESC output_desc = {};
    if (FAILED(ctx->output->GetDesc(&output_desc))) return fallback;

    UINT32 path_count = 0, mode_count = 0;
    if (GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &path_count, &mode_count) != ERROR_SUCCESS) return fallback;
    std::vector<DISPLAYCONFIG_PATH_INFO> paths(path_count);
    std::vector<DISPLAYCONFIG_MODE_INFO> modes(mode_count);
    if (QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &path_count, paths.data(), &mode_count, modes.data(), nullptr) != ERROR_SUCCESS) {
        return fallback;
    }
    for (UINT32 i = 0; i < path_count; ++i) {
        DISPLAYCONFIG_SOURCE_DEVICE_NAME source = {};
        source.header.type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = sizeof(source);
        source.header.adapterId = paths[i].sourceInfo.adapterId;
        source.header.id = paths[i].sourceInfo.id;
        if (DisplayConfigGetDeviceInfo(&source.header) != ERROR_SUCCESS ||
            wcscmp(source.viewGdiDeviceName, output_desc.DeviceName) != 0) {
            continue;
        }
        DISPLAYCONFIG_SDR_WHITE_LEVEL white = {};
        white.header.type = DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL;
        white.header.size = sizeof(white);
        white.header.adapterId = paths[i].targetInfo.adapterId;
        white.header.id = paths[i].targetInfo.id;
        if (DisplayConfigGetDeviceInfo(&white.header) == ERROR_SUCCESS && white.SDRWhiteLevel > 0) {
            // 1000 stands for 80 nits.
            return static_cast<float>(white.SDRWhiteLevel) * 80.0f / 1000.0f;
        }
    }
    return fallback;
}

static const char HDR_SHADER[] = R"(
Texture2D<float4> source : register(t0);
cbuffer Constants : register(b0) { float gain; float3 unused; };

float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 p = float2((id << 1) & 2, id & 2);
    return float4(p * float2(2, -2) + float2(-1, 1), 0, 1);
}

float3 srgb(float3 c) {
    float3 low = c * 12.92;
    float3 high = 1.055 * pow(c, 1.0 / 2.4) - 0.055;
    return lerp(high, low, step(c, 0.0031308));
}

// scRGB is linear with 1.0 at 80 nits. `gain` brings SDR white to 1.0; what is brighter than
// that (HDR highlights) or outside sRGB's colours is clipped.
float4 ps(float4 position : SV_Position) : SV_Target {
    float3 c = saturate(source.Load(int3(position.xy, 0)).rgb * gain);
    return float4(srgb(c), 1);
}
)";

static bool ensure_hdr_pipeline(pyromirror_capture_context* ctx) {
    if (ctx->hdr_ps) return true;
    ComPtr<ID3DBlob> vs, ps;
    if (FAILED(D3DCompile(HDR_SHADER, sizeof(HDR_SHADER) - 1, nullptr, nullptr, nullptr, "vs", "vs_5_0", 0, 0, &vs, nullptr)) ||
        FAILED(D3DCompile(HDR_SHADER, sizeof(HDR_SHADER) - 1, nullptr, nullptr, nullptr, "ps", "ps_5_0", 0, 0, &ps, nullptr))) {
        return false;
    }
    D3D11_BUFFER_DESC constants = {};
    constants.ByteWidth = 16;
    constants.Usage = D3D11_USAGE_DEFAULT;
    constants.BindFlags = D3D11_BIND_CONSTANT_BUFFER;
    ComPtr<ID3D11PixelShader> pixel_shader;
    if (FAILED(ctx->device->CreateVertexShader(vs->GetBufferPointer(), vs->GetBufferSize(), nullptr, &ctx->hdr_vs)) ||
        FAILED(ctx->device->CreatePixelShader(ps->GetBufferPointer(), ps->GetBufferSize(), nullptr, &pixel_shader)) ||
        FAILED(ctx->device->CreateBuffer(&constants, nullptr, &ctx->hdr_constants))) {
        return false;
    }
    ctx->hdr_ps = pixel_shader;
    return true;
}

static bool ensure_shared(pyromirror_capture_context* ctx, const D3D11_TEXTURE2D_DESC& desc);

// Converts an scRGB desktop image to SDR, into the shared texture.
static bool convert_hdr(pyromirror_capture_context* ctx, ID3D11Texture2D* frame) {
    D3D11_TEXTURE2D_DESC desc = {};
    frame->GetDesc(&desc);
    if (!ensure_hdr_pipeline(ctx)) return false;

    D3D11_TEXTURE2D_DESC sdr_desc = desc;
    sdr_desc.Format = DXGI_FORMAT_B8G8R8A8_UNORM;
    if (!ensure_shared(ctx, sdr_desc)) return false;
    if (!ctx->shared_target || ctx->shared_target_id != ctx->shared_id) {
        ctx->shared_target.Reset();
        if (FAILED(ctx->device->CreateRenderTargetView(ctx->shared.Get(), nullptr, &ctx->shared_target))) return false;
        ctx->shared_target_id = ctx->shared_id;
    }

    // The duplicated image cannot be relied on to be bindable to a shader; a copy of ours can.
    D3D11_TEXTURE2D_DESC source_desc = {};
    if (ctx->hdr_source) ctx->hdr_source->GetDesc(&source_desc);
    if (!ctx->hdr_source || source_desc.Width != desc.Width || source_desc.Height != desc.Height || source_desc.Format != desc.Format) {
        source_desc = {};
        source_desc.Width = desc.Width;
        source_desc.Height = desc.Height;
        source_desc.MipLevels = 1;
        source_desc.ArraySize = 1;
        source_desc.Format = desc.Format;
        source_desc.SampleDesc.Count = 1;
        source_desc.Usage = D3D11_USAGE_DEFAULT;
        source_desc.BindFlags = D3D11_BIND_SHADER_RESOURCE;
        ctx->hdr_source.Reset();
        ctx->hdr_source_view.Reset();
        if (FAILED(ctx->device->CreateTexture2D(&source_desc, nullptr, &ctx->hdr_source)) ||
            FAILED(ctx->device->CreateShaderResourceView(ctx->hdr_source.Get(), nullptr, &ctx->hdr_source_view))) {
            ctx->hdr_source.Reset();
            return false;
        }
    }
    ctx->context->CopyResource(ctx->hdr_source.Get(), frame);

    // The slider can be moved at any time; look now and then.
    if (ctx->sdr_white_nits <= 0.0f || ++ctx->frames_since_white_check >= 120) {
        ctx->sdr_white_nits = query_sdr_white_nits(ctx);
        ctx->frames_since_white_check = 0;
    }
    const float constants[4] = { 80.0f / ctx->sdr_white_nits, 0.0f, 0.0f, 0.0f };
    ctx->context->UpdateSubresource(ctx->hdr_constants.Get(), 0, nullptr, constants, 0, 0);

    D3D11_VIEWPORT viewport = { 0.0f, 0.0f, static_cast<float>(desc.Width), static_cast<float>(desc.Height), 0.0f, 1.0f };
    ID3D11RenderTargetView* target = ctx->shared_target.Get();
    ID3D11ShaderResourceView* view = ctx->hdr_source_view.Get();
    ID3D11Buffer* buffer = ctx->hdr_constants.Get();
    auto* c = ctx->context.Get();
    c->OMSetRenderTargets(1, &target, nullptr);
    c->RSSetViewports(1, &viewport);
    c->IASetInputLayout(nullptr);
    c->IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
    c->VSSetShader(ctx->hdr_vs.Get(), nullptr, 0);
    c->PSSetShader(ctx->hdr_ps.Get(), nullptr, 0);
    c->PSSetShaderResources(0, 1, &view);
    c->PSSetConstantBuffers(0, 1, &buffer);
    c->Draw(3, 0);
    // Leave nothing bound: both textures are used elsewhere right after.
    ID3D11ShaderResourceView* no_view = nullptr;
    c->PSSetShaderResources(0, 1, &no_view);
    c->OMSetRenderTargets(0, nullptr, nullptr);
    return true;
}

static HRESULT duplicate(pyromirror_capture_context* ctx) {
    ctx->duplication.Reset();
    // The newer call can hand out an HDR desktop as it is (scRGB), which convert_hdr turns into a
    // proper SDR picture. It behaves like the old one on SDR monitors.
    ComPtr<IDXGIOutput5> output5;
    if (!ctx->hdr_failed && SUCCEEDED(ctx->output.As(&output5))) {
        const DXGI_FORMAT formats[] = { DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_B8G8R8A8_UNORM };
        HRESULT hr = output5->DuplicateOutput1(ctx->device.Get(), 0, 2, formats, &ctx->duplication);
        if (SUCCEEDED(hr)) return hr;
        ctx->duplication.Reset();
    }
    return ctx->output->DuplicateOutput(ctx->device.Get(), &ctx->duplication);
}

extern "C" pyromirror_capture_context* pyromirror_capture_create(const pyromirror_capture_config* config, char* error, uint32_t error_size) {
    auto* ctx = new pyromirror_capture_context();
    std::string err;

    ComPtr<IDXGIAdapter1> adapter;
    bool ok = find_output(config ? config->output_index : -1, adapter, ctx->output, err);
    if (ok) {
        const D3D_FEATURE_LEVEL levels[] = { D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0 };
        HRESULT hr = D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                       levels, 2, D3D11_SDK_VERSION, &ctx->device, nullptr, &ctx->context);
        if (hr == E_INVALIDARG) {
            // Systems without the Windows 8 platform update do not know 11_1.
            hr = D3D11CreateDevice(adapter.Get(), D3D_DRIVER_TYPE_UNKNOWN, nullptr, 0,
                                   levels + 1, 1, D3D11_SDK_VERSION, &ctx->device, nullptr, &ctx->context);
        }
        if (FAILED(hr)) {
            err = hr_message("D3D11CreateDevice", hr);
            ok = false;
        }
    }
    if (ok) {
        HRESULT hr = duplicate(ctx);
        if (FAILED(hr)) {
            err = hr_message("IDXGIOutput1::DuplicateOutput", hr);
            if (hr == E_ACCESSDENIED) {
                err += ": the secure desktop (UAC prompt, lock screen) is active";
            } else if (hr == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE) {
                err += ": too many applications are already duplicating this output";
            } else if (hr == DXGI_ERROR_UNSUPPORTED) {
                err += ": this output cannot be duplicated from this process (wrong GPU or session)";
            }
            ok = false;
        }
    }

    if (ok) {
        DXGI_ADAPTER_DESC1 adapter_desc = {};
        if (SUCCEEDED(adapter->GetDesc1(&adapter_desc))) {
            ctx->adapter_luid = adapter_desc.AdapterLuid;
        }
        // For GPU mode; either is enough, and without both it still works, just less safely.
        ComPtr<ID3D11Device5> device5;
        if (SUCCEEDED(ctx->device.As(&device5)) && SUCCEEDED(ctx->context.As(&ctx->context4))) {
            device5->CreateFence(0, D3D11_FENCE_FLAG_NONE, IID_PPV_ARGS(&ctx->fence));
        }
        D3D11_QUERY_DESC query_desc = { D3D11_QUERY_EVENT, 0 };
        ctx->device->CreateQuery(&query_desc, &ctx->copy_done);
    }

    if (!ok) {
        if (error && error_size) {
            snprintf(error, error_size, "%s", err.c_str());
        }
        delete ctx;
        return nullptr;
    }
    return ctx;
}

extern "C" void pyromirror_capture_release(pyromirror_capture_context* ctx) {
    if (ctx && ctx->mapped) {
        ctx->context->Unmap(ctx->staging.Get(), 0);
        ctx->mapped = false;
    }
}

extern "C" int pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame) {
    if (!ctx || !out_frame) return PYROMIRROR_CAPTURE_ERROR;
    pyromirror_capture_release(ctx);

    if (!ctx->gpu && ctx->shared_pending && ctx->shared) {
        ctx->shared_pending = false;
        return copy_to_staging(ctx, ctx->shared.Get()) ? map_staging(ctx, out_frame) : PYROMIRROR_CAPTURE_ERROR;
    }

    if (!ctx->duplication) {
        // Lost earlier (mode change, UAC, lock screen, fullscreen switch); keep retrying.
        HRESULT hr = duplicate(ctx);
        if (FAILED(hr)) {
            if (hr == E_ACCESSDENIED || hr == DXGI_ERROR_ACCESS_LOST || hr == DXGI_ERROR_SESSION_DISCONNECTED ||
                hr == DXGI_ERROR_NOT_CURRENTLY_AVAILABLE) {
                Sleep(timeout_ms < 100 ? timeout_ms : 100);
                return PYROMIRROR_CAPTURE_NO_FRAME;
            }
            ctx->last_error = hr_message("IDXGIOutput1::DuplicateOutput", hr);
            return PYROMIRROR_CAPTURE_ERROR;
        }
    }

    DXGI_OUTDUPL_FRAME_INFO info = {};
    ComPtr<IDXGIResource> resource;
    HRESULT hr = ctx->duplication->AcquireNextFrame(timeout_ms, &info, &resource);
    if (hr == DXGI_ERROR_WAIT_TIMEOUT) {
        return PYROMIRROR_CAPTURE_NO_FRAME;
    }
    if (hr == DXGI_ERROR_ACCESS_LOST || hr == DXGI_ERROR_INVALID_CALL) {
        ctx->duplication.Reset();
        return PYROMIRROR_CAPTURE_NO_FRAME;
    }
    if (FAILED(hr)) {
        ctx->last_error = hr_message("IDXGIOutputDuplication::AcquireNextFrame", hr);
        return PYROMIRROR_CAPTURE_ERROR;
    }

    update_cursor(ctx, info);
    LARGE_INTEGER prepare_start;
    QueryPerformanceCounter(&prepare_start);
    auto prepare_us = [&]() -> uint32_t {
        LARGE_INTEGER now, frequency;
        QueryPerformanceCounter(&now);
        QueryPerformanceFrequency(&frequency);
        return static_cast<uint32_t>((now.QuadPart - prepare_start.QuadPart) * 1000000 / frequency.QuadPart);
    };

    // LastPresentTime == 0 means only the mouse pointer moved; the desktop image is unchanged.
    ComPtr<ID3D11Texture2D> tex;
    if (info.LastPresentTime.QuadPart == 0 || FAILED(resource.As(&tex))) {
        ctx->duplication->ReleaseFrame();
        return PYROMIRROR_CAPTURE_NO_FRAME;
    }

    D3D11_TEXTURE2D_DESC frame_desc = {};
    tex->GetDesc(&frame_desc);
    ctx->hdr_frames = frame_desc.Format == DXGI_FORMAT_R16G16B16A16_FLOAT;
    if (ctx->hdr_frames) {
        bool converted = convert_hdr(ctx, tex.Get());
        tex.Reset();
        resource.Reset();
        ctx->duplication->ReleaseFrame();
        if (!converted) {
            // Use the duplication call that converts by itself from now on.
            ctx->hdr_failed = true;
            ctx->hdr_frames = false;
            ctx->duplication.Reset();
            return PYROMIRROR_CAPTURE_NO_FRAME;
        }
        if (ctx->gpu) {
            wait_for_gpu(ctx);
            out_frame->data = nullptr;
            out_frame->width = frame_desc.Width;
            out_frame->height = frame_desc.Height;
            out_frame->stride = 0;
            out_frame->format = PYROMIRROR_CAPTURE_FORMAT_BGRX;
            out_frame->gpu_texture = ctx->shared_id;
            out_frame->prepare_us = prepare_us();
            return PYROMIRROR_CAPTURE_FRAME;
        }
        if (!copy_to_staging(ctx, ctx->shared.Get())) return PYROMIRROR_CAPTURE_ERROR;
        int result = map_staging(ctx, out_frame);
        out_frame->prepare_us = prepare_us();
        return result;
    }

    if (ctx->gpu) {
        D3D11_TEXTURE2D_DESC desc = {};
        tex->GetDesc(&desc);
        if (ensure_shared(ctx, desc)) {
            // Copy and hand the frame back to DWM straight away; holding it stalls desktop
            // composition.
            ctx->context->CopyResource(ctx->shared.Get(), tex.Get());
            tex.Reset();
            resource.Reset();
            ctx->duplication->ReleaseFrame();
            wait_for_gpu(ctx);

            out_frame->data = nullptr;
            out_frame->width = desc.Width;
            out_frame->height = desc.Height;
            out_frame->stride = 0;
            out_frame->format = PYROMIRROR_CAPTURE_FORMAT_BGRX;
            out_frame->gpu_texture = ctx->shared_id;
            out_frame->prepare_us = prepare_us();
            return PYROMIRROR_CAPTURE_FRAME;
        }
        // No shared texture on this device; carry on with pixels.
        ctx->gpu = false;
    }

    // Copy and hand the frame back to DWM straight away; holding it stalls desktop composition.
    bool copied = copy_to_staging(ctx, tex.Get());
    tex.Reset();
    resource.Reset();
    ctx->duplication->ReleaseFrame();
    if (!copied) return PYROMIRROR_CAPTURE_ERROR;
    int result = map_staging(ctx, out_frame);
    out_frame->prepare_us = prepare_us();
    return result;
}

extern "C" bool pyromirror_capture_set_gpu(pyromirror_capture_context* ctx, bool enable) {
    if (!ctx) return false;
    if (ctx->gpu && !enable && ctx->shared) {
        ctx->shared_pending = true;
    }
    ctx->gpu = enable;
    return true;
}

extern "C" uintptr_t pyromirror_capture_export_texture(pyromirror_capture_context* ctx) {
    ComPtr<IDXGIResource1> resource;
    HANDLE handle = nullptr;
    if (!ctx || !ctx->shared || FAILED(ctx->shared.As(&resource)) ||
        FAILED(resource->CreateSharedHandle(nullptr, GENERIC_ALL, nullptr, &handle))) {
        return 0;
    }
    return reinterpret_cast<uintptr_t>(handle);
}

extern "C" bool pyromirror_capture_get_adapter_luid(pyromirror_capture_context* ctx, uint8_t* luid) {
    if (!ctx || !luid) return false;
    static_assert(sizeof(LUID) == 8, "LUID is expected to be 8 bytes");
    memcpy(luid, &ctx->adapter_luid, sizeof(LUID));
    return true;
}

extern "C" void pyromirror_capture_get_cursor(pyromirror_capture_context* ctx, pyromirror_capture_cursor* out) {
    if (!ctx || !out) return;
    out->serial = ctx->cursor_serial;
    out->in_video = false;
    out->visible = ctx->cursor_visible;
    out->width = ctx->cursor_width;
    out->height = ctx->cursor_height;
    out->hot_x = ctx->cursor_hot_x;
    out->hot_y = ctx->cursor_hot_y;
    out->rgba = ctx->cursor_rgba.data();
}

extern "C" bool pyromirror_capture_get_bounds(pyromirror_capture_context* ctx, int32_t* x, int32_t* y, uint32_t* width, uint32_t* height) {
    DXGI_OUTPUT_DESC desc = {};
    if (!ctx || FAILED(ctx->output->GetDesc(&desc))) {
        return false;
    }
    *x = desc.DesktopCoordinates.left;
    *y = desc.DesktopCoordinates.top;
    *width = static_cast<uint32_t>(desc.DesktopCoordinates.right - desc.DesktopCoordinates.left);
    *height = static_cast<uint32_t>(desc.DesktopCoordinates.bottom - desc.DesktopCoordinates.top);
    return true;
}

extern "C" bool pyromirror_capture_hdr_active(pyromirror_capture_context* ctx) {
    ComPtr<IDXGIOutput6> output6;
    DXGI_OUTPUT_DESC1 desc = {};
    if (!ctx || FAILED(ctx->output.As(&output6)) || FAILED(output6->GetDesc1(&desc))) {
        return false;
    }
    return desc.ColorSpace == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
}

extern "C" const char* pyromirror_capture_last_error(pyromirror_capture_context* ctx) {
    return ctx ? ctx->last_error.c_str() : "";
}

extern "C" void pyromirror_capture_destroy(pyromirror_capture_context* ctx) {
    if (!ctx) return;
    pyromirror_capture_release(ctx);
    delete ctx;
}

#endif // _WIN32
