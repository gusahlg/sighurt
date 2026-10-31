#
# Makefile for the NetSurf "sighurt" frontend (docs/rewrite/NETSURF.md).
# The core plus these sources. build.rs archives the objects; the executable is
# only how the NetSurf makefile is willing to finish a link.
#
CFLAGS += -std=c99 -Dsighurt -Dnssighurt -g -fPIC
CFLAGS += -D_POSIX_C_SOURCE=200809L -D_XOPEN_SOURCE=700 -D_BSD_SOURCE -D_DEFAULT_SOURCE
CFLAGS += -Ifrontends/$(TARGET)
LDFLAGS += -lm
$(eval $(call pkg_config_find_and_add,freetype2,freetype2))
$(eval $(call pkg_config_find_and_add,libpng,libpng))

# `any` keeps every message. The splitter writes into this frontend's res/.
MESSAGES_FILTER=any
MESSAGES_TARGET=$(FRONTEND_RESOURCES_DIR)

S_FRONTEND := window.c plot.c font.c sched.c fetch.c clipboard.c download.c bitmap.c api.c main.c

SOURCES = $(S_COMMON) $(S_IMAGE) $(S_BROWSER) $(S_PDF) $(S_FRONTEND)
EXETARGET := nssighurt

install-sighurt:
package-sighurt:
