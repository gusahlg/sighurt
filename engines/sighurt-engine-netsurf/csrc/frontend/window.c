/* One gui window per view. Callbacks only queue events. A window the core
 * opens itself (target=_blank, existing != NULL) stays pending until its first
 * set_url, then is destroyed on the next poll, after the core call has returned.
 * GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

#include "netsurf/window.h"
#include "netsurf/console.h"

static struct gui_window *windows;
struct nsh_opening nsh_opening;

struct gui_window *nsh_windows(void)
{
	return windows;
}

int nsh_live_views(void)
{
	int n = 0;
	for (struct gui_window *g = windows; g; g = g->next) {
		if (g->bw && !g->pending && !g->doomed)
			n++;
	}
	return n;
}

struct gui_window *nsh_view_by_id(uint64_t view_id)
{
	if (view_id == 0)
		return NULL;
	for (struct gui_window *g = windows; g; g = g->next) {
		if (g->view_id == view_id && !g->pending)
			return g;
	}
	return NULL;
}

static void add_dirty(struct gui_window *g, const struct rect *r)
{
	g->dirty = true;
	if (!r) {
		g->dirty_all = true;
		return;
	}
	if (g->dirty_all)
		return;
	if (!g->has_dirty) {
		g->dirty_r = *r;
		g->has_dirty = true;
		return;
	}
	if (r->x0 < g->dirty_r.x0)
		g->dirty_r.x0 = r->x0;
	if (r->y0 < g->dirty_r.y0)
		g->dirty_r.y0 = r->y0;
	if (r->x1 > g->dirty_r.x1)
		g->dirty_r.x1 = r->x1;
	if (r->y1 > g->dirty_r.y1)
		g->dirty_r.y1 = r->y1;
}

static int quiet(const struct gui_window *g)
{
	return !g || g->pending || g->doomed;
}

static struct gui_window *gw_create(struct browser_window *bw, struct gui_window *existing,
				    gui_window_create_flags flags)
{
	(void)flags;
	struct gui_window *g = calloc(1, sizeof *g);
	if (!g)
		return NULL;
	g->bw = bw;
	g->zoom = 1.0;
	g->scale_120 = 120;
	g->w = 1;
	g->h = 1;
	if (nsh_opening.active) {
		g->view_id = nsh_opening.view_id;
		g->w = nsh_opening.w > 0 ? nsh_opening.w : 1;
		g->h = nsh_opening.h > 0 ? nsh_opening.h : 1;
		g->scale_120 = nsh_opening.scale_120 > 0 ? nsh_opening.scale_120 : 120;
	} else if (existing) {
		g->pending = true;
		g->opener = existing->view_id;
		g->w = existing->w;
		g->h = existing->h;
		g->scale_120 = existing->scale_120;
		g->zoom = existing->zoom;
	}
	g->next = windows;
	windows = g;
	return g;
}

static void gw_destroy(struct gui_window *g)
{
	if (!g->pending && g->view_id != 0) {
		nsh_downloads_drop_view(g->view_id);
		nsh_drop_events(g->view_id);
	}
	for (struct gui_window **p = &windows; *p; p = &(*p)->next) {
		if (*p == g) {
			*p = g->next;
			break;
		}
	}
	free(g);
}

static nserror gw_invalidate(struct gui_window *g, const struct rect *rect)
{
	if (quiet(g))
		return NSERROR_OK;
	add_dirty(g, rect);
	nsh_emit(NSH_EV_FRAME, g->view_id, 0, 0, 0, 0, 0, NULL);
	return NSERROR_OK;
}

int nsh_clamp_scroll(struct gui_window *g)
{
	int ew = 0, eh = 0, ox = g->sx, oy = g->sy;
	if (g->bw && browser_window_get_extents(g->bw, true, &ew, &eh) == NSERROR_OK) {
		int mx = ew > g->w ? ew - g->w : 0, my = eh > g->h ? eh - g->h : 0;
		if (g->sx > mx)
			g->sx = mx;
		if (g->sy > my)
			g->sy = my;
	}
	if (g->sx < 0)
		g->sx = 0;
	if (g->sy < 0)
		g->sy = 0;
	return g->sx != ox || g->sy != oy;
}

int nsh_scroll_by(struct gui_window *g, int dx, int dy)
{
	int ox = g->sx, oy = g->sy;
	/* Saturating: callers pass "to the end" as a huge offset. */
	long nx = (long)g->sx + dx, ny = (long)g->sy + dy;
	g->sx = nx > (1 << 28) ? (1 << 28) : (int)nx;
	g->sy = ny > (1 << 28) ? (1 << 28) : (int)ny;
	nsh_clamp_scroll(g);
	int moved = g->sx != ox || g->sy != oy;
	if (moved && !quiet(g)) {
		add_dirty(g, NULL);
		nsh_emit(NSH_EV_FRAME, g->view_id, 0, 0, 0, 0, 0, NULL);
	}
	return moved;
}

static bool gw_get_scroll(struct gui_window *g, int *sx, int *sy)
{
	*sx = g->sx;
	*sy = g->sy;
	return true;
}

static nserror gw_set_scroll(struct gui_window *g, const struct rect *rect)
{
	if (!rect)
		return NSERROR_BAD_PARAMETER;
	g->sx = rect->x0;
	g->sy = rect->y0;
	nsh_clamp_scroll(g);
	if (!quiet(g)) {
		add_dirty(g, NULL);
		nsh_emit(NSH_EV_FRAME, g->view_id, 0, 0, 0, 0, 0, NULL);
	}
	return NSERROR_OK;
}

static nserror gw_get_dimensions(struct gui_window *g, int *width, int *height)
{
	*width = g->w > 0 ? g->w : 1;
	*height = g->h > 0 ? g->h : 1;
	return NSERROR_OK;
}

static nserror gw_event(struct gui_window *g, enum gui_window_event event)
{
	if (quiet(g))
		return NSERROR_OK;
	switch (event) {
	case GW_EVENT_START_THROBBER:
		g->seen_start = true;
		nsh_emit(NSH_EV_LOADING, g->view_id, 1, 0, 0, 0, 0, NULL);
		break;
	case GW_EVENT_STOP_THROBBER:
		/* A stop before the first start is the window's initial state. */
		if (g->seen_start)
			nsh_emit(NSH_EV_LOADING, g->view_id, 0, 0, 0, 0, 0, NULL);
		break;
	case GW_EVENT_NEW_CONTENT:
		g->gadget = NULL;
		g->sx = g->sy = 0;
		nsh_emit(NSH_EV_NEW_CONTENT, g->view_id, 0, 0, 0, 0, 0, NULL);
		break;
	case GW_EVENT_UPDATE_EXTENT:
		nsh_clamp_scroll(g);
		add_dirty(g, NULL);
		nsh_emit(NSH_EV_FRAME, g->view_id, 0, 0, 0, 0, 0, NULL);
		break;
	case GW_EVENT_REMOVE_CARET:
		nsh_emit(NSH_EV_REMOVE_CARET, g->view_id, 0, 0, 0, 0, 0, NULL);
		break;
	default:
		break;
	}
	return NSERROR_OK;
}

static void gw_set_title(struct gui_window *g, const char *title)
{
	if (quiet(g))
		return;
	nsh_emit(NSH_EV_TITLE, g->view_id, 0, 0, 0, 0, 0, title);
}

static nserror gw_set_url(struct gui_window *g, struct nsurl *url)
{
	const char *s = url ? nsurl_access(url) : NULL;
	if (g->pending) {
		nsh_emit(NSH_EV_OPEN, g->opener, 0, 0, 0, 0, 0, s);
		g->doomed = true;
		return NSERROR_OK;
	}
	nsh_emit(NSH_EV_URL, g->view_id, 0, 0, 0, 0, 0, s);
	return NSERROR_OK;
}

static void gw_set_icon(struct gui_window *g, struct hlcache_handle *icon)
{
	(void)g;
	(void)icon;
}

static void gw_set_status(struct gui_window *g, const char *text)
{
	if (quiet(g))
		return;
	nsh_emit(NSH_EV_STATUS, g->view_id, 0, 0, 0, 0, 0, text);
}

static void gw_set_pointer(struct gui_window *g, enum gui_pointer_shape shape)
{
	g->pointer = (int)shape;
	if (quiet(g))
		return;
	nsh_emit(NSH_EV_POINTER, g->view_id, (int)shape, 0, 0, 0, 0, NULL);
}

static void gw_place_caret(struct gui_window *g, int x, int y, int height, const struct rect *clip)
{
	(void)clip;
	if (quiet(g))
		return;
	/* The core places the caret in content space; the browser wants it in the view. */
	nsh_emit(NSH_EV_CARET, g->view_id, x - g->sx, y - g->sy, height, 0, 0, NULL);
}

static bool gw_drag_start(struct gui_window *g, gui_drag_type type, const struct rect *rect)
{
	(void)g;
	(void)type;
	(void)rect;
	return true;
}

static nserror gw_save_link(struct gui_window *g, struct nsurl *url, const char *title)
{
	(void)g;
	(void)url;
	(void)title;
	return NSERROR_OK;
}

static void gw_file_open(struct gui_window *g, struct hlcache_handle *hl, struct form_control *gadget)
{
	(void)hl;
	if (quiet(g))
		return;
	g->gadget = gadget;
	nsh_emit(NSH_EV_FILE, g->view_id, 0, 0, 0, 0, 0, NULL);
}

static void gw_console(struct gui_window *g, browser_window_console_source src, const char *msg, size_t len,
		       browser_window_console_flags flags)
{
	/* Count only. The message can be page data, so it is not queued or logged. */
	(void)src;
	(void)msg;
	(void)flags;
	if (quiet(g))
		return;
	(void)len;
	nsh_emit(NSH_EV_CONSOLE, g->view_id, 1, 0, 0, 0, 0, NULL);
}

struct gui_window_table nsh_window_table = {
	.create = gw_create,
	.destroy = gw_destroy,
	.invalidate = gw_invalidate,
	.get_scroll = gw_get_scroll,
	.set_scroll = gw_set_scroll,
	.get_dimensions = gw_get_dimensions,
	.event = gw_event,
	.set_title = gw_set_title,
	.set_url = gw_set_url,
	.set_icon = gw_set_icon,
	.set_status = gw_set_status,
	.set_pointer = gw_set_pointer,
	.place_caret = gw_place_caret,
	.drag_start = gw_drag_start,
	.save_link = gw_save_link,
	.file_gadget_open = gw_file_open,
	.console_log = gw_console,
};

void nsh_reap_doomed(void)
{
	struct gui_window *g = windows;
	while (g) {
		struct gui_window *next = g->next;
		if (g->doomed && g->bw) {
			struct browser_window *bw = g->bw;
			g->bw = NULL;
			g->doomed = false;
			browser_window_destroy(bw);
		}
		g = next;
	}
}

static void apply_scale(struct gui_window *g)
{
	if (!g->bw)
		return;
	double s = g->zoom * ((double)g->scale_120 / 120.0);
	if (s < 0.1)
		s = 0.1;
	if (s > 10.0)
		s = 10.0;
	browser_window_set_scale(g->bw, (float)s, true);
}

nsh_view *nsh_view_open(uint64_t view_id, int w, int h, int scale_120)
{
	if (nsh_core_state != 1 || view_id == 0)
		return NULL;
	if (w < 1)
		w = 1;
	if (h < 1)
		h = 1;
	if (scale_120 < 1)
		scale_120 = 120;
	nsh_opening.active = 1;
	nsh_opening.view_id = view_id;
	nsh_opening.w = w;
	nsh_opening.h = h;
	nsh_opening.scale_120 = scale_120;
	struct browser_window *bw = NULL;
	nserror err = browser_window_create(BW_CREATE_HISTORY, NULL, NULL, NULL, &bw);
	nsh_opening.active = 0;
	if (err != NSERROR_OK || !bw)
		return NULL;
	nsh_view *v = nsh_view_by_id(view_id);
	if (!v) {
		browser_window_destroy(bw);
		return NULL;
	}
	apply_scale(v);
	return v;
}

void nsh_view_close(nsh_view *view)
{
	if (!view)
		return;
	if (!view->bw) {
		gw_destroy(view);
		return;
	}
	struct browser_window *bw = view->bw;
	view->bw = NULL;
	browser_window_destroy(bw);
}

void nsh_view_resize(nsh_view *view, int w, int h, int scale_120)
{
	if (!view || !view->bw || view->pending)
		return;
	if (w < 1 || h < 1)
		return;
	int scale_changed = scale_120 > 0 && scale_120 != view->scale_120;
	if (scale_120 > 0)
		view->scale_120 = scale_120;
	view->w = w;
	view->h = h;
	nsh_clamp_scroll(view);
	if (scale_changed)
		apply_scale(view);
	else
		browser_window_schedule_reformat(view->bw);
	view->dirty = true;
	view->dirty_all = true;
}

void nsh_view_set_scale(nsh_view *view, double zoom)
{
	if (!view || !view->bw)
		return;
	if (zoom < 0.1)
		zoom = 0.1;
	if (zoom > 10.0)
		zoom = 10.0;
	view->zoom = zoom;
	apply_scale(view);
	view->dirty = true;
	view->dirty_all = true;
}

void nsh_view_set_hidden(nsh_view *view, int hidden)
{
	if (!view)
		return;
	view->hidden = hidden != 0;
}

int nsh_view_hidden(const nsh_view *view)
{
	return view && view->hidden;
}

int nsh_view_width(const nsh_view *view)
{
	return view ? view->w : 0;
}

int nsh_view_height(const nsh_view *view)
{
	return view ? view->h : 0;
}

int nsh_view_scale_120(const nsh_view *view)
{
	return view ? view->scale_120 : 0;
}

int nsh_view_dirty_rect(const nsh_view *view, nsh_rect *out)
{
	if (!view || !view->dirty)
		return 0;
	if (view->dirty_all || !view->has_dirty)
		return 2;
	if (out) {
		out->x0 = view->dirty_r.x0;
		out->y0 = view->dirty_r.y0;
		out->x1 = view->dirty_r.x1;
		out->y1 = view->dirty_r.y1;
	}
	return 1;
}

void nsh_file_answer(uint64_t view_id, const char *path)
{
	struct gui_window *g = nsh_view_by_id(view_id);
	if (!g || !g->gadget)
		return;
	struct form_control *gadget = g->gadget;
	g->gadget = NULL;
	if (path && path[0] && g->bw)
		browser_window_set_gadget_filename(g->bw, gadget, path);
}
