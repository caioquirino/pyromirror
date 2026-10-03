#if defined(__linux__)

// PipeWire screen capture.
//
// The node and the PipeWire remote come from the xdg-desktop-portal ScreenCast session set up on
// the Rust side (src/portal.rs). Frames are requested as CPU-mappable buffers (MemFd / MemPtr),
// copied out on the PipeWire thread, and handed to the caller from there.

#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <chrono>
#include <condition_variable>
#include <mutex>
#include <string>
#include <vector>

#include <pipewire/pipewire.h>
#include <spa/buffer/meta.h>
#include <spa/param/buffers.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/video/raw.h>

#include "../include/pyromirror_capture.h"

struct pyromirror_capture_context {
    struct pw_thread_loop* loop = nullptr;
    struct pw_context* context = nullptr;
    struct pw_core* core = nullptr;
    struct pw_stream* stream = nullptr;
    struct spa_hook stream_listener = {};
    struct pw_stream_events events = {};

    // Negotiated format; only touched on the PipeWire thread.
    uint32_t width = 0;
    uint32_t height = 0;
    uint32_t format = PYROMIRROR_CAPTURE_FORMAT_BGRX;
    bool streaming = false;

    // Shared between the PipeWire thread and the caller.
    std::mutex mutex;
    std::condition_variable cond;
    std::vector<uint8_t> back;
    pyromirror_capture_frame back_info = {};
    bool fresh = false;
    bool failed = false;
    std::string last_error;

    // Owned by the caller between acquire calls.
    std::vector<uint8_t> front;
};

static void fail(pyromirror_capture_context* ctx, const std::string& message) {
    std::lock_guard<std::mutex> lock(ctx->mutex);
    ctx->failed = true;
    ctx->last_error = message;
    ctx->cond.notify_all();
}

static void on_state_changed(void* data, enum pw_stream_state, enum pw_stream_state state, const char* error) {
    auto* ctx = static_cast<pyromirror_capture_context*>(data);
    if (state == PW_STREAM_STATE_STREAMING) {
        ctx->streaming = true;
    } else if (state == PW_STREAM_STATE_ERROR) {
        fail(ctx, std::string("PipeWire stream error: ") + (error ? error : "unknown"));
    } else if (state == PW_STREAM_STATE_UNCONNECTED && ctx->streaming) {
        fail(ctx, "PipeWire stream ended (screen sharing was stopped)");
    }
}

static void on_param_changed(void* data, uint32_t id, const struct spa_pod* param) {
    auto* ctx = static_cast<pyromirror_capture_context*>(data);
    if (!param || id != SPA_PARAM_Format) return;

    struct spa_video_info_raw info = {};
    if (spa_format_video_raw_parse(param, &info) < 0) {
        fail(ctx, "could not parse the negotiated PipeWire video format");
        return;
    }

    switch (info.format) {
    case SPA_VIDEO_FORMAT_BGRx:
    case SPA_VIDEO_FORMAT_BGRA:
        ctx->format = PYROMIRROR_CAPTURE_FORMAT_BGRX;
        break;
    case SPA_VIDEO_FORMAT_RGBx:
    case SPA_VIDEO_FORMAT_RGBA:
        ctx->format = PYROMIRROR_CAPTURE_FORMAT_RGBX;
        break;
    default:
        fail(ctx, "compositor negotiated an unsupported pixel format");
        return;
    }
    ctx->width = info.size.width;
    ctx->height = info.size.height;

    // Ask for buffers we can read from the CPU; without this a compositor may hand out DMA-BUFs,
    // which are not generally mappable.
    uint8_t buffer[512];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
    const struct spa_pod* params[1];
    params[0] = static_cast<const struct spa_pod*>(spa_pod_builder_add_object(&b,
        SPA_TYPE_OBJECT_ParamBuffers, SPA_PARAM_Buffers,
        SPA_PARAM_BUFFERS_dataType, SPA_POD_CHOICE_FLAGS_Int((1 << SPA_DATA_MemPtr) | (1 << SPA_DATA_MemFd))));
    pw_stream_update_params(ctx->stream, params, 1);
}

static void on_process(void* data) {
    auto* ctx = static_cast<pyromirror_capture_context*>(data);

    // Only the newest queued buffer matters.
    struct pw_buffer* newest = nullptr;
    while (struct pw_buffer* b = pw_stream_dequeue_buffer(ctx->stream)) {
        if (newest) pw_stream_queue_buffer(ctx->stream, newest);
        newest = b;
    }
    if (!newest) return;

    struct spa_buffer* buf = newest->buffer;
    const struct spa_meta_header* header = static_cast<const struct spa_meta_header*>(
        spa_buffer_find_meta_data(buf, SPA_META_Header, sizeof(struct spa_meta_header)));
    bool corrupted = header && (header->flags & SPA_META_HEADER_FLAG_CORRUPTED);

    if (!corrupted && buf->n_datas > 0 && ctx->width > 0 && ctx->height > 0) {
        const struct spa_data* d = &buf->datas[0];
        const struct spa_chunk* chunk = d->chunk;
        bool usable = d->data && chunk && chunk->size > 0 && !(chunk->flags & SPA_CHUNK_FLAG_CORRUPTED);
        if (usable) {
            uint32_t min_stride = ctx->width * 4;
            uint32_t stride = chunk->stride > 0 ? static_cast<uint32_t>(chunk->stride) : min_stride;
            uint64_t needed = static_cast<uint64_t>(stride) * (ctx->height - 1) + min_stride;
            if (stride >= min_stride && static_cast<uint64_t>(chunk->offset) + needed <= d->maxsize) {
                const uint8_t* src = static_cast<const uint8_t*>(d->data) + chunk->offset;
                std::lock_guard<std::mutex> lock(ctx->mutex);
                ctx->back.resize(needed);
                memcpy(ctx->back.data(), src, needed);
                ctx->back_info.width = ctx->width;
                ctx->back_info.height = ctx->height;
                ctx->back_info.stride = stride;
                ctx->back_info.format = ctx->format;
                ctx->fresh = true;
                ctx->cond.notify_all();
            }
        }
    }

    pw_stream_queue_buffer(ctx->stream, newest);
}

static const struct spa_pod* build_format(struct spa_pod_builder* b) {
    struct spa_rectangle def_size = SPA_RECTANGLE(1920, 1080);
    struct spa_rectangle min_size = SPA_RECTANGLE(1, 1);
    struct spa_rectangle max_size = SPA_RECTANGLE(16384, 16384);
    struct spa_fraction def_rate = SPA_FRACTION(60, 1);
    struct spa_fraction min_rate = SPA_FRACTION(0, 1);
    struct spa_fraction max_rate = SPA_FRACTION(1000, 1);

    // No SPA_FORMAT_VIDEO_modifier property: that restricts negotiation to shared-memory buffers.
    return static_cast<const struct spa_pod*>(spa_pod_builder_add_object(b,
        SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        SPA_FORMAT_VIDEO_format, SPA_POD_CHOICE_ENUM_Id(5,
            SPA_VIDEO_FORMAT_BGRx,
            SPA_VIDEO_FORMAT_BGRx, SPA_VIDEO_FORMAT_BGRA,
            SPA_VIDEO_FORMAT_RGBx, SPA_VIDEO_FORMAT_RGBA),
        SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(&def_size, &min_size, &max_size),
        SPA_FORMAT_VIDEO_framerate, SPA_POD_CHOICE_RANGE_Fraction(&def_rate, &min_rate, &max_rate)));
}

static void destroy(pyromirror_capture_context* ctx) {
    if (ctx->loop) pw_thread_loop_stop(ctx->loop);
    if (ctx->stream) pw_stream_destroy(ctx->stream);
    if (ctx->core) pw_core_disconnect(ctx->core);
    if (ctx->context) pw_context_destroy(ctx->context);
    if (ctx->loop) pw_thread_loop_destroy(ctx->loop);
    delete ctx;
}

extern "C" pyromirror_capture_context* pyromirror_capture_create(const pyromirror_capture_config* config, char* error, uint32_t error_size) {
    auto set_error = [&](const char* message) {
        if (error && error_size) snprintf(error, error_size, "%s", message);
    };
    if (!config || config->pipewire_fd < 0) {
        set_error("no PipeWire remote was provided");
        return nullptr;
    }

    pw_init(nullptr, nullptr);

    auto* ctx = new pyromirror_capture_context();
    int fd = config->pipewire_fd;

    ctx->loop = pw_thread_loop_new("pyromirror-pw", nullptr);
    if (ctx->loop) ctx->context = pw_context_new(pw_thread_loop_get_loop(ctx->loop), nullptr, 0);
    if (!ctx->loop || !ctx->context || pw_thread_loop_start(ctx->loop) < 0) {
        set_error("failed to start a PipeWire thread loop");
        close(fd);
        destroy(ctx);
        return nullptr;
    }

    pw_thread_loop_lock(ctx->loop);

    // Takes ownership of fd, also on failure.
    ctx->core = pw_context_connect_fd(ctx->context, fd, nullptr, 0);
    if (!ctx->core) {
        pw_thread_loop_unlock(ctx->loop);
        set_error("failed to connect to the PipeWire remote from the portal");
        destroy(ctx);
        return nullptr;
    }

    ctx->stream = pw_stream_new(ctx->core, "PyroMirror screen capture",
        pw_properties_new(
            PW_KEY_MEDIA_TYPE, "Video",
            PW_KEY_MEDIA_CATEGORY, "Capture",
            PW_KEY_MEDIA_ROLE, "Screen",
            nullptr));
    if (!ctx->stream) {
        pw_thread_loop_unlock(ctx->loop);
        set_error("failed to create a PipeWire stream");
        destroy(ctx);
        return nullptr;
    }

    ctx->events.version = PW_VERSION_STREAM_EVENTS;
    ctx->events.state_changed = on_state_changed;
    ctx->events.param_changed = on_param_changed;
    ctx->events.process = on_process;
    pw_stream_add_listener(ctx->stream, &ctx->stream_listener, &ctx->events, ctx);

    uint8_t buffer[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
    const struct spa_pod* params[1] = { build_format(&b) };

    int res = pw_stream_connect(ctx->stream, PW_DIRECTION_INPUT, config->pipewire_node,
        static_cast<enum pw_stream_flags>(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS),
        params, 1);
    pw_thread_loop_unlock(ctx->loop);

    if (res < 0) {
        char message[128];
        snprintf(message, sizeof(message), "pw_stream_connect failed: %s", strerror(-res));
        set_error(message);
        destroy(ctx);
        return nullptr;
    }
    return ctx;
}

extern "C" int pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame) {
    if (!ctx || !out_frame) return PYROMIRROR_CAPTURE_ERROR;

    std::unique_lock<std::mutex> lock(ctx->mutex);
    ctx->cond.wait_for(lock, std::chrono::milliseconds(timeout_ms), [&] { return ctx->fresh || ctx->failed; });
    if (ctx->failed) return PYROMIRROR_CAPTURE_ERROR;
    if (!ctx->fresh) return PYROMIRROR_CAPTURE_NO_FRAME;

    ctx->front.swap(ctx->back);
    ctx->fresh = false;
    *out_frame = ctx->back_info;
    out_frame->data = ctx->front.data();
    return PYROMIRROR_CAPTURE_FRAME;
}

extern "C" void pyromirror_capture_release(pyromirror_capture_context*) {
}

extern "C" bool pyromirror_capture_hdr_active(pyromirror_capture_context*) {
    return false;
}

extern "C" const char* pyromirror_capture_last_error(pyromirror_capture_context* ctx) {
    if (!ctx) return "";
    std::lock_guard<std::mutex> lock(ctx->mutex);
    // Stable for the caller: last_error is only written once, when the stream fails.
    return ctx->last_error.c_str();
}

extern "C" void pyromirror_capture_destroy(pyromirror_capture_context* ctx) {
    if (ctx) destroy(ctx);
}

#endif // __linux__
