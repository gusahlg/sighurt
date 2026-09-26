//! `sig`, the Sighurt browser executable.
//!
//! Wires the pieces together: the core's state and policy, the Wayland host, the chrome, and the
//! engines selected at build time by Cargo features. It owns process-wide setup (logging, the
//! TLS provider, panic handling), the owner event loop, and every effect with I/O.
//!
//! Sighurt is based on Oxide by Nikhil Ranjan (<https://github.com/niklabh/oxide>).

mod app;
mod diagnostics;
mod effects;
mod persistence;
mod registry;
mod services;

use std::process::ExitCode;

const USAGE: &str = "\
Usage: sig [OPTIONS] [URL|PATH ...]

Opens each URL or file in its own tab, or the home page when none is given.

Options:
  -h, --help     Print this help and exit
  -V, --version  Print the version and exit
";

fn main() -> ExitCode {
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!(
                    "sig {} (Sighurt, based on Oxide by Nikhil Ranjan)",
                    env!("CARGO_PKG_VERSION")
                );
                return ExitCode::SUCCESS;
            }
            option if option.starts_with('-') && option.len() > 1 => {
                eprint!("sig: unknown option {option}\n\n{USAGE}");
                return ExitCode::from(2);
            }
            _ => {}
        }
    }
    eprintln!(
        "sig: the rewrite is in progress and this build cannot show pages yet \
         (see docs/rewrite/STATUS.md)"
    );
    ExitCode::FAILURE
}
