use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use regex::bytes::Regex as BytesRegex;
use reqwest::blocking::Client;
use reqwest::header::{RANGE, COOKIE};
use scraper::{Html, Selector};

use crate::extract::extract_apkm;
use crate::util::{as_str, fetch_bytes, ARCHES, VENV_PYTHON};

pub fn stream_download_to(client: &Client, url: &str, part_path: &Path, log: &mut Vec<String>, cookies: Option<&str>) -> Result<(u64, bool), String> {
    let existing_sz = if part_path.exists() { fs::metadata(part_path).map(|m| m.len()).unwrap_or(0) } else { 0 };
    let mut req = client.get(url);
    if existing_sz > 0 { req = req.header(RANGE, format!("bytes={existing_sz}-")); }
    if let Some(ck) = cookies { req = req.header(COOKIE, ck); }
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
    match stream_download_to(client, url, part_path, log, None) {
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

pub fn dl_gplay(client: &Client, pkg: &str, tmp: &Path, arch: &str, _vu: Option<&str>, log: &mut Vec<String>) -> Result<(), String> {
    log.push("Trying Google Play...".into());
    let python = if Path::new(VENV_PYTHON).exists() { VENV_PYTHON } else { "python3" };
    let arch_flag = match arch {
        "arm64_v8a" => "arm64",
        "armeabi_v7a" => "armv7",
        _ => "arm64",
    };
    // gplaydl download_batch hangs on large splits — get URLs via its API, download via reqwest
    // Uses randomized device profiles from gplaydl's profiles directory to avoid fingerprinting
    let script = r#"
import json, sys, os, random
from gplaydl.auth import ensure_auth, fetch_token
from gplaydl.api import get_details, get_delivery, purchase, AuthExpiredError, PlayAPIError
from gplaydl.profiles import load_all_profiles, ARM64_PROFILES, ARMV7_PROFILES

pkg, arch_flag = sys.argv[1], sys.argv[2]

def get_auth():
    # Pick a random profile and try the dispenser directly
    pool = ARM64_PROFILES if arch_flag == "arm64" else ARMV7_PROFILES
    if pool:
        shuffled = list(pool)
        random.shuffle(shuffled)
        for name, profile in shuffled:
            try:
                import httpx
                resp = httpx.post(
                    "https://auroraoss.com/api/auth",
                    json=profile, headers={
                        "User-Agent": "com.aurora.store-4.6.1-70",
                        "Content-Type": "application/json",
                    }, timeout=30
                )
                if resp.status_code == 200:
                    data = resp.json()
                    if data.get("authToken"):
                        return data
            except httpx.RequestError:
                continue
            except json.JSONDecodeError:
                continue
    # Fallback: gplaydl's priority-ordered cache + dispenser
    a = ensure_auth(arch=arch_flag)
    if a is None:
        sys.exit(1)
    return a

def do():
    a = get_auth()
    det = get_details(pkg, a)
    vc = det.version_code
    purchase(pkg, vc, a)
    d = get_delivery(pkg, vc, a)
    out = {"version_code": vc, "title": det.title,
        "download_url": d.download_url, "download_size": d.download_size,
        "cookies": [{"name":c["name"],"value":c["value"]} for c in d.cookies],
        "splits": [{"name":s.name,"url":s.url,"size":s.size} for s in d.splits],
        "additional_files": [{"file_type":af.file_type,"version_code":af.version_code,
            "size":af.size,"url":af.url,"gzipped":af.gzipped,
            "cookies":af.cookies,"type_label":af.type_label,"extension":af.extension} for af in d.additional_files]}
    return out

try:
    out = do()
except AuthExpiredError:
    # Force fresh token on retry
    from gplaydl.auth import clear_auth
    clear_auth()
    try:
        out = do()
    except (AuthExpiredError, PlayAPIError) as e:
        print(f"ERROR:{e}",file=sys.stderr);sys.exit(1)
except PlayAPIError as e:
    print(f"ERROR:{e}",file=sys.stderr);sys.exit(1)
json.dump(out,sys.stdout)
"#;
    let out = Command::new(python)
        .arg("-c").arg(script)
        .arg("--").arg(pkg).arg(arch_flag)
        .output().map_err(|e| format!("python: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!("gplaydl API: {}", stderr.trim()));
    }
    let json_str = String::from_utf8_lossy(&out.stdout);
    let val: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| format!("parse delivery JSON: {e}"))?;
    let vc = val["version_code"].as_u64().unwrap_or(0);
    let title = val["title"].as_str().unwrap_or(pkg);
    log.push(format!("Google Play: {} (vc {})", title, vc));

    let dl_dir = tmp.parent().unwrap_or(tmp).join("gplay_splits");
    fs::create_dir_all(&dl_dir).ok();

    let cookie_hdr = {
        let cookies = val["cookies"].as_array();
        cookies.map_or(String::new(), |cs| {
            cs.iter().filter_map(|c| {
                Some(format!("{}={}", c["name"].as_str()?, c["value"].as_str()?))
            }).collect::<Vec<_>>().join("; ")
        })
    };

    let mut urls: Vec<(String, PathBuf)> = vec![];
    if let Some(base_url) = val["download_url"].as_str() {
        urls.push((base_url.to_string(), dl_dir.join(format!("{pkg}-{vc}.apk"))));
    }
    if let Some(splits) = val["splits"].as_array() {
        for s in splits {
            let name = s["name"].as_str().unwrap_or("");
            let size = s["size"].as_u64().unwrap_or(0);
            if size > 500_000_000 { log.push(format!("  {} ({:.0} MB)", name, size as f64/1e6)); }
            // Keep arch-specific config only if it matches; keep non-arch configs always
            if name.starts_with("config.") && !name.contains(arch) {
                let is_arch_split = ARCHES.iter().any(|a| name.contains(a));
                if is_arch_split { log.push(format!("Skipping {} (arch mismatch)", name)); continue; }
            }
            if let Some(url) = s["url"].as_str() {
                urls.push((url.to_string(), dl_dir.join(format!("{pkg}-{vc}-{name}.apk"))));
            }
        }
    }
    if urls.is_empty() { return Err("no download URLs from Google Play".into()); }

    log.push(format!("Downloading {} file(s) from Google Play...", urls.len()));
    for (i, (url, dest)) in urls.iter().enumerate() {
        log.push(format!("  [{}/{}] {}...", i+1, urls.len(), dest.file_name().unwrap_or_default().to_string_lossy()));
        let part = dest.with_extension("part");
        match stream_download_to(client, url, &part, log, if cookie_hdr.is_empty() { None } else { Some(&cookie_hdr) }) {
            Ok((sz, _)) if sz > 50_000 => {
                fs::rename(&part, dest).map_err(|e| format!("rename: {e}"))?;
                log.push(format!("  ✓ {:.1} MB", sz as f64 / 1_000_000.0));
            }
            Ok((sz, _)) => { fs::remove_file(&part).ok(); log.push(format!("  ✗ too small ({sz} bytes)")); }
            Err(e) => { fs::remove_file(&part).ok(); log.push(format!("  ✗ {e}")); }
        }
    }

    let base_path = dl_dir.join(format!("{pkg}-{vc}.apk"));
    if base_path.exists() && fs::metadata(&base_path).map(|m| m.len()).unwrap_or(0) > 50_000 {
        fs::copy(&base_path, tmp).map_err(|e| format!("gplay copy: {e}"))?;
        return Ok(());
    }
    let mut best: Option<PathBuf> = None;
    if let Ok(entries) = fs::read_dir(&dl_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("apk") { continue; }
            let sz = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if best.as_ref().map_or(true, |b| fs::metadata(b).map(|m| m.len()).unwrap_or(0) < sz) { best = Some(p); }
        }
    }
    match best {
        Some(src) => { fs::copy(&src, tmp).map_err(|e| format!("gplay copy: {e}"))?; Ok(()) }
        None => Err("no APK downloaded from Google Play".into()),
    }
}

pub fn dl_apkpure(client: &Client, pkg: &str, tmp: &Path, arch: &str, version_url: Option<&str>, log: &mut Vec<String>) -> Result<(), String> {
    log.push("Trying APKPure...".into());
    let part_path = tmp.with_extension("part");
    if let Some(vurl) = version_url { if vurl.contains("apkpure.com") {
        let html = fetch_bytes(client, vurl).map_err(|e| format!("version page: {e}"))?; let doc = Html::parse_document(&as_str(&html));
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
    if let Ok(html) = fetch_bytes(client, &url) { let doc = Html::parse_document(&as_str(&html)); let sel = Selector::parse("a[href*='download']").unwrap();
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
    let dl_paths: Vec<String> = re.captures_iter(&as_str(&html)).map(|c| c[1].to_string()).collect();
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
    let doc = Html::parse_document(&as_str(&html)); let sel = Selector::parse("a[href*='/apk/']").unwrap();
    let app_url = doc.select(&sel).find(|a| a.value().attr("href").map_or(false, |h| h.contains(&slug)));
    let app_url = match app_url { Some(a) => format!("https://www.apkmirror.com{}", a.value().attr("href").unwrap()), None => return Err("app not found".into()), };
    let html2 = fetch_bytes(client, &app_url).map_err(|e| format!("app page: {e}"))?;
    let re = regex::Regex::new(r#"href="(/apk/[^"]*download[^"]*)""#).unwrap();
    let dl_paths: Vec<String> = re.captures_iter(&as_str(&html2)).map(|c| c[1].to_string()).collect();
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
