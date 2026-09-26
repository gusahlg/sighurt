//! # sighurt-ui — Sighurt's default chrome
//!
//! [`Root`] is the window's view. Top to bottom: the tab strip, the toolbar (back, forward,
//! reload or stop, and the URL bar), the content area and a one-line status bar. The content
//! area shows the active tab's engine page, or draws `sig://home` and error messages itself.
//!
//! Everything the chrome does is a command: buttons, the URL bar and key bindings all end in
//! [`RunCommand`]. The chrome moves focus for `location.focus` and hands every other command to
//! the [`Browser`]. Colors come from `[ui.colors]` in the config (see [`Colors`]).
//!
//! The tab strip, the toolbar and the status bar are views of their own, drawn from the
//! browser's state alone, and GPUI caches them, like the page: an engine's frame repaints its
//! page and none of the chrome, and the chrome changing repaints none of the page.

mod colors;
mod pages;
mod toolbar;

use gpui::prelude::*;
use gpui::{
    div, px, AnyView, App, Context, Div, Entity, FocusHandle, Focusable, MouseButton,
    MouseDownEvent, NavigationDirection, Pixels, SharedString, StyleRefinement, Window,
};
use sighurt_core::browser::{Browser, Shell};
use sighurt_core::commands::{self, RunCommand};
use sighurt_core::page::LoadState;

pub use colors::Colors;
use pages::EngineRow;
use toolbar::Toolbar;

/// The browser window's view.
pub struct Root {
    browser: Entity<Browser>,
    colors: Colors,
    /// What `sig://home` says about each configured engine; looked up once.
    engines: Vec<EngineRow>,
    /// The content area. Engine pages take focus inside it; shell pages focus it directly.
    content_focus: FocusHandle,
    tab_strip: Entity<Bar>,
    toolbar: Entity<Toolbar>,
    status_bar: Entity<Bar>,
}

impl Root {
    pub fn new(browser: Entity<Browser>, cx: &mut Context<Self>) -> Self {
        let config = browser.read(cx).config();
        let (colors, engines) = (Colors::new(&config.ui), pages::engine_rows(config));
        let content_focus = cx.focus_handle();
        Self {
            tab_strip: Bar::new(&browser, colors, toolbar::tab_strip, cx),
            toolbar: cx.new(|cx| Toolbar::new(browser.clone(), colors, content_focus.clone(), cx)),
            status_bar: Bar::new(&browser, colors, status_bar, cx),
            browser,
            colors,
            engines,
            content_focus,
        }
    }

    /// Runs `command`: `location.focus` here, everything else in the browser. Commands that
    /// change what the content area shows move focus there, except `tab.new`, which is for
    /// typing an address.
    fn run_command(&mut self, command: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (name, _) = commands::split(command);
        if name != "location.focus" {
            self.browser
                .update(cx, |browser, cx| browser.run_command(command, cx));
        }
        match name {
            "location.focus" | "tab.new" => self
                .toolbar
                .update(cx, |toolbar, cx| toolbar.focus_location(window, cx)),
            "tab.close" | "tab.next" | "tab.previous" | "tab.select" | "tab.last" | "page.open"
            | "page.back" | "page.forward" | "browser.home" => {
                self.toolbar
                    .update(cx, |toolbar, cx| toolbar.discard_edit(cx));
                window.focus(&self.content_focus);
            }
            _ => {}
        }
    }
}

impl Render for Root {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        rendered("root");
        let colors = self.colors;
        let tab = self.browser.read(cx).active_tab();

        let content = div()
            .id("sig-content")
            .key_context("page")
            .track_focus(&self.content_focus)
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col();
        let mut page_focus = None;
        let content = match (tab.shell(), tab.page(), tab.info().map(|info| &info.load)) {
            (Some(Shell::Home), ..) => content.child(pages::home(&self.engines, &colors)),
            (Some(Shell::Error { url, message }), ..) => {
                content.child(pages::error(url, message, &colors))
            }
            (None, _, Some(LoadState::Error(message))) => {
                content.child(pages::error(tab.url(), message, &colors))
            }
            (None, Some(page), _) => {
                page_focus = Some(page.focus_handle(cx));
                // Clipped, so a page can't paint over the chrome around it.
                let view =
                    AnyView::from(page.clone()).cached(StyleRefinement::default().size_full());
                content.child(div().flex_1().min_h_0().overflow_hidden().child(view))
            }
            (None, None, _) => content,
        };
        // Focus in the content area follows what it shows, and so does focus that went nowhere
        // (at startup, or with a page that was closed).
        let target = page_focus.unwrap_or_else(|| self.content_focus.clone());
        if !target.is_focused(window)
            && (window.focused(cx).is_none() || self.content_focus.contains_focused(window, cx))
        {
            window.focus(&target);
        }

        div()
            .id("sig-root")
            .size_full()
            .flex()
            .flex_col()
            .bg(colors.bg)
            .text_color(colors.text)
            .on_action(cx.listener(|this, action: &RunCommand, window, cx| {
                this.run_command(&action.0, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Back),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.run_command("page.back", window, cx)
                }),
            )
            .on_mouse_down(
                MouseButton::Navigate(NavigationDirection::Forward),
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.run_command("page.forward", window, cx)
                }),
            )
            .child(strip(&self.tab_strip, px(40.0)))
            .child(strip(&self.toolbar, px(44.0)))
            .child(content)
            .child(strip(&self.status_bar, px(20.0)))
    }
}

/// `view` as a strip `height` high across the window. GPUI caches it: it repaints only when
/// the view itself changed, not on every redraw of the window.
fn strip<V: Render>(view: &Entity<V>, height: Pixels) -> AnyView {
    AnyView::from(view.clone()).cached(StyleRefinement::default().h(height).flex_shrink_0())
}

/// A piece of the chrome drawn from the browser's state alone: the tab strip or the status bar.
/// It redraws when the browser changes, and only then.
struct Bar {
    browser: Entity<Browser>,
    colors: Colors,
    draw: fn(&Browser, &Colors) -> Div,
}

impl Bar {
    fn new(
        browser: &Entity<Browser>,
        colors: Colors,
        draw: fn(&Browser, &Colors) -> Div,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            cx.observe(browser, |_, _, cx| cx.notify()).detach();
            Self {
                browser: browser.clone(),
                colors,
                draw,
            }
        })
    }
}

impl Render for Bar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.draw)(self.browser.read(cx), &self.colors)
    }
}

/// What the page says about the link under the pointer, or else the latest word from a download.
fn status_bar(browser: &Browser, colors: &Colors) -> Div {
    rendered("status");
    let status = browser
        .active_tab()
        .info()
        .and_then(|info| info.status.as_deref())
        .or(browser.status())
        .map(|status| SharedString::from(status.to_string()));
    div()
        .size_full()
        .px_2()
        .border_t_1()
        .border_color(colors.border)
        .text_xs()
        .text_color(colors.text_muted)
        .truncate()
        .children(status)
}

/// Notes that a view rendered, for the tests that check what redraws when.
fn rendered(_view: &'static str) {
    #[cfg(test)]
    tests::RENDERS.with_borrow_mut(|renders| renders.push(_view));
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{Modifiers, TestAppContext, VisualTestContext};
    use sighurt_core::config::Config;

    use super::*;

    thread_local! {
        /// Views rendered on this thread, in order.
        pub(super) static RENDERS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    /// The views rendered since the last call, each named once.
    fn renders() -> Vec<&'static str> {
        let mut renders = RENDERS.with_borrow_mut(std::mem::take);
        renders.sort_unstable();
        renders.dedup();
        renders
    }

    /// A window on a browser with a tab for each of `urls`, and the views it rendered.
    fn open(
        urls: Vec<String>,
        config: Config,
        cx: &mut TestAppContext,
    ) -> (Entity<Root>, Entity<Browser>, &mut VisualTestContext) {
        let browser = cx.new(|cx| Browser::new(config, urls, cx));
        let (root, cx) = cx.add_window_view(|_, cx| Root::new(browser.clone(), cx));
        assert_eq!(renders(), ["root", "status", "tabs", "toolbar"]);
        (root, browser, cx)
    }

    /// Two tabs of an engine that starts and then says nothing, so both pages stay up, loading,
    /// until the test ends. An engine's frame is a `notify` on its page.
    #[cfg(unix)]
    #[test]
    fn frames_repaint_only_their_page() {
        // The shell keeps the engine's stdout open until its stdin closes.
        let config = Config::parse(Some(
            "[engines.quiet]\ncommand = ['sh', '-c', 'cat > /dev/null; true']\nextensions = ['quiet']",
        ))
        .unwrap();
        let urls = vec!["file:///a.quiet".into(), "file:///b.quiet".into()];
        let mut cx = TestAppContext::single();
        let (_, browser, cx) = open(urls, config, &mut cx);
        let page = |i: usize, cx: &mut VisualTestContext| {
            browser.read_with(cx, |browser, _| browser.tabs()[i].page().unwrap().clone())
        };
        let (front, back) = (page(0, cx), page(1, cx));

        // A tab in the background doesn't redraw the window at all.
        back.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(renders(), Vec::<&str>::new());
        // The page in front redraws inside a root view that recycles the chrome.
        front.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(renders(), ["root"]);
        // The chrome redraws when what it shows changes.
        browser.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(renders(), ["root", "status", "tabs", "toolbar"]);
        // Typing redraws the toolbar alone.
        cx.dispatch_action(RunCommand("location.focus".into()));
        renders();
        cx.simulate_keystrokes("x");
        assert_eq!(renders(), ["root", "toolbar"]);
    }

    #[test]
    fn chrome_controls_run_commands() {
        let mut cx = TestAppContext::single();
        let (root, browser, cx) = open(Vec::new(), Config::parse(None).unwrap(), &mut cx);
        let new_tab = cx
            .debug_bounds("sig-new-tab")
            .expect("the tab strip was drawn");
        // Buttons work in a tab strip recycled from an earlier frame.
        root.update(cx, |_, cx| cx.notify());
        cx.run_until_parked();
        assert_eq!(renders(), ["root"]);
        cx.simulate_click(new_tab.center(), Modifiers::none());
        browser.read_with(cx, |browser, _| {
            assert_eq!((browser.tabs().len(), browser.active()), (2, 1));
            assert_eq!(browser.active_tab().url(), "sig://home");
        });
        // The new tab's URL bar has focus and its text selected: typing replaces it.
        cx.simulate_keystrokes("s i g : n o p e enter");
        let tab = browser.read_with(cx, |browser, _| browser.active_tab().title());
        assert_eq!(tab, "Can't open page");
    }
}
