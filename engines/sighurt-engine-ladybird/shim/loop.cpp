// See loop.h. Ported from Core::EventLoopManagerUnix (LibCore/EventLoopImplementationUnix.cpp), with
// epoll instead of poll, timers kept in the thread's own table, and no signal handling at all:
// Ladybird's signal handlers are kept and never installed, since signals are the process's (sig's).

#include "loop.h"

#include <AK/HashMap.h>
#include <AK/Time.h>
#include <AK/Vector.h>
#include <LibCore/Event.h>
#include <LibCore/EventLoop.h>
#include <LibCore/EventLoopImplementation.h>
#include <LibCore/EventReceiver.h>
#include <LibCore/Notifier.h>
#include <LibCore/ThreadEventQueue.h>

#include <atomic>
#include <errno.h>
#include <map>
#include <memory>
#include <mutex>
#include <pthread.h>
#include <signal.h>
#include <sys/epoll.h>
#include <sys/eventfd.h>
#include <time.h>
#include <unistd.h>
#include <vector>

namespace Sighurt {

namespace {

int64_t now_ns()
{
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return int64_t(ts.tv_sec) * 1'000'000'000 + ts.tv_nsec;
}

struct Timer {
    int64_t fire_ns { 0 };
    int64_t interval_ns { 0 };
    bool reload { false };
    AK::WeakPtr<Core::EventReceiver> owner;
    pthread_t owner_thread {};
};

struct FdEntry {
    uint32_t mask { 0 };
    std::vector<Core::Notifier*> notifiers;
};

struct ThreadData {
    ThreadData()
    {
        thread_id = pthread_self();
        epoll_fd = epoll_create1(EPOLL_CLOEXEC);
        wake_fd = eventfd(0, EFD_CLOEXEC | EFD_NONBLOCK);
        VERIFY(epoll_fd >= 0 && wake_fd >= 0);
        epoll_event ev {};
        ev.events = EPOLLIN;
        ev.data.fd = wake_fd;
        VERIFY(epoll_ctl(epoll_fd, EPOLL_CTL_ADD, wake_fd, &ev) == 0);
    }
    ~ThreadData()
    {
        close(epoll_fd);
        close(wake_fd);
    }

    std::recursive_mutex mutex;
    int epoll_fd { -1 };
    int wake_fd { -1 };
    pthread_t thread_id {};
    std::vector<Timer*> timers;
    std::map<int, FdEntry> fds;
};

std::mutex g_threads_lock;
std::map<pthread_t, ThreadData*>& threads()
{
    static auto* map = new std::map<pthread_t, ThreadData*>;
    return *map;
}

thread_local ThreadData* t_data = nullptr;
pthread_key_t g_key;
pthread_once_t g_key_once = PTHREAD_ONCE_INIT;

void destroy_thread_data(void* value)
{
    t_data = nullptr;
    auto* data = static_cast<ThreadData*>(value);
    {
        std::lock_guard lock(g_threads_lock);
        threads().erase(data->thread_id);
    }
    delete data;
}

ThreadData& this_thread()
{
    pthread_once(&g_key_once, [] { pthread_key_create(&g_key, destroy_thread_data); });
    if (!t_data) {
        t_data = new ThreadData;
        pthread_setspecific(g_key, t_data);
        std::lock_guard lock(g_threads_lock);
        threads()[t_data->thread_id] = t_data;
    }
    return *t_data;
}

ThreadData* thread_for(pthread_t id)
{
    // g_threads_lock is held by the caller.
    auto it = threads().find(id);
    return it == threads().end() ? nullptr : it->second;
}

ThreadData* g_owner = nullptr;
std::atomic<uint64_t> g_wakes { 0 };

uint32_t epoll_events_for(Core::NotificationType type)
{
    uint32_t events = 0;
    if (has_flag(type, Core::NotificationType::Read))
        events |= EPOLLIN;
    if (has_flag(type, Core::NotificationType::Write))
        events |= EPOLLOUT;
    return events;
}

void apply_fd(ThreadData& data, int fd, FdEntry& entry, bool existed)
{
    epoll_event ev {};
    ev.events = entry.mask;
    ev.data.fd = fd;
    int rc = epoll_ctl(data.epoll_fd, existed ? EPOLL_CTL_MOD : EPOLL_CTL_ADD, fd, &ev);
    if (rc != 0)
        dbgln("sighurt loop: epoll_ctl({}) failed: {}", fd, strerror(errno));
}

void recompute_mask(FdEntry& entry)
{
    entry.mask = 0;
    for (auto* notifier : entry.notifiers)
        entry.mask |= epoll_events_for(notifier->type());
}

// Signals: Ladybird registers SIGCHLD (ProcessMonitor) always, and SIGINT and SIGTERM (quitting its
// own event loop, which sig never runs) only in its profiling mode. None is installed: a handler of
// Ladybird's would be process-wide, setup that belongs to sig's main. SIGCHLD's would reap with
// waitpid(-1), which takes the exit status of children that are not Ladybird's (WPE's bubblewrap, a
// program the host started): the shim reaps Ladybird's helpers itself, one pidfd each (shim.cpp,
// watch_child). SIGINT and SIGTERM stay sig's to handle (it shuts the runtime down). The handlers are
// kept, by id, so unregistering works, and never called. Never destroyed, like the manager.
std::vector<std::pair<int, AK::Function<void(int)>>>& signal_handlers()
{
    static auto* handlers = new std::vector<std::pair<int, AK::Function<void(int)>>>;
    return *handlers;
}
int g_next_signal_id = 0;

std::atomic<uint64_t> g_pumps { 0 };

class Implementation final : public Core::EventLoopImplementation {
public:
    Implementation()
        : m_wake_fd(this_thread().wake_fd)
    {
    }

    int exec() override
    {
        for (;;) {
            if (auto code = exit_code_if_requested(); code.has_value())
                return *code;
            pump(PumpMode::WaitForEvents);
        }
    }

    size_t pump(PumpMode mode) override
    {
        auto& data = this_thread();
        wait(data, mode);
        return Core::ThreadEventQueue::current().process();
    }

    void quit(int code) override { request_exit(code); }

    void wake() override
    {
        uint64_t one = 1;
        auto result = write(m_wake_fd, &one, sizeof(one));
        (void)result; // EAGAIN: a wake is pending already. EBADF: the thread is gone.
    }

private:
    void wait(ThreadData& data, PumpMode mode)
    {
        std::lock_guard lock(data.mutex);
        bool pending = Core::ThreadEventQueue::current().has_pending_events();
        auto start = now_ns();
        int timeout_ms = 0;
        if (mode == PumpMode::WaitForEvents && !pending) {
            int64_t earliest = -1;
            for (auto* timer : data.timers)
                if (timer->fire_ns != INT64_MAX && (earliest < 0 || timer->fire_ns < earliest))
                    earliest = timer->fire_ns;
            if (earliest < 0)
                timeout_ms = -1;
            else
                timeout_ms = int(std::min<int64_t>(INT32_MAX, std::max<int64_t>(0, (earliest - start + 999'999) / 1'000'000)));
        }
        epoll_event events[32];
        int count;
        // The mutex is not held while sleeping: other threads register timers and notifiers.
        data.mutex.unlock();
        do {
            count = epoll_wait(data.epoll_fd, events, 32, timeout_ms);
        } while (count < 0 && errno == EINTR);
        data.mutex.lock();
        if (count < 0)
            return;
        for (int i = 0; i < count; ++i) {
            int fd = events[i].data.fd;
            if (fd == data.wake_fd) {
                uint64_t value;
                (void)!read(data.wake_fd, &value, sizeof(value));
                g_wakes.fetch_add(1);
                continue;
            }
            auto it = data.fds.find(fd);
            if (it == data.fds.end())
                continue;
            uint32_t revents = events[i].events;
            // Notifiers may be destroyed by an earlier activation; post by weak reference.
            for (auto* notifier : std::vector<Core::Notifier*>(it->second.notifiers)) {
                auto type = Core::NotificationType::None;
                if (revents & EPOLLIN)
                    type |= Core::NotificationType::Read;
                if (revents & EPOLLOUT)
                    type |= Core::NotificationType::Write;
                if (revents & EPOLLHUP)
                    type |= Core::NotificationType::Read | Core::NotificationType::Write | Core::NotificationType::HangUp;
                if (revents & EPOLLERR)
                    type |= Core::NotificationType::Error;
                type &= notifier->type();
                if (type != Core::NotificationType::None)
                    Core::ThreadEventQueue::current().post_event(notifier, Core::Event::Type::NotifierActivation);
            }
        }
        // Expired timers.
        auto after = now_ns();
        for (size_t i = 0; i < data.timers.size();) {
            auto* timer = data.timers[i];
            if (timer->fire_ns > after) {
                ++i;
                continue;
            }
            auto owner = timer->owner.strong_ref();
            if (timer->reload) {
                auto next = timer->fire_ns + timer->interval_ns;
                if (next <= after)
                    next = after + timer->interval_ns;
                timer->fire_ns = next;
                ++i;
            } else {
                // A fired one-shot timer stays until its owner unregisters it (as in the Unix
                // loop), so the id the owner holds is never a stale pointer.
                timer->fire_ns = INT64_MAX;
                ++i;
            }
            if (owner)
                Core::ThreadEventQueue::current().post_event(owner, Core::Event::Type::Timer);
        }
    }

    int m_wake_fd;
};

class Manager final : public Core::EventLoopManager {
public:
    NonnullOwnPtr<Core::EventLoopImplementation> make_implementation() override
    {
        return adopt_own(*new Implementation);
    }

    intptr_t register_timer(Core::EventReceiver& object, int milliseconds, bool should_reload) override
    {
        auto& data = this_thread();
        std::lock_guard lock(data.mutex);
        auto* timer = new Timer;
        timer->owner_thread = data.thread_id;
        timer->owner = object;
        timer->interval_ns = int64_t(milliseconds) * 1'000'000;
        timer->fire_ns = now_ns() + timer->interval_ns;
        timer->reload = should_reload;
        data.timers.push_back(timer);
        return reinterpret_cast<intptr_t>(timer);
    }

    void unregister_timer(intptr_t id) override
    {
        auto* timer = reinterpret_cast<Timer*>(id);
        std::lock_guard lock(g_threads_lock);
        auto* data = thread_for(timer->owner_thread);
        if (!data)
            return;
        std::lock_guard data_lock(data->mutex);
        std::erase(data->timers, timer);
        delete timer;
    }

    void register_notifier(Core::Notifier& notifier) override
    {
        auto& data = this_thread();
        std::lock_guard lock(data.mutex);
        int fd = notifier.fd();
        auto it = data.fds.find(fd);
        bool existed = it != data.fds.end();
        auto& entry = data.fds[fd];
        entry.notifiers.push_back(&notifier);
        recompute_mask(entry);
        apply_fd(data, fd, entry, existed);
        notifier.set_owner_thread(data.thread_id);
    }

    void unregister_notifier(Core::Notifier& notifier) override
    {
        std::lock_guard lock(g_threads_lock);
        auto* data = thread_for(notifier.owner_thread());
        if (!data)
            return;
        std::lock_guard data_lock(data->mutex);
        auto it = data->fds.find(notifier.fd());
        if (it == data->fds.end())
            return;
        auto& list = it->second.notifiers;
        std::erase(list, &notifier);
        if (list.empty()) {
            epoll_ctl(data->epoll_fd, EPOLL_CTL_DEL, it->first, nullptr);
            data->fds.erase(it);
        } else {
            recompute_mask(it->second);
            apply_fd(*data, it->first, it->second, true);
        }
    }

    void did_post_event() override
    {
        // The posting thread's loop must notice: its epoll descriptor is what a host waits on.
        uint64_t one = 1;
        (void)!write(this_thread().wake_fd, &one, sizeof(one));
    }

    // Kept, never installed (see signal_handlers).
    int register_signal(int number, AK::Function<void(int)> handler) override
    {
        VERIFY(number > 0 && number < NSIG);
        int id = ++g_next_signal_id;
        signal_handlers().emplace_back(id, move(handler));
        return id;
    }

    void unregister_signal(int id) override
    {
        auto& handlers = signal_handlers();
        for (size_t i = 0; i < handlers.size(); ++i) {
            if (handlers[i].first == id) {
                handlers.erase(handlers.begin() + i);
                return;
            }
        }
    }
};

}

void loop_install()
{
    // Never destroyed: timers and notifiers unregister at process exit, after statics are gone.
    static Manager& manager = *new Manager;
    Core::EventLoopManager::install(manager); // VERIFYs that none is installed yet.
    g_owner = &this_thread();
}

int loop_fd()
{
    return g_owner ? g_owner->epoll_fd : -1;
}

PumpResult loop_pump()
{
    PumpResult result;
    auto& loop = Core::EventLoop::current();
    g_pumps.fetch_add(1);
    result.dispatched = loop.pump(Core::EventLoop::WaitMode::PollForEvents);
    auto& data = *g_owner;
    {
        std::lock_guard lock(data.mutex);
        int64_t earliest = -1;
        for (auto* timer : data.timers)
            if (timer->fire_ns != INT64_MAX && (earliest < 0 || timer->fire_ns < earliest))
                earliest = timer->fire_ns;
        if (earliest >= 0)
            result.next_timer_ms = std::max<int64_t>(0, (earliest - now_ns() + 999'999) / 1'000'000);
    }
    result.more_queued = Core::ThreadEventQueue::current().has_pending_events();
    return result;
}

uint64_t loop_wake_count()
{
    return g_wakes.load();
}

}
