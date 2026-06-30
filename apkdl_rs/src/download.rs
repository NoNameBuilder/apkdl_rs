use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::bytes::Regex as BytesRegex;
use reqwest::blocking::Client;
use reqwest::header::RANGE;
use scraper::{Html, Selector};

use crate::extract::extract_apkm;
use crate::util::{as_str, fetch_bytes, TEMP_PREFIX, VENV_PYTHON};

pub fn stream_download_to(client: &Client, url: &str, part_path: &Path, log: &mut Vec<String>) -> Result<(u64, bool), String> {
    let existing_sz = if part_path.exists() { fs::metadata(part_path).map(|m| m.len()).unwrap_or(0) } else { 0 };
    let mut req = client.get(url);
    if existing_sz > 0 { req = req.header(RANGE, format!("bytes={existing_sz}-")); }
    let resp = req.send().map_err(|e| format!("HTTP: {e}"))?;
    let status = resp.status();
    let (file, resumed) = if status.as_u16() == 206 && existing_sz > 0 {
        (OpenOptions::new().append(true).open(part_path).map_err(|e| format!("append: {e}"))?, true)
    } else {
        (File::create(part_path).map_err(|e| format!("create: {e}"))?, false)
    };
    if resumed { log.push(format!("Resuming from {:.1} MB...", existing_sz as f64 / 1_000_000.0)); }
    let mut writer = BufWriter::new(file);
    let mut reader = BufReader::new(resp);
    let mut buf = [0u8; 65536];
    let mut written: u64 = if resumed { existing_sz } else { 0 };
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 { break; }
        writer.write_all(&buf[..n]).map_err(|e| format!("write: {e}"))?;
        written += n as u64;
    }
    Ok((written, resumed))
}

pub fn try_download_apk(client: &Client, url: &str, tmp: &Path, arch: &str, part_path: &Path, log: &mut Vec<String>) -> Result<(), String> {
    match stream_download_to(client, url, part_path, log) {
        Ok((sz, resumed)) if sz > 50_000 => {
            log.push(format!("Downloaded {:.1} MB{}", sz as f64 / 1_000_000.0, if resumed { " (resumed)" } else { "" }));
            if url.contains(".apkm") || url.contains(".xapk") {
                let ext = if url.contains(".apkm") { "apkm" } else { "xapk" };
                let fname = tmp.with_extension(ext);
                fs::copy(part_path, &fname).map_err(|e| format!("copy: {e}"))?; fs::remove_file(part_path).ok();
                return extract_apkm(&fname, tmp, arch, log);
            } else { fs::rename(part_path, tmp).map_err(|e| format!("rename: {e}"))?; return Ok(()); }
        }
        Ok((sz, _)) => { fs::remove_file(part_path).ok(); log.push(format!("Too small: {sz} bytes")); Err("too small".into()) }
        Err(e) => { fs::remove_file(part_path).ok(); Err(e) }
    }
}

pub fn dl_gplay(client: &Client, pkg: &str, tmp: &Path, _arch: &str, _vu: Option<&str>, log: &mut Vec<String>) -> Result<(), String> {
    let _ = client; let python = if Path::new(VENV_PYTHON).exists() { VENV_PYTHON } else { "python3" };
    let gplay_dir = std::env::temp_dir().join(TEMP_PREFIX).join("gplay"); fs::create_dir_all(&gplay_dir).ok();
    log.push("Trying Google Play...".into());
    let ok = Command::new(python).args(["-m", "gplaydl", "download", pkg, "-o", &gplay_dir.to_string_lossy()]).status().map(|s| s.success()).unwrap_or(false);
    if !ok { return Err("gplaydl command failed".into()); }
    let mut best: Option<PathBuf> = None;
    if let Ok(entries) = fs::read_dir(&gplay_dir) {
        for e in entries.flatten() { let p = e.path(); let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".apk") && !name.contains("split") && !name.contains("asset") {
                let sz = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                if best.as_ref().map_or(true, |b| fs::metadata(b).map(|m| m.len()).unwrap_or(0) < sz) { best = Some(p); }
            }
        }
    }
    match best { Some(src) => { fs::copy(&src, tmp).map_err(|e| format!("gplay copy: {e}"))?; Ok(()) } None => Err("no APK found".into()) }
}

pub fn dl_apkpure(client: &Client, pkg: &str, tmp: &Path, arch: &str, version_url: Option<&str>, log: &mut Vec<String>) -> Result<(), String> {
    log.push("Trying APKPure...".into());
    let part_path = tmp.with_extension("part");
    if let Some(vurl) = version_url { if vurl.contains("apkpure.com") {
        let html = fetch_bytes(client, vurl).map_err(|e| format!("version page: {e}"))?; let doc = Html::parse_document(as_str(&html));
        let sel = Selector::parse("a[href*='download']").unwrap();
        for a in doc.select(&sel) { let href = match a.value().attr("href") { Some(h) => h, None => continue }; let dl_url = if href.starts_with("http") { href.to_string() } else { format!("https://apkpure.com{href}") };
            if dl_url.contains(".apk") || dl_url.contains(".xapk") { if try_download_apk(client, &dl_url, tmp, arch, &part_path, log).is_ok() { return Ok(()); } }
        }
    }}
    for fmt in &["XAPK", "APK"] {
        let url = format!("https://d.apkpure.net/b/{fmt}/{pkg}?version=latest");
        if try_download_apk(client, &url, tmp, arch, &part_path, log).is_ok() { return Ok(()); }
    }
    let slug = pkg.replace('.', "-"); let url = format!("https://apkpure.com/{slug}/download");
    if let Ok(html) = fetch_bytes(client, &url) { let doc = Html::parse_document(as_str(&html)); let sel = Selector::parse("a[href*='download']").unwrap();
        for a in doc.select(&sel) { let href = match a.value().attr("href") { Some(h) => h, None => continue }; let dl_url = if href.starts_with("http") { href.to_string() } else { format!("https://apkpure.com{href}") };
            if dl_url.contains(".apk") || dl_url.contains(".xapk") { let part2 = tmp.with_extension("part2"); if try_download_apk(client, &dl_url, tmp, arch, &part2, log).is_ok() { return Ok(()); } }
        }
    }
    Err("APKPure: all attempts failed".into())
}

pub fn dl_apkmirror(client: &Client, pkg: &str, tmp: &Path, arch: &str, version_url: Option<&str>, log: &mut Vec<String>) -> Result<(), String> {
    log.push("Trying APKMirror...".into());
    let part_path = tmp.with_extension("part");
    let app_url = if let Some(vurl) = version_url { if vurl.contains("apkmirror.com") { vurl.to_string() } else { return dl_apkmirror_default(client, pkg, tmp, arch, &part_path, log); } }
    else { return dl_apkmirror_default(client, pkg, tmp, arch, &part_path, log); };
    let html = fetch_bytes(client, &app_url).map_err(|e| format!("version: {e}"))?;
    let re = regex::Regex::new(r#"href="(/apk/[^"]*download[^"]*)""#).unwrap();
    let dl_paths: Vec<String> = re.captures_iter(as_str(&html)).map(|c| c[1].to_string()).collect();
    for dp in &dl_paths[..3] { let dl_page = format!("https://www.apkmirror.com{dp}");
        if let Ok(html3) = fetch_bytes(client, &dl_page) {
            let file_re = BytesRegex::new(r#""(https?://[^"]*\.(?:apk|apkm|xapk))""#).unwrap();
            for cap in file_re.captures_iter(&html3) { let url = std::str::from_utf8(&cap[1]).unwrap().to_string();
                if try_download_apk(client, &url, tmp, arch, &part_path, log).is_ok() { return Ok(()); }
            }
        }
    }
    Err("APKMirror: all attempts failed".into())
}

fn dl_apkmirror_default(client: &Client, pkg: &str, tmp: &Path, arch: &str, part_path: &Path, log: &mut Vec<String>) -> Result<(), String> {
    let slug = pkg.replace('.', "-");
    let html = fetch_bytes(client, &format!("https://www.apkmirror.com/?s={}", pkg.replace('.', "+"))).map_err(|e| format!("search: {e}"))?;
    let doc = Html::parse_document(as_str(&html)); let sel = Selector::parse("a[href*='/apk/']").unwrap();
    let app_url = doc.select(&sel).find(|a| a.value().attr("href").map_or(false, |h| h.contains(&slug)));
    let app_url = match app_url { Some(a) => format!("https://www.apkmirror.com{}", a.value().attr("href").unwrap()), None => return Err("app not found".into()), };
    let html2 = fetch_bytes(client, &app_url).map_err(|e| format!("app page: {e}"))?;
    let re = regex::Regex::new(r#"href="(/apk/[^"]*download[^"]*)""#).unwrap();
    let dl_paths: Vec<String> = re.captures_iter(as_str(&html2)).map(|c| c[1].to_string()).collect();
    for dp in &dl_paths[..3] { let dl_page = format!("https://www.apkmirror.com{dp}");
        if let Ok(html3) = fetch_bytes(client, &dl_page) {
            let file_re = BytesRegex::new(r#""(https?://[^"]*\.(?:apk|apkm|xapk))""#).unwrap();
            for cap in file_re.captures_iter(&html3) { let url = std::str::from_utf8(&cap[1]).unwrap().to_string();
                if try_download_apk(client, &url, tmp, arch, part_path, log).is_ok() { return Ok(()); }
            }
        }
    }
    Err("APKMirror: all attempts failed".into())
}
