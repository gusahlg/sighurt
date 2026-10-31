/* Shared by the sighurt frontend. Not part of the Rust API. GPL-2.0-only. */
#ifndef NSH_INTERNAL_H
#define NSH_INTERNAL_H

#include <stddef.h>
#include <stdint.h>
#include <stdbool.h>

#include "ns_sighurt.h"

#include "utils/errors.h"
#include "utils/nsurl.h"
#include "netsurf/browser_window.h"
#include "netsurf/plotters.h"

struct gui_window {
	struct gui_window *next;
	struct browser_window *bw;
	uint64_t view_id;
	uint64_t opener;
	int w, h;
	int scale_120;
	double zoom;
	int sx, sy;
	bool seen_start;
	bool dirty;
	bool dirty_all;
	bool has_dirty;
	struct rect dirty_r;
	bool pending; /* spawned for target=_blank; decided at the first set_url */
	bool doomed; /* destroy on the next poll, not inside the callback */
	bool hidden;
	int pointer;
	struct form_control *gadget;
};

/* The image buffer is R,G,B,A, not premultiplied. The plotter reads these fields. */
struct bitmap {
	uint8_t *px;
	int w, h;
	bool opaque;
};

struct nsh_canvas {
	uint32_t *px;
	int w, h, stride; /* stride is pixels, not bytes */
	struct rect clip;
};

/* 0 none, 1 live, 2 finished. */
extern int nsh_core_state;

double nsh_now_ms(void);
nserror nsh_schedule(int ms, void (*fn)(void *), void *p);
/* Runs at most `limit` due callbacks, housekeeping included. Returns 1 if more are due. */
int nsh_sched_run(int limit);
/* Milliseconds until the next page callback, or the once-a-minute housekeeping wake.
 * -1 if nothing should wake the owner. */
int nsh_sched_deadline(int live_views);
void nsh_sched_clear(void);
void nsh_sched_mark_housekeeping(void);

int nsh_fonts_init(const nsh_config *cfg);
void nsh_fonts_fini(void);

void nsh_resources_set(const nsh_config *cfg);

nserror nsh_plot_text(const struct redraw_context *ctx, const plot_font_style_t *fs, int x, int y,
		      const char *s, size_t len);

extern const struct plotter_table nsh_plotters;
extern struct gui_layout_table nsh_layout_table;
extern struct gui_bitmap_table nsh_bitmap_table;
extern struct gui_window_table nsh_window_table;
extern struct gui_download_table nsh_download_table;
extern struct gui_clipboard_table nsh_clipboard_table;
extern struct gui_fetch_table nsh_fetch_table;
extern struct gui_misc_table nsh_misc_table;

struct gui_window *nsh_windows(void);
int nsh_live_views(void);
struct gui_window *nsh_view_by_id(uint64_t view_id);

/* What the next gw_create is being opened for. active is 0 when the core
 * creates a window itself (target=_blank). */
struct nsh_opening {
	int active;
	uint64_t view_id;
	int w, h, scale_120;
};
extern struct nsh_opening nsh_opening;

void nsh_emit(int kind, uint64_t view_id, int i0, int i1, int i2, int i3, uint64_t u0,
	      const char *text);
void nsh_drop_events(uint64_t view_id);
void nsh_clear_events(void);
void nsh_reap_doomed(void);

void nsh_downloads_drop_view(uint64_t view_id);
void nsh_clipboard_clear(void);

#endif
