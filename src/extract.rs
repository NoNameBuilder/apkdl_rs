use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;
use zip::ZipArchive;

use crate::util::{run_quiet, run_status, ARCHES};

/// Find APKEditor.jar next to the binary or in current dir
fn apkeditor_path() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    let candidates = [
        exe.parent().map(|p| p.join("APKEditor.jar")),
        Some(PathBuf::from("APKEditor.jar")),
    ];
    for c in candidates.iter().flatten() {
        if c.exists() { return c.clone(); }
    }
    PathBuf::from("APKEditor.jar")
}

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

    // No splits — just copy base
    if splits.is_empty() { fs::copy(&base, out).map_err(|e| format!("copy: {e}"))?; return Ok(()); }

    // Use APKEditor.jar to merge splits at binary level
    let jar = apkeditor_path();
    if !jar.exists() {
        log.push("APKEditor.jar not found — saving base only".into());
        fs::copy(&base, out).map_err(|e| format!("copy: {e}"))?; return Ok(());
    }

    log.push(format!("Merging {} split(s) with APKEditor...", splits.len()));
    let mut merge_cmd = Command::new("java");
    merge_cmd.arg("-jar").arg(&jar);
    merge_cmd.args(["m", "-i", &dir.to_string_lossy(), "-o", &out.to_string_lossy(), "-f", "-extractNativeLibs", "false"]);
    run_quiet(&mut merge_cmd)?;

    // Sign the merged APK
    log.push("Signing...".into());
    let ks_path = std::env::temp_dir().join("apkdl_debug.keystore");
    if !ks_path.exists() {
        run_status(Command::new("keytool").args(["-genkeypair", "-alias", "androiddebugkey", "-keyalg", "RSA", "-keysize", "2048", "-validity", "10000", "-keystore", &ks_path.to_string_lossy(), "-storepass", "android", "-keypass", "android", "-dname", "CN=Android Debug,O=Android,C=US"]))?;
    }
    let sign_result = run_status(Command::new("apksigner").args(["sign", "--ks", &ks_path.to_string_lossy(), "--ks-key-alias", "androiddebugkey", "--ks-pass", "pass:android", "--key-pass", "pass:android", &out.to_string_lossy()]));
    if sign_result.is_err() {
        log.push("apksigner not found, trying jarsigner...".into());
        let jar_result = run_status(Command::new("jarsigner").args(["-sigalg", "SHA1withRSA", "-digestalg", "SHA1", "-keystore", &ks_path.to_string_lossy(), "-storepass", "android", "-keypass", "android", &out.to_string_lossy(), "androiddebugkey"]));
        if jar_result.is_err() {
            log.push("Warning: signing failed — output may not install.".into());
        }
    }
    Ok(())
}
