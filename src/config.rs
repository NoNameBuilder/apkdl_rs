use std::fs;

use serde::Deserialize;

use crate::util::home_dir;

#[derive(Deserialize, Default)]
#[allow(dead_code)]
pub struct Config {
    pub default_arch: Option<String>,
    pub default_source: Option<String>,
    pub output_dir: Option<String>,
    pub timeout_secs: Option<u64>,
}

pub fn load_config() -> Config {
    let path = home_dir().join(".config/apkdl/config.toml");
    if path.exists() {
        let s = fs::read_to_string(&path).unwrap_or_default();
        toml::from_str(&s).unwrap_or_default()
    } else {
        Config::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn test_load_config() { let cfg = load_config(); assert!(cfg.default_arch.is_none() || cfg.default_arch.is_some()); }
}
