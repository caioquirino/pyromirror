#if defined(_WIN32)

// DXGI Desktop Duplication capture.
//
// Each changed desktop image is copied into a CPU-readable staging texture and handed out as a
// mapped BGRA pointer.

#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <dxgi1_6.h>
#include <d3d11.h>
#include <wrl/client.h>
#include <stdio.h>
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

static HRESULT duplicate(pyromirror_capture_context* ctx) {
    ctx->duplication.Reset();
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

    // LastPresentTime == 0 means only the mouse pointer moved; the desktop image is unchanged.
    ComPtr<ID3D11Texture2D> tex;
    if (info.LastPresentTime.QuadPart == 0 || FAILED(resource.As(&tex))) {
        ctx->duplication->ReleaseFrame();
        return PYROMIRROR_CAPTURE_NO_FRAME;
    }

    D3D11_TEXTURE2D_DESC desc = {};
    tex->GetDesc(&desc);
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
        hr = ctx->device->CreateTexture2D(&staging_desc, nullptr, &ctx->staging);
        if (FAILED(hr)) {
            ctx->duplication->ReleaseFrame();
            ctx->last_error = hr_message("CreateTexture2D(staging)", hr);
            return PYROMIRROR_CAPTURE_ERROR;
        }
        ctx->staging_desc = staging_desc;
    }

    // Copy and hand the frame back to DWM straight away; holding it stalls desktop composition.
    ctx->context->CopyResource(ctx->staging.Get(), tex.Get());
    tex.Reset();
    resource.Reset();
    ctx->duplication->ReleaseFrame();

    D3D11_MAPPED_SUBRESOURCE mapped = {};
    hr = ctx->context->Map(ctx->staging.Get(), 0, D3D11_MAP_READ, 0, &mapped);
    if (FAILED(hr)) {
        ctx->last_error = hr_message("ID3D11DeviceContext::Map", hr);
        return PYROMIRROR_CAPTURE_ERROR;
    }
    ctx->mapped = true;

    // Desktop duplication always delivers DXGI_FORMAT_B8G8R8A8_UNORM.
    out_frame->data = static_cast<const uint8_t*>(mapped.pData);
    out_frame->width = desc.Width;
    out_frame->height = desc.Height;
    out_frame->stride = mapped.RowPitch;
    out_frame->format = PYROMIRROR_CAPTURE_FORMAT_BGRX;
    return PYROMIRROR_CAPTURE_FRAME;
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
