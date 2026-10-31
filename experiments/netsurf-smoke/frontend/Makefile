#
# Makefile for the NetSurf "sighurt" frontend (docs/rewrite/NETSURF.md). Derived from the monkey
# frontend's: the core plus this directory's sources, linked into a test executable here.
#
CFLAGS += -std=c99 -Dsighurt -Dnssighurt -g
CFLAGS += -D_POSIX_C_SOURCE=200809L -D_XOPEN_SOURCE=700 -D_BSD_SOURCE -D_DEFAULT_SOURCE
LDFLAGS += -lm
$(eval $(call pkg_config_find_and_add,freetype2,freetype2))
$(eval $(call pkg_config_find_and_add,libpng,libpng))

MESSAGES_FILTER=monkey
MESSAGES_TARGET=$(FRONTEND_RESOURCES_DIR)

S_FRONTEND := smoke.c

SOURCES = $(S_COMMON) $(S_IMAGE) $(S_BROWSER) $(S_PDF) $(S_FRONTEND)
EXETARGET := nssighurt

install-sighurt:
package-sighurt:
