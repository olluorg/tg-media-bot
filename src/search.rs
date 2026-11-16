//! Video search powered by yt-dlp with in-memory caching and concurrency control.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::proc;
use crate::util;

const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const CACHE_TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchItem {
    pub id: String,
    pub url: String,
    pub title: String,
    pub uploader: String,
    pub duration_secs: u64,
    pub duration_str: String,
    pub thumb_url: Option<String>,
}

#[derive(Deserialize, Debug)]
struct RawEntry {
    pub id: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
    pub uploader: Option<String>,
    pub channel: Option<String>,
    pub duration: Option<f64>,
    pub duration_string: Option<String>,
    pub thumbnails: Option<Vec<RawThumb>>,
}

#[derive(Deserialize, Debug)]
struct RawThumb {
    pub url: Option<String>,
}

static SEARCH_CACHE: Mutex<Option<HashMap<String, (Instant, Vec<SearchItem>)>>> = Mutex::new(None);

fn get_cached(query: &str) -> Option<Vec<SearchItem>> {
    let mut guard = SEARCH_CACHE.lock().ok()?;
    let map = guard.as_mut()?;
    let key = query.trim().to_lowercase();
    if let Some((instant, items)) = map.get(&key) {
        if instant.elapsed() < CACHE_TTL {
            return Some(items.clone());
        }
    }
    None
}

fn put_cached(query: &str, items: Vec<SearchItem>) {
    if let Ok(mut guard) = SEARCH_CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if map.len() > 200 {
            map.retain(|_, (t, _)| t.elapsed() < CACHE_TTL);
        }
        map.insert(query.trim().to_lowercase(), (Instant::now(), items));
    }
}

/// Search YouTube using yt-dlp --flat-playlist --dump-json.
pub async fn search_youtube(
    cfg: &Config,
    slots: &Semaphore,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchItem>, String> {
    let clean_query = query.trim();
    if clean_query.is_empty() {
        return Ok(Vec::new());
    }

    if let Some(cached) = get_cached(clean_query) {
        return Ok(cached.into_iter().take(limit).collect());
    }

    let _permit = slots.acquire().await.map_err(|e| e.to_string())?;

    let mut args: Vec<String> = vec![
        "--ignore-config".to_string(),
        "--flat-playlist".to_string(),
        "--dump-json".to_string(),
        "--no-warnings".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--socket-timeout".to_string(),
        "15".to_string(),
        "--retries".to_string(),
        "2".to_string(),
        "--default-search".to_string(),
        format!("ytsearch{limit}"),
    ];

    if let Some(proxy) = &cfg.proxy_url {
        args.extend(["--proxy".to_string(), proxy.clone()]);
    }
    if let Some(file) = cfg.cookies_path() {
        args.extend(["--cookies".to_string(), file.display().to_string()]);
    } else if let Some(browser) = &cfg.browser_cookies {
        args.extend(["--cookies-from-browser".to_string(), browser.clone()]);
    }

    args.push(clean_query.to_string());

    let mut cmd = proc::command(&cfg.ytdlp_bin, cfg.child_nice);
    cmd.args(&args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut items = Vec::new();
    let res = proc::run(cmd, SEARCH_TIMEOUT, |line| {
        if line.is_empty() {
            return ControlFlow::Continue(());
        }
        if let Ok(raw) = serde_json::from_str::<RawEntry>(line) {
            if let Some(id) = raw.id {
                let url = raw.url.unwrap_or_else(|| format!("https://www.youtube.com/watch?v={id}"));
                let title = raw.title.unwrap_or_else(|| "Untitled".to_string());
                let uploader = raw.uploader.or(raw.channel).unwrap_or_else(|| "YouTube".to_string());
                let duration_secs = raw.duration.unwrap_or(0.0) as u64;
                let duration_str = raw.duration_string.unwrap_or_else(|| {
                    if duration_secs > 0 {
                        util::fmt_duration(duration_secs)
                    } else {
                        "".to_string()
                    }
                });
                let thumb_url = raw
                    .thumbnails
                    .and_then(|t| t.into_iter().rev().find_map(|x| x.url))
                    .or_else(|| Some(format!("https://i.ytimg.com/vi/{id}/hqdefault.jpg")));

                items.push(SearchItem { id, url, title, uploader, duration_secs, duration_str, thumb_url });
            }
        }
        ControlFlow::Continue(())
    })
    .await;

    match res {
        Ok(out) => {
            if !out.ok && items.is_empty() {
                return Err(out.stderr);
            }
            put_cached(clean_query, items.clone());
            Ok(items.into_iter().take(limit).collect())
        }
        Err(proc::RunError::Timeout) => Err("Search timed out".to_string()),
        Err(e) => Err(format!("{e:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_cache_put_get() {
        let items = vec![SearchItem {
            id: "vid1".into(),
            url: "https://www.youtube.com/watch?v=vid1".into(),
            title: "Test Video".into(),
            uploader: "Tester".into(),
            duration_secs: 100,
            duration_str: "1:40".into(),
            thumb_url: Some("https://example.com/thumb.jpg".into()),
        }];

        put_cached("query123", items.clone());
        let cached = get_cached("  QUERY123  ").expect("should be cached case-insensitively");
        assert_eq!(cached.len(), 1);
        assert_eq!(cached[0].id, "vid1");
    }

    #[test]
    fn test_parse_raw_entry() {
        let json_line = r#"{"id":"dQw4w9WgXcQ","title":"Rick Astley - Never Gonna Give You Up","uploader":"RickAstleyVEVO","duration":213,"duration_string":"3:33","thumbnails":[{"url":"https://example.com/t.jpg"}]}"#;
        let parsed: RawEntry = serde_json::from_str(json_line).expect("must parse");
        assert_eq!(parsed.id.as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(parsed.title.as_deref(), Some("Rick Astley - Never Gonna Give You Up"));
        assert_eq!(parsed.duration, Some(213.0));
    }
}
