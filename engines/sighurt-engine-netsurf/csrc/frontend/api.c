/* Init, shutdown, the event queue and the calls Rust makes. GPL-2.0-only. */
#include "internal.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>

#include "utils/log.h"
#include "utils/messages.h"
#include "utils/nsoption.h"
#include "netsurf/netsurf.h"
#include "netsurf/misc.h"
#include "netsurf/bitmap.h"
#include "netsurf/mouse.h"
#include "netsurf/keypress.h"
#include "content/fetch.h"
#include "content/backing_store.h"
#include "desktop/browser_history.h"

int nsh_core_state;
uint64_t nsh_input_view;
static int overflowed;

extern void user_agent_override(const char *ua);

static struct netsurf_table core_table;
static int registered;

/* --- events ----------------------------------------------------------- */

/* The queue grows. Only a replaceable event may be dropped to make room, and only past 4096
 * waiting; past 65536 the queue refuses more and says so (nsh_events_overflowed). */
enum { SOFT_CAP = 4096, HARD_CAP = 65536 };

struct queued {
	int kind;
	uint64_t view;
	int i0, i1, i2, i3;
	uint64_t u0;
	char *text;
};

static struct queued *queue;
static int qn, qcap;
static char *held_text;
struct nsh_run_effects nsh_run_effects;

size_t nsh_utf8_cut(const char *s, size_t len, size_t max)
{
	if (len <= max)
		return len;
	size_t n = max;
	/* Back off continuation bytes so the cut never splits a character. */
	while (n > 0 && ((unsigned char)s[n] & 0xc0) == 0x80)
		n--;
	return n;
}

/* Titles and status text are cut to NSH_MAX_TEXT, as the browser cuts them. Everything else is
 * copied whole: a URL (the url, a launch, an open, a form, a failed load's) is never shortened,
 * since a shortened URL points elsewhere; the browser drops one over its bound. A failed load's
 * message is cut where its text is built (window.c), clipboard text where it is shadowed. */
static char *dup_text(int kind, const char *text)
{
	if (!text)
		return NULL;
	size_t n = strlen(text);
	if (kind == NSH_EV_TITLE || kind == NSH_EV_STATUS)
		n = nsh_utf8_cut(text, n, NSH_MAX_TEXT);
	char *d = malloc(n + 1);
	if (!d)
		return NULL;
	memcpy(d, text, n);
	d[n] = '\0';
	return d;
}

static void drop_at(int i)
{
	free(queue[i].text);
	if (i < qn - 1)
		memmove(&queue[i], &queue[i + 1], (size_t)(qn - i - 1) * sizeof queue[0]);
	qn--;
}

static int replaceable(int kind)
{
	return kind == NSH_EV_TITLE || kind == NSH_EV_STATUS || kind == NSH_EV_POINTER
		|| kind == NSH_EV_FRAME || kind == NSH_EV_LOADING || kind == NSH_EV_CONSOLE;
}

int nsh_events_overflowed(void)
{
	return overflowed;
}

void nsh_emit(int kind, uint64_t view_id, int i0, int i1, int i2, int i3, uint64_t u0, const char *text)
{
	if (nsh_core_state == 2)
		return;
	if (kind != NSH_EV_FRAME)
		nsh_run_effects.other_events++;
	if (kind == NSH_EV_CONSOLE) {
		for (int i = 0; i < qn; i++) {
			if (queue[i].kind == NSH_EV_CONSOLE && queue[i].view == view_id) {
				queue[i].i0 += i0 > 0 ? i0 : 1;
				return;
			}
		}
	}
	/* A newer replaceable event supersedes the older one of its kind: the older is dropped, the
	 * newer takes its own place at the end. */
	if (replaceable(kind)) {
		for (int i = 0; i < qn;) {
			int match = queue[i].kind == kind && queue[i].view == view_id
				&& (kind != NSH_EV_LOADING || queue[i].i0 == i0);
			if (match)
				drop_at(i);
			else
				i++;
		}
	}
	if (qn >= SOFT_CAP) {
		for (int i = 0; i < qn; i++) {
			if (replaceable(queue[i].kind)) {
				drop_at(i);
				break;
			}
		}
	}
	if (qn >= HARD_CAP) {
		overflowed = 1;
		return;
	}
	if (qn == qcap) {
		int ncap = qcap ? qcap * 2 : 64;
		struct queued *grown = realloc(queue, (size_t)ncap * sizeof *queue);
		if (!grown) {
			overflowed = 1;
			return;
		}
		queue = grown;
		qcap = ncap;
	}
	struct queued *e = &queue[qn++];
	e->kind = kind;
	e->view = view_id;
	e->i0 = i0;
	e->i1 = i1;
	e->i2 = i2;
	e->i3 = i3;
	e->u0 = u0;
	e->text = dup_text(kind, text);
}

void nsh_drop_events(uint64_t view_id)
{
	for (int i = 0; i < qn;) {
		if (queue[i].view == view_id)
			drop_at(i);
		else
			i++;
	}
}

void nsh_clear_events(void)
{
	while (qn)
		drop_at(0);
	free(held_text);
	held_text = NULL;
	free(queue);
	queue = NULL;
	qcap = 0;
}

size_t nsh_next_event(nsh_event *out)
{
	free(held_text);
	held_text = NULL;
	if (!out || qn == 0)
		return 0;
	struct queued e = queue[0];
	/* text moves to held_text; don't free it in drop_at. */
	queue[0].text = NULL;
	drop_at(0);
	held_text = e.text;
	out->kind = (uint32_t)e.kind;
	out->view_id = e.view;
	out->i0 = e.i0;
	out->i1 = e.i1;
	out->i2 = e.i2;
	out->i3 = e.i3;
	out->u0 = e.u0;
	out->text = held_text;
	out->text_len = held_text ? strlen(held_text) : 0;
	return 1;
}

/* --- misc table ------------------------------------------------------- */

static nserror gui_login(struct nsurl *url, const char *realm, const char *username, const char *password,
			 nserror (*cb)(struct nsurl *, const char *, const char *, const char *, void *), void *cbpw)
{
	(void)url;
	(void)realm;
	(void)username;
	(void)password;
	(void)cb;
	(void)cbpw;
	/* Fail closed. The callback is never called. */
	return NSERROR_INVALID;
}

static void gui_quit(void)
{
}

static nserror gui_launch(struct nsurl *url)
{
	/* Attributed to the view the user's input was running for; a launch the core made on its
	 * own (a meta refresh to steam://) is dropped. */
	if (nsh_input_view && url)
		nsh_emit(NSH_EV_LAUNCH, nsh_input_view, 0, 0, 0, 0, 0, nsurl_access(url));
	return NSERROR_OK;
}

struct gui_misc_table nsh_misc_table = {
	.schedule = nsh_schedule,
	.quit = gui_quit,
	.launch_url = gui_launch,
	.login = gui_login,
};

/* --- poll ------------------------------------------------------------- */

static int collect_fds(const fd_set *set, int maxfd, int *out, int *n)
{
	*n = 0;
	int truncated = 0;
	for (int fd = 0; fd <= maxfd; fd++) {
		if (!FD_ISSET(fd, set))
			continue;
		if (*n >= NSH_FD_MAX) {
			truncated = 1;
			break;
		}
		out[(*n)++] = fd;
	}
	return truncated;
}

int nsh_poll(nsh_poll_info *info)
{
	if (!info)
		return 1;
	memset(info, 0, sizeof *info);
	info->timeout_ms = -1;
	info->maxfd = -1;
	if (nsh_core_state != 1)
		return 0;
	/* Destroy target=_blank windows the previous core call spawned. */
	nsh_reap_doomed();
	int more = nsh_sched_run(32);
	fd_set readfds, writefds, exceptfds;
	FD_ZERO(&readfds);
	FD_ZERO(&writefds);
	FD_ZERO(&exceptfds);
	int maxfd = -1;
	/* fetch_fdset drives curl and may schedule a 0ms callback. Read the
	 * deadline only after it. */
	fetch_fdset(&readfds, &writefds, &exceptfds, &maxfd);
	int ms = nsh_sched_deadline(nsh_live_views());
	if (more)
		ms = 0;
	int truncated = 0;
	if (maxfd >= 0) {
		truncated |= collect_fds(&readfds, maxfd, info->read_fds, &info->nread);
		truncated |= collect_fds(&writefds, maxfd, info->write_fds, &info->nwrite);
		truncated |= collect_fds(&exceptfds, maxfd, info->except_fds, &info->nexcept);
	}
	/* A fetch with no deadline of its own must not look idle. */
	if ((maxfd >= 0 || truncated) && ms < 0)
		ms = 10;
	info->timeout_ms = ms;
	info->maxfd = maxfd;
	return 0;
}

/* --- views that call the core ---------------------------------------- */

static int live_view(const nsh_view *view)
{
	return nsh_core_state == 1 && view && view->bw && !view->pending;
}

int nsh_view_navigate(nsh_view *view, const char *url)
{
	if (!live_view(view) || !url)
		return 0;
	struct nsurl *u = NULL;
	if (nsurl_create(url, &u) != NSERROR_OK)
		return 0;
	nsh_input_view = view->view_id;
	nserror err = browser_window_navigate(view->bw, u, NULL, BW_NAVIGATE_HISTORY, NULL, NULL, NULL);
	nsh_input_view = 0;
	nsurl_unref(u);
	return err == NSERROR_OK;
}

int nsh_view_follow(nsh_view *view, const char *url)
{
	if (!live_view(view) || !url)
		return 0;
	struct nsurl *u = NULL;
	if (nsurl_create(url, &u) != NSERROR_OK)
		return 0;
	/* The page's own navigation: the current document is the referrer (so a web page cannot
	 * reach file:), and it goes into the history. */
	struct nsurl *referrer = browser_window_access_url(view->bw);
	nsh_input_view = view->view_id;
	nserror err = browser_window_navigate(view->bw, u, referrer, BW_NAVIGATE_HISTORY, NULL, NULL, NULL);
	nsh_input_view = 0;
	nsurl_unref(u);
	return err == NSERROR_OK;
}

void nsh_view_reload(nsh_view *view)
{
	if (live_view(view))
		browser_window_reload(view->bw, true);
}

void nsh_view_stop(nsh_view *view)
{
	if (live_view(view))
		browser_window_stop(view->bw);
}

void nsh_view_back(nsh_view *view)
{
	if (live_view(view))
		browser_window_history_back(view->bw, false);
}

void nsh_view_forward(nsh_view *view)
{
	if (live_view(view))
		browser_window_history_forward(view->bw, false);
}

int nsh_view_back_available(const nsh_view *view)
{
	return live_view(view) && browser_window_history_back_available(view->bw);
}

int nsh_view_forward_available(const nsh_view *view)
{
	return live_view(view) && browser_window_history_forward_available(view->bw);
}

void nsh_view_mouse(nsh_view *view, int x, int y, unsigned state, unsigned mods)
{
	if (!live_view(view))
		return;
	unsigned m = state | mods;
	/* The core's positions are in the content, not the view: add the scroll offset. */
	x += view->sx;
	y += view->sy;
	unsigned click = NSH_MOUSE_PRESS_1 | NSH_MOUSE_PRESS_2 | NSH_MOUSE_CLICK_1 | NSH_MOUSE_CLICK_2
		| NSH_MOUSE_DOUBLE_CLICK | NSH_MOUSE_TRIPLE_CLICK | NSH_MOUSE_DRAG_1 | NSH_MOUSE_DRAG_2;
	nsh_input_view = view->view_id;
	if (m & click)
		browser_window_mouse_click(view->bw, (browser_mouse_state)m, x, y);
	else
		browser_window_mouse_track(view->bw, (browser_mouse_state)m, x, y);
	nsh_input_view = 0;
}

void nsh_view_scroll(nsh_view *view, int x, int y, int dx, int dy)
{
	if (!live_view(view))
		return;
	/* Something inside the page that scrolls (a box, a frame) takes it first; the page itself
	 * is the frontend's to scroll. */
	if (!browser_window_scroll_at_point(view->bw, x + view->sx, y + view->sy, dx, dy))
		nsh_scroll_by(view, dx, dy);
}

static int key_inner(nsh_view *view, uint32_t key);

int nsh_view_key(nsh_view *view, uint32_t key)
{
	if (!live_view(view))
		return 0;
	nsh_input_view = view->view_id;
	int r = key_inner(view, key);
	nsh_input_view = 0;
	return r;
}

static int key_inner(nsh_view *view, uint32_t key)
{
	if (!live_view(view))
		return 0;
	if (browser_window_key_press(view->bw, key))
		return 1;
	/* Not taken by a field or a selection: scroll the page, as the other frontends do. */
	int page = view->h * 9 / 10 > 1 ? view->h * 9 / 10 : 1;
	switch (key) {
	case NSH_KEY_UP: return nsh_scroll_by(view, 0, -48);
	case NSH_KEY_DOWN: return nsh_scroll_by(view, 0, 48);
	case NSH_KEY_LEFT: return nsh_scroll_by(view, -48, 0);
	case NSH_KEY_RIGHT: return nsh_scroll_by(view, 48, 0);
	case NSH_KEY_PAGE_UP: return nsh_scroll_by(view, 0, -page);
	case NSH_KEY_PAGE_DOWN: return nsh_scroll_by(view, 0, page);
	case NSH_KEY_LINE_START:
	case NSH_KEY_TEXT_START: return nsh_scroll_by(view, -view->sx, -view->sy);
	case NSH_KEY_LINE_END:
	case NSH_KEY_TEXT_END: return nsh_scroll_by(view, 0, 1 << 27);
	default: return 0;
	}
}

size_t nsh_view_link_at(nsh_view *view, int x, int y, char *buf, size_t buflen)
{
	if (buf && buflen > 0)
		buf[0] = '\0';
	if (!live_view(view))
		return 0;
	struct browser_window_features features;
	memset(&features, 0, sizeof features);
	if (browser_window_get_features(view->bw, x + view->sx, y + view->sy, &features) != NSERROR_OK || !features.link)
		return 0;
	const char *s = nsurl_access(features.link);
	if (!s)
		return 0;
	size_t n = strlen(s);
	/* Whole or not at all: the caller asks again with a buffer of the size returned. */
	if (buf && n < buflen) {
		memcpy(buf, s, n);
		buf[n] = '\0';
	}
	return n + 1;
}

int nsh_view_form_at(nsh_view *view, int x, int y)
{
	if (!live_view(view))
		return 0;
	struct browser_window_features features;
	memset(&features, 0, sizeof features);
	if (browser_window_get_features(view->bw, x + view->sx, y + view->sy, &features) != NSERROR_OK)
		return 0;
	if (features.form_features == CTX_FORM_TEXT)
		return 1;
	if (features.form_features == CTX_FORM_FILE)
		return 2;
	return 0;
}

int nsh_view_selection(nsh_view *view, char *buf, size_t buflen)
{
	if (!live_view(view) || !buf || buflen == 0)
		return -1;
	buf[0] = '\0';
	char *s = browser_window_get_selection(view->bw);
	if (!s)
		return 0;
	size_t n = strlen(s);
	int ret = (int)n;
	if (n >= buflen)
		n = buflen - 1;
	memcpy(buf, s, n);
	buf[n] = '\0';
	free(s);
	return ret;
}

/* Paints `clip` of the view into its stage. */
static bool redraw_stage(nsh_view *view, const nsh_rect *clip)
{
	struct nsh_canvas cv = {
		.px = view->stage,
		.w = view->stage_w,
		.h = view->stage_h,
		.stride = view->stage_w,
		.clip = { clip->x0, clip->y0, clip->x1, clip->y1 },
	};
	/* White first, one row filled and copied down: the core fills its own background, but a
	 * failed or partial redraw must not show the previous picture through. */
	int cw = cv.clip.x1 - cv.clip.x0;
	uint32_t *first = cv.px + (size_t)cv.clip.y0 * (size_t)cv.stride + (size_t)cv.clip.x0;
	for (int x = 0; x < cw; x++)
		first[x] = 0xffffffffu;
	for (int y = cv.clip.y0 + 1; y < cv.clip.y1; y++)
		memcpy(cv.px + (size_t)y * (size_t)cv.stride + (size_t)cv.clip.x0, first, (size_t)cw * 4);
	struct redraw_context ctx = {
		.interactive = true,
		.background_images = true,
		.plot = &nsh_plotters,
		.priv = &cv,
	};
	struct rect core_clip = cv.clip;
	return browser_window_redraw(view->bw, -view->sx, -view->sy, &core_clip, &ctx);
}

int nsh_view_paint(nsh_view *view, uint32_t *bgra, int w, int h, size_t stride)
{
	if (!live_view(view) || !bgra || w < 1 || h < 1)
		return 0;
	if (view->hidden)
		return 0;
	if (stride < (size_t)w * 4 || (stride % 4) != 0)
		return 0;
	if (!view->stage || view->stage_w != w || view->stage_h != h) {
		free(view->stage);
		view->stage = malloc((size_t)w * (size_t)h * 4);
		view->stage_w = view->stage ? w : 0;
		view->stage_h = view->stage ? h : 0;
		if (!view->stage)
			return 0;
		view->dirty = true;
		view->dirty_all = true;
	}
	nsh_rect clip = { 0, 0, w, h };
	bool ok = true;
	if (nsh_view_dirty_rect(view, &clip) != 0)
		ok = redraw_stage(view, &clip);
	/* A failed redraw paints everything again next time. */
	view->dirty = !ok;
	view->dirty_all = !ok;
	view->has_dirty = false;
	if (!ok)
		return 0;
	if (stride == (size_t)w * 4) {
		memcpy(bgra, view->stage, (size_t)w * (size_t)h * 4);
	} else {
		for (int y = 0; y < h; y++)
			memcpy((uint8_t *)bgra + (size_t)y * stride, view->stage + (size_t)y * (size_t)w, (size_t)w * 4);
	}
	return 1;
}

/* --- lifetime --------------------------------------------------------- */

static nserror set_defaults(struct nsoption_s *defaults)
{
	(void)defaults;
	return NSERROR_OK;
}

static bool log_ensure(FILE *fptr)
{
	(void)fptr;
	return true;
}

static void fail(const char *what)
{
	fprintf(stderr, "nsh_init: %s\n", what);
	nsh_core_state = 2;
}

int nsh_state(void)
{
	return nsh_core_state;
}

int nsh_init(const nsh_config *cfg)
{
	if (nsh_core_state != 0)
		return 1;
	if (!cfg || !cfg->font_sans || !cfg->font_sans_bold || !cfg->font_serif || !cfg->font_mono) {
		fail("missing fonts");
		return 1;
	}
	nsh_resources_set(cfg);
	core_table = (struct netsurf_table) {
		.misc = &nsh_misc_table,
		.window = &nsh_window_table,
		.download = &nsh_download_table,
		.clipboard = &nsh_clipboard_table,
		.fetch = &nsh_fetch_table,
		.bitmap = &nsh_bitmap_table,
		.layout = &nsh_layout_table,
		.search = &nsh_search_table,
		.llcache = null_llcache_table,
	};
	if (netsurf_register(&core_table) != NSERROR_OK) {
		fail("netsurf_register");
		return 1;
	}
	registered = 1;
	if (!nsh_fonts_init(cfg)) {
		fail("fonts");
		return 1;
	}
	int argc = 1;
	char arg0[] = "sig";
	char *argv[] = { arg0, NULL };
	if (nslog_init(log_ensure, &argc, argv) != NSERROR_OK) {
		fail("nslog");
		return 1;
	}
	/* Drop info logs. Page data must not reach the log. */
	nslog_set_filter("level:error");
	if (nsoption_init(set_defaults, &nsoptions, &nsoptions_default) != NSERROR_OK) {
		fail("nsoption");
		return 1;
	}
	nsoption_set_bool(enable_javascript, false);
	nsoption_set_bool(send_referer, false);
	nsoption_set_bool(target_blank, true);
	if (cfg->ca_bundle && cfg->ca_bundle[0]) {
		char *bundle = strdup(cfg->ca_bundle);
		if (bundle)
			nsoption_set_charp(ca_bundle, bundle);
	}
	user_agent_override(cfg->user_agent);
	bitmap_fmt_t fmt = { .layout = BITMAP_LAYOUT_R8G8B8A8, .pma = false };
	bitmap_set_format(&fmt);
	if (cfg->messages.data && cfg->messages.len > 0) {
		if (messages_add_from_inline(cfg->messages.data, cfg->messages.len) != NSERROR_OK) {
			fail("messages");
			return 1;
		}
	}
	if (netsurf_init(cfg->profile_dir) != NSERROR_OK) {
		fail("netsurf_init");
		return 1;
	}
	nsh_sched_mark_housekeeping();
	nsh_core_state = 1;
	return 0;
}

void nsh_shutdown(void)
{
	if (nsh_core_state != 1)
		return;
	nsh_reap_doomed();
	while (nsh_windows())
		nsh_view_close(nsh_windows());
	if (registered)
		netsurf_exit();
	nsh_fonts_fini();
	nsh_sched_clear();
	nsh_clear_events();
	nsh_clipboard_clear();
	nsh_windows_free_tombstones();
	nsh_core_state = 2;
}
