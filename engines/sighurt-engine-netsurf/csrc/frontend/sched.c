/* The core's only clock: callbacks after a delay. Housekeeping (a NULL
 * argument and a delay of at least 5s) stays out of the pump deadline and
 * runs when the owner wakes, or at most once a minute while a view is live.
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
};

static struct cb *cbs;
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
	c->housekeeping = (p == NULL && ms >= 5000);
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
		c->fn(c->p);
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
		if (next < 0 || c->at < next)
			next = c->at;
	}
	if (live_views && housekeeping) {
		double at = last_housekeeping + 60000.0;
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

void nsh_sched_clear(void)
{
	while (cbs) {
		struct cb *c = cbs;
		cbs = c->next;
		free(c);
	}
}

/* Set when the core comes up, so the first housekeeping wake is a minute later. */
void nsh_sched_mark_housekeeping(void)
{
	last_housekeeping = nsh_now_ms();
}
