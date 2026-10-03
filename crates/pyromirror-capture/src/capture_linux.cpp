#if !defined(_WIN32)

#include <stdint.h>
#include <stdbool.h>
#include <stdlib.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sys/mman.h>
#include <mutex>
#include <condition_variable>
#include <vector>
#include <chrono>
#include <algorithm>

#include <pipewire/pipewire.h>
#include <spa/param/video/format-utils.h>
#include <spa/param/video/raw.h>
#include <spa/param/buffers.h>
#include <spa/buffer/meta.h>
#include <spa/debug/types.h>
#include <dbus/dbus.h>

#include "../include/pyromirror_capture.h"

struct pyromirror_capture_context {
    struct pw_thread_loop* loop = nullptr;
    struct pw_context* context = nullptr;
    struct pw_core* core = nullptr;
    struct pw_stream* stream = nullptr;
    struct spa_hook stream_listener = {};

    std::mutex mutex;
    std::condition_variable cond;
    std::vector<uint8_t> current_frame_pixels;

    uint32_t width = 1920;
    uint32_t height = 1080;
    uint32_t stride = 0;
    uint32_t format = 0;
    bool format_is_bgr = true;
    bool format_negotiated = false;
    uintptr_t dmabuf_fd = 0;
    bool has_frame = false;
    bool active = false;
    uint64_t timeline = 0;
};

/// Attempts to request a screencast stream from KDE Plasma KWin via D-Bus
static uint32_t request_kwin_stream_node() {
    DBusError err;
    dbus_error_init(&err);

    DBusConnection* conn = dbus_bus_get(DBUS_BUS_SESSION, &err);
    if (!conn) {
        fprintf(stderr, "[pyromirror-capture] D-Bus session bus unavailable: %s\n", err.message ? err.message : "unknown");
        dbus_error_free(&err);
        return 0;
    }

    const char* paths[] = { "/org/kde/KWin/ScreenCast", "/ScreenCast", "/org/kde/kwin/ScreenCast" };
    uint32_t node_id = 0;

    for (const char* path : paths) {
        DBusMessage* msg = dbus_message_new_method_call(
            "org.kde.KWin",
            path,
            "org.kde.KWin.ScreenCast",
            "streamOutput"
        );
        if (!msg) continue;

        const char* output_name = "";
        uint32_t mask = 1; // 1 = embedded cursor
        dbus_message_append_args(msg,
            DBUS_TYPE_STRING, &output_name,
            DBUS_TYPE_UINT32, &mask,
            DBUS_TYPE_INVALID);

        DBusMessage* reply = dbus_connection_send_with_reply_and_block(conn, msg, 1500, &err);
        dbus_message_unref(msg);

        if (reply) {
            DBusMessageIter iter;
            if (dbus_message_iter_init(reply, &iter)) {
                do {
                    int type = dbus_message_iter_get_arg_type(&iter);
                    if (type == DBUS_TYPE_UINT32) {
                        dbus_message_iter_get_basic(&iter, &node_id);
                        break;
                    }
                } while (dbus_message_iter_next(&iter));
            }
            dbus_message_unref(reply);
            if (node_id > 0) {
                fprintf(stderr, "[pyromirror-capture] Acquired KWin ScreenCast PipeWire node: %u\n", node_id);
                dbus_connection_unref(conn);
                return node_id;
            }
        }
        dbus_error_free(&err);
    }

    dbus_connection_unref(conn);
    return 0;
}

static void on_stream_state_changed(void *data, enum pw_stream_state old_state, enum pw_stream_state state, const char *error) {
    auto* ctx = static_cast<pyromirror_capture_context*>(data);
    fprintf(stderr, "[pyromirror-capture] PipeWire stream state: %s -> %s%s%s\n",
            pw_stream_state_as_string(old_state),
            pw_stream_state_as_string(state),
            error ? ": " : "", error ? error : "");
    if (state == PW_STREAM_STATE_PAUSED) {
        pw_stream_set_active(ctx->stream, true);
    }
}

static void on_stream_param_changed(void *data, uint32_t id, const struct spa_pod *param) {
    if (!param || id != SPA_PARAM_Format) return;
    auto* ctx = static_cast<pyromirror_capture_context*>(data);

    struct spa_video_info_raw info = {};
    if (spa_format_video_raw_parse(param, &info) >= 0) {
        std::lock_guard<std::mutex> lock(ctx->mutex);
        if (info.size.width > 0 && info.size.height > 0) {
            ctx->width = info.size.width;
            ctx->height = info.size.height;
        }
        ctx->format = info.format;
        ctx->format_is_bgr = (info.format == SPA_VIDEO_FORMAT_BGRx ||
                              info.format == SPA_VIDEO_FORMAT_BGRA ||
                              info.format == SPA_VIDEO_FORMAT_xBGR_210LE ||
                              info.format == SPA_VIDEO_FORMAT_ABGR_210LE);
        ctx->format_negotiated = true;
        fprintf(stderr, "[pyromirror-capture] Negotiated video format: %ux%u (format=%u, bgr=%d)\n",
                ctx->width, ctx->height, info.format, ctx->format_is_bgr);
    }

    pw_thread_loop_signal(ctx->loop, false);
}

static const struct spa_pod* build_video_enum_format(struct spa_pod_builder* b, bool with_modifiers) {
    struct spa_pod_frame f[2];
    spa_pod_builder_push_object(b, &f[0], SPA_TYPE_OBJECT_Format, SPA_PARAM_EnumFormat);
    spa_pod_builder_add(b,
        SPA_FORMAT_mediaType, SPA_POD_Id(SPA_MEDIA_TYPE_video),
        SPA_FORMAT_mediaSubtype, SPA_POD_Id(SPA_MEDIA_SUBTYPE_raw),
        0);

    // Provide complete list of formats supported by modern Wayland / KWin / PipeWire
    spa_pod_builder_prop(b, SPA_FORMAT_VIDEO_format, 0);
    spa_pod_builder_push_choice(b, &f[1], SPA_CHOICE_Enum, 0);
    // Preferred default format
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_BGRx);
    // Accepted formats
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_BGRx);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_BGRA);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_RGBx);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_RGBA);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_xBGR);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_xRGB);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_ABGR);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_ARGB);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_BGR);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_RGB);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_xBGR_210LE);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_ABGR_210LE);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_NV12);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_YUY2);
    spa_pod_builder_id(b, SPA_VIDEO_FORMAT_I420);
    spa_pod_builder_pop(b, &f[1]);

    if (with_modifiers) {
        spa_pod_builder_prop(b, SPA_FORMAT_VIDEO_modifier, SPA_POD_PROP_FLAG_MANDATORY | SPA_POD_PROP_FLAG_DONT_FIXATE);
        spa_pod_builder_push_choice(b, &f[1], SPA_CHOICE_Enum, 0);
        // Default modifier: DRM_FORMAT_MOD_INVALID (implicit modifier)
        spa_pod_builder_long(b, 0x00ffffffffffffffULL);
        spa_pod_builder_long(b, 0x00ffffffffffffffULL);
        // DRM_FORMAT_MOD_LINEAR
        spa_pod_builder_long(b, 0ULL);
        spa_pod_builder_pop(b, &f[1]);
    }

    struct spa_rectangle def_rect = SPA_RECTANGLE(1920, 1080);
    struct spa_rectangle min_rect = SPA_RECTANGLE(1, 1);
    struct spa_rectangle max_rect = SPA_RECTANGLE(8192, 8192);

    struct spa_fraction def_frac = SPA_FRACTION(60, 1);
    struct spa_fraction min_frac = SPA_FRACTION(0, 1);
    struct spa_fraction max_frac = SPA_FRACTION(1000, 1);

    spa_pod_builder_add(b,
        SPA_FORMAT_VIDEO_size, SPA_POD_CHOICE_RANGE_Rectangle(&def_rect, &min_rect, &max_rect),
        SPA_FORMAT_VIDEO_framerate, SPA_POD_CHOICE_RANGE_Fraction(&def_frac, &min_frac, &max_frac),
        0);

    return (const struct spa_pod*)spa_pod_builder_pop(b, &f[0]);
}

static void on_stream_process(void *data) {
    auto* ctx = static_cast<pyromirror_capture_context*>(data);
    struct pw_buffer *b = pw_stream_dequeue_buffer(ctx->stream);
    if (!b) return;

    struct spa_buffer *buf = b->buffer;
    if (buf && buf->n_datas > 0) {
        struct spa_data *d = &buf->datas[0];

        // If producer flagged buffer as corrupted (no damage/empty frame), recycle it
        if (d->chunk && (d->chunk->flags & SPA_CHUNK_FLAG_CORRUPTED)) {
            pw_stream_queue_buffer(ctx->stream, b);
            return;
        }

        std::lock_guard<std::mutex> lock(ctx->mutex);
        if (d->type == SPA_DATA_DmaBuf && d->fd >= 0) {
            ctx->dmabuf_fd = (uintptr_t)d->fd;
        }

        uint8_t *src_data = static_cast<uint8_t*>(d->data);
        uint32_t chunk_size = (d->chunk && d->chunk->size > 0) ? d->chunk->size : (ctx->width * ctx->height * 4);

        if (src_data != nullptr && chunk_size > 0) {
            uint32_t offset = d->chunk ? d->chunk->offset : 0;
            if (ctx->current_frame_pixels.size() != chunk_size) {
                ctx->current_frame_pixels.resize(chunk_size);
            }
            memcpy(ctx->current_frame_pixels.data(), src_data + offset, chunk_size);
            ctx->stride = (d->chunk && d->chunk->stride > 0) ? d->chunk->stride : (ctx->width * 4);
        } else if (d->type == SPA_DATA_DmaBuf && d->fd >= 0) {
            off_t fd_size = lseek(d->fd, 0, SEEK_END);
            uint32_t map_size = (fd_size > 0) ? static_cast<uint32_t>(fd_size) : (d->maxsize > 0 ? d->maxsize : chunk_size);
            if (map_size < chunk_size) map_size = chunk_size;

            void* mapped = mmap(NULL, map_size, PROT_READ, MAP_SHARED, d->fd, d->mapoffset);
            if (mapped != MAP_FAILED) {
                if (ctx->current_frame_pixels.size() != map_size) {
                    ctx->current_frame_pixels.resize(map_size);
                }
                memcpy(ctx->current_frame_pixels.data(), mapped, map_size);
                if (d->chunk && d->chunk->stride > 0) {
                    ctx->stride = d->chunk->stride;
                } else if (ctx->height > 0 && map_size >= ctx->height * ctx->width * 4) {
                    ctx->stride = map_size / ctx->height;
                } else {
                    ctx->stride = ctx->width * 4;
                }
                munmap(mapped, map_size);
            } else {
                static bool warned_mmap = false;
                if (!warned_mmap) {
                    fprintf(stderr, "[pyromirror-capture] Warning: mmap DMA-BUF fd %d failed: %s\n", (int)d->fd, strerror(errno));
                    warned_mmap = true;
                }
            }
        }

        ctx->has_frame = true;
        ctx->timeline++;
        ctx->cond.notify_all();
    }

    pw_stream_queue_buffer(ctx->stream, b);
}

static const struct pw_stream_events stream_events = {
    .version = PW_VERSION_STREAM_EVENTS,
    .destroy = NULL,
    .state_changed = on_stream_state_changed,
    .control_info = NULL,
    .io_changed = NULL,
    .param_changed = on_stream_param_changed,
    .add_buffer = NULL,
    .remove_buffer = NULL,
    .process = on_stream_process,
    .drained = NULL,
    .command = NULL,
    .trigger_done = NULL,
};

extern "C" pyromirror_capture_context* pyromirror_capture_create(void) {
    pw_init(NULL, NULL);

    auto* ctx = new pyromirror_capture_context();
    ctx->loop = pw_thread_loop_new("pyromirror-pw", NULL);
    if (!ctx->loop) {
        delete ctx;
        return nullptr;
    }

    ctx->context = pw_context_new(pw_thread_loop_get_loop(ctx->loop), NULL, 0);
    if (!ctx->context) {
        pw_thread_loop_destroy(ctx->loop);
        delete ctx;
        return nullptr;
    }

    if (pw_thread_loop_start(ctx->loop) < 0) {
        pw_context_destroy(ctx->context);
        pw_thread_loop_destroy(ctx->loop);
        delete ctx;
        return nullptr;
    }

    pw_thread_loop_lock(ctx->loop);
    ctx->core = pw_context_connect(ctx->context, NULL, 0);
    pw_thread_loop_unlock(ctx->loop);

    if (!ctx->core) {
        pw_thread_loop_stop(ctx->loop);
        pw_context_destroy(ctx->context);
        pw_thread_loop_destroy(ctx->loop);
        delete ctx;
        return nullptr;
    }

    uint32_t node_id = request_kwin_stream_node();
    if (node_id == 0) {
        node_id = PW_ID_ANY;
    }

    pw_thread_loop_lock(ctx->loop);

    char target_id_str[32];
    snprintf(target_id_str, sizeof(target_id_str), "%u", node_id);

    struct pw_properties* props = pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Video",
        PW_KEY_MEDIA_CATEGORY, "Capture",
        PW_KEY_MEDIA_ROLE, "Screen",
        NULL
    );
    if (node_id != PW_ID_ANY) {
        pw_properties_set(props, PW_KEY_TARGET_OBJECT, target_id_str);
    }

    ctx->stream = pw_stream_new(ctx->core, "PyroMirror Desktop Capture", props);
    if (!ctx->stream) {
        pw_thread_loop_unlock(ctx->loop);
        pw_core_disconnect(ctx->core);
        pw_thread_loop_stop(ctx->loop);
        pw_context_destroy(ctx->context);
        pw_thread_loop_destroy(ctx->loop);
        delete ctx;
        return nullptr;
    }

    pw_stream_add_listener(ctx->stream, &ctx->stream_listener, &stream_events, ctx);

    uint8_t buffer[4096];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
    const struct spa_pod *params[2];
    params[0] = build_video_enum_format(&b, true);  // Primary: with DMA-BUF modifiers
    params[1] = build_video_enum_format(&b, false); // Fallback: without modifiers (SHM/MemFd)

    int res = pw_stream_connect(ctx->stream,
        PW_DIRECTION_INPUT,
        node_id,
        (enum pw_stream_flags)(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS),
        params, 2);

    if (res >= 0) {
        // Wait up to 1 second for initial format negotiation so resolution is accurate
        if (!ctx->format_negotiated) {
            pw_thread_loop_timed_wait(ctx->loop, 1);
        }
    }

    pw_thread_loop_unlock(ctx->loop);

    if (res < 0) {
        fprintf(stderr, "[pyromirror-capture] Warning: pw_stream_connect failed: %s\n", strerror(-res));
    }

    ctx->active = true;
    return ctx;
}

extern "C" bool pyromirror_capture_acquire(pyromirror_capture_context* ctx, uint32_t timeout_ms, pyromirror_capture_frame* out_frame) {
    if (!ctx || !ctx->active) return false;

    std::unique_lock<std::mutex> lock(ctx->mutex);
    if (!ctx->has_frame) {
        if (!ctx->cond.wait_for(lock, std::chrono::milliseconds(timeout_ms), [&]{ return ctx->has_frame; })) {
            return false;
        }
    }

    out_frame->os_handle = ctx->dmabuf_fd;
    out_frame->width = ctx->width;
    out_frame->height = ctx->height;
    out_frame->format = 0; // DRM_FORMAT_ARGB8888
    out_frame->fence_handle = 0;
    out_frame->timeline_value = ctx->timeline;

    return true;
}

extern "C" bool pyromirror_capture_read_pixels(pyromirror_capture_context* ctx, uint8_t* out_pixels, uint32_t pitch) {
    if (!ctx || !ctx->active || !out_pixels) return false;

    std::lock_guard<std::mutex> lock(ctx->mutex);
    if (ctx->current_frame_pixels.empty()) return false;

    uint32_t w = ctx->width;
    uint32_t h = ctx->height;
    uint32_t src_stride = ctx->stride > 0 ? ctx->stride : (w * 4);
    const uint8_t* src = ctx->current_frame_pixels.data();

    uint32_t row_copy = std::min<uint32_t>(w * 4, pitch);

    for (uint32_t y = 0; y < h; ++y) {
        const uint8_t* s = src + y * src_stride;
        uint8_t* d = out_pixels + y * pitch;
        if (ctx->format == SPA_VIDEO_FORMAT_xBGR_210LE || ctx->format == SPA_VIDEO_FORMAT_ABGR_210LE) {
            const uint32_t* src32 = reinterpret_cast<const uint32_t*>(s);
            for (uint32_t x = 0; x < w; ++x) {
                uint32_t p = src32[x];
                d[x * 4 + 0] = static_cast<uint8_t>((p >> 22) & 0xFF); // R
                d[x * 4 + 1] = static_cast<uint8_t>((p >> 12) & 0xFF); // G
                d[x * 4 + 2] = static_cast<uint8_t>((p >> 2) & 0xFF);  // B
                d[x * 4 + 3] = 255;                                    // A
            }
        } else if (ctx->format_is_bgr) {
            for (uint32_t x = 0; x < w; ++x) {
                d[x * 4 + 0] = s[x * 4 + 2]; // R <- B
                d[x * 4 + 1] = s[x * 4 + 1]; // G <- G
                d[x * 4 + 2] = s[x * 4 + 0]; // B <- R
                d[x * 4 + 3] = 255;          // A
            }
        } else {
            memcpy(d, s, row_copy);
        }
    }

    return true;
}

extern "C" void pyromirror_capture_get_resolution(pyromirror_capture_context* ctx, uint32_t* width, uint32_t* height) {
    if (!ctx || !width || !height) return;
    std::lock_guard<std::mutex> lock(ctx->mutex);
    *width = ctx->width;
    *height = ctx->height;
}

extern "C" void pyromirror_capture_release(pyromirror_capture_context* ctx) {
    (void)ctx;
}

extern "C" void pyromirror_capture_destroy(pyromirror_capture_context* ctx) {
    if (!ctx) return;
    ctx->active = false;

    if (ctx->loop) {
        pw_thread_loop_stop(ctx->loop);
    }
    if (ctx->stream) {
        pw_stream_destroy(ctx->stream);
    }
    if (ctx->core) {
        pw_core_disconnect(ctx->core);
    }
    if (ctx->context) {
        pw_context_destroy(ctx->context);
    }
    if (ctx->loop) {
        pw_thread_loop_destroy(ctx->loop);
    }

    delete ctx;
}

#endif // !_WIN32
