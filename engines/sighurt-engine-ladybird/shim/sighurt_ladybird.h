/* The C ABI between sighurt-engine-ladybird (Rust) and Ladybird's LibWebView (C++).
 *
 * Every function runs on the owner thread: the one that called lb_init. Nothing here blocks.
 * Strings are UTF-8, NUL-terminated, owned by the caller of a function, or by the shim until the
 * next call of lb_next_event for strings in an event. Views are 64-bit ids the shim allocates.
 * The library is dlopen'ed with RTLD_LOCAL by the crate; it exports only what is declared here. */
#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif
#pragma GCC visibility push(default)

#define LB_ABI_VERSION 1

/* Sizes of the Ladybird types this library was compiled against, to be compared with what the
 * libraries at run time were built with (the ABI-skew guard). */
typedef struct lb_abi {
    uint32_t version;
    uint32_t sizeof_shared_image_buffer;
    uint32_t sizeof_view_implementation;
    uint32_t sizeof_headless_web_view;
    uint32_t sizeof_bitmap;
    uint32_t vulkan_dmabuf; /* USE_VULKAN_DMABUF_IMAGES */
    /* Filled at run time from the library's own objects (see lb_abi_probe), not from headers. */
    uint32_t probe_shared_image_buffer;
    uint32_t probe_view_implementation;
} lb_abi;

void lb_abi_describe(lb_abi* out);

enum {
    LB_EV_VIEW_READY = 1,   /* the view's own blank page is loaded: it takes navigations */
    LB_EV_LOAD_START,       /* s0 = url */
    LB_EV_URL_CHANGED,      /* s0 = url: a commit or a same-document change */
    LB_EV_LOAD_FINISH,      /* s0 = url */
    LB_EV_TITLE,            /* s0 = title */
    LB_EV_LOADING,          /* i0 = 0/1 */
    LB_EV_CURSOR,           /* i0 = Gfx::StandardCursor */
    LB_EV_HOVER,            /* s0 = link URL, or NULL when the pointer left it */
    LB_EV_OPEN_REQUESTED,   /* s0 = url: the page wants a new window/tab */
    LB_EV_FRAME,            /* a new frame is ready */
    LB_EV_CRASHED,          /* i0 = reason; s0 = text */
    LB_EV_CLOSED,           /* the view is gone */
    LB_EV_HISTORY,          /* i0 = current index, i1 = count; s0 = entries, one URL per line */
    LB_EV_JS_RESULT,        /* i0 = request id; s0 = result (JSON), or NULL on failure */
    LB_EV_INPUT_METHOD,     /* i0 = 1 when a text input has focus */
    LB_EV_FIND_RESULT       /* i0 = current match, i1 = total (-1 unknown) */
};

typedef struct lb_event {
    int kind;
    uint64_t view;
    int64_t i0;
    int64_t i1;
    const char* s0;
    const char* s1;
} lb_event;

/* argv: extra Ladybird arguments after the program name; profile: directory of the profile;
 * prefix: the Ladybird prefix (its libexec/ holds the helper processes), or NULL for the one the
 * library was built against. Returns 0 on success, else a message in err (size err_len). */
int lb_init(const char* const* argv, int argc, const char* profile, const char* prefix, char* err, size_t err_len);

/* The owner loop's descriptor: readable when lb_pump has work. */
int lb_loop_fd(void);
/* Does what is ready, without waiting. Returns the milliseconds until the next timer is due
 * (-1: none), and sets *more to 1 if queued work is left. */
int64_t lb_pump(int* more);
/* 1 and fills *out with the next event, or 0. */
int lb_next_event(lb_event* out);
/* How many times the owner loop's wake descriptor fired (for the idle test). */
uint64_t lb_wake_count(void);

uint64_t lb_view_open(int width_px, int height_px, double scale, int visible, double zoom);
void lb_view_close(uint64_t view);
void lb_navigate(uint64_t view, const char* url);
void lb_reload(uint64_t view);
void lb_stop(uint64_t view);
void lb_traverse(uint64_t view, int delta);
void lb_set_viewport(uint64_t view, int width_px, int height_px, double scale);
void lb_set_visible(uint64_t view, int visible);
void lb_set_focus(uint64_t view, int focused);
void lb_set_zoom(uint64_t view, double zoom);
void lb_run_js(uint64_t view, const char* script, int64_t request);

/* Input. Positions are in device pixels of the view. modifiers: 1 alt, 2 ctrl, 4 shift, 8 super. */
void lb_input_key(uint64_t view, const char* w3c_code, const char* key, int pressed, int repeat, int modifiers, uint32_t code_point);
void lb_input_pointer_move(uint64_t view, int x, int y, int modifiers);
void lb_input_pointer_leave(uint64_t view);
/* button: 1 primary, 2 secondary, 4 middle, 8 back, 16 forward */
void lb_input_pointer_button(uint64_t view, int x, int y, int button, int pressed, int click_count, int modifiers);
void lb_input_scroll(uint64_t view, int x, int y, double dx, double dy, int precise, int modifiers);
void lb_input_commit_text(uint64_t view, const char* text);

typedef struct lb_frame {
    const uint8_t* pixels; /* BGRA8888 premultiplied, valid until the next lb_pump or lb_view_close */
    int width;             /* the valid size, in pixels */
    int height;
    size_t stride;
    int dmabuf;            /* 1 if the buffer is a Vulkan dmabuf image (still mapped) */
} lb_frame;
/* Fills *out with the view's front buffer; 0 if it has none yet. */
int lb_frame_get(uint64_t view, lb_frame* out);

void lb_shutdown(void);

#pragma GCC visibility pop
#ifdef __cplusplus
}
#endif
