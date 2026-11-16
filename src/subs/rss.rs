//! Rolling RSS fallback worker with dynamic jitter delay and per-channel cooldown.

use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderValue, IF_MODIFIED_SINCE, IF_NONE_MATCH, USER_AGENT};
use tokio::sync::mpsc::UnboundedSender;

use super::resolver;
use super::types::NewVideoNotification;
use super::websub;
use crate::app::App;
use crate::state::Store;

pub struct RssWorkerConfig {
    pub min_delay: Duration,
    pub max_delay: Duration,
    pub channel_cooldown: Duration,
    pub websub_callback: Option<String>,
    pub websub_secret: Option<String>,
}

pub async fn run_rss_worker(
    app: Arc<App>,
    http: reqwest::Client,
    notify_tx: UnboundedSender<NewVideoNotification>,
    cfg: RssWorkerConfig,
) {
    log::info!(
        "Starting YouTube RSS fallback worker (delay: {:?}..{:?}, per-channel cooldown: {:?})",
        cfg.min_delay,
        cfg.max_delay,
        cfg.channel_cooldown
    );

    let mut last_lease_check = std::time::Instant::now() - Duration::from_secs(3600);

    loop {
        // 1. Periodically renew WebSub leases (e.g. once every hour)
        if last_lease_check.elapsed() > Duration::from_secs(3600) {
            last_lease_check = std::time::Instant::now();
            if let Some(cb_url) = &cfg.websub_callback {
                renew_expiring_leases(&app.store, &http, cb_url, cfg.websub_secret.as_deref()).await;
            }
        }

        // 2. Fetch the channel with the oldest check time that has exceeded the cooldown
        let now = crate::state::now_secs();
        let checked_before = now.saturating_sub(cfg.channel_cooldown.as_secs());
        let next_channel = app.store.get_next_channel_for_rss(checked_before);

        let Some((channel_id, etag, last_modified, last_video_id)) = next_channel else {
            // No channels ready for check right now.
            // Sleep until the earliest channel becomes eligible, or 15s if table is empty.
            let sleep_secs = if let Some(oldest_check) = app.store.get_oldest_channel_check_time() {
                let eligible_at = oldest_check + cfg.channel_cooldown.as_secs();
                eligible_at.saturating_sub(now).clamp(5, 60)
            } else {
                15
            };
            tokio::time::sleep(Duration::from_secs(sleep_secs)).await;
            continue;
        };

        // 3. Make conditional GET request
        let feed_url = format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}");
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"),
        );
        if let Some(et) = &etag {
            if let Ok(val) = HeaderValue::from_str(et) {
                headers.insert(IF_NONE_MATCH, val);
            }
        }
        if let Some(lm) = &last_modified {
            if let Ok(val) = HeaderValue::from_str(lm) {
                headers.insert(IF_MODIFIED_SINCE, val);
            }
        }

        match http.get(&feed_url).headers(headers).timeout(Duration::from_secs(15)).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status == reqwest::StatusCode::NOT_MODIFIED {
                    // 304 Not Modified: no new videos
                    app.store.touch_channel_checked(&channel_id);
                } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    log::warn!("YouTube returned 429 Too Many Requests; circuit breaker active for 15 minutes");
                    tokio::time::sleep(Duration::from_secs(900)).await;
                    continue;
                } else if status.is_success() {
                    let new_etag = resp.headers().get("etag").and_then(|h| h.to_str().ok()).map(str::to_string);
                    let new_lm = resp.headers().get("last-modified").and_then(|h| h.to_str().ok()).map(str::to_string);

                    if let Ok(xml) = resp.text().await {
                        if let Ok((title, entries)) = resolver::parse_feed(&xml) {
                            process_feed_entries(
                                &channel_id,
                                &title,
                                entries,
                                last_video_id.as_deref(),
                                &notify_tx,
                                &app.store,
                                new_etag.as_deref(),
                                new_lm.as_deref(),
                            );
                        } else {
                            app.store.touch_channel_checked(&channel_id);
                        }
                    } else {
                        app.store.touch_channel_checked(&channel_id);
                    }
                } else {
                    log::debug!("RSS check for {} returned status {}", channel_id, status);
                    app.store.touch_channel_checked(&channel_id);
                }
            }
            Err(e) => {
                log::debug!("RSS check failed for {}: {}", channel_id, e);
                app.store.touch_channel_checked(&channel_id);
            }
        }

        // Random dynamic jitter delay between min_delay and max_delay
        let delay = random_delay(cfg.min_delay, cfg.max_delay);
        tokio::time::sleep(delay).await;
    }
}

fn random_delay(min: Duration, max: Duration) -> Duration {
    if min >= max {
        return min;
    }
    let min_ms = min.as_millis() as u64;
    let max_ms = max.as_millis() as u64;
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
    let span = max_ms - min_ms;
    let offset = (nanos as u64) % span;
    Duration::from_millis(min_ms + offset)
}

fn process_feed_entries(
    channel_id: &str,
    channel_title: &str,
    entries: Vec<super::types::FeedEntry>,
    last_video_id: Option<&str>,
    notify_tx: &UnboundedSender<NewVideoNotification>,
    store: &Store,
    new_etag: Option<&str>,
    new_lm: Option<&str>,
) {
    if entries.is_empty() {
        store.touch_channel_checked(channel_id);
        return;
    }

    let newest_video_id = entries.first().map(|e| e.video_id.clone());

    // If last_video_id is None, this is the very first check for this channel.
    // Record the current newest video ID so we don't spam archive videos.
    if last_video_id.is_none() {
        store.update_channel_rss(channel_id, new_etag, new_lm, newest_video_id.as_deref());
        return;
    }

    let last_known = last_video_id.unwrap();
    let mut new_videos = Vec::new();

    for entry in entries {
        if entry.video_id == last_known {
            break;
        }
        new_videos.push(entry);
    }

    // Process from oldest of the new to newest
    for entry in new_videos.into_iter().rev() {
        let notif = NewVideoNotification {
            channel_id: channel_id.to_string(),
            channel_title: if !channel_title.is_empty() { channel_title.to_string() } else { entry.author_name },
            video_id: entry.video_id,
            video_title: entry.title,
            published_at: entry.published_at,
        };
        let _ = notify_tx.send(notif);
    }

    store.update_channel_rss(channel_id, new_etag, new_lm, newest_video_id.as_deref());
}

async fn renew_expiring_leases(store: &Store, http: &reqwest::Client, callback_url: &str, secret: Option<&str>) {
    // Renew leases that expire in less than 2 days (172800 seconds)
    let threshold = crate::state::now_secs() + 172800;
    let channels = store.get_channels_to_renew(threshold);

    for channel_id in channels {
        let lease = 864000u64; // 10 days
        if let Ok(()) = websub::send_hub_request(http, callback_url, &channel_id, "subscribe", secret, lease).await {
            store.update_channel_lease(&channel_id, crate::state::now_secs() + lease);
        }
        // Small delay between renewal requests
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_random_delay_bounds() {
        let min = Duration::from_secs(3);
        let max = Duration::from_secs(8);
        for _ in 0..50 {
            let d = random_delay(min, max);
            assert!(d >= min);
            assert!(d <= max);
        }
    }
}
