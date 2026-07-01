use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use zip::ZipArchive;

use crate::util::{run_status, ARCHES};

pub fn extract_apkm(path: &Path, out: &Path, arch_filter: &str, log: &mut Vec<String>) -> Result<(), String> {
    let tmp = TempDir::new().map_err(|e| format!("tmpdir: {e}"))?;
    let dir = tmp.path();
    let file = File::open(path).map_err(|e| format!("open: {e}"))?;
    let mut zip = ZipArchive::new(file).map_err(|e| format!("zip: {e}"))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("entry {i}: {e}"))?;
        let name = entry.name().to_string();
        let out_path = dir.join(&name);
        if entry.is_dir() { fs::create_dir_all(&out_path).ok(); continue; }
        if let Some(p) = out_path.parent() { fs::create_dir_all(p).ok(); }
        let mut f = File::create(&out_path).map_err(|e| format!("create {name}: {e}"))?;
        io::copy(&mut entry, &mut f).map_err(|e| format!("extract {name}: {e}"))?;
    }
    merge_apk_dir(dir, out, arch_filter, log)
}

pub fn merge_apk_dir(dir: &Path, out: &Path, arch_filter: &str, log: &mut Vec<String>) -> Result<(), String> {
    let entries: Vec<_> = fs::read_dir(dir).map_err(|e| format!("read: {e}"))?.filter_map(|e| e.ok()).collect();
    let mut base: Option<PathBuf> = None;
    let mut splits: Vec<PathBuf> = vec![];
    fn apk_arch(name: &str) -> Option<&'static str> {
        for &a in ARCHES { if name.contains(a) { return Some(a); } } None
    }
    for e in &entries {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("apk") { continue; }
        let name = e.file_name().to_string_lossy().to_string();
        if name == "base.apk" { base = Some(p); }
        else {
            if let Some(a) = apk_arch(&name) { if a != arch_filter { continue; } }
            splits.push(p);
        }
    }
    if base.is_none() {
        let apks: Vec<_> = entries.iter().filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("apk")).collect();
        if let Some(first) = apks.first() { base = Some(first.path()); }
        splits = apks.iter().skip(1).map(|e| e.path()).collect();
    }
    let base = base.ok_or_else(|| "no APK found in bundle".to_string())?;
    if splits.is_empty() { fs::copy(&base, out).map_err(|e| format!("copy: {e}"))?; return Ok(()); }

    log.push(format!("Merging {} split(s)...", splits.len()));
    let work = match TempDir::new() {
        Ok(t) => t,
        Err(_) => { fs::copy(&base, out).map_err(|e| format!("copy: {e}"))?; return Ok(()); }
    };
    let base_dir = work.path().join("base");
    let base_ok = run_status(Command::new("apktool").args(["d", "-f", &base.to_string_lossy(), "-o", &base_dir.to_string_lossy()]));
    if base_ok.is_err() {
        log.push("Base APK cannot be merged — saving only base".into());
        fs::copy(&base, out).map_err(|e| format!("copy: {e}"))?; return Ok(());
    }
    for (i, sp) in splits.iter().enumerate() {
        log.push(format!("Split {}/{}: {}", i + 1, splits.len(), sp.file_name().unwrap().to_string_lossy()));
        let sd = work.path().join("split");
        if sd.exists() { fs::remove_dir_all(&sd).ok(); }
        if run_status(Command::new("apktool").args(["d", "-f", &sp.to_string_lossy(), "-o", &sd.to_string_lossy()])).is_err() {
            log.push(format!("  Extracting assets directly..."));
            // Asset-only splits: unzip and copy assets/lib without decompiling
            if let Ok(file) = File::open(sp) {
                if let Ok(mut zip) = zip::ZipArchive::new(file) {
                    for i in 0..zip.len() {
                        if let Ok(mut entry) = zip.by_index(i) {
                            let name = entry.name().to_string();
                            let out_path = sd.join(&name);
                            if entry.is_dir() { fs::create_dir_all(&out_path).ok(); continue; }
                            if let Some(p) = out_path.parent() { fs::create_dir_all(p).ok(); }
                            if let Ok(mut f) = File::create(&out_path) { io::copy(&mut entry, &mut f).ok(); }
                        }
                    }
                }
            }
            // still copy assets/lib if any were extracted
            for folder in &["lib", "assets", "unknown"] {
                let src = sd.join(folder); if src.exists() { let dst = base_dir.join(folder); if dst.exists() { fs::remove_dir_all(&dst).ok(); } cp_dir(&src, &dst).ok(); }
            }
            continue;
        }
        for ent in walkdir(&sd).into_iter().filter(|e| e.to_string_lossy().contains("smali")) {
            let rel = ent.strip_prefix(&sd).unwrap(); let dst = base_dir.join(rel);
            if ent.is_dir() { fs::create_dir_all(&dst).ok(); }
            else { if let Some(p) = dst.parent() { fs::create_dir_all(p).ok(); } fs::copy(&ent, &dst).ok(); }
        }
        for folder in &["lib", "assets", "unknown"] {
            let src = sd.join(folder); if src.exists() { let dst = base_dir.join(folder); if dst.exists() { fs::remove_dir_all(&dst).ok(); } cp_dir(&src, &dst).ok(); }
        }
    }
    log.push("Rebuilding...".into());
    let merged = work.path().join("merged-unsigned.apk");
    run_status(Command::new("apktool").args(["b", "-f", &base_dir.to_string_lossy(), "-o", &merged.to_string_lossy()]))?;
    log.push("Signing...".into());
    let ks_path = std::env::temp_dir().join("apkdl_debug.keystore");
    if !ks_path.exists() {
        run_status(Command::new("keytool").args(["-genkeypair", "-alias", "androiddebugkey", "-keyalg", "RSA", "-keysize", "2048", "-validity", "10000", "-keystore", &ks_path.to_string_lossy(), "-storepass", "android", "-keypass", "android", "-dname", "CN=Android Debug,O=Android,C=US"]))?;
    }
    let sign_result = run_status(Command::new("apksigner").args(["sign", "--ks", &ks_path.to_string_lossy(), "--ks-key-alias", "androiddebugkey", "--ks-pass", "pass:android", "--key-pass", "pass:android", &merged.to_string_lossy()]));
    if sign_result.is_err() {
        // fallback: jarsigner (Java SDK)
        run_status(Command::new("jarsigner").args(["-sigalg", "SHA1withRSA", "-digestalg", "SHA1", "-keystore", &ks_path.to_string_lossy(), "-storepass", "android", "-keypass", "android", &merged.to_string_lossy(), "androiddebugkey"]))
            .map_err(|e| format!("sign failed (tried apksigner and jarsigner): {e}"))?;
    }
    fs::copy(&merged, out).map_err(|e| format!("copy result: {e}"))?;
    Ok(())
}

pub fn walkdir(dir: &Path) -> Vec<PathBuf> {
    let mut res = vec![]; if !dir.exists() { return res; }
    fn walk(d: &Path, acc: &mut Vec<PathBuf>) { if let Ok(entries) = fs::read_dir(d) { for e in entries.flatten() { acc.push(e.path()); if e.path().is_dir() { walk(&e.path(), acc); } } } }
    walk(dir, &mut res); res
}

pub fn cp_dir(src: &Path, dst: &Path) -> io::Result<()> {
    if !src.exists() { return Ok(()); }
    for e in src.read_dir()? { let e = e?; let target = dst.join(e.file_name()); if e.path().is_dir() { fs::create_dir_all(&target)?; cp_dir(&e.path(), &target)?; } else { fs::copy(&e.path(), &target)?; } } Ok(())
}
