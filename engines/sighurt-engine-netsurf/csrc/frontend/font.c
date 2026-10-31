/* FreeType at 90 dpi, unhinted, advance summed in 26.6. That is what the text
 * plotter and the layout table both use, so a line wraps where it is drawn.
 * A missing font fails init; it does not exit the process. GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

#include <ft2build.h>
#include FT_FREETYPE_H

#include "netsurf/layout.h"
#include "utils/utf8.h"

static FT_Library ftlib;
static int ft_ready;
static FT_Face faces[3][2]; /* sans, serif, mono  x  regular, bold */

static int open_face(FT_Face *out, const char *path)
{
	*out = NULL;
	if (!path || !path[0])
		return 0;
	return FT_New_Face(ftlib, path, 0, out) == 0 && *out;
}

int nsh_fonts_init(const nsh_config *cfg)
{
	if (FT_Init_FreeType(&ftlib) != 0)
		return 0;
	ft_ready = 1;
	const char *paths[3][2] = {
		{ cfg->font_sans, cfg->font_sans_bold },
		{ cfg->font_serif, cfg->font_sans_bold },
		{ cfg->font_mono, cfg->font_mono },
	};
	for (int i = 0; i < 3; i++) {
		for (int j = 0; j < 2; j++) {
			if (!open_face(&faces[i][j], paths[i][j])) {
				nsh_fonts_fini();
				return 0;
			}
		}
	}
	return 1;
}

void nsh_fonts_fini(void)
{
	for (int i = 0; i < 3; i++) {
		for (int j = 0; j < 2; j++) {
			if (faces[i][j]) {
				FT_Done_Face(faces[i][j]);
				faces[i][j] = NULL;
			}
		}
	}
	if (ft_ready) {
		FT_Done_FreeType(ftlib);
		ft_ready = 0;
	}
}

static FT_Face face_for(const plot_font_style_t *fs)
{
	int fam = fs->family == PLOT_FONT_FAMILY_SERIF ? 1
		: fs->family == PLOT_FONT_FAMILY_MONOSPACE ? 2 : 0;
	int bold = fs->weight >= 600;
	FT_Face f = faces[fam][bold];
	if (!f)
		f = faces[0][0];
	/* NetSurf's size is in points; its CSS lengths assume 90 dpi. */
	FT_Set_Char_Size(f, 0, (FT_F26Dot6)(fs->size / 16), 90, 90);
	FT_Matrix m = {
		0x10000,
		(fs->flags & (FONTF_ITALIC | FONTF_OBLIQUE)) ? 0x3000 : 0,
		0,
		0x10000,
	};
	FT_Set_Transform(f, &m, NULL);
	return f;
}

static int advance(FT_Face f, uint32_t cp)
{
	if (FT_Load_Char(f, cp, FT_LOAD_NO_HINTING | FT_LOAD_NO_BITMAP))
		return 0;
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
	FT_Face f = face_for(fs);
	int sum = 0;
	for (size_t i = 0; i < len;)
		sum += advance(f, next_cp(s, len, &i));
	*w = (sum + 32) >> 6;
	return NSERROR_OK;
}

static nserror l_position(const plot_font_style_t *fs, const char *s, size_t len, int x,
			  size_t *off, int *ax)
{
	FT_Face f = face_for(fs);
	int sum = 0;
	size_t i = 0;
	while (i < len) {
		size_t j = i;
		int a = advance(f, next_cp(s, len, &j));
		if (((sum + a / 2) >> 6) > x)
			break;
		sum += a;
		i = j;
	}
	*off = i;
	*ax = (sum + 32) >> 6;
	return NSERROR_OK;
}

static nserror l_split(const plot_font_style_t *fs, const char *s, size_t len, int x,
		       size_t *off, int *ax)
{
	FT_Face f = face_for(fs);
	int sum = 0;
	int space_w = 0;
	long space = -1;
	size_t i = 0;
	while (i < len) {
		size_t j = i;
		uint32_t cp = next_cp(s, len, &j);
		int a = advance(f, cp);
		if (((sum + a + 32) >> 6) > x)
			break;
		if (cp == ' ') {
			space = (long)i;
			space_w = sum;
		}
		sum += a;
		i = j;
	}
	if (i >= len) {
		*off = len;
		*ax = (sum + 32) >> 6;
		return NSERROR_OK;
	}
	if (space >= 0) {
		*off = (size_t)space;
		*ax = (space_w + 32) >> 6;
		return NSERROR_OK;
	}
	while (i < len) {
		size_t j = i;
		uint32_t cp = next_cp(s, len, &j);
		if (cp == ' ')
			break;
		sum += advance(f, cp);
		i = j;
	}
	*off = i ? i : len;
	*ax = (sum + 32) >> 6;
	return NSERROR_OK;
}

struct gui_layout_table nsh_layout_table = {
	.width = l_width,
	.position = l_position,
	.split = l_split,
};

nserror nsh_plot_text(const struct redraw_context *ctx, const plot_font_style_t *fs, int x, int y,
		      const char *s, size_t len)
{
	struct nsh_canvas *cv = ctx->priv;
	FT_Face f = face_for(fs);
	int pen = x << 6;
	for (size_t i = 0; i < len;) {
		uint32_t cp = next_cp(s, len, &i);
		if (FT_Load_Char(f, cp, FT_LOAD_RENDER | FT_LOAD_NO_HINTING))
			continue;
		FT_GlyphSlot g = f->glyph;
		int gx = ((pen + 32) >> 6) + g->bitmap_left;
		int gy = y - g->bitmap_top;
		for (unsigned r = 0; r < g->bitmap.rows; r++) {
			for (unsigned c = 0; c < g->bitmap.width; c++) {
				int a = g->bitmap.buffer[r * g->bitmap.pitch + c];
				if (!a)
					continue;
				int px = gx + (int)c;
				int py = gy + (int)r;
				if (px < cv->clip.x0 || px >= cv->clip.x1 || py < cv->clip.y0 || py >= cv->clip.y1)
					continue;
				uint32_t *d = &cv->px[(size_t)py * (size_t)cv->stride + (size_t)px];
				uint32_t dst = *d;
				int cr = fs->foreground & 255;
				int cg = (fs->foreground >> 8) & 255;
				int cb = (fs->foreground >> 16) & 255;
				if (a >= 255) {
					*d = 0xff000000u | ((uint32_t)cr << 16) | ((uint32_t)cg << 8) | (uint32_t)cb;
				} else {
					int dr = (dst >> 16) & 255;
					int dg = (dst >> 8) & 255;
					int db = dst & 255;
					dr += ((cr - dr) * a + 127) / 255;
					dg += ((cg - dg) * a + 127) / 255;
					db += ((cb - db) * a + 127) / 255;
					*d = 0xff000000u | ((uint32_t)dr << 16) | ((uint32_t)dg << 8) | (uint32_t)db;
				}
			}
		}
		pen += (int)(g->linearHoriAdvance >> 10);
	}
	return NSERROR_OK;
}
