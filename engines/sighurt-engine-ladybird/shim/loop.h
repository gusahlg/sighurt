// The event loop of Ladybird's UI process, rebuilt so that `sig` owns the thread that runs it.
//
// Core::EventLoopManager is the one hook Ladybird has for this (its Qt frontend uses it): the
// manager decides how timers, notifiers, wakes and signals work, and what `pump` does. This one
// keeps a single epoll instance per thread. For the thread that calls `lb_loop_init` (the owner
// thread) the epoll descriptor is exported (`sighurt_loop_fd`): it is readable exactly when a
// `pump` has work (a notifier's descriptor, a wake, a queued event), so the host waits on one
// descriptor and calls `pump` when it fires. Timers have no descriptor: `pump` returns when the
// next one is due and the host sleeps until then. Nothing in here ticks, and no signal handler is
// installed (signals are the process's).
//
// Must not own: anything of the browser (views, pages, frames).
#pragma once

#include <stddef.h>
#include <stdint.h>

namespace Sighurt {

// Installs the manager. It must run before anything else in the process touches
// Core::EventLoopManager; the first Core::EventLoop (made by Application) is the owner's.
void loop_install();

// The owner thread's epoll descriptor.
int loop_fd();

// One pump of the owner thread's loop without waiting: dispatches what is ready (notifiers,
// expired timers, queued events). Returns the milliseconds until the next timer (-1: none) and
// whether queued events remain.
struct PumpResult {
    int64_t next_timer_ms { -1 };
    bool more_queued { false };
    size_t dispatched { 0 };
};
PumpResult loop_pump();

// Counts how often the owner loop was woken, for the idle test.
uint64_t loop_wake_count();

}
