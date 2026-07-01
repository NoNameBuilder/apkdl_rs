use std::collections::HashSet;

use reqwest::blocking::Client;
use scraper::{Html, Selector};

use crate::util::{as_str, fetch_bytes};

pub struct Version {
    pub label: String,
    pub url: String,
}

pub fn search_play(client: &Client, query: &str) -> Vec<(String, String)> {
    let q = query.split_whitespace().collect::<Vec<_>>().join("+");
    let html = match fetch_bytes(client, &format!("https://play.google.com/store/search?q={q}&c=apps")) { Ok(h) => h, _ => return vec![], };
    let text = as_str(&html); let mut seen = HashSet::new(); let mut results = vec![];
    for pat in &[r#"aria-label="([^"]*)"[^>]*href="/store/apps/details\?id=([^"&]+)""#, r#"href="/store/apps/details\?id=([^"&]+)"[^>]*aria-label="([^"]*)""#, r#"data-title="([^"]+)"[^>]*href="/store/apps/details\?id=([^"&]+)""#] {
        let r = regex::Regex::new(pat).unwrap();
        for c in r.captures_iter(&text) {
            let mut t = c[1].trim().to_string();
            let mut p = c[2].trim().to_string();
            // pattern 2 has groups reversed — detect and fix
            if !p.contains('.') && t.contains('.') { std::mem::swap(&mut t, &mut p); }
            if seen.insert(p.clone()) && !t.is_empty() { results.push((t, p)); }
        }
    }
    let r = regex::Regex::new(r#"href="/store/apps/details\?id=([^"&]+)""#).unwrap();
    for c in r.captures_iter(&text) { let p = c[1].trim().to_string(); if seen.insert(p.clone()) { let name = p.split('.').last().unwrap_or(&p).replace('-', " ").replace('_', " "); let t = name.chars().next().map(|x| x.to_uppercase().collect::<String>() + &name[1..]).unwrap_or(name); results.push((t, p)); } }
    results
}

pub fn list_versions_apkmirror(client: &Client, pkg: &str) -> Result<Vec<Version>, String> {
    let slug = pkg.replace('.', "-");
    let search_html = fetch_bytes(client, &format!("https://www.apkmirror.com/?s={}", pkg.replace('.', "+")))?;
    let doc = Html::parse_document(&as_str(&search_html));
    let sel = Selector::parse("a[href*='/apk/']").unwrap();
    let app_link = doc.select(&sel).find(|a| a.value().attr("href").map_or(false, |h| h.contains(&slug)));
    let app_url = match app_link { Some(a) => format!("https://www.apkmirror.com{}", a.value().attr("href").unwrap()), None => return Err("not found".into()), };
    let html = fetch_bytes(client, &app_url)?; let text = as_str(&html);
    let re = regex::Regex::new(r#"href="(/apk/[^"]+)">\s*<div[^>]*>\s*([^<]+)"#).unwrap();
    let mut seen = HashSet::new(); let mut versions = vec![];
    for cap in re.captures_iter(&text) {
        let url = format!("https://www.apkmirror.com{}", &cap[1]); let label = cap[2].trim().to_string();
        if seen.insert(url.clone()) && !label.is_empty() { versions.push(Version { label, url }); }
    }
    Ok(versions)
}

pub fn list_versions_apkpure(client: &Client, pkg: &str) -> Result<Vec<Version>, String> {
    let slug = pkg.replace('.', "-");
    let url = format!("https://apkpure.com/{slug}/old-versions");
    let html = fetch_bytes(client, &url)?; let doc = Html::parse_document(&as_str(&html));
    let sel = Selector::parse("a[href*='/version/']").unwrap();
    let mut seen = HashSet::new(); let mut versions = vec![];
    for a in doc.select(&sel) {
        let href = a.value().attr("href").unwrap_or("").to_string(); if href.is_empty() { continue; }
        let full = if href.starts_with("http") { href.clone() } else { format!("https://apkpure.com{href}") };
        let label = href.trim_end_matches('/').rsplit('/').next().unwrap_or("latest").replace('-', " ");
        if seen.insert(full.clone()) { versions.push(Version { label, url: full }); }
    }
    Ok(versions)
}
