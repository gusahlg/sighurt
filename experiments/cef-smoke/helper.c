// sig-cef-helper, as a smoke test: the browser's CEF subprocess (renderer, GPU, utility). It
// does nothing but hand control to CEF; libcef.so is linked here, never into the browser.
#ifndef CEF_API_VERSION
#define CEF_API_VERSION 15000
#endif
#include <stdio.h>
#include <string.h>
#include "include/capi/cef_app_capi.h"
#include "include/cef_api_hash.h"
int main(int argc, char** argv) {
  if (strcmp(cef_api_hash(CEF_API_VERSION, 0), CEF_API_HASH_PLATFORM)) return 3;
  cef_main_args_t a = {argc, argv};
  return cef_execute_process(&a, NULL, NULL);
}
