/* Software plotters into an ARGB8888 buffer (little-endian B,G,R,A). NetSurf's
 * colour has red in the low byte. Rows use the buffer's pixel stride, which
 * may be wider than the viewport. Knockout is off. GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>
#include <math.h>

#include "netsurf/plot_style.h"
#include "netsurf/bitmap.h"

static struct nsh_canvas *cvof(const struct redraw_context *ctx)
{
	return ctx->priv;
}

static int has_op(plot_operation_type_t t)
{
	return t != PLOT_OP_TYPE_NONE;
}

static inline uint32_t blend(uint32_t dst, int r, int g, int b, int a)
{
	if (a >= 255)
		return 0xff000000u | ((uint32_t)r << 16) | ((uint32_t)g << 8) | (uint32_t)b;
	int dr = (dst >> 16) & 255, dg = (dst >> 8) & 255, db = dst & 255;
	dr += ((r - dr) * a + 127) / 255;
	dg += ((g - dg) * a + 127) / 255;
	db += ((b - db) * a + 127) / 255;
	return 0xff000000u | ((uint32_t)dr << 16) | ((uint32_t)dg << 8) | (uint32_t)db;
}

static void put(struct nsh_canvas *cv, int x, int y, colour c, int a)
{
	if (x < cv->clip.x0 || x >= cv->clip.x1 || y < cv->clip.y0 || y >= cv->clip.y1)
		return;
	if (x < 0 || y < 0 || x >= cv->w || y >= cv->h)
		return;
	uint32_t *d = &cv->px[(size_t)y * (size_t)cv->stride + (size_t)x];
	*d = blend(*d, c & 255, (c >> 8) & 255, (c >> 16) & 255, a);
}

static void fill_rect(struct nsh_canvas *cv, int x0, int y0, int x1, int y1, colour c)
{
	if (x0 < cv->clip.x0)
		x0 = cv->clip.x0;
	if (y0 < cv->clip.y0)
		y0 = cv->clip.y0;
	if (x1 > cv->clip.x1)
		x1 = cv->clip.x1;
	if (y1 > cv->clip.y1)
		y1 = cv->clip.y1;
	for (int y = y0; y < y1; y++)
		for (int x = x0; x < x1; x++)
			put(cv, x, y, c, 255);
}

static nserror p_clip(const struct redraw_context *ctx, const struct rect *r)
{
	struct nsh_canvas *cv = cvof(ctx);
	cv->clip.x0 = r->x0 < 0 ? 0 : r->x0;
	cv->clip.y0 = r->y0 < 0 ? 0 : r->y0;
	cv->clip.x1 = r->x1 > cv->w ? cv->w : r->x1;
	cv->clip.y1 = r->y1 > cv->h ? cv->h : r->y1;
	if (cv->clip.x1 < cv->clip.x0)
		cv->clip.x1 = cv->clip.x0;
	if (cv->clip.y1 < cv->clip.y0)
		cv->clip.y1 = cv->clip.y0;
	return NSERROR_OK;
}

static void hline(struct nsh_canvas *cv, int x0, int x1, int y, colour c, int w, plot_operation_type_t t)
{
	int step = w ? w : 1;
	for (int x = x0; x < x1; x++) {
		if (t == PLOT_OP_TYPE_DOT && ((x - x0) / step) % 2)
			continue;
		if (t == PLOT_OP_TYPE_DASH && ((x - x0) / (3 * step)) % 2)
			continue;
		for (int k = 0; k < w; k++)
			put(cv, x, y + k, c, 255);
	}
}

static void vline(struct nsh_canvas *cv, int x, int y0, int y1, colour c, int w, plot_operation_type_t t)
{
	int step = w ? w : 1;
	for (int y = y0; y < y1; y++) {
		if (t == PLOT_OP_TYPE_DOT && ((y - y0) / step) % 2)
			continue;
		if (t == PLOT_OP_TYPE_DASH && ((y - y0) / (3 * step)) % 2)
			continue;
		for (int k = 0; k < w; k++)
			put(cv, x + k, y, c, 255);
	}
}

static int stroke_width(const plot_style_t *s)
{
	int w = plot_style_fixed_to_int(s->stroke_width);
	return w < 1 ? 1 : w;
}

static nserror p_rect(const struct redraw_context *ctx, const plot_style_t *s, const struct rect *r)
{
	struct nsh_canvas *cv = cvof(ctx);
	if (has_op(s->fill_type) && s->fill_colour != NS_TRANSPARENT)
		fill_rect(cv, r->x0, r->y0, r->x1, r->y1, s->fill_colour);
	if (has_op(s->stroke_type) && s->stroke_colour != NS_TRANSPARENT) {
		int w = stroke_width(s);
		hline(cv, r->x0, r->x1, r->y0, s->stroke_colour, w, s->stroke_type);
		hline(cv, r->x0, r->x1, r->y1 - w, s->stroke_colour, w, s->stroke_type);
		vline(cv, r->x0, r->y0, r->y1, s->stroke_colour, w, s->stroke_type);
		vline(cv, r->x1 - w, r->y0, r->y1, s->stroke_colour, w, s->stroke_type);
	}
	return NSERROR_OK;
}

static nserror p_line(const struct redraw_context *ctx, const plot_style_t *s, const struct rect *l)
{
	struct nsh_canvas *cv = cvof(ctx);
	int w = stroke_width(s);
	int x0 = l->x0, y0 = l->y0, x1 = l->x1, y1 = l->y1;
	if (y0 == y1) {
		hline(cv, x0 < x1 ? x0 : x1, (x0 < x1 ? x1 : x0) + 1, y0, s->stroke_colour, w, s->stroke_type);
		return NSERROR_OK;
	}
	if (x0 == x1) {
		vline(cv, x0, y0 < y1 ? y0 : y1, (y0 < y1 ? y1 : y0) + 1, s->stroke_colour, w, s->stroke_type);
		return NSERROR_OK;
	}
	int dx = abs(x1 - x0), dy = -abs(y1 - y0);
	int sx = x0 < x1 ? 1 : -1, sy = y0 < y1 ? 1 : -1;
	int err = dx + dy;
	for (;;) {
		for (int k = 0; k < w; k++)
			put(cv, x0, y0 + k, s->stroke_colour, 255);
		if (x0 == x1 && y0 == y1)
			break;
		int e2 = 2 * err;
		if (e2 >= dy) {
			err += dy;
			x0 += sx;
		}
		if (e2 <= dx) {
			err += dx;
			y0 += sy;
		}
	}
	return NSERROR_OK;
}

static nserror p_polygon(const struct redraw_context *ctx, const plot_style_t *s, const int *p, unsigned n)
{
	struct nsh_canvas *cv = cvof(ctx);
	if (!has_op(s->fill_type) || n < 3)
		return NSERROR_OK;
	int ymin = p[1], ymax = p[1];
	for (unsigned i = 1; i < n; i++) {
		int y = p[2 * i + 1];
		if (y < ymin)
			ymin = y;
		if (y > ymax)
			ymax = y;
	}
	if (ymin < cv->clip.y0)
		ymin = cv->clip.y0;
	if (ymax > cv->clip.y1)
		ymax = cv->clip.y1;
	int *xs = malloc(n * sizeof *xs);
	if (!xs)
		return NSERROR_NOMEM;
	for (int y = ymin; y < ymax; y++) {
		unsigned k = 0;
		for (unsigned i = 0, j = n - 1; i < n; j = i++) {
			int yi = p[2 * i + 1], yj = p[2 * j + 1];
			if ((yi <= y && yj > y) || (yj <= y && yi > y))
				xs[k++] = p[2 * i] + (int)((long)(y - yi) * (p[2 * j] - p[2 * i]) / (yj - yi));
		}
		for (unsigned a = 1; a < k; a++) {
			for (unsigned b = a; b > 0 && xs[b - 1] > xs[b]; b--) {
				int t = xs[b];
				xs[b] = xs[b - 1];
				xs[b - 1] = t;
			}
		}
		for (unsigned a = 0; a + 1 < k; a += 2)
			for (int x = xs[a]; x < xs[a + 1]; x++)
				put(cv, x, y, s->fill_colour, 255);
	}
	free(xs);
	return NSERROR_OK;
}

static nserror p_disc(const struct redraw_context *ctx, const plot_style_t *s, int cx, int cy, int radius)
{
	struct nsh_canvas *cv = cvof(ctx);
	if (radius < 0)
		return NSERROR_OK;
	int sw = stroke_width(s);
	for (int y = -radius; y <= radius; y++) {
		for (int x = -radius; x <= radius; x++) {
			int d = x * x + y * y;
			if (has_op(s->fill_type) && d <= radius * radius)
				put(cv, cx + x, cy + y, s->fill_colour, 255);
			else if (has_op(s->stroke_type) && d <= radius * radius && d >= (radius - sw) * (radius - sw))
				put(cv, cx + x, cy + y, s->stroke_colour, 255);
		}
	}
	return NSERROR_OK;
}

static nserror p_arc(const struct redraw_context *ctx, const plot_style_t *s, int cx, int cy, int radius,
		     int a1, int a2)
{
	struct nsh_canvas *cv = cvof(ctx);
	if (a2 < a1)
		a2 += 360;
	for (int a = a1 * 4; a <= a2 * 4; a++) {
		double t = a / 4.0 * M_PI / 180.0;
		put(cv, cx + (int)lround(radius * cos(t)), cy - (int)lround(radius * sin(t)), s->stroke_colour, 255);
	}
	return NSERROR_OK;
}

static nserror p_path(const struct redraw_context *ctx, const plot_style_t *s, const float *p, unsigned n,
		      const float transform[6])
{
	(void)ctx;
	(void)s;
	(void)p;
	(void)n;
	(void)transform;
	/* SVG paths through libsvgtiny are not drawn yet (plan step 11). */
	return NSERROR_OK;
}

static nserror p_bitmap(const struct redraw_context *ctx, struct bitmap *b, int x, int y, int w, int h,
			colour bg, bitmap_flags_t flags)
{
	(void)bg;
	struct nsh_canvas *cv = cvof(ctx);
	if (!b || !b->px || w <= 0 || h <= 0)
		return NSERROR_OK;
	int x0 = x, x1 = x + w, y0 = y, y1 = y + h;
	if (flags & BITMAPF_REPEAT_X) {
		x0 = cv->clip.x0 - ((cv->clip.x0 - x) % w + w) % w;
		x1 = cv->clip.x1;
	}
	if (flags & BITMAPF_REPEAT_Y) {
		y0 = cv->clip.y0 - ((cv->clip.y0 - y) % h + h) % h;
		y1 = cv->clip.y1;
	}
	for (int ty = y0; ty < y1; ty += h) {
		for (int tx = x0; tx < x1; tx += w) {
			int ya = ty < cv->clip.y0 ? cv->clip.y0 : ty;
			int yb = ty + h > cv->clip.y1 ? cv->clip.y1 : ty + h;
			int xa = tx < cv->clip.x0 ? cv->clip.x0 : tx;
			int xb = tx + w > cv->clip.x1 ? cv->clip.x1 : tx + w;
			for (int py = ya; py < yb; py++) {
				int sy = (int)((int64_t)(py - ty) * b->h / h);
				if (sy < 0 || sy >= b->h)
					continue;
				for (int px = xa; px < xb; px++) {
					int sx = (int)((int64_t)(px - tx) * b->w / w);
					if (sx < 0 || sx >= b->w)
						continue;
					const uint8_t *src = &b->px[((size_t)sy * (size_t)b->w + (size_t)sx) * 4];
					uint32_t *d = &cv->px[(size_t)py * (size_t)cv->stride + (size_t)px];
					*d = blend(*d, src[0], src[1], src[2], b->opaque ? 255 : src[3]);
				}
			}
		}
	}
	return NSERROR_OK;
}

static nserror p_noop_g(const struct redraw_context *ctx, const char *name)
{
	(void)ctx;
	(void)name;
	return NSERROR_OK;
}

static nserror p_noop(const struct redraw_context *ctx)
{
	(void)ctx;
	return NSERROR_OK;
}

const struct plotter_table nsh_plotters = {
	.clip = p_clip,
	.arc = p_arc,
	.disc = p_disc,
	.line = p_line,
	.rectangle = p_rect,
	.polygon = p_polygon,
	.path = p_path,
	.bitmap = p_bitmap,
	.text = nsh_plot_text,
	.group_start = p_noop_g,
	.group_end = p_noop,
	.flush = p_noop,
	.option_knockout = false,
};
