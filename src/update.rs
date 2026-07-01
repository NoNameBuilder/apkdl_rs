use std::fs;
use std::path::PathBuf;
use std::process::Command;

use crate::util::home_dir;

pub fn self_update() -> Result<(), String> {
    let exe = std::env::current_exe().unwrap_or_default();
    let src = exe.ancestors().find(|a| a.join("Cargo.toml").exists())
        .map(|a| a.to_path_buf())
        .unwrap_or_else(|| std::env::var("APKDL_SRC_DIR").map(PathBuf::from).unwrap_or_else(|_| home_dir().join("apkdl_rs/apkdl_rs")));
    if !src.join("Cargo.toml").exists() { return Err(format!("Source not found at {}", src.display())); }
    let ok = Command::new("cargo").args(["build", "--release"]).current_dir(&src).status().map(|s| s.success()).unwrap_or(false);
    if !ok { return Err("Build failed".into()); }
    let bin = src.join("target/release/apkdl"); if !bin.exists() { return Err("Binary not found after build".into()); }
    let dest = PathBuf::from("/usr/local/bin/apkdl"); fs::create_dir_all("/usr/local/bin").ok();
    fs::copy(&bin, &dest).map_err(|e| format!("Install failed: {e} — try sudo"))?;
    Ok(())
}
