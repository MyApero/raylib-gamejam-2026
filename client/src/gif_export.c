// Minimal C bridge around raylib's bundled msf_gif encoder. It keeps GIF
// encoding out of the browser JavaScript thread and presents a tiny FFI to
// the Emscripten Rust client.
#define MSF_GIF_IMPL
#include "../../raylib/examples/core/msf_gif.h"

#include <stddef.h>

static MsfGifState state;
static MsfGifResult completed;
static int active = 0;

int hexel_gif_begin(int width, int height) {
    if (active) {
        MsfGifResult abandoned = msf_gif_end(&state);
        msf_gif_free(abandoned);
    }
    state = (MsfGifState){ 0 };
    msf_gif_begin(&state, width, height);
    active = 1;
    return 1;
}

void hexel_gif_frame(unsigned char *rgba, int delay_cs, int pitch) {
    // 8-bit palette keeps the browser heap bounded for a multi-minute
    // history while still preserving the game's crisp pixel-art look.
    if (active && rgba != NULL) msf_gif_frame(&state, rgba, delay_cs, 8, pitch);
}

unsigned char *hexel_gif_end(size_t *length) {
    if (length != NULL) *length = 0;
    if (!active) return NULL;
    completed = msf_gif_end(&state);
    active = 0;
    if (length != NULL) *length = completed.dataSize;
    return completed.data;
}

void hexel_gif_free(void *data) {
    if (data != NULL && data == completed.data) {
        msf_gif_free(completed);
        completed = (MsfGifResult){ 0 };
    }
}
