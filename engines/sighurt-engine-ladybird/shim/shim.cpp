// Ladybird's UI process half, as a library with a C ABI (sighurt_ladybird.h).
//
// Owns: one WebView::Application (the profile, the helper processes) and a LbView per page, built on
// HeadlessWebView; the queue of events the Rust side drains; the translation of host input into
// Web:: input events. Must not own: any browser policy (tabs, routing, permissions: the host's), the
// Wayland connection (it never touches a display), or the event loop's machinery (loop.cpp).

#include "sighurt_ladybird.h"

#include "loop.h"

#include <AK/ByteString.h>
#include <AK/LexicalPath.h>
#include <AK/String.h>
#include <AK/Utf16String.h>
#include <LibCore/EventLoop.h>
#include <AK/HashTable.h>
#include <LibCore/Directory.h>
#include <LibCore/File.h>
#include <LibCore/StandardPaths.h>
#include <LibCore/Timer.h>
#include <LibFileSystem/FileSystem.h>
#include <LibWeb/Loader/DownloadFilename.h>
#include <LibWebView/ExternalURLHandler.h>
#include <LibWebView/FileDownloader.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibGfx/SystemTheme.h>
#include <LibMain/Main.h>
#include <LibURL/Parser.h>
#include <LibWeb/Clipboard/SystemClipboard.h>
#include <LibWeb/HTML/SelectedFile.h>
#include <LibWeb/Page/InputEvent.h>
#include <LibWebView/Application.h>
#include <LibWebView/HeadlessWebView.h>
#include <LibWebView/Utilities.h>

#include <LibWebView/ProcessManager.h>

#include <list>
#include <map>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>
#include <map>
#include <memory>
#include <string>
#include <string.h>
#include <vector>

#ifndef SIGHURT_LADYBIRD_PREFIX
#    define SIGHURT_LADYBIRD_PREFIX ""
#endif

namespace {

struct Event {
    int kind { 0 };
    uint64_t view { 0 };
    int64_t i0 { 0 };
    int64_t i1 { 0 };
    std::string s0;
    std::string s1;
    bool has_s0 { false };
    std::string data;
    bool has_data { false };
};

// A page can make Ladybird call back as often as it likes (a title or a cursor changing in a loop,
// pushState): events that only state the latest value of something replace an undelivered earlier
// one of the same kind for the same view, so the queue stays bounded by views times kinds.
std::list<Event> g_events;
std::map<std::pair<int, uint64_t>, std::list<Event>::iterator> g_replaceable;
Event g_current;

bool is_replaceable(int kind)
{
    switch (kind) {
    case LB_EV_TITLE:
    case LB_EV_LOADING:
    case LB_EV_CURSOR:
    case LB_EV_HOVER:
    case LB_EV_FRAME:
    case LB_EV_HISTORY:
    case LB_EV_INPUT_METHOD:
        return true;
    default:
        return false;
    }
}

// The view a request for a service is charged to: a popup's requests are its opener's (defined
// after LbView).
uint64_t shim_service_view(uint64_t);

bool is_service_event(int kind)
{
    return kind == LB_EV_CLIPBOARD_WRITE || kind == LB_EV_CLIPBOARD_WANT || kind == LB_EV_DOWNLOAD
        || kind == LB_EV_PICK_FILES || kind == LB_EV_OPEN_EXTERNAL;
}

void push_event(Event&& event)
{
    if (is_service_event(event.kind))
        event.view = shim_service_view(event.view);
    auto key = std::make_pair(event.kind, event.view);
    bool replace = is_replaceable(event.kind);
    if (replace) {
        if (auto it = g_replaceable.find(key); it != g_replaceable.end()) {
            g_events.erase(it->second);
            g_replaceable.erase(it);
        }
    }
    g_events.push_back(move(event));
    if (replace)
        g_replaceable[key] = std::prev(g_events.end());
}

void push(int kind, uint64_t view, int64_t i0 = 0, int64_t i1 = 0, std::string const* s0 = nullptr, std::string const* s1 = nullptr, std::string const* data = nullptr)
{
    Event event;
    event.kind = kind;
    event.view = view;
    event.i0 = i0;
    event.i1 = i1;
    if (s0) {
        event.s0 = *s0;
        event.has_s0 = true;
    }
    if (s1)
        event.s1 = *s1;
    if (data) {
        event.data = *data;
        event.has_data = true;
    }
    push_event(move(event));
}

// Same, moving the payload instead of copying it (a download's bytes).
void push_moved(int kind, uint64_t view, std::string const& s0, std::string const& s1, std::string&& data)
{
    Event event;
    event.kind = kind;
    event.view = view;
    event.s0 = s0;
    event.has_s0 = true;
    event.s1 = s1;
    event.data = move(data);
    event.has_data = true;
    push_event(move(event));
}

// Ladybird's helper processes (WebContent, RequestServer, ...) are children of this process. Its own
// monitor reaps with waitpid(-1) from a SIGCHLD handler, which would also reap the children of other
// engines and of the host, so loop.cpp never installs that handler. Each helper is watched through a
// pidfd instead and reaped by its own pid, and what ProcessMonitor would have done is done here.
struct Child {
    int pidfd { -1 };
    RefPtr<Core::Notifier> notifier;
};

HashMap<pid_t, Child>& children()
{
    static auto* map = new HashMap<pid_t, Child>;
    return *map;
}

// A wait status the way waitpid reports it, from what waitid says.
int wait_status_from(siginfo_t const& info)
{
    if (info.si_code == CLD_EXITED)
        return (info.si_status & 0xff) << 8;
    return (info.si_status & 0x7f) | (info.si_code == CLD_DUMPED ? 0x80 : 0);
}

// Whether the helper has exited (and reaps it, by its pidfd: the pid cannot be another process's, even
// if the number was reused). Sets `status`; an error that means someone else reaped it counts as exited.
bool reap_exited(int pidfd, Optional<int>& status)
{
    siginfo_t info {};
    int rc;
    do {
        rc = waitid(P_PIDFD, pidfd, &info, WEXITED | WNOHANG);
    } while (rc < 0 && errno == EINTR);
    if (rc < 0) {
        if (errno == ECHILD)
            return true; // Reaped elsewhere (SIGCHLD ignored, another component's waitpid(-1)).
        return false;
    }
    if (info.si_pid == 0)
        return false; // Not yet.
    status = wait_status_from(info);
    return true;
}

void reap_child(pid_t pid)
{
    // Out of the table at once (a reused pid is a new helper), the notifier destroyed after this callback.
    auto child = children().take(pid);
    if (!child.has_value())
        return;
    Optional<int> status;
    if (!reap_exited(child->pidfd, status)) {
        children().set(pid, move(*child)); // Spurious wake-up.
        return;
    }
    Core::deferred_invoke([notifier = move(child->notifier), pidfd = child->pidfd]() mutable {
        notifier = nullptr;
        close(pidfd);
    });
    auto& manager = WebView::Application::process_manager();
    if (auto process = manager.remove_process(pid); process.has_value()) {
        // Crash reports would go to the user's own Ladybird directory: off.
        process->save_crash_report({});
        manager.on_process_exited(process.release_value(), status);
    }
}

void watch_child(pid_t pid)
{
    if (pid == getpid() || children().contains(pid))
        return;
    int pidfd = int(syscall(SYS_pidfd_open, pid, 0));
    if (pidfd < 0) {
        dbgln("sighurt: pidfd_open({}) failed: {}", pid, strerror(errno));
        return;
    }
    auto notifier = Core::Notifier::construct(pidfd, Core::NotificationType::Read);
    notifier->on_activation = [pid] { reap_child(pid); };
    children().set(pid, { pidfd, move(notifier) });
}

void watch_children()
{
    auto& manager = WebView::Application::process_manager();
    manager.on_process_added = [](WebView::Process& process) { watch_child(process.pid()); };
    manager.for_each_process([](WebView::Process& process) { watch_child(process.pid()); });
}

std::string to_std(StringView view) { return std::string(view.characters_without_null_termination(), view.length()); }
std::string to_std(String const& string) { return to_std(string.bytes_as_string_view()); }
std::string to_std(Utf16String const& string) { return to_std(string.to_utf8()); }

constexpr size_t max_clipboard_bytes = 16 * 1024 * 1024;

i64 now_ns()
{
    timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return i64(ts.tv_sec) * 1'000'000'000 + ts.tv_nsec;
}

// The view the host last gave input, focus or a navigation: Ladybird's download and external-URL
// callbacks do not say which view they are for, so those requests are attributed to it.
uint64_t g_active_view = 0;
// The view whose external-URL confirmation is being answered (the launch follows at once).
uint64_t g_launch_view = 0;
int64_t g_next_token = 0;
HashMap<int64_t, WebView::ExternalURLHandler::LaunchCallback> g_launches;
HashMap<int64_t, uint64_t> g_pickers;
// Navigations held for the host (the patched LibWebView's on_navigation_start_requested).
HashMap<int64_t, Function<void(bool)>> g_held_navigations;
ByteString g_profile;
// Staged downloads: destination path -> the name the page suggested and the view it came from
// (0: not known).
struct Staged {
    ByteString name;
    uint64_t view { 0 };
};
HashMap<ByteString, Staged> g_staged;
RefPtr<Core::Timer> g_download_timer;
// Most a staged download may be when handed to the host (it travels in memory).
constexpr u64 max_download_bytes = 64ull * 1024 * 1024;
constexpr u64 max_picked_file_bytes = 32ull * 1024 * 1024;

HashTable<uint64_t> g_history_dirty;

void poll_downloads();
// The shim's id of the view with Ladybird's id, or 0 (defined after LbView).
uint64_t shim_view_for_ladybird_id(u64);

void ensure_download_timer()
{
    if (g_download_timer)
        return;
    // Only while a download is staged: no timer otherwise.
    g_download_timer = Core::Timer::create_repeating(250, [] { poll_downloads(); });
    g_download_timer->start();
}

// Everything Ladybird asks the shell for goes to the host, which decides; the defaults of
// WebView::Application (the system clipboard's neighbours, the user's Downloads folder, xdg-open)
// are replaced here.
class HostURLHandler final : public WebView::ExternalURLHandler {
public:
    static NonnullRefPtr<HostURLHandler> create() { return adopt_ref(*new HostURLHandler); }

    // The host opens the URL once it has allowed the request; launching is a request to it.
    virtual void launch(URL::URL const& url, LaunchCallback callback) const override
    {
        auto token = ++g_next_token;
        g_launches.set(token, move(callback));
        auto text = to_std(url.serialize());
        push(LB_EV_OPEN_EXTERNAL, g_launch_view ? g_launch_view : g_active_view, 0, token, &text);
    }

private:
    HostURLHandler()
        : ExternalURLHandler("the desktop"_string, false)
    {
    }
};

class SighurtApplication final : public WebView::Application {
    WEB_VIEW_APPLICATION(SighurtApplication)

public:
    explicit SighurtApplication(Optional<ByteString> ladybird_binary_path)
        : WebView::Application(move(ladybird_binary_path))
    {
    }

    // sig owns the process: no coordination with other Ladybird instances, no profile of the user's.
    virtual bool should_coordinate_browser_process() const override { return false; }

    // A download goes to a private staging directory of the profile, and when it is complete the
    // host is given the file (a Download service request); the user's folders are the host's.
    virtual ErrorOr<LexicalPath> default_path_for_download(URL::URL const&, Optional<u64> ladybird_view_id, ByteString const& file) const override
    {
        // The patched LibWebView says which view the download is for.
        return stage_download(file, ladybird_view_id.has_value() ? shim_view_for_ladybird_id(*ladybird_view_id) : 0);
    }

    virtual ErrorOr<LexicalPath> default_path_for_downloaded_file(ByteString const& file) const override
    {
        return stage_download(file, 0);
    }

    ErrorOr<LexicalPath> stage_download(ByteString const& file, uint64_t view) const
    {
        auto directory = ByteString::formatted("{}/downloads-staging", g_profile);
        TRY(Core::Directory::create(directory, Core::Directory::CreateDirectories::Yes));
        static u64 counter = 0;
        auto path = LexicalPath::join(directory, ByteString::formatted("{}-{}.download", getpid(), ++counter));
        g_staged.set(path.string(), Staged { Web::sanitize_suggested_download_filename(file), view });
        ensure_download_timer();
        return path;
    }

    // Nothing is opened by Ladybird: the host's handler asks the host.
    virtual void resolve_external_url_handler(URL::URL const&, WebView::ExternalURLHandlerCallback callback) const override
    {
        callback(HostURLHandler::create());
    }

    // The primary selection and the Copy actions of Ladybird's own menus do not leave it: the page's
    // copies arrive as a clipboard item of the view, which asks the host.
    virtual void set_clipboard_text(String, ClipboardType) override { }
    virtual Utf16String clipboard_text(ClipboardType) const override { return {}; }
};

void poll_downloads()
{
    auto& downloader = WebView::Application::the().file_downloader();
    Vector<ByteString> finished;
    Vector<u64> too_big;
    for (auto const& download : downloader.downloads()) {
        auto it = g_staged.find(download.destination.string());
        if (it == g_staged.end())
            continue;
        using Status = WebView::FileDownloader::DownloadStatus;
        if (download.status == Status::InProgress || download.status == Status::Paused) {
            // A download larger than the host takes is stopped while it runs, not after it filled the disk.
            if (download.downloaded_size > max_download_bytes || (download.total_size.has_value() && *download.total_size > max_download_bytes))
                too_big.append(download.id);
            continue;
        }
        finished.append(download.destination.string());
        if (download.status != Status::Completed) {
            (void)FileSystem::remove(download.destination.string(), FileSystem::RecursionMode::Disallowed);
            continue;
        }
        auto size = FileSystem::size_from_stat(download.destination.string());
        if (size.is_error() || size.value() > max_download_bytes) {
            dbgln("sighurt: a download of {} bytes is dropped (limit {})", size.is_error() ? 0 : size.value(), max_download_bytes);
            (void)FileSystem::remove(download.destination.string(), FileSystem::RecursionMode::Disallowed);
            continue;
        }
        auto file = Core::File::open(download.destination.string(), Core::File::OpenMode::Read);
        auto contents = file.is_error() ? ErrorOr<ByteBuffer>(file.release_error()) : file.value()->read_until_eof();
        (void)FileSystem::remove(download.destination.string(), FileSystem::RecursionMode::Disallowed);
        if (contents.is_error())
            continue;
        std::string url = to_std(download.url.serialize());
        std::string name = to_std(it->value.name);
        auto view = it->value.view != 0 ? it->value.view : g_active_view;
        std::string bytes(reinterpret_cast<char const*>(contents.value().data()), contents.value().size());
        contents = ByteBuffer {}; // One copy of the file, not two.
        push_moved(LB_EV_DOWNLOAD, view, url, name, move(bytes));
    }
    for (auto id : too_big) {
        auto destination = downloader.download(id).map([](auto const& download) { return download.destination.string(); });
        downloader.cancel_download(id);
        if (destination.has_value()) {
            dbgln("sighurt: a download over {} bytes is cancelled", max_download_bytes);
            (void)FileSystem::remove(*destination, FileSystem::RecursionMode::Disallowed);
            g_staged.remove(*destination);
        }
    }
    for (auto const& path : finished)
        g_staged.remove(path);
    if (g_staged.is_empty() && g_download_timer) {
        g_download_timer->stop();
        g_download_timer = nullptr;
    }
}

bool is_internal_page(URL::URL const& url)
{
    auto scheme = url.scheme();
    if (scheme == "resource"sv)
        return true;
    if (scheme != "about"sv)
        return false;
    auto serialized = url.serialize();
    return serialized != "about:blank"sv && serialized != "about:srcdoc"sv;
}

// HTML's "navigate to a fragment": the same address but for a fragment, which `to` has.
bool is_fragment_navigation(std::string const& from, std::string const& to)
{
    auto without = [](std::string const& url) { return url.substr(0, url.find('#')); };
    return !from.empty() && to.find('#') != std::string::npos && without(from) == without(to);
}

class LbView final : public WebView::HeadlessWebView {
public:
    static LbView* create(Core::AnonymousBuffer theme, Web::DevicePixelSize size, double scale)
    {
        auto view = adopt_own(*new LbView(move(theme), size, scale));
        view->initialize_client(CreateNewClient::Yes);
        auto* raw = view.ptr();
        views().set(raw->m_id, move(view));
        // A spare WebContent process loaded its blank page before the view took it over, and then
        // no load of it is ever reported to the view.
        if (raw->has_initial_entry())
            raw->become_ready();
        return raw;
    }

    static HashMap<u64, NonnullOwnPtr<LbView>>& views()
    {
        static auto* map = new HashMap<u64, NonnullOwnPtr<LbView>>;
        return *map;
    }

    static LbView* find(uint64_t id)
    {
        auto it = views().find(id);
        return it == views().end() ? nullptr : it->value.ptr();
    }

    uint64_t id() const { return m_id; }

    void apply_viewport(Web::DevicePixelSize size, double scale)
    {
        m_device_pixel_ratio = scale;
        reset_viewport_size(size);
    }

    RefPtr<Gfx::Bitmap> front_bitmap(bool* dmabuf, Web::DevicePixelSize* painted) const
    {
        if (!m_client_state.has_usable_bitmap || !m_client_state.front_bitmap.shared_image_buffer)
            return {};
        auto& buffer = *m_client_state.front_bitmap.shared_image_buffer;
#ifdef USE_VULKAN_DMABUF_IMAGES
        *dmabuf = buffer.linux_dmabuf_handle() != nullptr;
#else
        *dmabuf = false;
#endif
        *painted = m_client_state.front_bitmap.last_painted_size;
        return buffer.bitmap_if_present();
    }

    void close_now()
    {
        request_close();
        force_close();
    }

    // The page asked for a window of its own: it is a view the host never sees. It loads, and once
    // it has a URL the host is asked to open that URL (in a view of its own); this one closes.
    bool is_popup { false };
    uint64_t popup_opener { 0 };

    // Loads `url`, or holds it until the view's initial about:blank has finished.
    void navigate(URL::URL const& url)
    {
        if (m_initial_done)
            load(url);
        else
            m_queued_load = url;
    }
    bool initial_done() const { return m_initial_done; }

    // The view takes navigations: its blank page is loaded (by its own load, or already, when it took
    // over a spare WebContent process that had loaded it before the view existed).
    void become_ready()
    {
        if (m_initial_done)
            return;
        m_initial_done = true;
        if (!m_wanted_visible)
            set_system_visibility_state(Web::HTML::VisibilityState::Hidden);
        if (!is_popup)
            push(LB_EV_VIEW_READY, m_id);
        if (m_queued_load.has_value()) {
            auto url = m_queued_load.release_value();
            m_queued_load.clear();
            load(url);
        }
    }

    // A view that took over a spare process has its initial history entry at once.
    bool has_initial_entry() { return !traversable().session_history().is_empty(); }

    // What the host wants: applied at once once the view is ready, and then when it is.
    void set_wanted_visibility(bool visible)
    {
        m_wanted_visible = visible;
        if (m_initial_done)
            set_system_visibility_state(visible ? Web::HTML::VisibilityState::Visible : Web::HTML::VisibilityState::Hidden);
    }

private:
    LbView(Core::AnonymousBuffer theme, Web::DevicePixelSize size, double scale)
        : HeadlessWebView(move(theme), size)
        , m_id(++s_next_id)
    {
        m_device_pixel_ratio = scale;
        wire();
    }

    void wire()
    {
        auto id = m_id;
        on_title_change = [id](auto const& title) {
            auto text = to_std(title);
            push(LB_EV_TITLE, id, 0, 0, &text);
        };
        on_url_change = [this, id](auto const& url) {
            if (!m_initial_done)
                return;
            auto text = to_std(url.serialize());
            if (is_popup) {
                if (text != "about:blank")
                    finish_popup(text);
                return;
            }
            // A navigation to a fragment of the document shown: Ladybird sets loading and never
            // clears it, so the next real navigation would not report that it started.
            bool fragment = is_fragment_navigation(m_last_url, text);
            m_last_url = text;
            push(LB_EV_URL_CHANGED, id, 0, 0, &text);
            push_history();
            if (fragment && is_loading())
                set_loading_state(false);
        };
        on_load_start = [this, id] {
            if (is_popup || !m_initial_done)
                return;
            auto text = to_std(url().serialize());
            push(LB_EV_LOAD_START, id, 0, 0, &text);
        };
        on_load_finish = [this, id](auto const& url) {
            if (!m_initial_done) {
                // The view's own about:blank is loaded. A navigation started before this is
                // dropped by Ladybird, so the host's first one waits here.
                become_ready();
                return;
            }
            if (is_popup)
                return;
            auto text = to_std(url.serialize());
            push(LB_EV_LOAD_FINISH, id, 0, 0, &text);
            push_history();
        };
        on_loading_state_change = [this, id](bool loading) {
            if (!is_popup && m_initial_done)
                push(LB_EV_LOADING, id, loading ? 1 : 0);
        };
        on_cursor_change = [id](Gfx::Cursor const& cursor) {
            int standard = int(Gfx::StandardCursor::Arrow);
            if (cursor.has<Gfx::StandardCursor>())
                standard = int(cursor.get<Gfx::StandardCursor>());
            push(LB_EV_CURSOR, id, standard);
        };
        on_link_hover = [id](auto const& url) {
            auto text = to_std(url.serialize());
            push(LB_EV_HOVER, id, 0, 0, &text);
        };
        on_link_unhover = [id] { push(LB_EV_HOVER, id); };
        on_ready_to_paint = [this, id] {
            if (!is_popup)
                push(LB_EV_FRAME, id);
        };
        on_web_content_crashed = [id](auto reason) {
            std::string text = "the web content process crashed";
            push(LB_EV_CRASHED, id, int64_t(reason), 0, &text);
        };
        on_close = [id] { push(LB_EV_CLOSED, id); };
        on_top_level_navigation_commit = [this, id] {
            if (m_initial_done && !is_popup)
                push(LB_EV_COMMIT, id);
            push_history();
        };
        on_browser_history_traversal_complete = [this] { push_history(); };
        on_input_method_state_change = [this, id] {
            // The caret of the focused editable field, in device pixels. The text around it is
            // not reported: Ladybird does not say whether the field is a password.
            auto const& state = input_method_state();
            std::string caret = "0,0,0,0";
            if (state.is_enabled && state.caret_rect.has_value()) {
                auto const& rect = *state.caret_rect;
                caret = std::to_string(rect.x().value()) + "," + std::to_string(rect.y().value()) + "," + std::to_string(rect.width().value()) + "," + std::to_string(rect.height().value());
            }
            push(LB_EV_INPUT_METHOD, id, state.is_enabled ? 1 : 0, 0, &caret);
        };
        on_find_in_page = [id](size_t current, Optional<size_t> const& total) {
            push(LB_EV_FIND_RESULT, id, int64_t(current), total.has_value() ? int64_t(*total) : -1);
        };

        // A window.open or target=_blank: see is_popup.
        on_new_web_view = [this, id](auto, auto, Optional<Web::PageId> page_index) {
            auto* popup = page_index.has_value() ? create_child_popup(*page_index) : create_popup();
            popup->is_popup = true;
            popup->popup_opener = id;
            popup->arm_popup_timer();
            return popup->handle();
        };

        // A navigation the page started and the user caused (a click): the host may hand it to
        // another engine, so it is held until the host answers (patched LibWebView, nix/ladybird.nix).
        on_navigation_start_requested = [this, id](URL::URL const& url, bool user_activated, Function<void(bool)> proceed) {
            auto text = to_std(url.serialize());
            // Ladybird's own pages (about:settings and the like, which can open files and programs
            // outside the profile) are never shown; about:blank is the page's own business.
            if (is_internal_page(url)) {
                proceed(false);
                return;
            }
            // A window the page opened: its first navigation is not performed. The opener's browser
            // is asked to open the address in a view of its own, and this one closes.
            if (is_popup) {
                if (text == "about:blank" || text == "about:srcdoc") {
                    proceed(true);
                    return;
                }
                proceed(false);
                finish_popup(text);
                return;
            }
            // Only what the user started, and not the page's own fragments or the host's own loads.
            if (!user_activated || is_popup || !m_initial_done || is_fragment_navigation(m_last_url, text)) {
                proceed(true);
                return;
            }
            auto token = ++g_next_token;
            g_held_navigations.set(token, move(proceed));
            push(LB_EV_NAVIGATION_REQUESTED, id, 0, token, &text);
        };

        // The host has no file picker for pages yet that is not a service: ask it.
        on_request_file_picker = [id](auto const&, auto multiple) {
            auto token = ++g_next_token;
            g_pickers.set(token, id);
            push(LB_EV_PICK_FILES, id, multiple == Web::HTML::AllowMultipleFiles::Yes ? 1 : 0, token);
        };
        // A page's link to mailto: and the like: the host decides and opens it (launch, above).
        on_request_external_url_confirmation = [id](auto const&, auto const&, auto const&, Function<void(bool)> proceed) {
            g_launch_view = id;
            proceed(true);
            g_launch_view = 0;
        };

        // The API has no dialog service: alert, confirm and prompt are answered as dismissed, so a
        // page never hangs on one.
        on_request_alert = [this](auto const&) { alert_closed(); };
        on_request_confirm = [this](auto const&) { confirm_closed(false); };
        on_request_prompt = [this](auto const&, auto const&) { prompt_closed({}); };
    }

public:
    // The page copied: the host decides what becomes of it.
    virtual void insert_clipboard_item(Web::Clipboard::SystemClipboardItem item) override
    {
        Vector<std::pair<std::string, std::string>> items;
        size_t total = 0;
        for (auto const& representation : item.system_clipboard_representations) {
            auto const* data = representation.data.get_pointer<ByteString>();
            if (!data)
                continue; // A file the user copied on the desktop: never exposed.
            total += data->length();
            if (total > max_clipboard_bytes)
                return;
            items.append({ to_std(representation.name), std::string(data->characters(), data->length()) });
        }
        for (size_t i = 0; i < items.size(); ++i)
            push(LB_EV_CLIPBOARD_WRITE, m_id, 0, int64_t(items.size() - 1 - i), &items[i].first, nullptr, &items[i].second);
    }

    // A paste reads what the host granted a moment ago (grant_clipboard), and finding nothing asks
    // for it: the page sees an empty first paste, then the real one.
    virtual Web::Clipboard::SystemClipboardItem clipboard_item() const override
    {
        if (m_grant.has_value() && now_ns() < m_grant_until)
            return *m_grant;
        m_grant.clear();
        if (!m_clipboard_wanted) {
            m_clipboard_wanted = true;
            push(LB_EV_CLIPBOARD_WANT, m_id);
        }
        return {};
    }

    void grant_clipboard(std::string const& text)
    {
        m_clipboard_wanted = false;
        m_grant = Web::Clipboard::SystemClipboardItem { { { "text/plain"_string, ByteString(text.data(), text.size()) } } };
        m_grant_until = now_ns() + 1'000'000'000;
        paste_from_clipboard();
    }
    void deny_clipboard() { m_clipboard_wanted = false; }

    void picked(Vector<Web::HTML::SelectedFile> files) { file_picker_closed(move(files)); }

private:
    LbView* create_popup()
    {
        return create(m_theme, m_viewport_size, m_device_pixel_ratio);
    }

    LbView* create_child_popup(Web::PageId page_index)
    {
        auto view = adopt_own(*new LbView(m_theme, m_viewport_size, m_device_pixel_ratio));
        view->m_client_state.client = client();
        view->m_client_state.page_index = page_index;
        view->initialize_client(CreateNewClient::No);
        auto* raw = view.ptr();
        views().set(raw->m_id, move(view));
        return raw;
    }

    void finish_popup(std::string const& url)
    {
        if (!is_popup)
            return; // Once only.
        is_popup = false;
        push(LB_EV_OPEN_REQUESTED, popup_opener, 0, 0, &url);
        close_popup_later();
    }

    void close_popup_later()
    {
        auto id = m_id;
        Core::deferred_invoke([id] {
            if (auto* view = LbView::find(id)) {
                view->close_now();
                views().remove(id);
            }
        });
    }

public:
    // A popup that has not asked for an address within a bound (an about:blank one a script writes
    // into) is closed.
    void arm_popup_timer()
    {
        auto id = m_id;
        m_popup_timer = Core::Timer::create_single_shot(10'000, [id] {
            if (auto* view = LbView::find(id); view && view->is_popup) {
                view->is_popup = false;
                view->close_popup_later();
            }
        });
        m_popup_timer->start();
    }

private:

    void push_history() { g_history_dirty.set(m_id); }

public:
    // Called after a pump: the history of a view that changed, once however often it changed.
    void flush_history()
    {
        auto snapshot = traversable().session_history();
        auto entries = snapshot.entries();
        auto used = snapshot.used_steps();
        auto current = snapshot.current_used_step_index();
        std::string urls;
        int64_t count = 0;
        int64_t index = 0;
        if (current.has_value() && snapshot.has_only_top_level_used_steps() && entries.size() == used.size()) {
            for (auto& entry : entries) {
                urls += to_std(entry.url.serialize());
                urls += '\n';
                ++count;
            }
            index = int64_t(*current);
        } else {
            urls = to_std(url().serialize()) + "\n";
            count = 1;
        }
        push(LB_EV_HISTORY, m_id, index, count, &urls);
    }

private:

    static inline uint64_t s_next_id { 0 };
    uint64_t m_id;
    bool m_initial_done { false };
    bool m_wanted_visible { true };
    std::string m_last_url;
    mutable Optional<Web::Clipboard::SystemClipboardItem> m_grant;
    mutable bool m_clipboard_wanted { false };
    i64 m_grant_until { 0 };
    Optional<URL::URL> m_queued_load;
    RefPtr<Core::Timer> m_popup_timer;
};

OwnPtr<SighurtApplication> g_app;
Core::AnonymousBuffer g_theme;
std::vector<ByteString> g_arg_storage;
std::vector<char*> g_argv_pointers;
std::vector<StringView> g_arg_views;

void set_error(char* err, size_t len, std::string const& message)
{
    if (err && len)
        snprintf(err, len, "%s", message.c_str());
}

uint64_t shim_service_view(uint64_t id)
{
    if (auto* view = LbView::find(id); view && view->is_popup && view->popup_opener != 0)
        return view->popup_opener;
    return id;
}

uint64_t shim_view_for_ladybird_id(u64 ladybird_id)
{
    for (auto& entry : LbView::views())
        if (entry.value->view_id() == ladybird_id)
            return entry.key;
    return 0;
}

using namespace Web::UIEvents;

KeyCode key_code_for(std::string const& code)
{
    static std::map<std::string, KeyCode> const table = [] {
        std::map<std::string, KeyCode> map;
        for (char c = 'A'; c <= 'Z'; ++c)
            map[std::string("Key") + c] = code_point_to_key_code(c);
        for (char c = '0'; c <= '9'; ++c)
            map[std::string("Digit") + c] = code_point_to_key_code(c);
        map["Backspace"] = KeyCode::Key_Backspace;
        map["Tab"] = KeyCode::Key_Tab;
        map["Enter"] = KeyCode::Key_Return;
        map["NumpadEnter"] = KeyCode::Key_Return;
        map["Space"] = KeyCode::Key_Space;
        map["Escape"] = KeyCode::Key_Escape;
        map["PageUp"] = KeyCode::Key_PageUp;
        map["PageDown"] = KeyCode::Key_PageDown;
        map["End"] = KeyCode::Key_End;
        map["Home"] = KeyCode::Key_Home;
        map["ArrowLeft"] = KeyCode::Key_Left;
        map["ArrowUp"] = KeyCode::Key_Up;
        map["ArrowRight"] = KeyCode::Key_Right;
        map["ArrowDown"] = KeyCode::Key_Down;
        map["Delete"] = KeyCode::Key_Delete;
        map["Insert"] = KeyCode::Key_Insert;
        map["ShiftLeft"] = KeyCode::Key_LeftShift;
        map["ShiftRight"] = KeyCode::Key_RightShift;
        map["ControlLeft"] = KeyCode::Key_LeftControl;
        map["ControlRight"] = KeyCode::Key_RightControl;
        map["AltLeft"] = KeyCode::Key_LeftAlt;
        map["AltRight"] = KeyCode::Key_RightAlt;
        map["CapsLock"] = KeyCode::Key_CapsLock;
        map["Minus"] = KeyCode::Key_Minus;
        map["Equal"] = KeyCode::Key_Equal;
        map["BracketLeft"] = KeyCode::Key_LeftBracket;
        map["BracketRight"] = KeyCode::Key_RightBracket;
        map["Backslash"] = KeyCode::Key_Backslash;
        map["Semicolon"] = KeyCode::Key_Semicolon;
        map["Quote"] = KeyCode::Key_Apostrophe;
        map["Backquote"] = KeyCode::Key_Backtick;
        map["Comma"] = KeyCode::Key_Comma;
        map["Period"] = KeyCode::Key_Period;
        map["Slash"] = KeyCode::Key_Slash;
        map["F1"] = KeyCode::Key_F1;
        map["F2"] = KeyCode::Key_F2;
        map["F3"] = KeyCode::Key_F3;
        map["F4"] = KeyCode::Key_F4;
        map["F5"] = KeyCode::Key_F5;
        map["F6"] = KeyCode::Key_F6;
        map["F7"] = KeyCode::Key_F7;
        map["F8"] = KeyCode::Key_F8;
        map["F9"] = KeyCode::Key_F9;
        map["F10"] = KeyCode::Key_F10;
        map["F11"] = KeyCode::Key_F11;
        map["F12"] = KeyCode::Key_F12;
        return map;
    }();
    auto it = table.find(code);
    return it == table.end() ? KeyCode::Key_Invalid : it->second;
}

KeyModifier modifiers_from(int modifiers)
{
    int result = Mod_None;
    if (modifiers & 1)
        result |= Mod_Alt;
    if (modifiers & 2)
        result |= Mod_Ctrl;
    if (modifiers & 4)
        result |= Mod_Shift;
    if (modifiers & 8)
        result |= Mod_Super;
    return KeyModifier(result);
}

MouseButton button_from(int button)
{
    switch (button) {
    case 1:
        return MouseButton::Primary;
    case 2:
        return MouseButton::Secondary;
    case 4:
        return MouseButton::Middle;
    case 8:
        return MouseButton::Backward;
    case 16:
        return MouseButton::Forward;
    default:
        return MouseButton::None;
    }
}

Web::DevicePixelPoint point(int x, int y) { return { x, y }; }

}

// Stops Ladybird: views, the application (which asks its helpers to go), then every helper still
// running is signalled and reaped through its pidfd, all against one deadline. Also the way out of a
// failed lb_init. Nothing is left that could run after the library's globals are destroyed.
void teardown()
{
    for (auto& entry : LbView::views())
        entry.value->close_now();
    g_held_navigations.clear();
    g_launches.clear();
    g_pickers.clear();
    g_history_dirty.clear();
    LbView::views().clear();
    g_app = nullptr;
    // The download timer and the files it was watching.
    if (g_download_timer) {
        g_download_timer->stop();
        g_download_timer = nullptr;
    }
    for (auto& entry : g_staged)
        (void)FileSystem::remove(entry.key, FileSystem::RecursionMode::Disallowed);
    g_staged.clear();
    // The downloader keeps partial files beside the destinations: the whole staging directory goes.
    (void)FileSystem::remove(ByteString::formatted("{}/downloads-staging", g_profile), FileSystem::RecursionMode::Allowed);
    // Helpers: ask them all, then wait for them all, then force what is left.
    for (auto& entry : children())
        syscall(SYS_pidfd_send_signal, entry.value.pidfd, SIGTERM, nullptr, 0);
    auto deadline = now_ns() + 2'000'000'000;
    bool forced = false;
    for (;;) {
        Vector<pid_t> pids;
        for (auto& entry : children())
            pids.append(entry.key);
        for (auto pid : pids) {
            auto& child = *children().get(pid);
            Optional<int> status;
            if (reap_exited(child.pidfd, status)) {
                child.notifier = nullptr;
                close(child.pidfd);
                children().remove(pid);
            }
        }
        if (children().is_empty())
            break;
        if (now_ns() > deadline) {
            if (forced)
                break; // Nothing more can be done for a helper that survives SIGKILL.
            for (auto& entry : children())
                syscall(SYS_pidfd_send_signal, entry.value.pidfd, SIGKILL, nullptr, 0);
            forced = true;
            deadline = now_ns() + 1'000'000'000;
        }
        usleep(5'000);
    }
    for (auto& entry : children()) {
        entry.value.notifier = nullptr;
        close(entry.value.pidfd);
    }
    children().clear();
}

bool g_init_failed = false;

extern "C" {

void lb_abi_describe(lb_abi* out)
{
    memset(out, 0, sizeof(*out));
    out->version = LB_ABI_VERSION;
    out->sizeof_shared_image_buffer = sizeof(Gfx::SharedImageBuffer);
    out->sizeof_bitmap = sizeof(Gfx::Bitmap);
#ifdef USE_VULKAN_DMABUF_IMAGES
    out->vulkan_dmabuf = 1;
#endif
    // The probe: the library makes a buffer with its own constructor, and this file reads it with
    // the layout of the headers it was compiled with. A mismatch reads the wrong member.
    auto buffer = Gfx::SharedImageBuffer::create({ 2, 3 });
    auto bitmap = buffer.bitmap_if_present();
    out->probe_shared_image_buffer = (bitmap && bitmap->width() == 2 && bitmap->height() == 3) ? 1 : 0;
    out->probe_view_implementation = 0; // not probed
}

int lb_init(char const* const* argv, int argc, char const* profile, char const* prefix, char* err, size_t err_len)
{
    if (g_app || g_init_failed) {
        set_error(err, err_len, g_init_failed ? "Ladybird failed to start before; the process cannot start it again" : "lb_init was called twice");
        return 1;
    }
    g_init_failed = true; // Until it has succeeded: the loop's manager can be installed once.
    Sighurt::loop_install();
    g_profile = profile ? profile : "";

    ByteString binary_path = ByteString::formatted("{}/libexec", prefix && *prefix ? prefix : SIGHURT_LADYBIRD_PREFIX);

    g_arg_storage.clear();
    g_arg_storage.push_back("sig-ladybird");
    g_arg_storage.push_back("--profile-path");
    g_arg_storage.push_back(profile ? profile : "");
    g_arg_storage.push_back("--disable-sql-database");
    for (int i = 0; i < argc; ++i)
        g_arg_storage.push_back(argv[i]);
    g_argv_pointers.clear();
    g_arg_views.clear();
    for (auto& arg : g_arg_storage) {
        g_argv_pointers.push_back(const_cast<char*>(arg.characters()));
        g_arg_views.push_back(arg.view());
    }
    Main::Arguments arguments { int(g_argv_pointers.size()), g_argv_pointers.data(), Span<StringView>(g_arg_views.data(), g_arg_views.size()) };

    auto app = SighurtApplication::create(arguments, Optional<ByteString>(binary_path));
    if (app.is_error()) {
        set_error(err, err_len, to_std(app.error().string_literal()));
        return 2;
    }
    g_app = app.release_value();
    watch_children();
    // What an earlier run left in the staging directory is not a download of this one.
    (void)FileSystem::remove(ByteString::formatted("{}/downloads-staging", g_profile), FileSystem::RecursionMode::Allowed);

    auto theme_path = LexicalPath::join(WebView::s_ladybird_resource_root, "themes"sv, "Default.ini"sv);
    auto theme = Gfx::load_system_theme(theme_path.string());
    if (theme.is_error()) {
        set_error(err, err_len, "cannot load Ladybird's theme from " + to_std(theme_path.string()));
        teardown(); // Not a half-started Ladybird with its helpers and nobody to pump them.
        return 3;
    }
    g_theme = theme.release_value();
    g_init_failed = false;
    return 0;
}

int lb_loop_fd(void) { return Sighurt::loop_fd(); }

int64_t lb_pump(int* more)
{
    auto result = Sighurt::loop_pump();
    {
        auto dirty = move(g_history_dirty);
        g_history_dirty = {};
        for (auto id : dirty) {
            if (auto* view = LbView::find(id))
                view->flush_history();
        }
    }
    if (more)
        *more = (result.more_queued || result.dispatched > 0 || !g_events.empty()) ? 1 : 0;
    return result.next_timer_ms;
}

int lb_next_event(lb_event* out)
{
    if (g_events.empty())
        return 0;
    if (is_replaceable(g_events.front().kind)) {
        auto it = g_replaceable.find({ g_events.front().kind, g_events.front().view });
        if (it != g_replaceable.end() && it->second == g_events.begin())
            g_replaceable.erase(it);
    }
    g_current = move(g_events.front());
    g_events.pop_front();
    out->kind = g_current.kind;
    out->view = g_current.view;
    out->i0 = g_current.i0;
    out->i1 = g_current.i1;
    out->s0 = g_current.has_s0 ? g_current.s0.c_str() : nullptr;
    out->s1 = g_current.s1.empty() ? nullptr : g_current.s1.c_str();
    out->data = g_current.has_data ? g_current.data.data() : nullptr;
    out->len = g_current.data.size();
    return 1;
}

uint64_t lb_wake_count(void) { return Sighurt::loop_wake_count(); }

uint64_t lb_view_open(int width, int height, double scale, int visible, double zoom)
{
    auto* view = LbView::create(g_theme, { width, height }, scale);
    // A hidden page gets no rendering opportunities, and without them its own blank page never
    // finishes loading, so the view could not take a navigation (nor report ViewReady) before it is
    // shown, and the host shows it only after ViewReady. It starts visible and is hidden once ready.
    view->set_wanted_visibility(visible != 0);
    if (zoom != 1.0)
        view->set_zoom(zoom);
    return view->id();
}

void lb_view_close(uint64_t id)
{
    if (auto* view = LbView::find(id)) {
        view->close_now();
        LbView::views().remove(id);
    }
}

void lb_navigate(uint64_t id, char const* url)
{
    g_active_view = id;
    auto* view = LbView::find(id);
    if (!view)
        return;
    if (auto parsed = URL::Parser::basic_parse(StringView { url, strlen(url) }); parsed.has_value())
        view->navigate(*parsed);
}

void lb_reload(uint64_t id)
{
    if (auto* view = LbView::find(id))
        view->reload();
}

void lb_stop(uint64_t id)
{
    if (auto* view = LbView::find(id))
        view->stop_loading();
}

void lb_traverse(uint64_t id, int delta)
{
    if (auto* view = LbView::find(id))
        view->traverse_the_history_by_delta(delta);
}

void lb_set_viewport(uint64_t id, int width, int height, double scale)
{
    if (auto* view = LbView::find(id))
        view->apply_viewport({ width, height }, scale);
}

void lb_set_visible(uint64_t id, int visible)
{
    if (auto* view = LbView::find(id))
        view->set_wanted_visibility(visible != 0);
}

void lb_set_focus(uint64_t id, int focused)
{
    if (focused)
        g_active_view = id;
    if (auto* view = LbView::find(id))
        view->set_has_system_focus(focused != 0);
}

void lb_set_zoom(uint64_t id, double zoom)
{
    if (auto* view = LbView::find(id))
        view->set_zoom(zoom);
}

void lb_run_js(uint64_t id, char const* script, int64_t request)
{
    auto* view = LbView::find(id);
    if (!view)
        return;
    (void)request;
    view->run_javascript(MUST(String::from_utf8(StringView { script, strlen(script) })));
}

void lb_input_key(uint64_t id, char const* w3c_code, char const* key, int pressed, int repeat, int modifiers, uint32_t code_point)
{
    g_active_view = id;
    auto* view = LbView::find(id);
    if (!view)
        return;
    (void)key;
    Web::KeyEvent event {
        .type = pressed ? Web::KeyEvent::Type::KeyDown : Web::KeyEvent::Type::KeyUp,
        .key = key_code_for(w3c_code ? w3c_code : ""),
        .modifiers = modifiers_from(modifiers),
        .code_point = code_point,
        .repeat = repeat != 0,
        .should_insert_text = pressed && code_point != 0 && !(modifiers & 2),
        .browser_data = nullptr,
    };
    if (event.key == KeyCode::Key_Invalid && code_point != 0)
        event.key = code_point_to_key_code(code_point);
    view->enqueue_input_event(move(event));
}

void lb_input_pointer_move(uint64_t id, int x, int y, int modifiers)
{
    if (auto* view = LbView::find(id)) {
        view->enqueue_input_event(Web::MouseEvent {
            .type = Web::MouseEvent::Type::MouseMove,
            .position = point(x, y),
            .screen_position = point(x, y),
            .modifiers = modifiers_from(modifiers),
            .browser_data = nullptr,
        });
    }
}

void lb_input_pointer_leave(uint64_t id)
{
    if (auto* view = LbView::find(id)) {
        view->enqueue_input_event(Web::MouseEvent {
            .type = Web::MouseEvent::Type::MouseLeave,
            .position = {},
            .screen_position = {},
            .browser_data = nullptr,
        });
    }
}

void lb_input_pointer_button(uint64_t id, int x, int y, int button, int pressed, int click_count, int modifiers)
{
    g_active_view = id;
    if (auto* view = LbView::find(id)) {
        view->enqueue_input_event(Web::MouseEvent {
            .type = pressed ? Web::MouseEvent::Type::MouseDown : Web::MouseEvent::Type::MouseUp,
            .position = point(x, y),
            .screen_position = point(x, y),
            .button = button_from(button),
            .buttons = pressed ? button_from(button) : MouseButton::None,
            .modifiers = modifiers_from(modifiers),
            .click_count = click_count,
            .browser_data = nullptr,
        });
    }
}

void lb_input_scroll(uint64_t id, int x, int y, double dx, double dy, int precise, int modifiers)
{
    if (auto* view = LbView::find(id)) {
        view->enqueue_input_event(Web::MouseEvent {
            .type = Web::MouseEvent::Type::MouseWheel,
            .position = point(x, y),
            .screen_position = point(x, y),
            .modifiers = modifiers_from(modifiers),
            .wheel_delta_x = dx,
            .wheel_delta_y = dy,
            .wheel_delta_precision = precise ? Web::WheelDeltaPrecision::Precise : Web::WheelDeltaPrecision::Discrete,
            .browser_data = nullptr,
        });
    }
}

void lb_input_preedit(uint64_t id, char const* text)
{
    if (auto* view = LbView::find(id))
        view->set_marked_text_from_input_method(Utf16String::from_utf8(StringView { text, strlen(text) }));
}

void lb_clipboard_grant(uint64_t id, uint8_t const* data, size_t len)
{
    if (auto* view = LbView::find(id))
        view->grant_clipboard(std::string(reinterpret_cast<char const*>(data), len));
}

void lb_clipboard_deny(uint64_t id)
{
    if (auto* view = LbView::find(id))
        view->deny_clipboard();
}

void lb_pick_reply(int64_t token, char const* paths)
{
    auto entry = g_pickers.take(token);
    if (!entry.has_value())
        return;
    auto* view = LbView::find(*entry);
    if (!view)
        return;
    Vector<Web::HTML::SelectedFile> files;
    if (paths) {
        for (auto const& path : ByteString(paths).split('\n')) {
            auto size = FileSystem::size_from_stat(path);
            if (size.is_error() || size.value() > max_picked_file_bytes)
                continue;
            auto file = Core::File::open(path, Core::File::OpenMode::Read);
            if (file.is_error())
                continue;
            auto contents = file.value()->read_until_eof();
            if (contents.is_error())
                continue;
            files.append(Web::HTML::SelectedFile(Utf16String::from_utf8(LexicalPath::basename(path)), contents.release_value()));
        }
    }
    view->picked(move(files));
}

void lb_open_external_reply(int64_t token, int allowed)
{
    if (auto callback = g_launches.take(token); callback.has_value())
        (*callback)(allowed != 0);
}

void lb_navigation_answer(int64_t token, int allow)
{
    if (auto proceed = g_held_navigations.take(token); proceed.has_value())
        (*proceed)(allow != 0);
}

void lb_input_commit_text(uint64_t id, char const* text)
{
    if (auto* view = LbView::find(id))
        view->commit_text_from_input_method(Utf16String::from_utf8(StringView { text, strlen(text) }));
}

int lb_frame_get(uint64_t id, lb_frame* out)
{
    auto* view = LbView::find(id);
    if (!view)
        return 0;
    bool dmabuf = false;
    Web::DevicePixelSize painted;
    auto bitmap = view->front_bitmap(&dmabuf, &painted);
    if (!bitmap)
        return 0;
    out->pixels = bitmap->scanline_u8(0);
    out->width = min(painted.width().value(), bitmap->width());
    out->height = min(painted.height().value(), bitmap->height());
    out->stride = bitmap->pitch();
    out->dmabuf = dmabuf ? 1 : 0;
    return 1;
}

void lb_shutdown(void)
{
    teardown();
}

}
