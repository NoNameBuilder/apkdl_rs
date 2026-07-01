use reqwest::blocking::Client;
use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};

pub fn build_http(timeout_secs: u64) -> Result<Client, String> {
    let mut hdrs = HeaderMap::new();
    hdrs.insert(USER_AGENT, HeaderValue::from_static(
        "Mozilla/5.0 (Linux; Android 14) AppleWebKit/537.36"));
    Client::builder()
        .default_headers(hdrs)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .connect_timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}
