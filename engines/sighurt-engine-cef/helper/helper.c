// sig-cef-helper: the CEF subprocess of the Sighurt engine (renderer, GPU, utility). `sig`
// itself has no CEF code beyond the engine crate: Chromium runs its other processes from this
// binary (CEF's `browser_subprocess_path`), which links libcef.so; the browser process never does.
//
// Besides handing control to CEF it does one thing in the renderer: tell the browser which DOM
// node holds the keyboard focus, since Chromium's browser process does not say a focused field
// apart from anything else (no password hint, no caret place). The browser answers with the
// engine's `TextInputChanged` (docs/rewrite/CEF.md). Message "sig.focus", arguments:
//   0 int  1 when the node is editable (an input, a textarea, contenteditable), else 0
//   1 int  cef_dom_form_control_type_t of the node
//   2..5   int  x, y, width, height of the node's bounds in the page's DIPs
#ifndef CEF_API_VERSION
#define CEF_API_VERSION 15000
#endif
#include <stdio.h>
#include <string.h>

#include "include/capi/cef_app_capi.h"
#include "include/capi/cef_dom_capi.h"
#include "include/capi/cef_frame_capi.h"
#include "include/capi/cef_process_message_capi.h"
#include "include/capi/cef_render_process_handler_capi.h"
#include "include/capi/cef_values_capi.h"
#include "include/cef_api_hash.h"

// The handler objects live as long as the process: their reference counts are not needed.
static void CEF_CALLBACK add_ref(cef_base_ref_counted_t* self) { (void)self; }
static int CEF_CALLBACK release(cef_base_ref_counted_t* self) { (void)self; return 0; }
static int CEF_CALLBACK has_one_ref(cef_base_ref_counted_t* self) { (void)self; return 1; }

static void CEF_CALLBACK on_focused_node_changed(cef_render_process_handler_t* self,
                                                 cef_browser_t* browser, cef_frame_t* frame,
                                                 cef_domnode_t* node) {
  (void)self;
  int editable = 0, type = 0;
  cef_rect_t bounds = {0, 0, 0, 0};
  if (node) {
    editable = node->is_editable(node) ? 1 : 0;
    if (editable) {
      type = node->is_form_control_element(node) ? (int)node->get_form_control_element_type(node) : 0;
      bounds = node->get_element_bounds(node);
    }
  }
  if (frame) {
    cef_string_t name = {0};
    cef_string_utf8_to_utf16("sig.focus", 9, &name);
    cef_process_message_t* message = cef_process_message_create(&name);
    cef_string_clear(&name);
    cef_list_value_t* args = message->get_argument_list(message);
    args->set_int(args, 0, editable);
    args->set_int(args, 1, type);
    args->set_int(args, 2, bounds.x);
    args->set_int(args, 3, bounds.y);
    args->set_int(args, 4, bounds.width);
    args->set_int(args, 5, bounds.height);
    args->base.release(&args->base);
    // The frame takes the message.
    frame->send_process_message(frame, PID_BROWSER, message);
  }
  // Every reference CEF passed to the callback is ours to release.
  if (node) node->base.release(&node->base);
  if (frame) frame->base.release(&frame->base);
  if (browser) browser->base.release(&browser->base);
}

static cef_render_process_handler_t render_handler;
static cef_render_process_handler_t* CEF_CALLBACK get_render_process_handler(cef_app_t* self) {
  (void)self;
  return &render_handler;
}
static cef_app_t app;

int main(int argc, char** argv) {
  if (strcmp(cef_api_hash(CEF_API_VERSION, 0), CEF_API_HASH_PLATFORM)) return 3;
  render_handler.base.size = sizeof render_handler;
  render_handler.base.add_ref = add_ref;
  render_handler.base.release = release;
  render_handler.base.has_one_ref = has_one_ref;
  render_handler.base.has_at_least_one_ref = has_one_ref;
  render_handler.on_focused_node_changed = on_focused_node_changed;
  app.base = render_handler.base;
  app.base.size = sizeof app;
  app.get_render_process_handler = get_render_process_handler;
  cef_main_args_t args = {argc, argv};
  return cef_execute_process(&args, &app, NULL);
}
