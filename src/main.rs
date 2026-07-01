mod config;
mod download;
mod extract;
mod http;
mod search;
mod tui;
mod update;
mod util;

use std::path::{Path, PathBuf};

use clap::{Arg, ArgAction, Command as ClapCmd};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::blocking::Client;

use crate::config::load_config;
use crate::download::{dl_apkpure, dl_apkmirror, dl_gplay};
use crate::http::build_http;
use crate::search::search_play;
use crate::update::self_update;
use crate::util::TEMP_PREFIX;

fn main() {
    let cfg = load_config();
    let matches = ClapCmd::new("apkdl")
        .about("APK downloader — Google Play · APKMirror · APKPure")
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

    let timeout = cfg.timeout_secs.unwrap_or(7200);
    let client = build_http(timeout).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1); });
    let queries: Vec<String> = matches.get_many("query").unwrap_or_default().cloned().collect();

    if matches.get_flag("no-tui") || !queries.is_empty() {
        if let Err(e) = run_cli(&client, &cfg, &queries) {
            eprintln!("✗ {e}");
            std::process::exit(1);
        }
        return;
    }

    let app = crate::tui::App::new(client, cfg);
    if let Err(e) = crate::tui::run_tui(app) {
        eprintln!("TUI error: {e}");
        std::process::exit(1);
    }
}

fn run_cli(client: &Client, cfg: &config::Config, args: &[String]) -> Result<(), String> {
    let (query, out_name) = if args.is_empty() {
        eprint!("Search: ");
        let mut q = String::new();
        std::io::stdin().read_line(&mut q).map_err(|e| format!("read: {e}"))?;
        let q = q.trim().to_string();
        if q.is_empty() { return Err("no query".into()); }
        (q.clone(), format!("{}.apk", q.to_lowercase().replace(' ', "_")))
    } else if args.len() == 1 {
        let q = args[0].clone();
        (q.clone(), format!("{}.apk", q.to_lowercase().replace(' ', "_")))
    } else {
        let q = args[0..args.len()-1].join(" ");
        (q, args[args.len()-1].clone())
    };

    // Resolve query to a package name
    let pkg = if query.contains('.') {
        query.clone()
    } else {
        let results = search_play(client, &query);
        if results.is_empty() { return Err(format!("No results for \"{query}\"")); }
        for (i, (t, p)) in results.iter().enumerate().take(10) {
            // search_play tuples are inconsistent — detect which is the package name
            let (display_title, pkg_name) = if p.contains('.') { (t, p) } else { (p, t) };
            println!("{}. {} ({})", i + 1, display_title, pkg_name);
        }
        let pick = |idx: usize| -> String {
            let (t, p) = &results[idx.min(results.len() - 1)];
            if p.contains('.') { p.clone() } else if t.contains('.') { t.clone() } else { p.clone() }
        };
        if results.len() > 1 {
            print!("Choice [1-{}, default=1]: ", results.len().min(10));
            std::io::Write::flush(&mut std::io::stdout()).map_err(|e| format!("flush: {e}"))?;
            let mut choice = String::new();
            std::io::stdin().read_line(&mut choice).map_err(|e| format!("read: {e}"))?;
            let idx = choice.trim().parse::<usize>().unwrap_or(1).saturating_sub(1);
            pick(idx)
        } else {
            pick(0)
        }
    };
    println!("Package: {pkg}");

    // Set up temp workspace
    let _tmp_guard = tempfile::TempDir::new().ok();
    let tmp_root = _tmp_guard.as_ref().map(|t| t.path().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join(TEMP_PREFIX));
    std::fs::create_dir_all(&tmp_root).ok();
    let tmp = tmp_root.join(format!("{}_download_tmp", pkg.replace('.', "_")));

    let sources: [(&str, fn(&Client, &str, &Path, &str, Option<&str>, &mut Vec<String>) -> Result<(), String>); 3] = [
        ("Google Play", dl_gplay),
        ("APKMirror", dl_apkmirror),
        ("APKPure", dl_apkpure),
    ];

    let arch = cfg.default_arch.as_deref().unwrap_or("arm64_v8a");
    let mut log = Vec::new();
    let mut last_err = String::from("no source tried");

    for (name, func) in &sources {
        print!("  {name}...");
        std::io::Write::flush(&mut std::io::stdout()).map_err(|e| format!("flush: {e}"))?;
        let sp = ProgressBar::new_spinner();
        sp.set_style(ProgressStyle::default_spinner());

        let src_tmp = tmp_root.join(format!("{}_tmp", name.replace(' ', "_").to_lowercase()));
        match func(client, &pkg, &src_tmp, arch, None, &mut log) {
            Ok(()) => {
                sp.finish_and_clear();
                if src_tmp.exists() {
                    std::fs::copy(&src_tmp, &tmp).map_err(|e| format!("copy: {e}"))?;
                }
                println!(" ✓");
                last_err.clear();
                break;
            }
            Err(e) => {
                sp.finish_and_clear();
                println!(" ✗");
                last_err = e;
            }
        }
    }

    if !last_err.is_empty() {
        return Err(last_err);
    }

    // Copy to output
    let out_path = PathBuf::from(&out_name);
    if let Some(p) = out_path.parent() { let _ = std::fs::create_dir_all(p); }
    std::fs::copy(&tmp, &out_path).map_err(|e| format!("write {out_name}: {e}"))?;
    let sz = std::fs::metadata(&out_path).map(|m| m.len()).unwrap_or(0);
    println!("✓ {out_name} ({:.1} MB)", sz as f64 / 1_000_000.0);
    Ok(())
}
