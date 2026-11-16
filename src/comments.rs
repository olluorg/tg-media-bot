//! YouTube video comments extractor with in-memory caching and pagination.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

use crate::config::Config;
use crate::i18n::Lang;
use crate::proc;
use crate::util;

const COMMENTS_TIMEOUT: Duration = Duration::from_secs(25);
const CACHE_TTL: Duration = Duration::from_secs(600); // 10 minutes
pub const PAGE_SIZE: usize = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommentItem {
    pub id: String,
    pub author: String,
    pub text: String,
    pub like_count: u64,
    pub is_favorited: bool,
    pub author_is_uploader: bool,
    pub time_text: Option<String>,
}

#[derive(Deserialize, Debug)]
struct RawVideoMeta {
    pub title: Option<String>,
    pub comments: Option<Vec<RawComment>>,
}

#[derive(Deserialize, Debug)]
struct RawComment {
    pub id: Option<String>,
    pub author: Option<String>,
    pub text: Option<String>,
    pub like_count: Option<u64>,
    pub is_favorited: Option<bool>,
    pub author_is_uploader: Option<bool>,
    pub _time_text: Option<String>,
    pub time_text: Option<String>,
}

static COMMENTS_CACHE: Mutex<Option<HashMap<String, (Instant, Option<String>, Vec<CommentItem>)>>> = Mutex::new(None);

fn get_cached(video_id: &str) -> Option<(Option<String>, Vec<CommentItem>)> {
    let mut guard = COMMENTS_CACHE.lock().ok()?;
    let map = guard.as_mut()?;
    if let Some((instant, title, items)) = map.get(video_id) {
        if instant.elapsed() < CACHE_TTL {
            return Some((title.clone(), items.clone()));
        }
    }
    None
}

fn put_cached(video_id: &str, title: Option<String>, items: Vec<CommentItem>) {
    if let Ok(mut guard) = COMMENTS_CACHE.lock() {
        let map = guard.get_or_insert_with(HashMap::new);
        if map.len() > 100 {
            map.retain(|_, (t, _, _)| t.elapsed() < CACHE_TTL);
        }
        map.insert(video_id.to_string(), (Instant::now(), title, items));
    }
}

/// Fetch top comments for a YouTube video using yt-dlp.
pub async fn fetch_comments(
    cfg: &Config,
    slots: &Semaphore,
    video_id: &str,
) -> Result<(Option<String>, Vec<CommentItem>), String> {
    if let Some(cached) = get_cached(video_id) {
        return Ok(cached);
    }

    let _permit = slots.acquire().await.map_err(|e| e.to_string())?;

    let url = format!("https://www.youtube.com/watch?v={video_id}");
    let mut args: Vec<String> = vec![
        "--ignore-config".to_string(),
        "--no-playlist".to_string(),
        "--playlist-items".to_string(),
        "1".to_string(),
        "--dump-json".to_string(),
        "--skip-download".to_string(),
        "--write-comments".to_string(),
        "--no-warnings".to_string(),
        "--color".to_string(),
        "never".to_string(),
        "--socket-timeout".to_string(),
        "20".to_string(),
        "--retries".to_string(),
        "2".to_string(),
        "--extractor-args".to_string(),
        "youtube:max_comments=25,all,top".to_string(),
    ];

    if let Some(proxy) = &cfg.proxy_url {
        args.extend(["--proxy".to_string(), proxy.clone()]);
    }
    if let Some(file) = cfg.cookies_path() {
        args.extend(["--cookies".to_string(), file.display().to_string()]);
    } else if let Some(browser) = &cfg.browser_cookies {
        args.extend(["--cookies-from-browser".to_string(), browser.clone()]);
    }

    args.push(url);

    let mut cmd = proc::command(&cfg.ytdlp_bin, cfg.child_nice);
    cmd.args(&args);
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut full_output = String::new();
    let res = proc::run(cmd, COMMENTS_TIMEOUT, |line| {
        if !line.is_empty() {
            full_output.push_str(line);
            full_output.push('\n');
        }
        ControlFlow::Continue(())
    })
    .await;

    match res {
        Ok(out) => {
            if !out.ok && full_output.is_empty() {
                return Err(if !out.stderr.is_empty() { out.stderr } else { "Failed to extract comments".to_string() });
            }

            let meta: RawVideoMeta =
                serde_json::from_str(&full_output).map_err(|e| format!("json parse error: {e}"))?;
            let title = meta.title;
            let mut comments = Vec::new();

            if let Some(raw_comments) = meta.comments {
                for c in raw_comments {
                    let text = c.text.unwrap_or_default().trim().to_string();
                    if text.is_empty() {
                        continue;
                    }
                    comments.push(CommentItem {
                        id: c.id.unwrap_or_default(),
                        author: c.author.unwrap_or_else(|| "User".to_string()),
                        text,
                        like_count: c.like_count.unwrap_or(0),
                        is_favorited: c.is_favorited.unwrap_or(false),
                        author_is_uploader: c.author_is_uploader.unwrap_or(false),
                        time_text: c.time_text.or(c._time_text),
                    });
                }
            }

            // Sort: author comments and favorited first, then by likes descending
            comments.sort_by(|a, b| {
                let prio_a = a.author_is_uploader || a.is_favorited;
                let prio_b = b.author_is_uploader || b.is_favorited;
                prio_b.cmp(&prio_a).then_with(|| b.like_count.cmp(&a.like_count))
            });

            put_cached(video_id, title.clone(), comments.clone());
            Ok((title, comments))
        }
        Err(proc::RunError::Timeout) => Err("Comments extraction timed out".to_string()),
        Err(e) => Err(format!("{e:?}")),
    }
}

/// Renders a single page of comments with pagination inline keyboard.
pub fn render_comments_page(
    video_id: &str,
    video_title: Option<&str>,
    items: &[CommentItem],
    page: usize,
    lang: Lang,
) -> (String, Value) {
    if items.is_empty() {
        let empty_text = match lang {
            Lang::Ru => "💬 <b>Комментарии к видео</b>\n\nКомментарии не найдены или отключены автором ролика.",
            Lang::Uk => "💬 <b>Коментарі до відео</b>\n\nКоментарі не знайдено або вони вимкнені автором відео.",
            Lang::Es => "💬 <b>Comentarios del video</b>\n\nNo se encontraron comentarios o están desactivados.",
            _ => "💬 <b>Video Comments</b>\n\nNo comments found or they are disabled by the creator.",
        };
        let kb = json!({
            "inline_keyboard": [
                [{ "text": "✖️ Закрыть", "callback_data": "del" }]
            ]
        });
        return (empty_text.to_string(), kb);
    }

    let total_pages = (items.len() + PAGE_SIZE - 1) / PAGE_SIZE;
    let page = page.min(total_pages.saturating_sub(1));
    let start = page * PAGE_SIZE;
    let end = (start + PAGE_SIZE).min(items.len());
    let slice = &items[start..end];

    let header = match lang {
        Lang::Ru => "💬 <b>Топ комментарии к видео:</b>",
        Lang::Uk => "💬 <b>Топ коментарі до відео:</b>",
        Lang::Es => "💬 <b>Comentarios principales:</b>",
        _ => "💬 <b>Top video comments:</b>",
    };

    let title_line =
        if let Some(t) = video_title { format!("\n🎬 <b>{}</b>\n", util::html_escape(t)) } else { "\n".to_string() };

    let mut body = format!("{header}{title_line}\n");

    for (i, c) in slice.iter().enumerate() {
        let idx = start + i + 1;
        let mut badges = String::new();
        if c.author_is_uploader {
            badges.push_str(" 👑 <i>(Автор)</i>");
        }
        if c.is_favorited {
            badges.push_str(" 💖");
        }

        let likes_str = if c.like_count > 0 { format!(" · 👍 {}", fmt_count(c.like_count)) } else { String::new() };

        let time_str = c.time_text.as_deref().map(|t| format!(" · <i>{t}</i>")).unwrap_or_default();

        let truncated_text = util::truncate_chars(&c.text, 280);
        body.push_str(&format!(
            "<b>{idx}.</b> 👤 <b>{}</b>{}{}{}\n<i>«{}»</i>\n\n",
            util::html_escape(&c.author),
            badges,
            likes_str,
            time_str,
            util::html_escape(&truncated_text)
        ));
    }

    let page_footer = match lang {
        Lang::Ru => format!("📄 <i>Страница {} из {} (Всего: {})</i>", page + 1, total_pages, items.len()),
        Lang::Uk => format!("📄 <i>Сторінка {} з {} (Всього: {})</i>", page + 1, total_pages, items.len()),
        Lang::Es => format!("📄 <i>Página {} de {} (Total: {})</i>", page + 1, total_pages, items.len()),
        _ => format!("📄 <i>Page {} of {} (Total: {})</i>", page + 1, total_pages, items.len()),
    };
    body.push_str(&page_footer);

    let mut nav_row = Vec::new();
    if page > 0 {
        nav_row.push(json!({
            "text": "◀️ Назад",
            "callback_data": format!("comm:{video_id}:{}", page - 1)
        }));
    }
    nav_row.push(json!({
        "text": format!("{} / {}", page + 1, total_pages),
        "callback_data": "noop"
    }));
    if page + 1 < total_pages {
        nav_row.push(json!({
            "text": "Вперёд ▶️",
            "callback_data": format!("comm:{video_id}:{}", page + 1)
        }));
    }

    let close_btn = match lang {
        Lang::Ru => "✖️ Закрыть",
        Lang::Uk => "✖️ Закрити",
        Lang::Es => "✖️ Cerrar",
        _ => "✖️ Close",
    };

    let kb = json!({
        "inline_keyboard": [
            nav_row,
            [{ "text": close_btn, "callback_data": "del" }]
        ]
    });

    (body, kb)
}

fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_comments_page_rendering() {
        let comments = vec![
            CommentItem {
                id: "c1".into(),
                author: "Alice".into(),
                text: "Great video!".into(),
                like_count: 50,
                is_favorited: true,
                author_is_uploader: false,
                time_text: Some("1 day ago".into()),
            },
            CommentItem {
                id: "c2".into(),
                author: "Creator".into(),
                text: "Thanks for watching!".into(),
                like_count: 200,
                is_favorited: false,
                author_is_uploader: true,
                time_text: Some("1 day ago".into()),
            },
        ];

        let (text, kb) = render_comments_page("12345", Some("Test Video"), &comments, 0, Lang::Ru);
        assert!(text.contains("Alice"));
        assert!(text.contains("Creator"));
        assert!(text.contains("Test Video"));
        let kb_str = kb.to_string();
        assert!(kb_str.contains("1 / 1"));
    }
}
