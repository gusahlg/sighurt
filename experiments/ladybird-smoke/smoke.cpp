// Smoke test of an embedder of Ladybird's LibWebView (docs/rewrite/LADYBIRD.md): a WebView::Application
// (the UI process) with one HeadlessWebView, loads a URL, waits for the first frame after the load and
// writes it as a PNG. Modelled on Tests/LibWebView/TestLinkHoverStatus.cpp of the Ladybird tree.
//
//   smoke URL OUT.png [WIDTH HEIGHT]
//
// Env: LADYBIRD_PREFIX = the install prefix whose libexec/ holds WebContent, RequestServer, ...
#include <AK/ByteString.h>
#include <AK/LexicalPath.h>
#include <AK/Random.h>
#include <AK/ScopeGuard.h>
#include <AK/String.h>
#include <AK/Utf16String.h>
#include <LibCore/Directory.h>
#include <LibCore/Environment.h>
#include <LibCore/EventLoop.h>
#include <LibCore/File.h>
#include <LibCore/StandardPaths.h>
#include <LibCore/Timer.h>
#include <LibFileSystem/FileSystem.h>
#include <LibGfx/Bitmap.h>
#include <LibGfx/ImageFormats/PNGWriter.h>
#include <LibGfx/SharedImageBuffer.h>
#include <LibGfx/SystemTheme.h>
#include <LibMain/Main.h>
#include <LibURL/Parser.h>
#include <LibWebView/Application.h>
#include <LibWebView/HeadlessWebView.h>
#include <LibWebView/Utilities.h>

namespace {

class SmokeApplication : public WebView::Application {
    WEB_VIEW_APPLICATION(SmokeApplication)

public:
    explicit SmokeApplication(Optional<ByteString> ladybird_binary_path)
        : WebView::Application(move(ladybird_binary_path))
    {
    }

    virtual void create_platform_options(WebView::BrowserOptions& browser_options, WebView::RequestServerOptions&, WebView::WebContentOptions&) override
    {
        browser_options.headless_mode = WebView::HeadlessMode::Screenshot;
        browser_options.disable_sql_database = WebView::DisableSQLDatabase::Yes;
    }

    // sig owns the process's event loop; here the application only coordinates nothing.
    virtual bool should_coordinate_browser_process() const override { return false; }
    virtual bool should_use_temporary_profile_by_default() const override { return true; }
};

// HeadlessWebView with access to the frame the compositor presented (protected in ViewImplementation).
class SmokeView final : public WebView::HeadlessWebView {
public:
    static NonnullOwnPtr<SmokeView> create(Core::AnonymousBuffer theme, Web::DevicePixelSize size)
    {
        auto view = adopt_own(*new SmokeView(move(theme), size));
        view->initialize_client(CreateNewClient::Yes);
        return view;
    }

    RefPtr<Gfx::Bitmap> front_bitmap() const
    {
        if (!m_client_state.has_usable_bitmap || !m_client_state.front_bitmap.shared_image_buffer)
            return {};
        return m_client_state.front_bitmap.shared_image_buffer->bitmap_if_present();
    }
    bool has_usable_frame() const { return m_client_state.has_usable_bitmap; }
    bool front_is_dmabuf() const
    {
#ifdef USE_VULKAN_DMABUF_IMAGES
        return m_client_state.front_bitmap.shared_image_buffer && m_client_state.front_bitmap.shared_image_buffer->linux_dmabuf_handle();
#else
        return false;
#endif
    }

private:
    SmokeView(Core::AnonymousBuffer theme, Web::DevicePixelSize size)
        : HeadlessWebView(move(theme), size)
    {
    }
};

}

ErrorOr<int> ladybird_main(Main::Arguments arguments)
{
    if (arguments.argc < 3) {
        warnln("usage: smoke URL OUT.png [WIDTH HEIGHT]");
        return 2;
    }
    ByteString url_string = arguments.strings[1];
    ByteString out_path = arguments.strings[2];
    int width = arguments.argc > 4 ? ByteString(arguments.strings[3]).to_number<int>().value_or(800) : 800;
    int height = arguments.argc > 4 ? ByteString(arguments.strings[4]).to_number<int>().value_or(600) : 600;

    auto config_dir = ByteString::formatted("{}/ladybird-smoke-{}", Core::StandardPaths::tempfile_directory(), generate_random_uuid());
    TRY(Core::Directory::create(config_dir, Core::Directory::CreateDirectories::Yes));
    ScopeGuard cleanup([&] { (void)FileSystem::remove(config_dir, FileSystem::RecursionMode::Allowed); });
    MUST(Core::Environment::set("XDG_CONFIG_HOME"sv, config_dir, Core::Environment::Overwrite::Yes));
    MUST(Core::Environment::set("XDG_DATA_HOME"sv, config_dir, Core::Environment::Overwrite::Yes));
    MUST(Core::Environment::set("XDG_CACHE_HOME"sv, config_dir, Core::Environment::Overwrite::Yes));

    // Helpers are found as <binary path with libexec stripped>/libexec/<Name>; pass the prefix's libexec.
    Optional<ByteString> binary_path;
    if (auto prefix = Core::Environment::get("LADYBIRD_PREFIX"sv); prefix.has_value())
        binary_path = ByteString::formatted("{}/libexec", *prefix);
    auto app = TRY(SmokeApplication::create(arguments, binary_path));

    auto theme_path = LexicalPath::join(WebView::s_ladybird_resource_root, "themes"sv, "Default.ini"sv);
    auto theme = TRY(Gfx::load_system_theme(theme_path.string()));

    auto view = SmokeView::create(move(theme), { width, height });

    size_t loads_finished = 0;
    size_t frames_since_navigation = 0;
    view->on_load_finish = [&](auto const& url) {
        ++loads_finished;
        outln("on_load_finish {} (#{})", url, loads_finished);
    };
    view->on_title_change = [&](auto const& title) { outln("on_title_change {}", title); };
    view->on_url_change = [&](auto const& url) { outln("on_url_change {}", url); };
    view->on_ready_to_paint = [&] { ++frames_since_navigation; };

    auto& loop = Core::EventLoop::current();
    // The first load is about:blank, and navigating before it completes drops the navigation.
    loop.spin_until([&] { return loads_finished >= 1; });

    auto url = URL::Parser::basic_parse(url_string);
    if (!url.has_value()) {
        warnln("bad URL {}", url_string);
        return 2;
    }
    frames_since_navigation = 0;
    view->load(*url);
    loop.spin_until([&] { return loads_finished >= 2; });

    // A timeout, so a page that never presents can't hang the test.
    bool timed_out = false;
    auto timer = Core::Timer::create_single_shot(15000, [&] { timed_out = true; });
    timer->start();
    loop.spin_until([&] { return frames_since_navigation > 0 || timed_out; });
    // Let a couple more frames land (images, fonts).
    bool settle = false;
    auto settle_timer = Core::Timer::create_single_shot(1500, [&] { settle = true; });
    settle_timer->start();
    loop.spin_until([&] { return settle; });

    outln("frames_since_navigation={} usable={}", frames_since_navigation, view->has_usable_frame());
    auto bitmap = view->front_bitmap();
    if (!bitmap) {
        warnln("FAIL no frame (timed_out={})", timed_out);
        return 1;
    }
    outln("frame {}x{} dmabuf={}", bitmap->width(), bitmap->height(), view->front_is_dmabuf());

    auto encoded = TRY(Gfx::PNGWriter::encode(*bitmap));
    auto file = TRY(Core::File::open(out_path, Core::File::OpenMode::Write));
    TRY(file->write_until_depleted(encoded));
    outln("PASS wrote {}", out_path);

    view->close_child_web_views();
    return 0;
}
