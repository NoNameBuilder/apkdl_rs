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
use crate::search::{search_play, list_versions_apkmirror, list_versions_apkpure};
use crate::update::self_update;
use crate::util::TEMP_PREFIX;

fn main() {
    let cfg = load_config();
    let matches = ClapCmd::new("apkdl")
        .about("APK downloader — Google Play · APKMirror · APKPure")
        .version("3.1")
        .arg(Arg::new("apps").help("App name(s) or package(s)").num_args(0..).trailing_var_arg(true))
        .arg(Arg::new("install").long("install").help("Install apkdl binary to system PATH").action(ArgAction::SetTrue))
        .arg(Arg::new("update").long("update").help("Self-update: rebuild & reinstall apkdl").action(ArgAction::SetTrue))
        .arg(Arg::new("output").short('o').long("output").help("Output file or directory").num_args(1))
        .arg(Arg::new("arch").long("arch").help("Architecture filter").num_args(1))
        .arg(Arg::new("source").long("source").help("Force source: gplay, apkmirror, apkpure").num_args(1))
        .arg(Arg::new("list").short('l').long("list").help("List available versions").action(ArgAction::SetTrue))
        .arg(Arg::new("no-tui").long("no-tui").help("Run CLI mode").action(ArgAction::SetTrue))
        .get_matches();

    if matches.get_flag("install") || matches.get_flag("update") {
        match self_update() {
            Ok(()) => println!("✓ Installed"),
            Err(e) => eprintln!("✗ {e}"),
        }
        return;
    }

    let timeout = cfg.timeout_secs.unwrap_or(7200);
    let client = build_http(timeout).unwrap_or_else(|e| { eprintln!("{e}"); std::process::exit(1); });
    let apps: Vec<String> = matches.get_many("apps").unwrap_or_default().cloned().collect();

    if matches.get_flag("no-tui") || !apps.is_empty() {
        if let Err(e) = run_cli(&client, &matches) {
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

fn run_cli(client: &Client, matches: &clap::ArgMatches) -> Result<(), String> {
    let apps: Vec<String> = matches.get_many("apps").unwrap_or_default().cloned().collect();
    let out_spec = matches.get_one::<String>("output").cloned();
    let arch = matches.get_one::<String>("arch").cloned()
        .or_else(|| std::env::var("APKDL_ARCH").ok())
        .unwrap_or_else(|| "arm64_v8a".into());
    let force_source = matches.get_one::<String>("source").and_then(|s| match s.as_str() {
        "gplay" => Some(0usize),
        "apkmirror" => Some(1),
        "apkpure" => Some(2),
        _ => None,
    });
    let list_only = matches.get_flag("list");

    let sources: [(&str, fn(&Client, &str, &Path, &str, Option<&str>, &mut Vec<String>) -> Result<(), String>); 3] = [
        ("Google Play", dl_gplay),
        ("APKMirror", dl_apkmirror),
        ("APKPure", dl_apkpure),
    ];

    let queries = if apps.is_empty() {
        eprint!("Search: ");
        let mut q = String::new();
        std::io::stdin().read_line(&mut q).map_err(|e| format!("read: {e}"))?;
        let q = q.trim().to_string();
        if q.is_empty() { return Err("no query".into()); }
        vec![q]
    } else {
        apps
    };

    for query in &queries {
        let pkg = resolve_pkg(client, query)?;

        if list_only {
            list_versions(client, &pkg, arch.as_str())?;
            continue;
        }

        println!("\n▶ {pkg}");
        download_app(client, &pkg, out_spec.as_deref(), &arch, force_source, &sources)?;
    }
    Ok(())
}

fn resolve_pkg(client: &Client, query: &str) -> Result<String, String> {
    if query.contains('.') {
        return Ok(query.to_string());
    }
    let results = search_play(client, query);
    if results.is_empty() { return Err(format!("No results for \"{query}\"")); }
    for (i, (t, p)) in results.iter().enumerate().take(10) {
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
        Ok(pick(idx))
    } else {
        Ok(pick(0))
    }
}

fn list_versions(client: &Client, pkg: &str, _arch: &str) -> Result<(), String> {
    println!("  Versions:");
    if let Ok(versions) = list_versions_apkmirror(client, pkg) {
        for v in versions.iter().take(5) {
            println!("    [Mirror] {}", v.label);
        }
    }
    if let Ok(versions) = list_versions_apkpure(client, pkg) {
        for v in versions.iter().take(5) {
            println!("    [Pure] {}", v.label);
        }
    }
    Ok(())
}

fn download_app(
    client: &Client,
    pkg: &str,
    out_spec: Option<&str>,
    arch: &str,
    force_source: Option<usize>,
    sources: &[(&str, fn(&Client, &str, &Path, &str, Option<&str>, &mut Vec<String>) -> Result<(), String>); 3],
) -> Result<(), String> {
    let out_name = out_spec.map(|s| {
        let p = Path::new(s);
        if p.is_dir() || s.ends_with('/') {
            PathBuf::from(s).join(format!("{}.apk", pkg.split('.').last().unwrap_or(pkg)))
        } else {
            p.to_path_buf()
        }
    }).unwrap_or_else(|| PathBuf::from(format!("{}.apk", pkg.split('.').last().unwrap_or(pkg))));

    let _tmp_guard = tempfile::TempDir::new().ok();
    let tmp_root = _tmp_guard.as_ref().map(|t| t.path().to_path_buf())
        .unwrap_or_else(|| std::env::temp_dir().join(TEMP_PREFIX));
    std::fs::create_dir_all(&tmp_root).ok();
    let tmp = tmp_root.join(format!("{}_download_tmp", pkg.replace('.', "_")));

    let mut last_err = String::from("no source tried");

    if let Some(idx) = force_source {
        let (name, func) = &sources[idx];
        print!("  {name}...");
        std::io::Write::flush(&mut std::io::stdout()).unwrap();
        let sp = ProgressBar::new_spinner();
        sp.set_style(ProgressStyle::default_spinner());
        let src_tmp = tmp_root.join(format!("{}_tmp", name.replace(' ', "_").to_lowercase()));
        match func(client, pkg, &src_tmp, arch, None, &mut Vec::new()) {
            Ok(()) => { sp.finish_and_clear(); println!(" ✓"); last_err.clear(); if src_tmp.is_dir() { let _ = std::fs::remove_dir_all(&tmp); let _ = crate::extract::cp_dir(&src_tmp, &tmp); } else if src_tmp.exists() { std::fs::copy(&src_tmp, &tmp).ok(); } }
            Err(e) => { sp.finish_and_clear(); println!(" ✗ {e}"); last_err = e; }
        }
    } else {
        for (name, func) in sources {
            print!("  {name}...");
            std::io::Write::flush(&mut std::io::stdout()).unwrap();
            let sp = ProgressBar::new_spinner();
            sp.set_style(ProgressStyle::default_spinner());
            let src_tmp = tmp_root.join(format!("{}_tmp", name.replace(' ', "_").to_lowercase()));
            match func(client, pkg, &src_tmp, arch, None, &mut Vec::new()) {
                Ok(()) => { sp.finish_and_clear(); println!(" ✓"); last_err.clear(); if src_tmp.is_dir() { let _ = std::fs::remove_dir_all(&tmp); let _ = crate::extract::cp_dir(&src_tmp, &tmp); } else if src_tmp.exists() { std::fs::copy(&src_tmp, &tmp).ok(); } break; }
                Err(e) => { sp.finish_and_clear(); println!(" ✗ {e}"); last_err = e; }
            }
        }
    }

    if !last_err.is_empty() { return Err(last_err); }

    if let Some(p) = out_name.parent() { let _ = std::fs::create_dir_all(p); }
    if tmp.is_dir() {
        // Output is a directory of split APKs (merge failed/skipped)
        let _ = std::fs::remove_dir_all(&out_name);
        crate::extract::cp_dir(&tmp, &out_name).ok();
        println!("  ✓ {} ({} files)", out_name.display(), std::fs::read_dir(&out_name).map(|e| e.count()).unwrap_or(0));
    } else {
        std::fs::copy(&tmp, &out_name).map_err(|e| format!("write {}: {e}", out_name.display()))?;
        let sz = std::fs::metadata(&out_name).map(|m| m.len()).unwrap_or(0);
        println!("  ✓ {} ({:.1} MB)", out_name.display(), sz as f64 / 1_000_000.0);
    }
    Ok(())
}
