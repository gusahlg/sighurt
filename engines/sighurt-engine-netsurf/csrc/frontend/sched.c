/* The core's only clock: callbacks after a delay. Housekeeping (a NULL
 * argument and a delay of at least 5s) stays out of the pump deadline and
 * runs when the owner wakes, or at most once a minute while a view is live.
 *
 * A callback with an argument whose run only invalidated hidden windows and
 * scheduled itself again (an animated image in a background tab) is parked:
 * it stays out of the deadline too, and runs only when the owner wakes for
 * something else, until a window is shown or a visible one is invalidated
 * (nsh_sched_unpark). Fetches (NULL argument) and anything that emitted
 * another event are never parked, so a background tab still loads.
 * GPL-2.0-only. */
#include "internal.h"

#include <stdlib.h>
#include <math.h>
#include <time.h>

struct cb {
	struct cb *next;
	double at;
	void (*fn)(void *);
	void *p;
	int housekeeping;
	int parked;
};

static struct cb *cbs;
static int any_parked;

/* What the core scheduled while it started (the cache cleaners) is housekeeping whenever it is
 * rescheduled, whatever its delay or argument. */
enum { KNOWN_MAX = 16 };
static struct { void (*fn)(void *); void *p; } known[KNOWN_MAX];
static int nknown;

static int is_known(void (*fn)(void *), void *p)
{
	for (int i = 0; i < nknown; i++)
		if (known[i].fn == fn && known[i].p == p)
			return 1;
	return 0;
}
static double last_housekeeping;

double nsh_now_ms(void)
{
	struct timespec ts;
	clock_gettime(CLOCK_MONOTONIC, &ts);
	return (double)ts.tv_sec * 1000.0 + (double)ts.tv_nsec / 1e6;
}

static nserror cancel(void (*fn)(void *), void *p)
{
	nserror r = NSERROR_NOT_FOUND;
	for (struct cb **pp = &cbs; *pp;) {
		if ((*pp)->fn == fn && (*pp)->p == p) {
			struct cb *d = *pp;
			*pp = d->next;
			free(d);
			r = NSERROR_OK;
		} else {
			pp = &(*pp)->next;
		}
	}
	return r;
}

nserror nsh_schedule(int ms, void (*fn)(void *), void *p)
{
	cancel(fn, p);
	if (ms < 0)
		return NSERROR_OK;
	struct cb *c = calloc(1, sizeof *c);
	if (!c)
		return NSERROR_NOMEM;
	c->at = nsh_now_ms() + (double)ms;
	c->fn = fn;
	c->p = p;
	c->housekeeping = (p == NULL && ms >= 5000) || is_known(fn, p);
	c->next = cbs;
	cbs = c;
	return NSERROR_OK;
}

int nsh_sched_run(int limit)
{
	int ran = 0;
	for (;;) {
		double t = nsh_now_ms();
		struct cb **due = NULL;
		for (struct cb **pp = &cbs; *pp; pp = &(*pp)->next) {
			if ((*pp)->at <= t) {
				due = pp;
				break;
			}
		}
		if (!due)
			return 0;
		if (ran >= limit)
			return 1;
		struct cb *c = *due;
		*due = c->next;
		if (c->housekeeping)
			last_housekeeping = nsh_now_ms();
		struct nsh_run_effects before = nsh_run_effects;
		c->fn(c->p);
		struct nsh_run_effects *now = &nsh_run_effects;
		if (c->p && now->hidden_frames != before.hidden_frames
		    && now->visible_frames == before.visible_frames
		    && now->other_events == before.other_events) {
			/* Rescheduling replaced the entry: park the new one, if the run made one. */
			for (struct cb *n = cbs; n; n = n->next) {
				if (n->fn == c->fn && n->p == c->p && !n->housekeeping) {
					n->parked = 1;
					any_parked = 1;
				}
			}
		}
		free(c);
		ran++;
	}
}

int nsh_sched_deadline(int live_views)
{
	double t = nsh_now_ms();
	double next = -1;
	int housekeeping = 0;
	for (struct cb *c = cbs; c; c = c->next) {
		if (c->housekeeping) {
			housekeeping = 1;
			continue;
		}
		if (c->parked)
			continue;
		if (next < 0 || c->at < next)
			next = c->at;
	}
	if (live_views && housekeeping) {
		/* Once a minute, and never before the earliest housekeeping callback is due: a wake
		 * that finds nothing to run would be asked for again at once. */
		double earliest = -1;
		for (struct cb *c = cbs; c; c = c->next)
			if (c->housekeeping && (earliest < 0 || c->at < earliest))
				earliest = c->at;
		double at = last_housekeeping + 60000.0;
		if (at < earliest)
			at = earliest;
		if (next < 0 || at < next)
			next = at;
	}
	if (next < 0)
		return -1;
	if (next <= t)
		return 0;
	double ms = next - t;
	if (ms > 86400000.0)
		ms = 86400000.0;
	return (int)ceil(ms);
}

void nsh_sched_unpark(void)
{
	if (!any_parked)
		return;
	for (struct cb *c = cbs; c; c = c->next)
		c->parked = 0;
	any_parked = 0;
}

void nsh_sched_clear(void)
{
	while (cbs) {
		struct cb *c = cbs;
		cbs = c->next;
		free(c);
	}
	nknown = 0;
	any_parked = 0;
}

/* Set when the core comes up, so the first housekeeping wake is a minute later. */
void nsh_sched_mark_housekeeping(void)
{
	last_housekeeping = nsh_now_ms();
	for (struct cb *c = cbs; c && nknown < KNOWN_MAX; c = c->next) {
		c->housekeeping = 1;
		if (!is_known(c->fn, c->p)) {
			known[nknown].fn = c->fn;
			known[nknown].p = c->p;
			nknown++;
		}
	}
}
