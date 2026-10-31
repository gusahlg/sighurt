// CEF smoke test for Sighurt (docs/rewrite/CEF.md): the C API only, libcef.so opened with dlopen,
// windowless rendering with an external message pump driven by poll(2) (no ticks: the loop sleeps
// until CEF's OnScheduleMessagePumpWork asks for work), one page, one frame written as a PPM.
//
//   smoke LIBDIR HELPER URL OUT.ppm [ozone-platform] [accelerated]
// Prints one "key=value" line per measurement on stdout.
#define _GNU_SOURCE
#ifndef CEF_API_VERSION
#define CEF_API_VERSION 15000
#endif
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdatomic.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/eventfd.h>
#include <time.h>
#include <unistd.h>

#include "include/capi/cef_app_capi.h"
#include "include/capi/cef_browser_capi.h"
#include "include/capi/cef_browser_process_handler_capi.h"
#include "include/capi/cef_client_capi.h"
#include "include/capi/cef_command_line_capi.h"
#include "include/capi/cef_display_handler_capi.h"
#include "include/capi/cef_life_span_handler_capi.h"
#include "include/capi/cef_load_handler_capi.h"
#include "include/capi/cef_render_handler_capi.h"
#include "include/cef_api_hash.h"

#define W 1280
#define H 800

static double now_ms(void) {
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return t.tv_sec * 1e3 + t.tv_nsec / 1e6;
}
static double t0;

// libcef.so's entry points, found with dlsym.
static const char* (*p_api_hash)(int, int);
static int (*p_initialize)(const cef_main_args_t*, const cef_settings_t*, cef_app_t*, void*);
static void (*p_do_work)(void);
static void (*p_shutdown)(void);
static int (*p_create_browser)(const cef_window_info_t*, cef_client_t*, const cef_string_t*,
                               const cef_browser_settings_t*, cef_dictionary_value_t*,
                               cef_request_context_t*);
static int (*p_utf8_to_utf16)(const char*, size_t, cef_string_utf16_t*);

static void set_str(cef_string_t* d, const char* s) { p_utf8_to_utf16(s, strlen(s), d); }

// Static, never freed: the reference count is not needed for a smoke test.
#define REFCOUNT(T)                                                                      \
  static void CEF_CALLBACK T##_add(cef_base_ref_counted_t* s) { (void)s; }               \
  static int CEF_CALLBACK T##_rel(cef_base_ref_counted_t* s) { (void)s; return 0; }      \
  static int CEF_CALLBACK T##_one(cef_base_ref_counted_t* s) { (void)s; return 1; }
REFCOUNT(g)
#define BASE(o, T)                                                                       \
  (o).base.size = sizeof(T);                                                             \
  (o).base.add_ref = g_add;                                                              \
  (o).base.release = g_rel;                                                              \
  (o).base.has_one_ref = g_one;                                                          \
  (o).base.has_at_least_one_ref = g_one

static int wake_fd;
static _Atomic long schedule_calls;
static _Atomic long long next_delay_ms = -1;  // delay of the latest request

static double t_ctx = -1, t_loaded = -1, t_paint = -1, t_accel = -1;
static long paints, accel_paints;
static int pw, ph, saved;
static const char* out_path;
static int want_accel;

// --- OnScheduleMessagePumpWork: any thread; this is the whole of "the engine wakes the owner".
static void CEF_CALLBACK on_schedule(cef_browser_process_handler_t* s, int64_t delay_ms) {
  (void)s;
  atomic_fetch_add(&schedule_calls, 1);
  atomic_store(&next_delay_ms, delay_ms);
  uint64_t one = 1;
  if (write(wake_fd, &one, 8) < 0) {}
}
static void CEF_CALLBACK on_ctx(cef_browser_process_handler_t* s) {
  (void)s;
  t_ctx = now_ms() - t0;
}
static cef_browser_process_handler_t bph;

static const char* ozone;
static void CEF_CALLBACK on_cmdline(cef_app_t* s, const cef_string_t* proc,
                                    cef_command_line_t* cl) {
  (void)s;
  if (proc && proc->length) return;
  cef_string_t k = {0};
  if (ozone && *ozone) {
    set_str(&k, "ozone-platform");
    cef_string_t v = {0};
    set_str(&v, ozone);
    cl->append_switch_with_value(cl, &k, &v);
  }
  const char* extra = getenv("CEF_SMOKE_SWITCH");  // one more switch, "name" or "name=value"
  if (extra) {
    char* e = strdup(extra);
    char* eq = strchr(e, '=');
    cef_string_t kk = {0}, vv = {0};
    if (eq) {
      *eq = 0;
      set_str(&kk, e);
      set_str(&vv, eq + 1);
      cl->append_switch_with_value(cl, &kk, &vv);
    } else {
      set_str(&kk, e);
      cl->append_switch(cl, &kk);
    }
  }
}
static cef_browser_process_handler_t* CEF_CALLBACK get_bph(cef_app_t* s) {
  (void)s;
  return &bph;
}
static cef_app_t app;

// --- the render handler
static void CEF_CALLBACK get_view_rect(cef_render_handler_t* s, cef_browser_t* b, cef_rect_t* r) {
  (void)s; (void)b;
  r->x = 0; r->y = 0; r->width = W; r->height = H;
}
static int CEF_CALLBACK get_screen_info(cef_render_handler_t* s, cef_browser_t* b,
                                        cef_screen_info_t* i) {
  (void)s; (void)b;
  i->device_scale_factor = 1.0f;
  i->depth = 24; i->depth_per_component = 8; i->is_monochrome = 0;
  i->rect.x = 0; i->rect.y = 0; i->rect.width = W; i->rect.height = H;
  i->available_rect = i->rect;
  return 1;
}
static void CEF_CALLBACK on_paint(cef_render_handler_t* s, cef_browser_t* b,
                                  cef_paint_element_type_t type, size_t n,
                                  const cef_rect_t* dirty, const void* buf, int w, int h) {
  (void)s; (void)b; (void)n; (void)dirty;
  if (type != PET_VIEW) return;
  paints++;
  if (t_paint < 0) t_paint = now_ms() - t0;
  pw = w; ph = h;
  // Keep the last frame once the page has loaded (set in on_loading_state): written at exit.
  static unsigned char* keep;
  static size_t cap;
  size_t bytes = (size_t)w * h * 4;
  if (bytes > cap) { keep = realloc(keep, bytes); cap = bytes; }
  memcpy(keep, buf, bytes);
  // PPM from BGRA, written on every frame (cheap enough for a test; last wins).
  if (out_path && t_loaded >= 0) {
    FILE* f = fopen(out_path, "wb");
    if (f) {
      fprintf(f, "P6\n%d %d\n255\n", w, h);
      for (size_t i = 0; i < (size_t)w * h; i++) {
        unsigned char p[3] = {keep[i * 4 + 2], keep[i * 4 + 1], keep[i * 4]};
        fwrite(p, 1, 3, f);
      }
      fclose(f);
      saved++;
    }
  }
}
static void CEF_CALLBACK on_accel(cef_render_handler_t* s, cef_browser_t* b,
                                  cef_paint_element_type_t type, size_t n, const cef_rect_t* d,
                                  const cef_accelerated_paint_info_t* info) {
  (void)s; (void)b; (void)n; (void)d;
  if (type != PET_VIEW) return;
  accel_paints++;
  if (t_accel < 0) {
    t_accel = now_ms() - t0;
    fprintf(stdout, "accel_first planes=%d modifier=0x%llx format=%d fd0=%d stride0=%u offset0=%llu\n",
            info->plane_count, (unsigned long long)info->modifier, (int)info->format,
            info->planes[0].fd, info->planes[0].stride,
            (unsigned long long)info->planes[0].offset);
  }
}
static cef_render_handler_t rh;

// --- load and lifespan
static int done_loading;
static void CEF_CALLBACK on_loading(cef_load_handler_t* s, cef_browser_t* b, int loading, int back,
                                    int fwd) {
  (void)s; (void)b; (void)back; (void)fwd;
  if (!loading) { t_loaded = now_ms() - t0; done_loading = 1; }
}
static cef_load_handler_t lh;
static cef_browser_t* the_browser;
static double t_created = -1;
static double next_due_init = -1;
static void CEF_CALLBACK on_created(cef_life_span_handler_t* s, cef_browser_t* b) {
  (void)s;
  t_created = now_ms() - t0;
  the_browser = b;
  b->base.add_ref(&b->base);
}
static int closed;
static void CEF_CALLBACK on_before_close(cef_life_span_handler_t* s, cef_browser_t* b) {
  (void)s; (void)b;
  closed = 1;
}
static cef_life_span_handler_t lsh;
static void CEF_CALLBACK on_title(cef_display_handler_t* s, cef_browser_t* b,
                                  const cef_string_t* t) {
  (void)s; (void)b;
  char buf[256]; size_t n = 0;
  for (size_t i = 0; i < t->length && n < 255; i++) buf[n++] = t->str[i] < 128 ? (char)t->str[i] : '?';
  buf[n] = 0;
  fprintf(stdout, "title=%s at_ms=%.0f\n", buf, now_ms() - t0);
}
static cef_display_handler_t dh;

static cef_render_handler_t* CEF_CALLBACK get_rh(cef_client_t* s) { (void)s; return &rh; }
static cef_life_span_handler_t* CEF_CALLBACK get_lsh(cef_client_t* s) { (void)s; return &lsh; }
static cef_load_handler_t* CEF_CALLBACK get_lh(cef_client_t* s) { (void)s; return &lh; }
static cef_display_handler_t* CEF_CALLBACK get_dh(cef_client_t* s) { (void)s; return &dh; }
static cef_client_t client;

// --- measurements
static long rss_kb_of(int pid, long* pss) {
  char p[64], line[256];
  long rss = 0;
  snprintf(p, sizeof p, "/proc/%d/smaps_rollup", pid);
  FILE* f = fopen(p, "r");
  if (!f) return 0;
  while (fgets(line, sizeof line, f)) {
    if (!strncmp(line, "Rss:", 4)) rss = atol(line + 4);
    if (!strncmp(line, "Pss:", 4)) *pss += atol(line + 4);
  }
  fclose(f);
  return rss;
}
static void report_tree(const char* tag) {
  // Sum RSS and PSS over this process and all its descendants (CEF's helpers).
  char cmd[128];
  snprintf(cmd, sizeof cmd, "pgrep -P %d -a 2>/dev/null; true", getpid());
  long rss = 0, pss = 0;
  int pids[64], n = 0;
  pids[n++] = getpid();
  for (int i = 0; i < n && n < 64; i++) {
    snprintf(cmd, sizeof cmd, "pgrep -P %d", pids[i]);
    FILE* f = popen(cmd, "r");
    int c;
    while (f && fscanf(f, "%d", &c) == 1 && n < 64) pids[n++] = c;
    if (f) pclose(f);
  }
  long self_rss = 0, self_pss = 0;
  for (int i = 0; i < n; i++) {
    long ps = 0;
    long r = rss_kb_of(pids[i], &ps);
    if (i == 0) { self_rss = r; self_pss = ps; }
    rss += r; pss += ps;
    char comm[64] = "?", pth[64];
    snprintf(pth, sizeof pth, "/proc/%d/cmdline", pids[i]);
    FILE* cf = fopen(pth, "r");
    if (cf) {
      char buf[512]; size_t k = fread(buf, 1, sizeof buf - 1, cf); buf[k] = 0; fclose(cf);
      // report the --type= switch of CEF's children
      char* t = NULL;
      for (size_t o = 0; o < k; o += strlen(buf + o) + 1) if (!strncmp(buf + o, "--type=", 7)) t = buf + o + 7;
      snprintf(comm, sizeof comm, "%s", t ? t : (i == 0 ? "browser(this)" : "child"));
    }
    fprintf(stdout, "proc_%s pid=%d rss_kb=%ld pss_kb=%ld type=%s\n", tag, pids[i], r, ps, comm);
  }
  fprintf(stdout, "tree_%s procs=%d rss_kb=%ld pss_kb=%ld self_rss_kb=%ld self_pss_kb=%ld\n", tag,
          n, rss, pss, self_rss, self_pss);
}

int main(int argc, char** argv) {
  setvbuf(stdout, NULL, _IOLBF, 0);
  t0 = now_ms();
  if (argc < 5) { fprintf(stderr, "usage: smoke LIBDIR HELPER URL OUT.ppm [ozone] [accel]\n"); return 2; }
  const char* libdir = strdup(argv[1]);
  const char* helper = strdup(argv[2]);
  const char* url = strdup(argv[3]);
  // Copy what is needed later: Chromium overwrites the argv area to set the process title, so
  // argv[i] strings run together after cef_initialize (found by this test; CEF.md, risks).
  out_path = strdup(argv[4]);
  ozone = strdup(argc > 5 ? argv[5] : "");
  want_accel = argc > 6 && !strcmp(argv[6], "accel");

  char lib[1024];
  snprintf(lib, sizeof lib, "%s/libcef.so", libdir);
  double t_dl0 = now_ms();
  long rss_before = 0, pss_before = 0;
  rss_before = rss_kb_of(getpid(), &pss_before);
  void* h = dlopen(lib, RTLD_NOW | RTLD_LOCAL);
  if (!h) { fprintf(stderr, "dlopen: %s\n", dlerror()); return 1; }
  fprintf(stdout, "dlopen_ms=%.0f rss_before_kb=%ld\n", now_ms() - t_dl0, rss_before);
#define SYM(v, n) *(void**)&v = dlsym(h, n); if (!v) { fprintf(stderr, "no %s\n", n); return 1; }
  SYM(p_api_hash, "cef_api_hash");
  SYM(p_initialize, "cef_initialize");
  SYM(p_do_work, "cef_do_message_loop_work");
  SYM(p_shutdown, "cef_shutdown");
  SYM(p_create_browser, "cef_browser_host_create_browser");
  SYM(p_utf8_to_utf16, "cef_string_utf8_to_utf16");
  long rss_dl = 0, pss_dl = 0;
  rss_dl = rss_kb_of(getpid(), &pss_dl);
  fprintf(stdout, "rss_after_dlopen_kb=%ld\n", rss_dl);
  const char* hash = p_api_hash(CEF_API_VERSION, 0);
  fprintf(stdout, "api_hash=%s expected=%s match=%d\n", hash, CEF_API_HASH_PLATFORM,
          !strcmp(hash, CEF_API_HASH_PLATFORM));

  wake_fd = eventfd(0, EFD_NONBLOCK | EFD_CLOEXEC);
  BASE(bph, cef_browser_process_handler_t);
  bph.on_schedule_message_pump_work = on_schedule;
  bph.on_context_initialized = on_ctx;
  BASE(app, cef_app_t);
  app.on_before_command_line_processing = on_cmdline;
  app.get_browser_process_handler = get_bph;
  BASE(rh, cef_render_handler_t);
  rh.get_view_rect = get_view_rect;
  rh.get_screen_info = get_screen_info;
  rh.on_paint = on_paint;
  rh.on_accelerated_paint = on_accel;
  BASE(lh, cef_load_handler_t);
  lh.on_loading_state_change = on_loading;
  BASE(lsh, cef_life_span_handler_t);
  lsh.on_after_created = on_created;
  lsh.on_before_close = on_before_close;
  BASE(dh, cef_display_handler_t);
  dh.on_title_change = on_title;
  BASE(client, cef_client_t);
  client.get_render_handler = get_rh;
  client.get_life_span_handler = get_lsh;
  client.get_load_handler = get_lh;
  client.get_display_handler = get_dh;

  cef_main_args_t margs = {argc, argv};
  cef_settings_t st;
  memset(&st, 0, sizeof st);
  st.size = sizeof st;
  st.no_sandbox = getenv("CEF_SMOKE_NO_SANDBOX") ? 1 : 0;
  set_str(&st.browser_subprocess_path, helper);
  st.windowless_rendering_enabled = getenv("CEF_SMOKE_NOOSR") ? 0 : 1;
  st.external_message_pump = getenv("CEF_SMOKE_NOPUMP") ? 0 : 1;
  st.disable_signal_handlers = getenv("CEF_SMOKE_SIGNALS") ? 0 : 1;
  st.multi_threaded_message_loop = getenv("CEF_SMOKE_MT") ? 1 : 0;
  char b[1100];
  const char* cache = getenv("CEF_SMOKE_CACHE");
  if (cache) { set_str(&st.root_cache_path, cache); set_str(&st.cache_path, cache); }
  if (!getenv("CEF_SMOKE_NODIRS")) {
    snprintf(b, sizeof b, "%s", libdir); set_str(&st.resources_dir_path, b);
    snprintf(b, sizeof b, "%s/locales", libdir); set_str(&st.locales_dir_path, b);
  }
  if (getenv("CEF_SMOKE_LOCALE")) set_str(&st.locale, getenv("CEF_SMOKE_LOCALE"));
  st.log_severity = getenv("CEF_SMOKE_VERBOSE") ? LOGSEVERITY_VERBOSE : LOGSEVERITY_WARNING;
  if (getenv("CEF_SMOKE_LOG")) set_str(&st.log_file, getenv("CEF_SMOKE_LOG"));
  st.background_color = 0xffffffff;

  double t_init0 = now_ms() - t0;
  int ok = p_initialize(&margs, &st, &app, NULL);
  double t_init1 = now_ms() - t0;
  fprintf(stdout, "initialize ok=%d call_ms=%.0f (started at %.0f)\n", ok, t_init1 - t_init0, t_init0);
  if (!ok) return 1;

  cef_window_info_t wi;
  memset(&wi, 0, sizeof wi);
  wi.size = sizeof wi;
  wi.windowless_rendering_enabled = 1;
  wi.shared_texture_enabled = want_accel;
  wi.bounds.width = W; wi.bounds.height = H;
  cef_browser_settings_t bs;
  memset(&bs, 0, sizeof bs);
  bs.size = sizeof bs;
  bs.windowless_frame_rate = 60;
  cef_string_t u = {0};
  set_str(&u, url);
  int created = p_create_browser(&wi, &client, &u, &bs, NULL, NULL);
  fprintf(stdout, "create_browser=%d\n", created);
  if (getenv("CEF_SMOKE_KICK")) next_due_init = 0;

  // The owner loop of this test: sleep in poll() until CEF asks, run the slice it asked for.
  long loops = 0, works = 0;
  double deadline = t0 + 30000, settle = 0;
  double next_due = next_due_init < 0 ? -1 : now_ms();  // absolute ms
  int report_done = 0;
  long wake_at_loaded = 0;
  long long idle_start_sched = 0;
  double idle_start = 0;
  while (now_ms() < deadline) {
    double now = now_ms();
    int timeout = 1000;  // only so the test can end; not a tick (it does no work)
    if (next_due >= 0) { timeout = (int)(next_due - now); if (timeout < 0) timeout = 0; if (timeout > 1000) timeout = 1000; }
    struct pollfd pf = {wake_fd, POLLIN, 0};
    int r = poll(&pf, 1, timeout);
    loops++;
    if (r > 0) {
      uint64_t v; if (read(wake_fd, &v, 8) < 0) {}
      long long d = atomic_load(&next_delay_ms);
      next_due = now_ms() + (d > 0 ? (double)d : 0);
    }
    if (next_due >= 0 && now_ms() >= next_due) {
      next_due = -1;
      p_do_work();
      works++;
      // cefclient re-arms a bounded timer after every slice; the smoke test measures whether that
      // is needed (CEF_SMOKE_FALLBACK_MS, default: none, schedule requests only).
      if (getenv("CEF_SMOKE_FALLBACK_MS") && next_due < 0) next_due = now_ms() + atoi(getenv("CEF_SMOKE_FALLBACK_MS"));
    }
    if (done_loading && !report_done && t_paint >= 0 && now_ms() - t0 > t_loaded + 1500) {
      report_tree("loaded");
      report_done = 1;
      idle_start = now_ms();
      idle_start_sched = atomic_load(&schedule_calls);
      wake_at_loaded = loops;
      paints = 0;
      the_browser->get_host(the_browser)->close_browser(the_browser->get_host(the_browser), 1);
    }
    if (closed) break;
  }
  (void)idle_start; (void)idle_start_sched; (void)wake_at_loaded; (void)settle;
  fprintf(stdout, "created_ms=%.0f context_initialized_ms=%.0f first_paint_ms=%.0f loaded_ms=%.0f accel_first_ms=%.0f\n",
          t_created, t_ctx, t_paint, t_loaded, t_accel);
  fprintf(stdout, "paint_size=%dx%d accel_paints=%ld ppm_writes=%d\n", pw, ph, accel_paints, saved);
  fprintf(stdout, "loop_iterations=%ld do_work_calls=%ld schedule_calls=%ld\n", loops, works,
          (long)atomic_load(&schedule_calls));
  double ts = now_ms();
  p_shutdown();
  fprintf(stdout, "shutdown_ms=%.0f total_ms=%.0f\n", now_ms() - ts, now_ms() - t0);
  return 0;
}
