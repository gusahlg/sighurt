/* file: MIME types, and the embedded resource: files. The bytes belong to the
 * caller (the Rust crate's include_bytes); release does not free them.
 * GPL-2.0-only. */
#include "internal.h"

#include <stdio.h>
#include <string.h>

#include "netsurf/fetch.h"

static nsh_bytes css_default, css_quirks, css_internal, css_adblock;

void nsh_resources_set(const nsh_config *cfg)
{
	css_default = cfg->css_default;
	css_quirks = cfg->css_quirks;
	css_internal = cfg->css_internal;
	css_adblock = cfg->css_adblock;
}

static const char *base_name(const char *path)
{
	const char *s = strrchr(path, '/');
	return s ? s + 1 : path;
}

static const nsh_bytes *resource(const char *path)
{
	const char *name = base_name(path);
	if (!strcmp(name, "default.css"))
		return &css_default;
	if (!strcmp(name, "quirks.css"))
		return &css_quirks;
	if (!strcmp(name, "internal.css"))
		return &css_internal;
	if (!strcmp(name, "adblock.css"))
		return &css_adblock;
	return NULL;
}

static const char *fetch_filetype(const char *path)
{
	const char *e = strrchr(path, '.');
	if (e) {
		if (!strcmp(e, ".html") || !strcmp(e, ".htm"))
			return "text/html";
		if (!strcmp(e, ".css"))
			return "text/css";
		if (!strcmp(e, ".png"))
			return "image/png";
		if (!strcmp(e, ".jpg") || !strcmp(e, ".jpeg"))
			return "image/jpeg";
		if (!strcmp(e, ".gif"))
			return "image/gif";
		if (!strcmp(e, ".svg"))
			return "image/svg+xml";
		if (!strcmp(e, ".txt"))
			return "text/plain";
	}
	/* No known extension: text if the first bytes look like it, else a download. */
	FILE *f = fopen(path, "rb");
	if (f) {
		unsigned char buf[512];
		size_t n = fread(buf, 1, sizeof buf, f);
		fclose(f);
		size_t odd = 0;
		for (size_t i = 0; i < n; i++)
			odd += buf[i] == 0 || (buf[i] < 9) || (buf[i] > 13 && buf[i] < 32);
		if (n > 0 && odd == 0)
			return "text/plain";
	}
	return "application/octet-stream";
}

static struct nsurl *res_url(const char *path)
{
	(void)path;
	return NULL;
}

static nserror res_data(const char *path, const uint8_t **data, size_t *len)
{
	const nsh_bytes *b = resource(path);
	if (!b || !b->data)
		return NSERROR_NOT_FOUND;
	*data = b->data;
	*len = b->len;
	return NSERROR_OK;
}

static nserror res_release(const uint8_t *data)
{
	(void)data;
	return NSERROR_OK;
}

struct gui_fetch_table nsh_fetch_table = {
	.filetype = fetch_filetype,
	.get_resource_url = res_url,
	.get_resource_data = res_data,
	.release_resource_data = res_release,
};
