//! `sig`, the Sighurt browser: reads the config, starts the session ([`Browser`]) and opens it in
//! a window drawn by `sighurt-ui`. Swapping the UI means changing this file.
//!
//! Sighurt is based on Oxide by Nikhil Ranjan (<https://github.com/niklabh/oxide>).

use std::path::Path;

use gpui::{px, size, App, AppContext, Application, TitlebarOptions, WindowBounds, WindowOptions};
use sighurt_core::browser::Browser;
use sighurt_core::config::{self, Config};
use sighurt_core::{commands, router};
use sighurt_ui::Root;

const USAGE: &str = "\
Usage: sig [OPTIONS] [URL|PATH ...]

Opens each URL or file in its own tab, or the home page when none is given.

Options:
  --default-config  Print the built-in configuration and exit
  -h, --help        Print this help and exit
  -V, --version     Print the version and exit
";

fn main() {
    tracing_subscriber::fmt::init();

    let mut inputs = Vec::new();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => return print!("{USAGE}"),
            "-V" | "--version" => {
                return println!(
                    "sig {} (Sighurt, based on Oxide by Nikhil Ranjan)",
                    env!("CARGO_PKG_VERSION")
                )
            }
            "--default-config" => return print!("{}", config::DEFAULT_CONFIG),
            option if option.starts_with('-') && option.len() > 1 => {
                eprint!("sig: unknown option {option}\n\n{USAGE}");
                std::process::exit(2);
            }
            _ => inputs.push(arg),
        }
    }

    let config = Config::load();
    let urls: Vec<String> = inputs
        .iter()
        .map(|input| {
            let path = Path::new(input);
            if path.exists() {
                router::file_url(path)
            } else {
                router::fixup(input, &config.search)
            }
        })
        .collect();

    Application::new().run(move |cx: &mut App| {
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        commands::install_keys(&config.keys, cx);
        let browser = cx.new(|cx| Browser::new(config, urls, cx));
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(1024.0), px(720.0)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("Sighurt".into()),
                ..Default::default()
            }),
            window_min_size: Some(size(px(600.0), px(400.0))),
            ..Default::default()
        };
        cx.open_window(options, |_, cx| cx.new(|cx| Root::new(browser, cx)))
            .expect("open window");
    });
}
