//! YouTube URL resolver and RSS feed parser.

use reqwest::header::{HeaderMap, HeaderValue, USER_AGENT};
use std::time::Duration;

use super::types::{ChannelInfo, FeedEntry};

/// Returns true if the URL points to a specific video/short/stream rather than a channel.
pub fn is_video_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.contains("/watch") || lower.contains("youtu.be/") || lower.contains("/shorts/") || lower.contains("/live/")
}

/// Returns true if the URL looks like a YouTube channel URL.
pub fn is_channel_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    if is_video_url(url) {
        return false;
    }
    (lower.contains("youtube.com/") || lower.contains("m.youtube.com/"))
        && (lower.contains("/@") || lower.contains("/channel/") || lower.contains("/c/") || lower.contains("/user/"))
}

/// Extract channel ID (UC...) directly from URL if present.
pub fn extract_direct_channel_id(url: &str) -> Option<String> {
    let lower = url.to_ascii_lowercase();
    if let Some(idx) = lower.find("/channel/") {
        let rest = &url[idx + 9..];
        let id: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
        if id.starts_with("UC") && id.len() >= 20 {
            return Some(id);
        }
    }
    None
}

/// Resolves a YouTube channel URL into ChannelInfo (channel_id, title, latest_video_id).
pub async fn resolve_channel(http: &reqwest::Client, url: &str) -> Result<ChannelInfo, String> {
    let channel_id = if let Some(id) = extract_direct_channel_id(url) {
        id
    } else {
        // Scrape the channel page for the canonical channel ID
        resolve_channel_id_from_page(http, url).await?
    };

    // Now fetch the RSS feed for this channel ID to get the title and latest video
    fetch_channel_info_from_rss(http, &channel_id).await
}

/// Scrapes channel page HTML to extract the channel ID (UC...).
async fn resolve_channel_id_from_page(http: &reqwest::Client, url: &str) -> Result<String, String> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36"),
    );

    let resp = http
        .get(url)
        .headers(headers)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("failed to fetch channel page: {e}"))?;

    let text = resp.text().await.map_err(|e| format!("failed to read channel page: {e}"))?;

    // Search patterns for channel ID
    // 1. <meta itemprop="identifier" content="UC...">
    if let Some(id) = find_between(&text, "<meta itemprop=\"identifier\" content=\"", "\"") {
        if id.starts_with("UC") && id.len() >= 20 {
            return Ok(id.to_string());
        }
    }

    // 2. <link rel="canonical" href="https://www.youtube.com/channel/UC...">
    if let Some(id) = find_between(&text, "youtube.com/channel/", "\"") {
        let clean: String = id.chars().take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect();
        if clean.starts_with("UC") && clean.len() >= 20 {
            return Ok(clean);
        }
    }

    // 3. "channelId":"UC..."
    if let Some(id) = find_between(&text, "\"channelId\":\"", "\"") {
        if id.starts_with("UC") && id.len() >= 20 {
            return Ok(id.to_string());
        }
    }

    // 4. "externalId":"UC..."
    if let Some(id) = find_between(&text, "\"externalId\":\"", "\"") {
        if id.starts_with("UC") && id.len() >= 20 {
            return Ok(id.to_string());
        }
    }

    Err("could not find channel ID on page; ensure the channel exists and the link is valid".to_string())
}

/// Fetches the channel's RSS feed and parses channel title & latest video ID.
pub async fn fetch_channel_info_from_rss(http: &reqwest::Client, channel_id: &str) -> Result<ChannelInfo, String> {
    let feed_url = format!("https://www.youtube.com/feeds/videos.xml?channel_id={channel_id}");
    let resp = http
        .get(&feed_url)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("failed to fetch RSS feed: {e}"))?;

    if !resp.status().is_success() {
        return Err(format!("RSS feed returned status {}", resp.status()));
    }

    let xml = resp.text().await.map_err(|e| format!("failed to read RSS feed: {e}"))?;
    let (title, entries) = parse_feed(&xml)?;

    let latest_video_id = entries.first().map(|e| e.video_id.clone());

    Ok(ChannelInfo {
        channel_id: channel_id.to_string(),
        title: if title.is_empty() { channel_id.to_string() } else { title },
        latest_video_id,
    })
}

/// Parses a YouTube Atom feed string into (channel_title, Vec<FeedEntry>).
pub fn parse_feed(xml: &str) -> Result<(String, Vec<FeedEntry>), String> {
    // Channel title is usually in the top <title>...</title> before any <entry>
    let channel_title = if let Some(entry_start) = xml.find("<entry>") {
        find_between(&xml[..entry_start], "<title>", "</title>").unwrap_or_default().to_string()
    } else {
        find_between(xml, "<title>", "</title>").unwrap_or_default().to_string()
    };

    let mut entries = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<entry>") {
        let after_start = &rest[start + 7..];
        let Some(end) = after_start.find("</entry>") else {
            break;
        };
        let entry_xml = &after_start[..end];
        rest = &after_start[end + 8..];

        let video_id = find_between(entry_xml, "<yt:videoId>", "</yt:videoId>").unwrap_or_default().to_string();
        if video_id.is_empty() {
            continue;
        }

        let channel_id = find_between(entry_xml, "<yt:channelId>", "</yt:channelId>").unwrap_or_default().to_string();
        let title = find_between(entry_xml, "<title>", "</title>").unwrap_or_default().to_string();
        let author_name = find_between(entry_xml, "<name>", "</name>").unwrap_or_default().to_string();
        let link = find_between(entry_xml, "href=\"", "\"")
            .unwrap_or(&format!("https://www.youtube.com/watch?v={video_id}"))
            .to_string();

        let published_str = find_between(entry_xml, "<published>", "</published>").unwrap_or_default();
        let published_at = parse_iso8601_to_timestamp(published_str);

        entries.push(FeedEntry {
            video_id,
            channel_id,
            title: html_unescape(&title),
            link,
            author_name: html_unescape(&author_name),
            published_at,
        });
    }

    Ok((html_unescape(&channel_title), entries))
}

fn find_between<'a>(src: &'a str, start: &str, end: &str) -> Option<&'a str> {
    let s = src.find(start)? + start.len();
    let e = src[s..].find(end)? + s;
    Some(&src[s..e])
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

/// Approximate parse of ISO 8601 string like "2026-11-16T15:30:00+00:00" to unix timestamp.
pub fn parse_iso8601_to_timestamp(s: &str) -> u64 {
    // Basic format: YYYY-MM-DDTHH:MM:SS
    if s.len() < 19 {
        return 0;
    }
    let parts: Vec<&str> = s[..19].split('T').collect();
    if parts.len() != 2 {
        return 0;
    }
    let ymd: Vec<u64> = parts[0].split('-').filter_map(|p| p.parse().ok()).collect();
    let hms: Vec<u64> = parts[1].split(':').filter_map(|p| p.parse().ok()).collect();
    if ymd.len() != 3 || hms.len() != 3 {
        return 0;
    }
    let (year, month, day) = (ymd[0], ymd[1], ymd[2]);
    let (hour, min, sec) = (hms[0], hms[1], hms[2]);

    days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec
}

/// Days since Unix epoch (1970-01-01) for a civil date.
fn days_from_civil(y: u64, m: u64, d: u64) -> u64 {
    let mut y = y as i64;
    let m = m as i64;
    let d = d as i64;
    y -= if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 } as u64) + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146097 + doe as i64 - 719468) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_video_and_channel() {
        assert!(is_video_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));
        assert!(is_video_url("https://youtu.be/dQw4w9WgXcQ"));
        assert!(is_video_url("https://www.youtube.com/shorts/12345"));
        assert!(!is_channel_url("https://www.youtube.com/watch?v=dQw4w9WgXcQ"));

        assert!(is_channel_url("https://www.youtube.com/@veritasium"));
        assert!(is_channel_url("https://www.youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA"));
        assert!(!is_video_url("https://www.youtube.com/@veritasium"));
    }

    #[test]
    fn test_extract_direct_channel_id() {
        assert_eq!(
            extract_direct_channel_id("https://www.youtube.com/channel/UCHnyfMqiRRG1u-2MsSQLbXA"),
            Some("UCHnyfMqiRRG1u-2MsSQLbXA".to_string())
        );
        assert_eq!(extract_direct_channel_id("https://www.youtube.com/@veritasium"), None);
    }

    #[test]
    fn test_parse_iso8601() {
        let ts = parse_iso8601_to_timestamp("1970-01-01T00:00:00Z");
        assert_eq!(ts, 0);
        let ts = parse_iso8601_to_timestamp("2026-11-16T15:30:00+00:00");
        assert!(ts > 1_700_000_000);
    }
}
