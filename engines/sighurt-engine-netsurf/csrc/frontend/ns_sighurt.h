/* Sighurt's NetSurf frontend API. GPL-2.0-only, as NetSurf is.
 *
 * Rust calls only this header. It names no NetSurf type. Every call is on the
 * owner thread. One successful nsh_init per process: a later one fails.
 */
#ifndef NS_SIGHURT_H
#define NS_SIGHURT_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct gui_window nsh_view;

typedef struct nsh_bytes {
	const uint8_t *data;
	size_t len;
} nsh_bytes;

/* Pointers are borrowed. css_* and messages stay valid until nsh_shutdown.
 * The other strings need only live for the call. NULL user_agent, ca_bundle
 * and profile_dir mean "none". */
typedef struct nsh_config {
	const char *user_agent;
	const char *ca_bundle;
	const char *profile_dir;
	const char *font_sans;
	const char *font_sans_bold;
	const char *font_serif;
	const char *font_mono;
	nsh_bytes css_default;
	nsh_bytes css_quirks;
	nsh_bytes css_internal;
	nsh_bytes css_adblock;
	nsh_bytes messages;
} nsh_config;

typedef struct nsh_rect {
	int x0, y0, x1, y1;
} nsh_rect;

enum nsh_event_kind {
	NSH_EV_TITLE = 1,
	NSH_EV_URL = 2,
	NSH_EV_STATUS = 3,
	NSH_EV_POINTER = 4,
	NSH_EV_LOADING = 5, /* i0 is 1 while loading, 0 when it stopped */
	NSH_EV_FRAME = 6,
	NSH_EV_CARET = 7, /* i0, i1, i2: x, y, height */
	NSH_EV_REMOVE_CARET = 8,
	NSH_EV_NEW_CONTENT = 9,
	NSH_EV_LAUNCH = 10, /* text is the url */
	NSH_EV_CLIPBOARD = 11, /* text is utf-8; never logged */
	NSH_EV_DOWNLOAD = 12, /* u0 is the id; i0 is 0 created, 1 done, 2 error */
	NSH_EV_OPEN = 13, /* view_id is the opener; text is the url */
	NSH_EV_FILE = 14, /* a file input opened; answer with nsh_file_answer */
	NSH_EV_CONSOLE = 15 /* i0 is how many messages were coalesced; no text */
};

/* text is valid until the next nsh_next_event, nsh_poll, nsh_view_close or
 * nsh_shutdown. Copy it before then. */
typedef struct nsh_event {
	uint32_t kind;
	uint64_t view_id;
	int32_t i0, i1, i2, i3;
	uint64_t u0;
	const char *text;
	size_t text_len;
} nsh_event;

enum { NSH_FD_MAX = 64 };

/* timeout_ms: -1 nothing page-timed is waiting, 0 work is due, else milliseconds.
 * Housekeeping (cache cleans) is not in timeout_ms except a wake at most once
 * a minute while a view is live. maxfd is -1 when no fetcher descriptor is open. */
typedef struct nsh_poll_info {
	int timeout_ms;
	int maxfd;
	int nread, nwrite, nexcept;
	int read_fds[NSH_FD_MAX];
	int write_fds[NSH_FD_MAX];
	int except_fds[NSH_FD_MAX];
} nsh_poll_info;

/* Same bits as NetSurf's browser_mouse_state, so the values pass straight through. */
#define NSH_MOUSE_HOVER 0u
#define NSH_MOUSE_PRESS_1 (1u << 0)
#define NSH_MOUSE_PRESS_2 (1u << 1)
#define NSH_MOUSE_CLICK_1 (1u << 2)
#define NSH_MOUSE_CLICK_2 (1u << 3)
#define NSH_MOUSE_DOUBLE_CLICK (1u << 4)
#define NSH_MOUSE_TRIPLE_CLICK (1u << 5)
#define NSH_MOUSE_DRAG_1 (1u << 6)
#define NSH_MOUSE_DRAG_2 (1u << 7)
#define NSH_MOUSE_DRAG_ON (1u << 8)
#define NSH_MOUSE_HOLDING_1 (1u << 9)
#define NSH_MOUSE_HOLDING_2 (1u << 10)
#define NSH_MOUSE_MOD_1 (1u << 11) /* Shift */
#define NSH_MOUSE_MOD_2 (1u << 12) /* Ctrl */
#define NSH_MOUSE_MOD_3 (1u << 13) /* Alt */
#define NSH_MOUSE_LEAVE (1u << 14)

/* Same values as NetSurf's NS_KEY_*. A Unicode scalar below 0x20 that is not one of
 * these is still delivered as itself when the caller passes it. */
#define NSH_KEY_SELECT_ALL 1u
#define NSH_KEY_COPY_SELECTION 3u
#define NSH_KEY_DELETE_LEFT 8u
#define NSH_KEY_TAB 9u
#define NSH_KEY_NL 10u
#define NSH_KEY_SHIFT_TAB 11u
#define NSH_KEY_CR 13u
#define NSH_KEY_DELETE_LINE 21u
#define NSH_KEY_PASTE 22u
#define NSH_KEY_CUT_SELECTION 24u
#define NSH_KEY_CLEAR_SELECTION 26u
#define NSH_KEY_ESCAPE 27u
#define NSH_KEY_LEFT 28u
#define NSH_KEY_RIGHT 29u
#define NSH_KEY_UP 30u
#define NSH_KEY_DOWN 31u
#define NSH_KEY_DELETE_RIGHT 127u
#define NSH_KEY_LINE_START 128u
#define NSH_KEY_LINE_END 129u
#define NSH_KEY_TEXT_START 130u
#define NSH_KEY_TEXT_END 131u
#define NSH_KEY_WORD_LEFT 132u
#define NSH_KEY_DELETE_WORD_LEFT 133u
#define NSH_KEY_WORD_RIGHT 134u
#define NSH_KEY_DELETE_WORD_RIGHT 135u
#define NSH_KEY_PAGE_UP 136u
#define NSH_KEY_PAGE_DOWN 137u
#define NSH_KEY_DELETE_LINE_END 138u
#define NSH_KEY_DELETE_LINE_START 139u
#define NSH_KEY_UNDO 140u
#define NSH_KEY_REDO 141u

/* 0 not started, 1 running, 2 finished (shutdown, or an init that failed). */
int nsh_state(void);

/* 0 on success. Non-zero if this process already started NetSurf, or init failed.
 * A failed init is not retried. */
int nsh_init(const nsh_config *cfg);
void nsh_shutdown(void);

/* w or h below 1 becomes 1. scale_120 below 1 becomes 120. Empty metrics are the
 * caller's concern: pass the last real size. */
nsh_view *nsh_view_open(uint64_t view_id, int w, int h, int scale_120);
void nsh_view_close(nsh_view *view);
void nsh_view_navigate(nsh_view *view, const char *url);
void nsh_view_reload(nsh_view *view);
void nsh_view_stop(nsh_view *view);
void nsh_view_back(nsh_view *view);
void nsh_view_forward(nsh_view *view);
int nsh_view_back_available(const nsh_view *view);
int nsh_view_forward_available(const nsh_view *view);

/* w or h below 1 keeps the last size. scale_120 below 1 keeps the last scale. */
void nsh_view_resize(nsh_view *view, int w, int h, int scale_120);
void nsh_view_set_scale(nsh_view *view, double zoom);
void nsh_view_set_hidden(nsh_view *view, int hidden);
int nsh_view_hidden(const nsh_view *view);
int nsh_view_width(const nsh_view *view);
int nsh_view_height(const nsh_view *view);
int nsh_view_scale_120(const nsh_view *view);

/* Positions are buffer pixels. The core divides by its scale, which already
 * includes the display scale, so the caller does not divide again. */
void nsh_view_mouse(nsh_view *view, int x, int y, unsigned state, unsigned mods);
void nsh_view_scroll(nsh_view *view, int x, int y, int dx, int dy);
/* Returns 1 if the core consumed the key. */
int nsh_view_key(nsh_view *view, uint32_t key);

/* Copies the link url at a buffer position. Returns 1 if there was one. */
int nsh_view_link_at(nsh_view *view, int x, int y, char *buf, size_t buflen);
/* 0 none, 1 text field, 2 file input. */
int nsh_view_form_at(nsh_view *view, int x, int y);
/* Copies the core's selection. Returns the byte length, or -1. */
int nsh_view_selection(nsh_view *view, char *buf, size_t buflen);

/* stride is bytes per row. clip NULL paints the whole buffer. Returns 1 if it drew. */
int nsh_view_paint(nsh_view *view, uint32_t *bgra, int w, int h, size_t stride, const nsh_rect *clip);
/* 0 clean, 1 a rectangle (written to out), 2 the whole view. */
int nsh_view_dirty_rect(const nsh_view *view, nsh_rect *out);

/* Runs due callbacks, then polls fetchers. Fills info. Returns 0. */
int nsh_poll(nsh_poll_info *info);
/* Returns 1 and fills out, or 0 if the queue is empty. */
size_t nsh_next_event(nsh_event *out);

void nsh_clipboard_set_shadow(const char *utf8, size_t len);
void nsh_download_answer(uint64_t id, int accept);
/* path NULL or empty cancels. */
void nsh_file_answer(uint64_t view_id, const char *path);
/* Borrows the buffered body until the download is released. */
int nsh_download_info(uint64_t id, char *url, size_t url_len, char *name, size_t name_len,
		      char *mime, size_t mime_len, const uint8_t **data, size_t *len);

#ifdef __cplusplus
}
#endif

#endif
