use std::borrow::Cow;
use std::path::PathBuf;
use std::process::Command;

use reqwest::blocking::Client;

pub const VENV_PYTHON: &str = "/tmp/apkdl_venv/bin/python3";
pub const TEMP_PREFIX: &str = "apkdl_";
pub const ARCHES: &[&str] = &["arm64_v8a", "armeabi_v7a", "x86_64", "x86"];

pub fn as_str(b: &[u8]) -> Cow<'_, str> {
    String::from_utf8_lossy(b)
}

pub fn run_status(cmd: &mut Command) -> Result<(), String> {
    cmd.status().map_err(|e| format!("cmd: {e}")).and_then(|s| {
        if s.success() { Ok(()) } else { Err(format!("exit {s}")) }
    })
}

pub fn run_quiet(cmd: &mut Command) -> Result<(), String> {
    cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    cmd.status().map_err(|e| format!("cmd: {e}")).and_then(|s| {
        if s.success() { Ok(()) } else { Err(format!("exit {s}")) }
    })
}

pub fn home_dir() -> PathBuf {
    std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/tmp"))
}

pub fn fetch_bytes(client: &Client, url: &str) -> Result<Vec<u8>, String> {
    client.get(url).send().map_err(|e| format!("HTTP: {e}"))?
        .bytes().map(|b| b.to_vec()).map_err(|e| format!("body: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_arches() { assert!(ARCHES.contains(&"arm64_v8a")); assert!(ARCHES.contains(&"x86_64")); }
}
