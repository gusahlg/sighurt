/* The clipboard the core sees is a shadow. get reads it. set replaces it and
 * queues a write for the host. The text is never logged. GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>
#include <string.h>

#include "netsurf/clipboard.h"

static char *shadow;
static size_t shadow_len;

static void clip_get(char **buffer, size_t *length)
{
	if (!shadow) {
		*buffer = NULL;
		*length = 0;
		return;
	}
	*buffer = malloc(shadow_len + 1);
	if (!*buffer) {
		*length = 0;
		return;
	}
	memcpy(*buffer, shadow, shadow_len);
	(*buffer)[shadow_len] = '\0';
	*length = shadow_len;
}

static void replace_shadow(const char *buffer, size_t length)
{
	free(shadow);
	shadow = NULL;
	shadow_len = 0;
	if (!buffer || length == 0)
		return;
	if (length > 1024u * 1024u)
		length = 1024u * 1024u;
	shadow = malloc(length + 1);
	if (!shadow)
		return;
	memcpy(shadow, buffer, length);
	shadow[length] = '\0';
	shadow_len = length;
}

static void clip_set(const char *buffer, size_t length, nsclipboard_styles styles[], int n_styles)
{
	(void)styles;
	(void)n_styles;
	replace_shadow(buffer, length);
	/* Only a copy or cut the user's key press made reaches the browser. */
	if (nsh_input_view)
		nsh_emit(NSH_EV_CLIPBOARD, nsh_input_view, (int)shadow_len, 0, 0, 0, 0, shadow);
}

struct gui_clipboard_table nsh_clipboard_table = {
	.get = clip_get,
	.set = clip_set,
};

void nsh_clipboard_set_shadow(const char *utf8, size_t len)
{
	replace_shadow(utf8, len);
}

void nsh_clipboard_clear(void)
{
	free(shadow);
	shadow = NULL;
	shadow_len = 0;
}
