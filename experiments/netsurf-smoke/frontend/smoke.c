/*
 * NetSurf "sighurt" frontend, smoke-test edition (docs/rewrite/NETSURF.md).
 *
 * Implements NetSurf's frontend tables (misc, window, download, clipboard, fetch, bitmap, layout,
 * llcache) in one file, with software plotters that draw into a BGRA (ARGB8888, little-endian)
 * buffer and FreeType for text, then drives the browser core from a poll loop with no timer ticks:
 * the loop sleeps in select() on the fetchers' descriptors until the next scheduled callback.
 *
 *   nssighurt URL OUT.png [WIDTH HEIGHT]
 *
 * Environment: NS_RES (NetSurf's resources dir), NS_FONT, NS_FONT_BOLD, NS_FONT_MONO,
 * NS_FONT_SERIF (TrueType files), SSL_CERT_FILE.
 *
 * GPL-2.0-only, as NetSurf is (docs/rewrite/NETSURF.md, "License").
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdint.h>
#include <stdbool.h>
#include <math.h>
#include <time.h>
#include <errno.h>
#include <sys/select.h>
#include <png.h>
#include <ft2build.h>
#include FT_FREETYPE_H

#include "utils/config.h"
#include "utils/sys_time.h"
#include "utils/log.h"
#include "utils/messages.h"
#include "utils/filepath.h"
#include "utils/nsoption.h"
#include "utils/nsurl.h"
#include "utils/errors.h"
#include "utils/utf8.h"
#include "netsurf/misc.h"
#include "netsurf/netsurf.h"
#include "netsurf/window.h"
#include "netsurf/download.h"
#include "netsurf/clipboard.h"
#include "netsurf/fetch.h"
#include "netsurf/bitmap.h"
#include "netsurf/layout.h"
#include "netsurf/plotters.h"
#include "netsurf/plot_style.h"
#include "netsurf/browser_window.h"
#include "netsurf/mouse.h"
#include "netsurf/url_db.h"
#include "netsurf/cookie_db.h"
#include "content/fetch.h"
#include "content/backing_store.h"
#include "desktop/gui_table.h"
#include "desktop/download.h"

/* ------------------------------------------------------------------------ time */

static double now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return ts.tv_sec * 1000.0 + ts.tv_nsec / 1e6;
}

static double t_start;
/* -------------------------------------------------------------------- schedule
 * misc.schedule: the core asks for callbacks after a delay (-1 cancels). The owner loop runs the
 * due ones and learns how long it may sleep: that is the engine's pump deadline. */

struct cb { struct cb *next; double at; void (*fn)(void *); void *p; };
static struct cb *cbs;

static nserror sched_cancel(void (*fn)(void *), void *p)
{
	nserror r = NSERROR_NOT_FOUND;
	for (struct cb **pp = &cbs; *pp;) {
		if ((*pp)->fn == fn && (*pp)->p == p) {
			struct cb *d = *pp; *pp = d->next; free(d); r = NSERROR_OK;
		} else pp = &(*pp)->next;
	}
	return r;
}

static nserror gui_schedule(int ms, void (*fn)(void *), void *p)
{
	sched_cancel(fn, p);
	if (getenv("NS_TRACE")) fprintf(stderr, "[schedule %.1f] %d ms fn=%p p=%p\n", now_ms() - t_start, ms, (void *)fn, p);
	if (ms < 0) return NSERROR_OK;
	struct cb *c = calloc(1, sizeof *c);
	if (!c) return NSERROR_NOMEM;
	c->at = now_ms() + ms; c->fn = fn; c->p = p; c->next = cbs; cbs = c;
	return NSERROR_OK;
}

/* Runs what is due; returns ms until the next callback, or -1 for none. */
static int sched_run(void)
{
	for (;;) {
		double t = now_ms(), next = -1;
		struct cb **due = NULL;
		for (struct cb **pp = &cbs; *pp; pp = &(*pp)->next) {
			if ((*pp)->at <= t) { due = pp; break; }
			if (next < 0 || (*pp)->at < next) next = (*pp)->at;
		}
		if (!due) return next < 0 ? -1 : (int)ceil(next - t);
		struct cb *c = *due; *due = c->next;
		c->fn(c->p);
		free(c);
	}
}

/* Time to the earliest callback without running anything: -1 for none. */
static int sched_next(void)
{
	double t = now_ms(), next = -1;
	for (struct cb *c = cbs; c; c = c->next) if (next < 0 || c->at < next) next = c->at;
	return next < 0 ? -1 : next <= t ? 0 : (int)ceil(next - t);
}

/* ---------------------------------------------------------------------- canvas */

struct canvas { uint32_t *px; int w, h; struct rect clip; };

static inline uint32_t blend(uint32_t dst, int r, int g, int b, int a)
{
	if (a >= 255) return 0xff000000u | r << 16 | g << 8 | b;
	int dr = dst >> 16 & 255, dg = dst >> 8 & 255, db = dst & 255;
	dr += ((r - dr) * a + 127) / 255; dg += ((g - dg) * a + 127) / 255; db += ((b - db) * a + 127) / 255;
	return 0xff000000u | dr << 16 | dg << 8 | db;
}
#define CR(c) ((c) & 255)
#define CG(c) ((c) >> 8 & 255)
#define CB(c) ((c) >> 16 & 255)

static inline void put(struct canvas *cv, int x, int y, colour c, int a)
{
	if (x < cv->clip.x0 || x >= cv->clip.x1 || y < cv->clip.y0 || y >= cv->clip.y1) return;
	uint32_t *d = &cv->px[(size_t)y * cv->w + x];
	*d = blend(*d, CR(c), CG(c), CB(c), a);
}

static void fill_rect(struct canvas *cv, int x0, int y0, int x1, int y1, colour c)
{
	if (x0 < cv->clip.x0) x0 = cv->clip.x0;
	if (y0 < cv->clip.y0) y0 = cv->clip.y0;
	if (x1 > cv->clip.x1) x1 = cv->clip.x1;
	if (y1 > cv->clip.y1) y1 = cv->clip.y1;
	for (int y = y0; y < y1; y++)
		for (int x = x0; x < x1; x++) put(cv, x, y, c, 255);
}

/* ------------------------------------------------------------------- plotters */

static struct canvas *cvof(const struct redraw_context *ctx) { return ctx->priv; }
static bool has(plot_operation_type_t t) { return t != PLOT_OP_TYPE_NONE; }

static nserror p_clip(const struct redraw_context *ctx, const struct rect *r)
{
	struct canvas *cv = cvof(ctx);
	cv->clip.x0 = r->x0 < 0 ? 0 : r->x0; cv->clip.y0 = r->y0 < 0 ? 0 : r->y0;
	cv->clip.x1 = r->x1 > cv->w ? cv->w : r->x1; cv->clip.y1 = r->y1 > cv->h ? cv->h : r->y1;
	return NSERROR_OK;
}

static void hline(struct canvas *cv, int x0, int x1, int y, colour c, int w, plot_operation_type_t t)
{
	for (int x = x0; x < x1; x++) {
		if (t == PLOT_OP_TYPE_DOT && ((x - x0) / (w ? w : 1)) % 2) continue;
		if (t == PLOT_OP_TYPE_DASH && ((x - x0) / (3 * (w ? w : 1))) % 2) continue;
		for (int k = 0; k < w; k++) put(cv, x, y + k, c, 255);
	}
}
static void vline(struct canvas *cv, int x, int y0, int y1, colour c, int w, plot_operation_type_t t)
{
	for (int y = y0; y < y1; y++) {
		if (t == PLOT_OP_TYPE_DOT && ((y - y0) / (w ? w : 1)) % 2) continue;
		if (t == PLOT_OP_TYPE_DASH && ((y - y0) / (3 * (w ? w : 1))) % 2) continue;
		for (int k = 0; k < w; k++) put(cv, x + k, y, c, 255);
	}
}
static int sw(const plot_style_t *s)
{
	int w = plot_style_fixed_to_int(s->stroke_width);
	return w < 1 ? 1 : w;
}

static nserror p_rect(const struct redraw_context *ctx, const plot_style_t *s, const struct rect *r)
{
	struct canvas *cv = cvof(ctx);
	if (has(s->fill_type) && s->fill_colour != NS_TRANSPARENT)
		fill_rect(cv, r->x0, r->y0, r->x1, r->y1, s->fill_colour);
	if (has(s->stroke_type) && s->stroke_colour != NS_TRANSPARENT) {
		int w = sw(s);
		hline(cv, r->x0, r->x1, r->y0, s->stroke_colour, w, s->stroke_type);
		hline(cv, r->x0, r->x1, r->y1 - w, s->stroke_colour, w, s->stroke_type);
		vline(cv, r->x0, r->y0, r->y1, s->stroke_colour, w, s->stroke_type);
		vline(cv, r->x1 - w, r->y0, r->y1, s->stroke_colour, w, s->stroke_type);
	}
	return NSERROR_OK;
}

static nserror p_line(const struct redraw_context *ctx, const plot_style_t *s, const struct rect *l)
{
	struct canvas *cv = cvof(ctx);
	int w = sw(s), x0 = l->x0, y0 = l->y0, x1 = l->x1, y1 = l->y1;
	if (y0 == y1) { hline(cv, x0 < x1 ? x0 : x1, (x0 < x1 ? x1 : x0) + 1, y0, s->stroke_colour, w, s->stroke_type); return NSERROR_OK; }
	if (x0 == x1) { vline(cv, x0, y0 < y1 ? y0 : y1, (y0 < y1 ? y1 : y0) + 1, s->stroke_colour, w, s->stroke_type); return NSERROR_OK; }
	int dx = abs(x1 - x0), dy = -abs(y1 - y0), sx = x0 < x1 ? 1 : -1, sy = y0 < y1 ? 1 : -1, err = dx + dy;
	for (;;) {
		for (int k = 0; k < w; k++) put(cv, x0, y0 + k, s->stroke_colour, 255);
		if (x0 == x1 && y0 == y1) break;
		int e2 = 2 * err;
		if (e2 >= dy) { err += dy; x0 += sx; }
		if (e2 <= dx) { err += dx; y0 += sy; }
	}
	return NSERROR_OK;
}

static nserror p_polygon(const struct redraw_context *ctx, const plot_style_t *s, const int *p, unsigned n)
{
	struct canvas *cv = cvof(ctx);
	if (!has(s->fill_type) || n < 3) return NSERROR_OK;
	int ymin = p[1], ymax = p[1];
	for (unsigned i = 1; i < n; i++) { if (p[2*i+1] < ymin) ymin = p[2*i+1]; if (p[2*i+1] > ymax) ymax = p[2*i+1]; }
	if (ymin < cv->clip.y0) ymin = cv->clip.y0;
	if (ymax > cv->clip.y1) ymax = cv->clip.y1;
	int *xs = malloc(n * sizeof *xs);
	for (int y = ymin; y < ymax; y++) {
		unsigned k = 0;
		for (unsigned i = 0, j = n - 1; i < n; j = i++) {
			int yi = p[2*i+1], yj = p[2*j+1];
			if ((yi <= y && yj > y) || (yj <= y && yi > y))
				xs[k++] = p[2*i] + (int)((long)(y - yi) * (p[2*j] - p[2*i]) / (yj - yi));
		}
		for (unsigned a = 1; a < k; a++) for (unsigned b = a; b > 0 && xs[b-1] > xs[b]; b--) { int t = xs[b]; xs[b] = xs[b-1]; xs[b-1] = t; }
		for (unsigned a = 0; a + 1 < k; a += 2) for (int x = xs[a]; x < xs[a+1]; x++) put(cv, x, y, s->fill_colour, 255);
	}
	free(xs);
	return NSERROR_OK;
}

static nserror p_disc(const struct redraw_context *ctx, const plot_style_t *s, int cx, int cy, int r)
{
	struct canvas *cv = cvof(ctx);
	for (int y = -r; y <= r; y++) for (int x = -r; x <= r; x++) {
		int d = x * x + y * y;
		if (has(s->fill_type) && d <= r * r) put(cv, cx + x, cy + y, s->fill_colour, 255);
		else if (has(s->stroke_type) && d <= r * r && d >= (r - sw(s)) * (r - sw(s))) put(cv, cx + x, cy + y, s->stroke_colour, 255);
	}
	return NSERROR_OK;
}

static nserror p_arc(const struct redraw_context *ctx, const plot_style_t *s, int cx, int cy, int r, int a1, int a2)
{
	struct canvas *cv = cvof(ctx);
	if (a2 < a1) a2 += 360;
	for (int a = a1 * 4; a <= a2 * 4; a++) {
		double t = a / 4.0 * M_PI / 180;
		put(cv, cx + (int)lround(r * cos(t)), cy - (int)lround(r * sin(t)), s->stroke_colour, 255);
	}
	return NSERROR_OK;
}

static nserror p_path(const struct redraw_context *c, const plot_style_t *s, const float *p, unsigned n, const float t[6])
{
	return NSERROR_OK; /* SVG paths (libsvgtiny): not drawn by the smoke test */
}

/* The bitmap's buffer is R,G,B,A bytes, not premultiplied. */
struct bitmap { uint8_t *px; int w, h; bool opaque; };

static nserror p_bitmap(const struct redraw_context *ctx, struct bitmap *b, int x, int y, int w, int h,
			colour bg, bitmap_flags_t flags)
{
	struct canvas *cv = cvof(ctx);
	if (!b || w <= 0 || h <= 0) return NSERROR_OK;
	int x0 = x, x1 = x + w, y0 = y, y1 = y + h;
	if (flags & BITMAPF_REPEAT_X) { x0 = cv->clip.x0 - ((cv->clip.x0 - x) % w + w) % w; x1 = cv->clip.x1; }
	if (flags & BITMAPF_REPEAT_Y) { y0 = cv->clip.y0 - ((cv->clip.y0 - y) % h + h) % h; y1 = cv->clip.y1; }
	for (int ty = y0; ty < y1; ty += h) for (int tx = x0; tx < x1; tx += w) {
		int ya = ty < cv->clip.y0 ? cv->clip.y0 : ty, yb = ty + h > cv->clip.y1 ? cv->clip.y1 : ty + h;
		int xa = tx < cv->clip.x0 ? cv->clip.x0 : tx, xb = tx + w > cv->clip.x1 ? cv->clip.x1 : tx + w;
		for (int py = ya; py < yb; py++) {
			int sy = (int)((int64_t)(py - ty) * b->h / h);
			for (int px = xa; px < xb; px++) {
				int sx = (int)((int64_t)(px - tx) * b->w / w);
				const uint8_t *s = &b->px[((size_t)sy * b->w + sx) * 4];
				uint32_t *d = &cv->px[(size_t)py * cv->w + px];
				*d = blend(*d, s[0], s[1], s[2], b->opaque ? 255 : s[3]);
			}
		}
	}
	return NSERROR_OK;
}

/* ---------------------------------------------------------------------- fonts */

static FT_Library ftlib;
static FT_Face faces[3][2]; /* sans, serif, mono  x  regular, bold */

static void fonts_init(void)
{
	const char *sans = getenv("NS_FONT"), *bold = getenv("NS_FONT_BOLD"),
		   *mono = getenv("NS_FONT_MONO"), *serif = getenv("NS_FONT_SERIF");
	FT_Init_FreeType(&ftlib);
	const char *f[3][2] = { { sans, bold }, { serif, bold }, { mono, mono } };
	for (int i = 0; i < 3; i++) for (int j = 0; j < 2; j++)
		if (!f[i][j] || FT_New_Face(ftlib, f[i][j], 0, &faces[i][j])) { fprintf(stderr, "font %d/%d missing\n", i, j); exit(1); }
}

static FT_Face face_for(const plot_font_style_t *fs)
{
	int fam = fs->family == PLOT_FONT_FAMILY_SERIF ? 1 : fs->family == PLOT_FONT_FAMILY_MONOSPACE ? 2 : 0;
	FT_Face f = faces[fam][fs->weight >= 600];
	/* NetSurf's size is in points, and its CSS length conversion assumes 90 dpi. */
	FT_Set_Char_Size(f, 0, fs->size / 16, 90, 90);
	FT_Matrix m = { 0x10000, (fs->flags & (FONTF_ITALIC | FONTF_OBLIQUE)) ? 0x3000 : 0, 0, 0x10000 };
	FT_Set_Transform(f, &m, NULL);
	return f;
}

/* Advance of one code point, in 26.6 pixels. */
static int advance(FT_Face f, uint32_t cp)
{
	if (FT_Load_Char(f, cp, FT_LOAD_NO_HINTING | FT_LOAD_NO_BITMAP)) return 0;
	return (int)(f->glyph->linearHoriAdvance >> 10);
}

static uint32_t next_cp(const char *s, size_t len, size_t *i)
{
	uint32_t cp = utf8_to_ucs4(s + *i, len - *i);
	*i = utf8_next(s, len, *i);
	return cp;
}

static nserror l_width(const plot_font_style_t *fs, const char *s, size_t len, int *w)
{
	FT_Face f = face_for(fs); int sum = 0;
	for (size_t i = 0; i < len;) sum += advance(f, next_cp(s, len, &i));
	*w = (sum + 32) >> 6;
	return NSERROR_OK;
}

static nserror l_position(const plot_font_style_t *fs, const char *s, size_t len, int x, size_t *off, int *ax)
{
	FT_Face f = face_for(fs); int sum = 0; size_t i = 0;
	while (i < len) {
		size_t j = i; int a = advance(f, next_cp(s, len, &j));
		if (((sum + a / 2) >> 6) > x) break;
		sum += a; i = j;
	}
	*off = i; *ax = (sum + 32) >> 6;
	return NSERROR_OK;
}

static nserror l_split(const plot_font_style_t *fs, const char *s, size_t len, int x, size_t *off, int *ax)
{
	FT_Face f = face_for(fs); int sum = 0, space_w = 0; long space = -1; size_t i = 0;
	while (i < len) {
		size_t j = i; uint32_t cp = next_cp(s, len, &j); int a = advance(f, cp);
		if (((sum + a + 32) >> 6) > x) break;
		if (cp == ' ') { space = (long)i; space_w = sum; }
		sum += a; i = j;
	}
	if (i >= len) { *off = len; *ax = (sum + 32) >> 6; return NSERROR_OK; }
	if (space >= 0) { *off = space; *ax = (space_w + 32) >> 6; return NSERROR_OK; }
	while (i < len) { /* no break before the limit: the first one after it */
		size_t j = i; uint32_t cp = next_cp(s, len, &j);
		if (cp == ' ') break;
		sum += advance(f, cp); i = j;
	}
	*off = i ? i : len; *ax = (sum + 32) >> 6;
	return NSERROR_OK;
}

static nserror p_text(const struct redraw_context *ctx, const plot_font_style_t *fs, int x, int y,
		      const char *s, size_t len)
{
	struct canvas *cv = cvof(ctx);
	FT_Face f = face_for(fs); int pen = x << 6;
	for (size_t i = 0; i < len;) {
		uint32_t cp = next_cp(s, len, &i);
		if (FT_Load_Char(f, cp, FT_LOAD_RENDER | FT_LOAD_NO_HINTING)) continue;
		FT_GlyphSlot g = f->glyph;
		int gx = ((pen + 32) >> 6) + g->bitmap_left, gy = y - g->bitmap_top;
		for (unsigned r = 0; r < g->bitmap.rows; r++) for (unsigned c = 0; c < g->bitmap.width; c++) {
			int a = g->bitmap.buffer[r * g->bitmap.pitch + c];
			if (a) put(cv, gx + c, gy + r, fs->foreground, a);
		}
		pen += (int)(g->linearHoriAdvance >> 10);
	}
	return NSERROR_OK;
}

static nserror p_noop_g(const struct redraw_context *c, const char *n) { return NSERROR_OK; }
static nserror p_noop(const struct redraw_context *c) { return NSERROR_OK; }

static const struct plotter_table plotters = {
	.clip = p_clip, .arc = p_arc, .disc = p_disc, .line = p_line, .rectangle = p_rect,
	.polygon = p_polygon, .path = p_path, .bitmap = p_bitmap, .text = p_text,
	.group_start = p_noop_g, .group_end = p_noop, .flush = p_noop, .option_knockout = false,
};

/* --------------------------------------------------------------------- bitmaps */

static void *b_create(int w, int h, enum gui_bitmap_flags fl)
{
	struct bitmap *b = calloc(1, sizeof *b);
	if (!b) return NULL;
	b->w = w; b->h = h; b->opaque = fl & BITMAP_OPAQUE;
	b->px = calloc((size_t)w * h, 4);
	if (!b->px) { free(b); return NULL; }
	return b;
}
static void b_destroy(void *b) { free(((struct bitmap *)b)->px); free(b); }
static void b_set_opaque(void *b, bool o) { ((struct bitmap *)b)->opaque = o; }
static bool b_get_opaque(void *b) { return ((struct bitmap *)b)->opaque; }
static unsigned char *b_buffer(void *b) { return ((struct bitmap *)b)->px; }
static size_t b_stride(void *b) { return ((struct bitmap *)b)->w * 4; }
static int b_w(void *b) { return ((struct bitmap *)b)->w; }
static int b_h(void *b) { return ((struct bitmap *)b)->h; }
static void b_modified(void *b) { }
/* History thumbnails: the core renders one on every history add; a real frontend skips it. */
static nserror b_render(struct bitmap *b, struct hlcache_handle *c) { return NSERROR_OK; }
static struct gui_bitmap_table bitmap_table = {
	.create = b_create, .destroy = b_destroy, .set_opaque = b_set_opaque, .get_opaque = b_get_opaque,
	.get_buffer = b_buffer, .get_rowstride = b_stride, .get_width = b_w, .get_height = b_h,
	.modified = b_modified, .render = b_render,
};

/* --------------------------------------------------------------------- windows */

struct gui_window {
	struct gui_window *next;
	struct browser_window *bw;
	int w, h, sx, sy;
	char *title, *url, *status;
	bool loading, dirty;
	int events;
};
static struct gui_window *windows;
static int loading_done, loading_started, windows_created;

static struct gui_window *gw_create(struct browser_window *bw, struct gui_window *existing, gui_window_create_flags fl)
{
	struct gui_window *g = calloc(1, sizeof *g);
	g->bw = bw; g->w = atoi(getenv("NS_W") ?: "1024"); g->h = atoi(getenv("NS_H") ?: "768");
	g->next = windows; windows = g; windows_created++;
	fprintf(stderr, "[window] create #%d tab=%d clone=%d\n", windows_created, !!(fl & GW_CREATE_TAB), !!(fl & GW_CREATE_CLONE));
	return g;
}
static void gw_destroy(struct gui_window *g)
{
	for (struct gui_window **p = &windows; *p; p = &(*p)->next) if (*p == g) { *p = g->next; break; }
	free(g->title); free(g->url); free(g->status); free(g);
}
static nserror gw_invalidate(struct gui_window *g, const struct rect *r) { g->dirty = true; return NSERROR_OK; }
static bool gw_get_scroll(struct gui_window *g, int *x, int *y) { *x = g->sx; *y = g->sy; return true; }
static nserror gw_set_scroll(struct gui_window *g, const struct rect *r) { g->sx = r->x0; g->sy = r->y0; return NSERROR_OK; }
static nserror gw_get_dimensions(struct gui_window *g, int *w, int *h) { *w = g->w; *h = g->h; return NSERROR_OK; }
static nserror gw_event(struct gui_window *g, enum gui_window_event e)
{
	g->events++;
	if (getenv("NS_TRACE")) fprintf(stderr, "[event %.1f] %d\n", now_ms(), (int)e);
	if (e == GW_EVENT_START_THROBBER) { g->loading = true; loading_started++; }
	/* a stop before the first start is the window's initial state */
	if (e == GW_EVENT_STOP_THROBBER) { g->loading = false; if (loading_started) loading_done++; }
	return NSERROR_OK;
}
static void set_str(char **d, const char *s) { free(*d); *d = s ? strdup(s) : NULL; }
static void gw_set_title(struct gui_window *g, const char *t) { if (getenv("NS_TRACE")) fprintf(stderr, "[title] %s\n", t); set_str(&g->title, t); }
static nserror gw_set_url(struct gui_window *g, struct nsurl *u) { set_str(&g->url, nsurl_access(u)); return NSERROR_OK; }
static void gw_set_icon(struct gui_window *g, struct hlcache_handle *h) { }
static void gw_set_status(struct gui_window *g, const char *t) { if (getenv("NS_TRACE")) fprintf(stderr, "[status] %s\n", t); set_str(&g->status, t); }
static void gw_set_pointer(struct gui_window *g, enum gui_pointer_shape s) { }
static void gw_place_caret(struct gui_window *g, int x, int y, int h, const struct rect *c) { }
static bool gw_drag_start(struct gui_window *g, gui_drag_type t, const struct rect *r) { return false; }
static nserror gw_save_link(struct gui_window *g, struct nsurl *u, const char *t) { return NSERROR_OK; }
static void gw_console_log(struct gui_window *g, browser_window_console_source s, const char *m, size_t l, browser_window_console_flags f) { }

static struct gui_window_table window_table = {
	.create = gw_create, .destroy = gw_destroy, .invalidate = gw_invalidate, .get_scroll = gw_get_scroll,
	.set_scroll = gw_set_scroll, .get_dimensions = gw_get_dimensions, .event = gw_event,
	.set_title = gw_set_title, .set_url = gw_set_url, .set_icon = gw_set_icon, .set_status = gw_set_status,
	.set_pointer = gw_set_pointer, .place_caret = gw_place_caret, .drag_start = gw_drag_start,
	.save_link = gw_save_link, .console_log = gw_console_log,
};

/* ------------------------------------------------------- downloads, clipboard */

struct gui_download_window { int bytes; };
static struct gui_download_window *dl_create(download_context *c, struct gui_window *p)
{ fprintf(stderr, "[download] create\n"); return calloc(1, sizeof(struct gui_download_window)); }
static nserror dl_data(struct gui_download_window *d, const char *data, unsigned n) { d->bytes += n; return NSERROR_OK; }
static void dl_error(struct gui_download_window *d, const char *m) { free(d); }
static void dl_done(struct gui_download_window *d) { fprintf(stderr, "[download] done %d bytes\n", d->bytes); free(d); }
static struct gui_download_table download_table = { .create = dl_create, .data = dl_data, .error = dl_error, .done = dl_done };

static char *clip_text; static size_t clip_len;
static void clip_get(char **b, size_t *l) { *b = clip_text ? strndup(clip_text, clip_len) : NULL; *l = clip_len; }
static void clip_set(const char *b, size_t l, nsclipboard_styles st[], int n) { free(clip_text); clip_text = strndup(b, l); clip_len = l; }
static struct gui_clipboard_table clipboard_table = { .get = clip_get, .set = clip_set };

/* ----------------------------------------------------------- fetch, resources */

static const char *fetch_filetype(const char *path)
{
	const char *e = strrchr(path, '.');
	if (e) {
		if (!strcmp(e, ".html") || !strcmp(e, ".htm")) return "text/html";
		if (!strcmp(e, ".css")) return "text/css";
		if (!strcmp(e, ".png")) return "image/png";
		if (!strcmp(e, ".jpg")) return "image/jpeg";
		if (!strcmp(e, ".gif")) return "image/gif";
		if (!strcmp(e, ".txt")) return "text/plain";
	}
	return "application/octet-stream";
}

/* resource: URLs are served from memory (the crate embeds the files with include_bytes!); the
 * smoke test reads them from NS_RES. */
static nserror res_data(const char *path, const uint8_t **data, size_t *len)
{
	static const char *ok[] = { "default.css", "quirks.css", "internal.css", "adblock.css", NULL };
	int found = 0;
	for (const char **o = ok; *o; o++) found |= !strcmp(*o, path);
	if (!found) return NSERROR_NOT_FOUND;
	char full[4096]; snprintf(full, sizeof full, "%s/%s", getenv("NS_RES"), path);
	FILE *f = fopen(full, "rb");
	if (!f) return NSERROR_NOT_FOUND;
	fseek(f, 0, SEEK_END); long n = ftell(f); rewind(f);
	uint8_t *buf = malloc(n); if (fread(buf, 1, n, f) != (size_t)n) { fclose(f); free(buf); return NSERROR_NOT_FOUND; }
	fclose(f); *data = buf; *len = n;
	return NSERROR_OK;
}
static nserror res_release(const uint8_t *d) { free((void *)d); return NSERROR_OK; }
static struct nsurl *res_url(const char *path) { return NULL; }
static struct gui_fetch_table fetch_table = {
	.filetype = fetch_filetype, .get_resource_url = res_url,
	.get_resource_data = res_data, .release_resource_data = res_release,
};

/* ----------------------------------------------------------------------- misc */

static nserror launch_url(struct nsurl *u) { fprintf(stderr, "[misc] launch %s\n", nsurl_access(u)); return NSERROR_OK; }
static struct gui_misc_table misc_table = { .schedule = gui_schedule, .launch_url = launch_url };

/* ----------------------------------------------------------------------- output */

static void write_png(const char *path, struct canvas *cv)
{
	FILE *f = fopen(path, "wb"); if (!f) { perror(path); return; }
	png_structp p = png_create_write_struct(PNG_LIBPNG_VER_STRING, NULL, NULL, NULL);
	png_infop i = png_create_info_struct(p);
	png_init_io(p, f);
	png_set_IHDR(p, i, cv->w, cv->h, 8, PNG_COLOR_TYPE_RGB, PNG_INTERLACE_NONE, PNG_COMPRESSION_TYPE_DEFAULT, PNG_FILTER_TYPE_DEFAULT);
	png_write_info(p, i);
	uint8_t *row = malloc(cv->w * 3);
	for (int y = 0; y < cv->h; y++) {
		for (int x = 0; x < cv->w; x++) {
			uint32_t v = cv->px[(size_t)y * cv->w + x];
			row[x*3] = v >> 16; row[x*3+1] = v >> 8; row[x*3+2] = v;
		}
		png_write_row(p, row);
	}
	png_write_end(p, i); png_destroy_write_struct(&p, &i); free(row); fclose(f);
}

static nserror set_defaults(struct nsoption_s *d) { return NSERROR_OK; }

int main(int argc, char **argv)
{
	if (argc < 3) { fprintf(stderr, "usage: nssighurt URL OUT.png\n"); return 2; }
	double t0 = now_ms(); t_start = t0;
	struct netsurf_table table = {
		.misc = &misc_table, .window = &window_table, .download = &download_table,
		.clipboard = &clipboard_table, .fetch = &fetch_table, .bitmap = &bitmap_table,
		.layout = &(struct gui_layout_table){ .width = l_width, .position = l_position, .split = l_split },
		.llcache = null_llcache_table,
	};
	if (netsurf_register(&table) != NSERROR_OK) { fprintf(stderr, "register failed\n"); return 1; }
	fonts_init();
	nslog_init(NULL, &argc, argv);
	nsoption_init(set_defaults, &nsoptions, &nsoptions_default);
	nsoption_set_bool(enable_javascript, getenv("NS_JS") != NULL);
	nsoption_set_bool(send_referer, false);
	const char *ca = getenv("SSL_CERT_FILE");
	if (ca) nsoption_set_charp(ca_bundle, strdup(ca));
	char msgs[4096]; snprintf(msgs, sizeof msgs, "%s/en/Messages", getenv("NS_RES"));
	messages_add_from_file(msgs);
	if (netsurf_init(NULL) != NSERROR_OK) { fprintf(stderr, "init failed\n"); return 1; }
	double t_init = now_ms();

	struct nsurl *u; nsurl_create(argv[1], &u);
	struct browser_window *bw;
	if (browser_window_create(BW_CREATE_HISTORY, u, NULL, NULL, &bw) != NSERROR_OK) { fprintf(stderr, "create failed\n"); return 1; }
	nsurl_unref(u);

	/* The owner loop: run due callbacks, then sleep until a fetch descriptor is ready or the next
	 * callback is due; nothing ticks. */
	int iterations = 0; double deadline = now_ms() + 30000;
	while (now_ms() < deadline) {
		sched_run();
		fd_set r, w, e; int maxfd = -1;
		FD_ZERO(&r); FD_ZERO(&w); FD_ZERO(&e);
		/* fetch_fdset polls the fetchers (completing transfers, which schedules work) and
		 * then reports their descriptors: read the next deadline only after it. */
		fetch_fdset(&r, &w, &e, &maxfd);
		int ms = sched_next();
		if (getenv("NS_TRACE")) fprintf(stderr, "[loop %.1f] sched=%d maxfd=%d loading_done=%d\n", now_ms() - t0, ms, maxfd, loading_done);
		if (loading_done) break; /* stop throbber after a start: the page has loaded */
		struct timeval tv = { ms / 1000, (ms % 1000) * 1000 }, *tvp = ms < 0 ? NULL : &tv;
		if (maxfd < 0 && ms < 0) break; /* nothing to wait for */
		select(maxfd + 1, &r, &w, &e, tvp);
		iterations++;
	}
	double t_loaded = now_ms();
	if (getenv("NS_IDLE")) { /* idle probe: count the loop's wakeups on a loaded, untouched page */
		double end = now_ms() + atof(getenv("NS_IDLE")) * 1000; int wakes = 0;
		while (now_ms() < end) {
			sched_run();
			fd_set r, w, e; int maxfd = -1; FD_ZERO(&r); FD_ZERO(&w); FD_ZERO(&e);
			fetch_fdset(&r, &w, &e, &maxfd);
			int ms = sched_next(); double left = end - now_ms();
			if (ms < 0 || ms > left) ms = (int)left;
			struct timeval tv = { ms / 1000, (ms % 1000) * 1000 };
			select(maxfd + 1, &r, &w, &e, &tv);
			wakes++; fprintf(stderr, "[idle %.1f s] wake %d\n", (now_ms() - t_loaded) / 1000, wakes);
		}
		printf("idle: %d wakeups in %s s\n", wakes, getenv("NS_IDLE"));
	}

	struct gui_window *g = windows;
	struct canvas cv = { .w = g->w, .h = g->h };
	cv.px = malloc((size_t)cv.w * cv.h * 4);
	for (size_t i = 0; i < (size_t)cv.w * cv.h; i++) cv.px[i] = 0xffffffffu;
	cv.clip = (struct rect){ 0, 0, cv.w, cv.h };
	struct redraw_context ctx = { .interactive = true, .background_images = true, .plot = &plotters, .priv = &cv };
	struct rect clip = cv.clip;
	bool ok = browser_window_redraw(bw, 0, 0, &clip, &ctx);
	double t_drawn = now_ms();
	write_png(argv[2], &cv);

	int ew = 0, eh = 0; browser_window_get_extents(bw, false, &ew, &eh);
	printf("title: %s\nurl: %s\nstatus: %s\nredraw ok: %d, content extent %dx%d\n",
	       g->title ?: "(none)", g->url ?: "(none)", g->status ?: "(none)", ok, ew, eh);
	printf("init %.1f ms, load %.1f ms (%d loop iterations), first redraw %.1f ms\n",
	       t_init - t0, t_loaded - t_init, iterations, t_drawn - t_loaded);
	/* A second redraw: the steady-state cost of repainting without relayout. */
	double t1 = now_ms();
	browser_window_redraw(bw, 0, 0, &clip, &ctx);
	printf("second redraw %.1f ms\n", now_ms() - t1);
	return 0;
}
