//! Notification broadcaster with rate-limiting and deduplication.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedReceiver;

use super::types::NewVideoNotification;
use crate::app::App;
use crate::telegram::TgError;
use crate::util;

pub async fn run_notify_worker(app: Arc<App>, mut notify_rx: UnboundedReceiver<NewVideoNotification>) {
    log::info!("Starting YouTube subscription notification worker");

    while let Some(notif) = notify_rx.recv().await {
        let subscribers = app.store.get_channel_subscribers(&notif.channel_id);
        if subscribers.is_empty() {
            continue;
        }

        let photo_url = format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", notif.video_id);
        let video_url = format!("https://www.youtube.com/watch?v={}", notif.video_id);

        for user_id in subscribers {
            // Deduplicate: atomic test-and-set in SQLite
            if !app.store.mark_notification_sent(user_id, &notif.video_id) {
                continue;
            }

            let lang = app.lang_for(user_id, None);
            let (caption, kb) = format_notification_card(&notif, &video_url, lang);

            // Attempt to send photo first, fallback to text message
            let send_result = app.bot.send_photo(user_id, None, &photo_url, &caption, Some(kb.clone()), None).await;

            match send_result {
                Ok(_) => {}
                Err(TgError::Api { code: 403, .. }) => {
                    log::info!("User {} blocked the bot; pausing subscriptions", user_id);
                    app.store.pause_user_subscriptions(user_id);
                }
                Err(e) => {
                    log::debug!("send_photo failed ({}), trying send_message", e);
                    let fallback_text = format!("{}\n\n🔗 {}", caption, video_url);
                    if let Err(e2) = app.bot.send_message(user_id, None, &fallback_text, Some(kb), None).await {
                        if let TgError::Api { code: 403, .. } = e2 {
                            app.store.pause_user_subscriptions(user_id);
                        }
                    }
                }
            }

            // Telegram rate limit: ~25 messages/sec max
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
    }
}

fn format_notification_card(notif: &NewVideoNotification, video_url: &str, lang: crate::i18n::Lang) -> (String, Value) {
    let (header, watch_btn, dl_video_btn, dl_audio_btn, unsub_btn) = match lang {
        crate::i18n::Lang::Ru => {
            ("🎬 <b>Новое видео на канале</b>", "▶️ Смотреть", "📥 Скачать видео", "🎵 Скачать MP3", "🔕 Отписаться")
        }
        crate::i18n::Lang::Uk => (
            "🎬 <b>Нове відео на каналі</b>",
            "▶️ Дивитися",
            "📥 Завантажити відео",
            "🎵 Завантажити MP3",
            "🔕 Відписатися",
        ),
        crate::i18n::Lang::Es => (
            "🎬 <b>Nuevo video en el canal</b>",
            "▶️ Ver",
            "📥 Descargar video",
            "🎵 Descargar MP3",
            "🔕 Desuscribirse",
        ),
        _ => ("🎬 <b>New video on channel</b>", "▶️ Watch", "📥 Download video", "🎵 Download MP3", "🔕 Unsubscribe"),
    };

    let caption = format!(
        "{}: <b>{}</b>\n\n📌 <b>{}</b>",
        header,
        util::html_escape(&notif.channel_title),
        util::html_escape(&notif.video_title)
    );

    let kb = json!({
        "inline_keyboard": [
            [
                { "text": watch_btn, "url": video_url }
            ],
            [
                { "text": dl_video_btn, "callback_data": format!("sub:dl:v:{}", notif.video_id) },
                { "text": dl_audio_btn, "callback_data": format!("sub:dl:a:{}", notif.video_id) }
            ],
            [
                { "text": unsub_btn, "callback_data": format!("sub:unsub:{}", notif.channel_id) }
            ]
        ]
    });

    (caption, kb)
}
