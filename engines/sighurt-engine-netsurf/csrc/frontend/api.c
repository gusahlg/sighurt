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

extern void user_agent_override(const char *ua);

static struct netsurf_table core_table;
static int registered;

/* --- events ----------------------------------------------------------- */

enum { QCAP = 128 };

struct queued {
	int kind;
	uint64_t view;
	int i0, i1, i2, i3;
	uint64_t u0;
	char *text;
};

static struct queued queue[QCAP];
static int qn;
static char *held_text;

static char *dup_text(const char *text)
{
	if (!text)
		return NULL;
	size_t n = strlen(text);
	if (n > 1024u * 1024u)
		n = 1024u * 1024u;
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

void nsh_emit(int kind, uint64_t view_id, int i0, int i1, int i2, int i3, uint64_t u0, const char *text)
{
	if (nsh_core_state == 2)
		return;
	if (kind == NSH_EV_CONSOLE) {
		for (int i = 0; i < qn; i++) {
			if (queue[i].kind == NSH_EV_CONSOLE && queue[i].view == view_id) {
				queue[i].i0 += i0 > 0 ? i0 : 1;
				return;
			}
		}
	}
	int replace = kind == NSH_EV_TITLE || kind == NSH_EV_STATUS || kind == NSH_EV_POINTER
		|| kind == NSH_EV_FRAME || kind == NSH_EV_LOADING;
	if (replace) {
		for (int i = 0; i < qn;) {
			int match = queue[i].kind == kind && queue[i].view == view_id
				&& (kind != NSH_EV_LOADING || queue[i].i0 == i0);
			if (match)
				drop_at(i);
			else
				i++;
		}
	}
	if (qn == QCAP)
		drop_at(0);
	struct queued *e = &queue[qn++];
	e->kind = kind;
	e->view = view_id;
	e->i0 = i0;
	e->i1 = i1;
	e->i2 = i2;
	e->i3 = i3;
	e->u0 = u0;
	e->text = dup_text(text);
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
	nsh_emit(NSH_EV_LAUNCH, 0, 0, 0, 0, 0, 0, url ? nsurl_access(url) : NULL);
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

void nsh_view_navigate(nsh_view *view, const char *url)
{
	if (!live_view(view) || !url)
		return;
	struct nsurl *u = NULL;
	if (nsurl_create(url, &u) != NSERROR_OK)
		return;
	browser_window_navigate(view->bw, u, NULL, BW_NAVIGATE_HISTORY, NULL, NULL, NULL);
	nsurl_unref(u);
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
	unsigned click = NSH_MOUSE_PRESS_1 | NSH_MOUSE_PRESS_2 | NSH_MOUSE_CLICK_1 | NSH_MOUSE_CLICK_2
		| NSH_MOUSE_DOUBLE_CLICK | NSH_MOUSE_TRIPLE_CLICK | NSH_MOUSE_DRAG_1 | NSH_MOUSE_DRAG_2;
	if (m & click)
		browser_window_mouse_click(view->bw, (browser_mouse_state)m, x, y);
	else
		browser_window_mouse_track(view->bw, (browser_mouse_state)m, x, y);
}

void nsh_view_scroll(nsh_view *view, int x, int y, int dx, int dy)
{
	if (live_view(view))
		browser_window_scroll_at_point(view->bw, x, y, dx, dy);
}

int nsh_view_key(nsh_view *view, uint32_t key)
{
	if (!live_view(view))
		return 0;
	return browser_window_key_press(view->bw, key) ? 1 : 0;
}

int nsh_view_link_at(nsh_view *view, int x, int y, char *buf, size_t buflen)
{
	if (!live_view(view) || !buf || buflen == 0)
		return 0;
	buf[0] = '\0';
	struct browser_window_features features;
	memset(&features, 0, sizeof features);
	if (browser_window_get_features(view->bw, x, y, &features) != NSERROR_OK || !features.link)
		return 0;
	const char *s = nsurl_access(features.link);
	if (!s)
		return 0;
	size_t n = strlen(s);
	if (n >= buflen)
		n = buflen - 1;
	memcpy(buf, s, n);
	buf[n] = '\0';
	return 1;
}

int nsh_view_form_at(nsh_view *view, int x, int y)
{
	if (!live_view(view))
		return 0;
	struct browser_window_features features;
	memset(&features, 0, sizeof features);
	if (browser_window_get_features(view->bw, x, y, &features) != NSERROR_OK)
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

int nsh_view_paint(nsh_view *view, uint32_t *bgra, int w, int h, size_t stride, const nsh_rect *clip)
{
	if (!live_view(view) || !bgra || w < 1 || h < 1)
		return 0;
	if (view->hidden)
		return 0;
	if (stride < (size_t)w * 4 || (stride % 4) != 0)
		return 0;
	struct nsh_canvas cv = {
		.px = bgra,
		.w = w,
		.h = h,
		.stride = (int)(stride / 4),
	};
	if (clip) {
		cv.clip.x0 = clip->x0 < 0 ? 0 : clip->x0;
		cv.clip.y0 = clip->y0 < 0 ? 0 : clip->y0;
		cv.clip.x1 = clip->x1 > w ? w : clip->x1;
		cv.clip.y1 = clip->y1 > h ? h : clip->y1;
	} else {
		cv.clip.x0 = 0;
		cv.clip.y0 = 0;
		cv.clip.x1 = w;
		cv.clip.y1 = h;
	}
	if (cv.clip.x1 < cv.clip.x0)
		cv.clip.x1 = cv.clip.x0;
	if (cv.clip.y1 < cv.clip.y0)
		cv.clip.y1 = cv.clip.y0;
	for (int y = cv.clip.y0; y < cv.clip.y1; y++)
		for (int x = cv.clip.x0; x < cv.clip.x1; x++)
			cv.px[(size_t)y * (size_t)cv.stride + (size_t)x] = 0xffffffffu;
	struct redraw_context ctx = {
		.interactive = true,
		.background_images = true,
		.plot = &nsh_plotters,
		.priv = &cv,
	};
	struct rect core_clip = cv.clip;
	bool ok = browser_window_redraw(view->bw, -view->sx, -view->sy, &core_clip, &ctx);
	view->dirty = false;
	view->dirty_all = false;
	view->has_dirty = false;
	return ok ? 1 : 0;
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
	nsh_core_state = 2;
}
