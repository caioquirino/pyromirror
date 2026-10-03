// Captures what the default audio output is playing (its monitor) through PipeWire, as
// interleaved 16-bit stereo at 48 kHz. PipeWire does any resampling and channel mixing.

#include <stdint.h>
#include <stdio.h>
#include <string.h>

#include <pipewire/pipewire.h>
#include <spa/param/audio/format-utils.h>

extern "C" {
typedef void (*pyromirror_audio_callback)(void* user, const int16_t* samples, uint32_t frames);
struct pyromirror_audio_context;
pyromirror_audio_context* pyromirror_audio_create(pyromirror_audio_callback callback, void* user, char* error, uint32_t error_size);
void pyromirror_audio_destroy(pyromirror_audio_context* ctx);
}

static const uint32_t RATE = 48000;
static const uint32_t CHANNELS = 2;

struct pyromirror_audio_context {
    struct pw_thread_loop* loop = nullptr;
    struct pw_stream* stream = nullptr;
    struct pw_stream_events events = {};
    pyromirror_audio_callback callback = nullptr;
    void* user = nullptr;
};

static void on_process(void* data) {
    auto* ctx = static_cast<pyromirror_audio_context*>(data);
    struct pw_buffer* b = pw_stream_dequeue_buffer(ctx->stream);
    if (!b) return;

    const struct spa_data* d = &b->buffer->datas[0];
    if (d->data && d->chunk && d->chunk->size > 0 &&
        static_cast<uint64_t>(d->chunk->offset) + d->chunk->size <= d->maxsize) {
        const uint8_t* samples = static_cast<const uint8_t*>(d->data) + d->chunk->offset;
        uint32_t frames = d->chunk->size / (sizeof(int16_t) * CHANNELS);
        if (frames > 0) {
            ctx->callback(ctx->user, reinterpret_cast<const int16_t*>(samples), frames);
        }
    }
    pw_stream_queue_buffer(ctx->stream, b);
}

extern "C" void pyromirror_audio_destroy(pyromirror_audio_context* ctx) {
    if (!ctx) return;
    if (ctx->loop) pw_thread_loop_stop(ctx->loop);
    if (ctx->stream) pw_stream_destroy(ctx->stream);
    if (ctx->loop) pw_thread_loop_destroy(ctx->loop);
    delete ctx;
}

extern "C" pyromirror_audio_context* pyromirror_audio_create(pyromirror_audio_callback callback, void* user, char* error, uint32_t error_size) {
    auto fail = [&](pyromirror_audio_context* ctx, const char* message) -> pyromirror_audio_context* {
        if (error && error_size) snprintf(error, error_size, "%s", message);
        pyromirror_audio_destroy(ctx);
        return nullptr;
    };

    pw_init(nullptr, nullptr);

    auto* ctx = new pyromirror_audio_context();
    ctx->callback = callback;
    ctx->user = user;

    ctx->loop = pw_thread_loop_new("pyromirror-audio", nullptr);
    if (!ctx->loop) return fail(ctx, "failed to create a PipeWire thread loop");

    ctx->events.version = PW_VERSION_STREAM_EVENTS;
    ctx->events.process = on_process;

    // stream.capture.sink makes this record the output device's monitor instead of a microphone.
    struct pw_properties* props = pw_properties_new(
        PW_KEY_MEDIA_TYPE, "Audio",
        PW_KEY_MEDIA_CATEGORY, "Capture",
        PW_KEY_MEDIA_ROLE, "Music",
        PW_KEY_STREAM_CAPTURE_SINK, "true",
        PW_KEY_NODE_LATENCY, "256/48000",
        nullptr);
    ctx->stream = pw_stream_new_simple(pw_thread_loop_get_loop(ctx->loop), "PyroMirror audio capture", props, &ctx->events, ctx);
    if (!ctx->stream) return fail(ctx, "failed to create a PipeWire audio stream (is PipeWire running?)");

    struct spa_audio_info_raw info = {};
    info.format = SPA_AUDIO_FORMAT_S16_LE;
    info.rate = RATE;
    info.channels = CHANNELS;
    info.position[0] = SPA_AUDIO_CHANNEL_FL;
    info.position[1] = SPA_AUDIO_CHANNEL_FR;

    uint8_t buffer[1024];
    struct spa_pod_builder b = SPA_POD_BUILDER_INIT(buffer, sizeof(buffer));
    const struct spa_pod* params[1] = { spa_format_audio_raw_build(&b, SPA_PARAM_EnumFormat, &info) };

    int res = pw_stream_connect(ctx->stream, PW_DIRECTION_INPUT, PW_ID_ANY,
        static_cast<enum pw_stream_flags>(PW_STREAM_FLAG_AUTOCONNECT | PW_STREAM_FLAG_MAP_BUFFERS | PW_STREAM_FLAG_RT_PROCESS),
        params, 1);
    if (res < 0) {
        char message[128];
        snprintf(message, sizeof(message), "pw_stream_connect failed: %s", strerror(-res));
        return fail(ctx, message);
    }

    if (pw_thread_loop_start(ctx->loop) < 0) return fail(ctx, "failed to start the PipeWire thread loop");
    return ctx;
}
