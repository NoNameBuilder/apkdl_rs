//! Unified APK downloader — TUI edition.

mod config;
mod download;
mod extract;
mod http;
mod search;
mod tui;
mod update;
mod util;

use clap::{Arg, ArgAction, Command as ClapCmd};

use crate::config::load_config;
use crate::http::build_http;
use crate::tui::{App, run_tui};
use crate::update::self_update;

fn main() {
    let cfg = load_config();
    let matches = ClapCmd::new("apkdl")
        .about("APK downloader — Google Play · APKPure · APKMirror")
        .version("3.0")
        .arg(Arg::new("query").help("App name or package").num_args(1..).trailing_var_arg(true))
        .arg(Arg::new("install").long("install").help("Install apkdl binary to system PATH").action(ArgAction::SetTrue))
        .arg(Arg::new("update").long("update").help("Self-update: rebuild & reinstall apkdl").action(ArgAction::SetTrue))
        .arg(Arg::new("no-tui").long("no-tui").help("Run in plain CLI mode (no TUI)").action(ArgAction::SetTrue))
        .get_matches();

    if matches.get_flag("install") || matches.get_flag("update") {
        if matches.get_flag("update") {
            match self_update() {
                Ok(()) => println!("✓ Updated & installed"),
                Err(e) => eprintln!("✗ {e}"),
            }
        } else {
            match self_update() {
                Ok(()) => println!("✓ Installed"),
                Err(e) => eprintln!("✗ {e}"),
            }
        }
        return;
    }

    let timeout = cfg.timeout_secs.unwrap_or(600);
    let client = build_http(timeout);

    if matches.get_flag("no-tui") {
        let _queries: Vec<String> = matches.get_many("query").unwrap_or_default().cloned().collect();
        if _queries.is_empty() {
            eprintln!("No query provided.");
            std::process::exit(1);
        }
        return;
    }

    let app = App::new(client, cfg);
    if let Err(e) = run_tui(app) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}
