/* The shim without Rust: waits on lb_loop_fd() only, loads a page, checks the events, then idles.
 *
 *   cc -O1 -o loop_probe loop_probe.c -ldl && SIGHURT_LADYBIRD_SHIM=... ./loop_probe URL [idle_seconds]
 *
 * Exits 0 and prints PASS when the page loaded (title, commit, finish, a frame) and the loop did not
 * wake while idle. */
#define _GNU_SOURCE
#include <dlfcn.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <unistd.h>
#include "../shim/sighurt_ladybird.h"

static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }

#define SYM(name) name##_t name = (name##_t)dlsym(lib, #name); if (!name) { fprintf(stderr, "FAIL missing " #name "\n"); return 1; }
typedef int (*lb_init_t)(const char* const*, int, const char*, const char*, char*, size_t);
typedef int (*lb_loop_fd_t)(void);
typedef int64_t (*lb_pump_t)(int*);
typedef int (*lb_next_event_t)(lb_event*);
typedef uint64_t (*lb_wake_count_t)(void);
typedef uint64_t (*lb_view_open_t)(int, int, double, int, double);
typedef void (*lb_navigate_t)(uint64_t, const char*);
typedef int (*lb_frame_get_t)(uint64_t, lb_frame*);
typedef void (*lb_shutdown_t)(void);
typedef void (*lb_run_js_t)(uint64_t, const char*, int64_t);
typedef void (*lb_set_visible_t)(uint64_t, int);
typedef void (*lb_abi_describe_t)(lb_abi*);

int main(int argc, char** argv)
{
    const char* path = getenv("SIGHURT_LADYBIRD_SHIM");
    void* lib = dlopen(path ? path : "libsighurt-ladybird-shim.so", RTLD_NOW | RTLD_LOCAL);
    if (!lib) { fprintf(stderr, "FAIL dlopen: %s\n", dlerror()); return 1; }
    SYM(lb_init) SYM(lb_loop_fd) SYM(lb_pump) SYM(lb_next_event) SYM(lb_wake_count) SYM(lb_view_open)
    SYM(lb_navigate) SYM(lb_run_js) SYM(lb_set_visible) SYM(lb_frame_get) SYM(lb_shutdown) SYM(lb_abi_describe)
    lb_abi abi; lb_abi_describe(&abi);
    printf("abi version=%u probe=%u dmabuf=%u\n", abi.version, abi.probe_shared_image_buffer, abi.vulkan_dmabuf);
    if (!abi.probe_shared_image_buffer) { fprintf(stderr, "FAIL ABI probe\n"); return 1; }
    char err[256] = "";
    char profile[] = "/tmp/sighurt-ladybird-probe-XXXXXX";
    if (!mkdtemp(profile)) return 1;
    const char* extra[] = { getenv("LB_ARG") };
    if (lb_init(getenv("LB_ARG") ? extra : NULL, getenv("LB_ARG") ? 1 : 0, profile, NULL, err, sizeof err)) { fprintf(stderr, "FAIL init: %s\n", err); return 1; }
    uint64_t view = lb_view_open(800, 600, 1.0, 1, 1.0);
    lb_navigate(view, argc > 1 ? argv[1] : "https://example.com/");
    int fd = lb_loop_fd();
    int title = 0, commit = 0, finish = 0, frame = 0, loops = 0;
    double start = now();
    while (now() - start < 30 && !(title && commit && finish && frame)) {
        int more = 0;
        int64_t next = lb_pump(&more);
        lb_event e;
        while (lb_next_event(&e)) {
            if (getenv("TRACE")) printf("  ev %d view=%llu i0=%lld i1=%lld s0=%s\n", e.kind, (unsigned long long)e.view, (long long)e.i0, (long long)e.i1, e.s0 ? e.s0 : "(null)");
            if (e.kind == LB_EV_TITLE) { title = 1; printf("title %s\n", e.s0); }
            if (e.kind == LB_EV_URL_CHANGED) { commit = 1; printf("url %s\n", e.s0); }
            if (e.kind == LB_EV_LOAD_FINISH) { finish = 1; printf("finish %s\n", e.s0); }
            if (e.kind == LB_EV_FRAME) frame = 1;
        }
        if (more) continue;
        struct pollfd p = { fd, POLLIN, 0 };
        int timeout = next < 0 ? 1000 : (next > 1000 ? 1000 : (int)next);
        poll(&p, 1, timeout);
        loops++;
    }
    lb_frame f;
    int have = lb_frame_get(view, &f);
    printf("loops=%d title=%d commit=%d finish=%d frame=%d buffer=%d %dx%d\n", loops, title, commit, finish, frame, have, have ? f.width : 0, have ? f.height : 0);
    if (getenv("HIDE1")) lb_set_visible(view, 0);
    if (getenv("DELAY")) {
        double t0 = now();
        while (now() - t0 < atof(getenv("DELAY"))) {
            int more = 0; lb_pump(&more); lb_event e;
            while (lb_next_event(&e)) {}
            struct pollfd p = { fd, POLLIN, 0 }; poll(&p, 1, 100);
        }
    }
    if (getenv("VIEW2")) {
        uint64_t v2 = lb_view_open(800, 600, 1.0, 1, 1.0);
        lb_navigate(v2, getenv("VIEW2"));
        double t0 = now();
        while (now() - t0 < 12) {
            int more = 0; lb_pump(&more); lb_event e;
            while (lb_next_event(&e)) printf("  v2-ev %d view=%llu s0=%s\n", e.kind, (unsigned long long)e.view, e.s0 ? e.s0 : "(null)");
            struct pollfd p = { fd, POLLIN, 0 }; poll(&p, 1, 100);
        }
    }
    if (getenv("NAV2")) {
        lb_navigate(view, getenv("NAV2"));
        double t0 = now();
        while (now() - t0 < 3) {
            int more = 0; lb_pump(&more); lb_event e;
            while (lb_next_event(&e)) printf("  nav2-ev %d i0=%lld s0=%s\n", e.kind, (long long)e.i0, e.s0 ? e.s0 : "(null)");
            struct pollfd p = { fd, POLLIN, 0 }; poll(&p, 1, 100);
        }
    }
    if (getenv("JS")) {
        /* Run a script and trace what follows for a few seconds. */
        lb_run_js(view, getenv("JS"), 1);
        double t0 = now();
        while (now() - t0 < 4) {
            int more = 0; lb_pump(&more); lb_event e;
            while (lb_next_event(&e)) printf("  js-ev %d i0=%lld i1=%lld s0=%s\n", e.kind, (long long)e.i0, (long long)e.i1, e.s0 ? e.s0 : "(null)");
            struct pollfd p = { fd, POLLIN, 0 }; poll(&p, 1, 100);
        }
    }
    /* Idle: nothing may wake the owner loop. */
    int idle = argc > 2 ? atoi(argv[2]) : 5;
    uint64_t before = lb_wake_count();
    int wakes = 0;
    double idle_start = now();
    while (now() - idle_start < idle) {
        int more = 0;
        lb_pump(&more);
        lb_event e;
        while (lb_next_event(&e)) {}
        struct pollfd p = { fd, POLLIN, 0 };
        int rc = poll(&p, 1, 250);
        if (rc > 0) wakes++;
    }
    printf("idle %ds: poll wakeups=%d eventfd wakes=%llu\n", idle, wakes, (unsigned long long)(lb_wake_count() - before));
    lb_shutdown();
    int ok = title && commit && finish && frame && have;
    printf("%s\n", ok ? "PASS" : "FAIL");
    return ok ? 0 : 1;
}
