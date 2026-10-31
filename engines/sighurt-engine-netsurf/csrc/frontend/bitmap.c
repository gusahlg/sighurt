/* Images are R,G,B,A bytes, not premultiplied. History thumbnails are not
 * drawn: render returns success and writes nothing. GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>

#include "netsurf/bitmap.h"

static void *b_create(int w, int h, enum gui_bitmap_flags flags)
{
	if (w < 1 || h < 1)
		return NULL;
	struct bitmap *b = calloc(1, sizeof *b);
	if (!b)
		return NULL;
	b->w = w;
	b->h = h;
	b->opaque = flags & BITMAP_OPAQUE;
	b->px = calloc((size_t)w * (size_t)h, 4);
	if (!b->px) {
		free(b);
		return NULL;
	}
	return b;
}

static void b_destroy(void *bitmap)
{
	struct bitmap *b = bitmap;
	if (!b)
		return;
	free(b->px);
	free(b);
}

static void b_set_opaque(void *bitmap, bool opaque)
{
	((struct bitmap *)bitmap)->opaque = opaque;
}

static bool b_get_opaque(void *bitmap)
{
	return ((struct bitmap *)bitmap)->opaque;
}

static unsigned char *b_buffer(void *bitmap)
{
	return ((struct bitmap *)bitmap)->px;
}

static size_t b_stride(void *bitmap)
{
	return (size_t)((struct bitmap *)bitmap)->w * 4;
}

static int b_w(void *bitmap)
{
	return ((struct bitmap *)bitmap)->w;
}

static int b_h(void *bitmap)
{
	return ((struct bitmap *)bitmap)->h;
}

static void b_modified(void *bitmap)
{
	(void)bitmap;
}

static nserror b_render(struct bitmap *bitmap, struct hlcache_handle *content)
{
	(void)bitmap;
	(void)content;
	return NSERROR_OK;
}

struct gui_bitmap_table nsh_bitmap_table = {
	.create = b_create,
	.destroy = b_destroy,
	.set_opaque = b_set_opaque,
	.get_opaque = b_get_opaque,
	.get_buffer = b_buffer,
	.get_rowstride = b_stride,
	.get_width = b_w,
	.get_height = b_h,
	.modified = b_modified,
	.render = b_render,
};
