/* Downloads are buffered up to 1 MiB and handed to the host as an event.
 * A larger body is an error. The bytes are not logged. GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

#include "desktop/download.h"
#include "netsurf/download.h"

#define DL_CAP (1024u * 1024u)

struct gui_download_window {
	struct gui_download_window *next;
	uint64_t id;
	uint64_t view_id;
	download_context *ctx;
	char *url;
	char *name;
	char *mime;
	uint8_t *data;
	size_t len;
	int state; /* 0 open, 1 done, 2 error, 3 refused */
};

static struct gui_download_window *downloads;
static uint64_t next_id = 1;

static void snprintf_copy(char *dst, size_t dst_len, const char *src);
static void free_dl(struct gui_download_window *d);

static void copy_str(char **dst, const char *src)
{
	*dst = NULL;
	if (!src)
		return;
	size_t n = strlen(src);
	char *d = malloc(n + 1);
	if (!d)
		return;
	memcpy(d, src, n + 1);
	*dst = d;
}

static void release_ctx(struct gui_download_window *d, int abort_fetch)
{
	if (!d->ctx)
		return;
	download_context *ctx = d->ctx;
	d->ctx = NULL;
	if (abort_fetch)
		download_context_abort(ctx);
	download_context_destroy(ctx);
}

static struct gui_download_window *dl_create(download_context *ctx, struct gui_window *parent)
{
	/* The core keeps the window it was given when the download began: a closed one is a
	 * tombstone (never freed before shutdown), and its download is refused. */
	if (!parent || parent->dead || parent->pending)
		return NULL;
	struct gui_download_window *d = calloc(1, sizeof *d);
	if (!d)
		return NULL;
	d->id = next_id++;
	d->ctx = ctx;
	d->view_id = parent->view_id;
	copy_str(&d->url, nsurl_access(download_context_get_url(ctx)));
	copy_str(&d->name, download_context_get_filename(ctx));
	copy_str(&d->mime, download_context_get_mime_type(ctx));
	d->next = downloads;
	downloads = d;
	nsh_emit(NSH_EV_DOWNLOAD, d->view_id, 0, 0, 0, 0, d->id, d->url);
	return d;
}

static nserror dl_data(struct gui_download_window *d, const char *data, unsigned int size)
{
	if (!d || d->state == 3)
		return NSERROR_INVALID;
	if ((size_t)size > DL_CAP || d->len + (size_t)size > DL_CAP) {
		d->state = 2;
		free(d->data);
		d->data = NULL;
		d->len = 0;
		nsh_emit(NSH_EV_DOWNLOAD, d->view_id, 2, 0, 0, 0, d->id, d->url);
		release_ctx(d, 1);
		return NSERROR_NOSPACE;
	}
	uint8_t *grown = realloc(d->data, d->len + size);
	if (!grown)
		return NSERROR_NOMEM;
	d->data = grown;
	memcpy(d->data + d->len, data, size);
	d->len += size;
	return NSERROR_OK;
}

static void dl_error(struct gui_download_window *d, const char *msg)
{
	(void)msg;
	if (!d || d->state == 3)
		return;
	d->state = 2;
	free(d->data);
	d->data = NULL;
	d->len = 0;
	nsh_emit(NSH_EV_DOWNLOAD, d->view_id, 2, 0, 0, 0, d->id, d->url);
	release_ctx(d, 0);
}

static void dl_done(struct gui_download_window *d)
{
	if (!d || d->state == 3)
		return;
	d->state = 1;
	nsh_emit(NSH_EV_DOWNLOAD, d->view_id, 1, (int)d->len, 0, 0, d->id, d->url);
	release_ctx(d, 0);
}

struct gui_download_table nsh_download_table = {
	.create = dl_create,
	.data = dl_data,
	.error = dl_error,
	.done = dl_done,
};

static struct gui_download_window *find_dl(uint64_t id)
{
	for (struct gui_download_window *d = downloads; d; d = d->next)
		if (d->id == id)
			return d;
	return NULL;
}

/* Releases the download: aborts it if it still runs, frees what it holds and unlinks it. The
 * core is told nothing more after an abort, so nothing calls the freed window. */
void nsh_download_answer(uint64_t id, int accept)
{
	(void)accept;
	for (struct gui_download_window **pp = &downloads; *pp; pp = &(*pp)->next) {
		if ((*pp)->id == id) {
			struct gui_download_window *d = *pp;
			*pp = d->next;
			free_dl(d);
			return;
		}
	}
}

int nsh_download_info(uint64_t id, char *url, size_t url_len, char *name, size_t name_len,
		      char *mime, size_t mime_len, const uint8_t **data, size_t *len)
{
	struct gui_download_window *d = find_dl(id);
	if (!d)
		return 0;
	if (url && url_len) {
		snprintf_copy(url, url_len, d->url);
	}
	if (name && name_len)
		snprintf_copy(name, name_len, d->name);
	if (mime && mime_len)
		snprintf_copy(mime, mime_len, d->mime);
	if (data)
		*data = d->data;
	if (len)
		*len = d->len;
	return 1;
}

static void snprintf_copy(char *dst, size_t dst_len, const char *src)
{
	if (!dst || dst_len == 0)
		return;
	if (!src) {
		dst[0] = '\0';
		return;
	}
	size_t n = strlen(src);
	if (n >= dst_len)
		n = dst_len - 1;
	memcpy(dst, src, n);
	dst[n] = '\0';
}

static void free_dl(struct gui_download_window *d)
{
	release_ctx(d, 1);
	free(d->url);
	free(d->name);
	free(d->mime);
	free(d->data);
	free(d);
}

void nsh_downloads_drop_view(uint64_t view_id)
{
	for (struct gui_download_window **pp = &downloads; *pp;) {
		if ((*pp)->view_id == view_id) {
			struct gui_download_window *d = *pp;
			*pp = d->next;
			free_dl(d);
		} else {
			pp = &(*pp)->next;
		}
	}
}
