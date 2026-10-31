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
#include <LibCore/StandardPaths.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibGfx/SystemTheme.h>
#include <LibMain/Main.h>
#include <LibURL/Parser.h>
#include <LibWeb/Page/InputEvent.h>
#include <LibWebView/Application.h>
#include <LibWebView/HeadlessWebView.h>
#include <LibWebView/Utilities.h>

#include <deque>
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
};

std::deque<Event> g_events;
Event g_current;

void push(int kind, uint64_t view, int64_t i0 = 0, int64_t i1 = 0, std::string const* s0 = nullptr, std::string const* s1 = nullptr)
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
    g_events.push_back(move(event));
}

std::string to_std(StringView view) { return std::string(view.characters_without_null_termination(), view.length()); }
std::string to_std(String const& string) { return to_std(string.bytes_as_string_view()); }
std::string to_std(Utf16String const& string) { return to_std(string.to_utf8()); }

class SighurtApplication final : public WebView::Application {
    WEB_VIEW_APPLICATION(SighurtApplication)

public:
    explicit SighurtApplication(Optional<ByteString> ladybird_binary_path)
        : WebView::Application(move(ladybird_binary_path))
    {
    }

    // sig owns the process: no coordination with other Ladybird instances, no profile of the user's.
    virtual bool should_coordinate_browser_process() const override { return false; }

    // A download would write into the user's folders; the host decides that (services), and the
    // shim does not yet ask it. Denied.
    virtual ErrorOr<LexicalPath> default_path_for_downloaded_file(ByteString const&) const override
    {
        return Error::from_string_literal("downloads are not supported by this engine yet");
    }
};

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
                m_initial_done = true;
                if (!is_popup)
                    push(LB_EV_VIEW_READY, id);
                if (m_queued_load.has_value()) {
                    auto url = m_queued_load.release_value();
                    m_queued_load.clear();
                    load(url);
                }
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
        on_top_level_navigation_commit = [this] { push_history(); };
        on_browser_history_traversal_complete = [this] { push_history(); };
        on_input_method_state_change = [this, id] {
            push(LB_EV_INPUT_METHOD, id, input_method_state().is_enabled ? 1 : 0);
        };
        on_find_in_page = [id](size_t current, Optional<size_t> const& total) {
            push(LB_EV_FIND_RESULT, id, int64_t(current), total.has_value() ? int64_t(*total) : -1);
        };

        // A window.open or target=_blank: see is_popup.
        on_new_web_view = [this, id](auto, auto, Optional<Web::PageId> page_index) {
            auto* popup = page_index.has_value() ? create_child_popup(*page_index) : create_popup();
            popup->is_popup = true;
            popup->popup_opener = id;
            return popup->handle();
        };

        // The host has no dialogs yet: they are answered as dismissed, so a page never hangs on one.
        on_request_alert = [this](auto const&) { alert_closed(); };
        on_request_confirm = [this](auto const&) { confirm_closed(false); };
        on_request_prompt = [this](auto const&, auto const&) { prompt_closed({}); };
    }

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
        is_popup = false; // Once only.
        push(LB_EV_OPEN_REQUESTED, popup_opener, 0, 0, &url);
        auto id = m_id;
        Core::deferred_invoke([id] {
            if (auto* view = LbView::find(id)) {
                view->close_now();
                views().remove(id);
            }
        });
    }

    void push_history()
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

    static inline uint64_t s_next_id { 0 };
    uint64_t m_id;
    bool m_initial_done { false };
    std::string m_last_url;
    Optional<URL::URL> m_queued_load;
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
    if (g_app) {
        set_error(err, err_len, "lb_init was called twice");
        return 1;
    }
    Sighurt::loop_install();

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

    auto theme_path = LexicalPath::join(WebView::s_ladybird_resource_root, "themes"sv, "Default.ini"sv);
    auto theme = Gfx::load_system_theme(theme_path.string());
    if (theme.is_error()) {
        set_error(err, err_len, "cannot load Ladybird's theme from " + to_std(theme_path.string()));
        return 3;
    }
    g_theme = theme.release_value();
    return 0;
}

int lb_loop_fd(void) { return Sighurt::loop_fd(); }

int64_t lb_pump(int* more)
{
    auto result = Sighurt::loop_pump();
    if (more)
        *more = (result.more_queued || result.dispatched > 0 || !g_events.empty()) ? 1 : 0;
    return result.next_timer_ms;
}

int lb_next_event(lb_event* out)
{
    if (g_events.empty())
        return 0;
    g_current = move(g_events.front());
    g_events.pop_front();
    out->kind = g_current.kind;
    out->view = g_current.view;
    out->i0 = g_current.i0;
    out->i1 = g_current.i1;
    out->s0 = g_current.has_s0 ? g_current.s0.c_str() : nullptr;
    out->s1 = g_current.s1.empty() ? nullptr : g_current.s1.c_str();
    return 1;
}

uint64_t lb_wake_count(void) { return Sighurt::loop_wake_count(); }

uint64_t lb_view_open(int width, int height, double scale, int visible, double zoom)
{
    auto* view = LbView::create(g_theme, { width, height }, scale);
    if (!visible)
        view->set_system_visibility_state(Web::HTML::VisibilityState::Hidden);
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
        view->set_system_visibility_state(visible ? Web::HTML::VisibilityState::Visible : Web::HTML::VisibilityState::Hidden);
}

void lb_set_focus(uint64_t id, int focused)
{
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
    // Views first, then the application (it stops the helper processes). The event loop is not
    // pumped again: closing is forced, not waited for.
    for (auto& entry : LbView::views())
        entry.value->close_now();
    LbView::views().clear();
    g_app = nullptr;
}

}
