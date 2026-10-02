/* The WPE WebKit smoke test (docs/rewrite/WPE.md; nix/wpe-smoke.sh runs it). Loads one page with
 * the WPE Platform API, then prints one JSON line with what the page reports about itself (title,
 * WebGL renderer, media types GStreamer can play; on webkit://gpu, the GL renderer and how frames
 * reach the UI process) and what a snapshot of it shows (size, the centre pixel).
 *
 *   wpe-smoke [--headless] [--hold SECONDS] URI
 *
 * Without --headless the view uses the default WPE display: Wayland under a compositor. --hold
 * keeps the page up after the report, for a screenshot. Exits with 0 once the report is printed;
 * with 1 if the load fails, the web process dies or nothing is reported within 60 s. */
#include <wpe/headless/wpe-headless.h>
#include <wpe/webkit.h>

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char probe[] =
    "(() => {"
    "  const gl = document.createElement('canvas').getContext('webgl');"
    "  const info = gl && gl.getExtension('WEBGL_debug_renderer_info');"
    "  const media = document.createElement('video');"
    "  return {"
    "    title: document.title,"
    "    webgl: gl ? (info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : 'yes') : null,"
    "    h264: media.canPlayType('video/mp4; codecs=\"avc1.42E01E\"'),"
    "    vp9: media.canPlayType('video/webm; codecs=\"vp9\"'),"
    "    opus: media.canPlayType('audio/ogg; codecs=\"opus\"'),"
    "    gpu: location.href == 'webkit://gpu' ? Object.fromEntries([...document.querySelectorAll('tr')]"
    "      .map(row => [...row.cells].map(cell => cell.textContent))"
    "      .filter(([key]) => ['GL_RENDERER', 'Renderer', 'Buffer format', 'Platform'].includes(key)))"
    "      : undefined,"
    "  };"
    "})()";

static GMainLoop *loop;
static int status = 1;
static guint hold_seconds;
static char *page_report;

static gboolean quit(gpointer data)
{
    g_main_loop_quit(loop);
    return G_SOURCE_REMOVE;
}

static void fail(const char *what, const char *why)
{
    fprintf(stderr, "wpe-smoke: %s: %s\n", what, why);
    g_main_loop_quit(loop);
}

static void snapshotted(GObject *view, GAsyncResult *result, gpointer data)
{
    GError *error = NULL;
    WebKitImage *image = webkit_web_view_get_snapshot_finish(WEBKIT_WEB_VIEW(view), result, &error);
    if (!image) {
        fail("snapshot", error->message);
        g_error_free(error);
        return;
    }
    int width = webkit_image_get_width(image), height = webkit_image_get_height(image);
    gsize size;
    const guint8 *pixels = g_bytes_get_data(webkit_image_as_bytes(image), &size);
    /* Premultiplied ARGB in native byte order: B, G, R, A on little-endian machines. */
    const guint8 *centre = pixels + (gsize)(height / 2) * webkit_image_get_stride(image) + (width / 2) * 4;
    printf("{\"page\":%s,\"snapshot\":{\"width\":%d,\"height\":%d,\"centre\":\"#%02x%02x%02x\"}}\n",
        page_report, width, height, centre[2], centre[1], centre[0]);
    fflush(stdout);
    g_object_unref(image);
    status = 0;
    g_timeout_add_seconds(hold_seconds, quit, NULL);
}

static void probed(GObject *view, GAsyncResult *result, gpointer data)
{
    GError *error = NULL;
    JSCValue *value = webkit_web_view_evaluate_javascript_finish(WEBKIT_WEB_VIEW(view), result, &error);
    if (!value) {
        fail("probe", error->message);
        g_error_free(error);
        return;
    }
    page_report = jsc_value_to_json(value, 0);
    g_object_unref(value);
    webkit_web_view_get_snapshot(WEBKIT_WEB_VIEW(view), WEBKIT_SNAPSHOT_REGION_VISIBLE,
        WEBKIT_SNAPSHOT_OPTIONS_NONE, NULL, snapshotted, NULL);
}

static void load_changed(WebKitWebView *view, WebKitLoadEvent event, gpointer data)
{
    if (event == WEBKIT_LOAD_FINISHED)
        webkit_web_view_evaluate_javascript(view, probe, -1, NULL, NULL, NULL, probed, NULL);
}

static gboolean load_failed(WebKitWebView *view, WebKitLoadEvent event, char *uri, GError *error, gpointer data)
{
    fail(uri, error->message);
    return TRUE;
}

static void web_process_terminated(WebKitWebView *view, WebKitWebProcessTerminationReason reason, gpointer data)
{
    fail("web process", reason == WEBKIT_WEB_PROCESS_CRASHED ? "crashed" : "terminated");
}

static gboolean timed_out(gpointer data)
{
    fail("timeout", "no report within 60 s");
    return G_SOURCE_REMOVE;
}

int main(int argc, char **argv)
{
    gboolean headless = FALSE;
    int i = 1;
    for (; i < argc - 1; i++) {
        if (!strcmp(argv[i], "--headless"))
            headless = TRUE;
        else if (!strcmp(argv[i], "--hold") && i + 2 < argc)
            hold_seconds = atoi(argv[++i]);
        else
            break;
    }
    if (i != argc - 1) {
        fprintf(stderr, "usage: %s [--headless] [--hold SECONDS] URI\n", argv[0]);
        return 2;
    }

    loop = g_main_loop_new(NULL, FALSE);
    WPEDisplay *display = headless ? wpe_display_headless_new() : NULL;
    WebKitWebView *view = g_object_new(WEBKIT_TYPE_WEB_VIEW, "display", display, NULL);
    g_clear_object(&display);
    g_signal_connect(view, "load-changed", G_CALLBACK(load_changed), NULL);
    g_signal_connect(view, "load-failed", G_CALLBACK(load_failed), NULL);
    g_signal_connect(view, "web-process-terminated", G_CALLBACK(web_process_terminated), NULL);
    g_timeout_add_seconds(60, timed_out, NULL);
    webkit_web_view_load_uri(view, argv[i]);
    g_main_loop_run(loop);

    g_object_unref(view);
    g_free(page_report);
    return status;
}
