//! Update routing: access control, commands, links and button presses.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::app::{stage_label, App, Cancel, Refusal, Request};
use crate::cachekey;
use crate::i18n::{fill, Lang, Strings, LANG_PROMPT};
use crate::state;
use crate::sys;
use crate::telegram::{CallbackQuery, InlineQuery, Media, MediaKind, Message, Source, Update, User};
use crate::types::{Kind, Quality, Stage};
use crate::util;

/// Links taken from one message.
const MAX_LINKS_PER_MESSAGE: usize = 50;
/// Lines of /status before "…and N more".
const STATUS_LINES: usize = 15;

/// Telegram command menu for one language.
pub fn command_list(t: &Strings) -> Vec<(&'static str, &'static str)> {
    vec![
        ("start", t.c_start),
        ("help", t.c_help),
        ("search", t.c_search),
        ("premium", t.c_premium),
        ("promo", t.c_promo),
        ("subscriptions", t.c_subscriptions),
        ("quality", t.c_quality),
        ("status", t.c_status),
        ("mystats", t.c_mystats),
        ("cancel", t.c_cancel),
        ("clear", t.c_clear),
        ("lang", t.c_lang),
        ("minimal", t.c_minimal),
        ("topic", t.c_topic),
    ]
}

pub fn main_menu_keyboard(lang: Lang) -> Value {
    let (btn_search, btn_subs, btn_prem, btn_settings) = match lang {
        Lang::Ru => ("🔍 Поиск", "📺 Мои подписки", "⭐ Premium", "⚙️ Настройки"),
        Lang::Uk => ("🔍 Пошук", "📺 Мої підписки", "⭐ Premium", "⚙️ Налаштування"),
        Lang::Es => ("🔍 Buscar", "📺 Mis suscripciones", "⭐ Premium", "⚙️ Ajustes"),
        _ => ("🔍 Search", "📺 Subscriptions", "⭐ Premium", "⚙️ Settings"),
    };
    json!({
        "keyboard": [
            [{ "text": btn_search }, { "text": btn_subs }],
            [{ "text": btn_prem }, { "text": btn_settings }]
        ],
        "resize_keyboard": true,
        "is_persistent": true
    })
}

#[derive(Debug, PartialEq, Eq)]
enum MenuAction {
    Search,
    Subscriptions,
    Premium,
    Settings,
}

fn parse_menu_action(text: &str) -> Option<MenuAction> {
    let t = text.trim();
    if t.starts_with("🔍")
        || t.eq_ignore_ascii_case("поиск")
        || t.eq_ignore_ascii_case("search")
        || t.eq_ignore_ascii_case("buscar")
        || t.eq_ignore_ascii_case("пошук")
    {
        Some(MenuAction::Search)
    } else if t.starts_with("📺")
        || t.contains("подписк")
        || t.contains("підписк")
        || t.contains("suscrip")
        || t.contains("subscri")
    {
        Some(MenuAction::Subscriptions)
    } else if t.starts_with("⭐") || t.eq_ignore_ascii_case("premium") {
        Some(MenuAction::Premium)
    } else if t.starts_with("⚙")
        || t.contains("настройк")
        || t.contains("налаштув")
        || t.contains("ajuste")
        || t.contains("setting")
    {
        Some(MenuAction::Settings)
    } else {
        None
    }
}

fn settings_menu(app: &App, user_id: i64, chat_id: i64, lang: Lang) -> (String, Value) {
    let t = lang.t();
    let cur_q = quality_name(t, app.store.default_quality(user_id));
    let minimal = app.store.minimal(chat_id);
    let stats = app.stats.get_user_stats(user_id);
    let size_fmt = crate::util::fmt_size(stats.total_bytes);

    let (text, min_label, btn_q, btn_lang, btn_stats, btn_clear) = match lang {
        Lang::Ru => (
            format!(
                "⚙️ <b>Настройки бота</b>\n\n\
                 • 🎚 Качество по умолчанию: <b>{cur_q}</b>\n\
                 • 🌐 Язык интерфейса: <b>{}</b>\n\
                 • 🤫 Минимальный режим: <b>{}</b>\n\
                 • 📊 Скачано вами: <b>{} видео</b> ({size_fmt})",
                lang.button(),
                if minimal { "включён" } else { "выключен" },
                stats.total_downloads
            ),
            if minimal {
                "🤫 Режим: Минимальный (вкл)"
            } else {
                "🤫 Режим: Обычный (выкл)"
            },
            "🎚 Качество",
            "🌐 Язык",
            "📊 Моя статистика",
            "🧹 Очистить чат",
        ),
        Lang::Uk => (
            format!(
                "⚙️ <b>Налаштування бота</b>\n\n\
                 • 🎚 Якість за замовчуванням: <b>{cur_q}</b>\n\
                 • 🌐 Мова інтерфейсу: <b>{}</b>\n\
                 • 🤫 Мінімальний режим: <b>{}</b>\n\
                 • 📊 Завантажено вами: <b>{} відео</b> ({size_fmt})",
                lang.button(),
                if minimal { "увімкнено" } else { "вимкнено" },
                stats.total_downloads
            ),
            if minimal {
                "🤫 Режим: Мінімальний (увімк)"
            } else {
                "🤫 Режим: Звичайний (вимк)"
            },
            "🎚 Якість",
            "🌐 Мова",
            "📊 Моя статистика",
            "🧹 Очистити чат",
        ),
        Lang::Es => (
            format!(
                "⚙️ <b>Ajustes del bot</b>\n\n\
                 • 🎚 Calidad predeterminada: <b>{cur_q}</b>\n\
                 • 🌐 Idioma: <b>{}</b>\n\
                 • 🤫 Modo mínimo: <b>{}</b>\n\
                 • 📊 Descargas: <b>{} videos</b> ({size_fmt})",
                lang.button(),
                if minimal { "activado" } else { "desactivado" },
                stats.total_downloads
            ),
            if minimal { "🤫 Modo: Mínimo (on)" } else { "🤫 Modo: Normal (off)" },
            "🎚 Calidad",
            "🌐 Idioma",
            "📊 Mis estadísticas",
            "🧹 Limpiar chat",
        ),
        _ => (
            format!(
                "⚙️ <b>Bot Settings</b>\n\n\
                 • 🎚 Default quality: <b>{cur_q}</b>\n\
                 • 🌐 Interface language: <b>{}</b>\n\
                 • 🤫 Minimal mode: <b>{}</b>\n\
                 • 📊 Your downloads: <b>{} videos</b> ({size_fmt})",
                lang.button(),
                if minimal { "enabled" } else { "disabled" },
                stats.total_downloads
            ),
            if minimal { "🤫 Mode: Minimal (on)" } else { "🤫 Mode: Normal (off)" },
            "🎚 Quality",
            "🌐 Language",
            "📊 My Stats",
            "🧹 Clear Chat",
        ),
    };

    let kb = json!({
        "inline_keyboard": [
            [
                { "text": btn_q, "callback_data": "cfg:q" },
                { "text": btn_lang, "callback_data": "cfg:lang" }
            ],
            [
                { "text": min_label, "callback_data": "cfg:min" }
            ],
            [
                { "text": btn_stats, "callback_data": "cfg:me" },
                { "text": btn_clear, "callback_data": "cfg:clear" }
            ]
        ]
    });

    (text, kb)
}

fn lang_keyboard() -> Value {
    let buttons: Vec<Value> =
        Lang::ALL.iter().map(|l| json!({ "text": l.button(), "callback_data": format!("l:{}", l.code()) })).collect();
    let rows: Vec<Value> = buttons.chunks(2).map(|c| Value::Array(c.to_vec())).collect();
    json!({ "inline_keyboard": rows })
}

/// Buttons for every quality choice; `data` builds the callback payload.
/// If `duration_secs` is given, heights whose estimated size exceeds `max_bytes` are omitted.
fn quality_rows(
    t: &Strings,
    max_height: u32,
    duration_secs: Option<u64>,
    max_bytes: u64,
    data: impl Fn(&str) -> String,
) -> Vec<Vec<Value>> {
    let button = |q: Quality| json!({ "text": q.label(t), "callback_data": data(&q.code()) });
    let heights = Quality::heights_for_duration(max_height, duration_secs, max_bytes);
    let mut rows = Vec::new();
    if !heights.is_empty() {
        rows.push(vec![button(Quality::Max), button(Quality::Audio)]);
    } else {
        rows.push(vec![button(Quality::Audio)]);
    }
    let height_buttons: Vec<Value> = heights.into_iter().map(|h| button(Quality::Height(h))).collect();
    for chunk in height_buttons.chunks(2) {
        rows.push(chunk.to_vec());
    }
    rows
}

fn picker_keyboard(t: &Strings, token: &str, max_height: u32, duration_secs: Option<u64>, max_bytes: u64) -> Value {
    json!({ "inline_keyboard": quality_rows(t, max_height, duration_secs, max_bytes, |code| format!("q:{token}:{code}")) })
}

fn default_keyboard(t: &Strings, max_height: u32) -> Value {
    let mut rows = quality_rows(t, max_height, None, u64::MAX, |code| format!("d:{code}"));
    rows.push(vec![json!({ "text": t.btn_ask, "callback_data": "d:ask" })]);
    json!({ "inline_keyboard": rows })
}

fn stats_keyboard() -> Value {
    json!({
        "inline_keyboard": [
            [
                { "text": "📊 Сводка", "callback_data": "st:summary" },
                { "text": "📈 Тренд 7д", "callback_data": "st:trend" }
            ],
            [
                { "text": "⚠️ Ошибки", "callback_data": "st:errors" },
                { "text": "🏆 Топ", "callback_data": "st:top" }
            ],
            [
                { "text": "🌐 Платформы", "callback_data": "st:platforms" },
                { "text": "⚙️ Система", "callback_data": "st:system" }
            ],
            [
                { "text": "💾 Скачать бэкап базы (.db)", "callback_data": "st:backup" }
            ]
        ]
    })
}

fn top_keyboard(current_days: u64) -> Value {
    let b_today = if current_days == 1 { "• Сегодня •" } else { "Сегодня" };
    let b_week = if current_days == 7 { "• 7 дней •" } else { "7 дней" };
    let b_all = if current_days == 0 { "• Всё время •" } else { "Всё время" };
    json!({
        "inline_keyboard": [
            [
                { "text": b_today, "callback_data": "st:top:1" },
                { "text": b_week, "callback_data": "st:top:7" },
                { "text": b_all, "callback_data": "st:top:0" }
            ],
            [
                { "text": "◀ Назад в меню", "callback_data": "st:summary" }
            ]
        ]
    })
}

fn premium_menu(app: &App, user_id: i64, lang: Lang) -> (String, Value) {
    let (is_premium, expires_at) = app.store.get_user_premium_info(user_id);
    let sub_count = app.store.count_user_subscriptions(user_id);
    let limit = app.cfg.premium_limit;
    let has_welcome = app.store.has_user_activated_welcome(user_id);

    let expiry_str = if expires_at == 0 {
        match lang {
            Lang::Ru => "бессрочно".to_string(),
            Lang::Uk => "безстроково".to_string(),
            Lang::Es => "permanente".to_string(),
            _ => "permanent".to_string(),
        }
    } else {
        crate::util::format_date_str(expires_at as i64)
    };

    let text = match lang {
        Lang::Ru => {
            if is_premium {
                format!(
                    "✨ <b>Ваш статус: Premium активен</b> (до {expiry_str})\n\n\
                     🔍 Поиск видео: <b>активен</b> (/search или текст в чате)\n\
                     💬 Чтение комментариев: <b>активно</b>\n\
                     📺 Отслеживание каналов: <b>{sub_count}/{limit}</b>\n\n\
                     <i>Вы можете продлить подписку звёздами Telegram Stars или промокодом ниже:</i>",
                )
            } else {
                format!(
                    "⭐ <b>Подписка Premium</b>\n\n\
                     С Premium вам доступны эксклюзивные функции:\n\
                     • 🔍 <b>Поиск видео по названию</b> через /search и в инлайн-режиме\n\
                     • 💬 <b>Топ-комментарии к видео</b> с удобной пагинацией\n\
                     • 📺 <b>Отслеживание YouTube-каналов</b> (до {limit} каналов)\n\
                     • ⚡ <b>Мгновенные уведомления</b> о новых видео\n\
                     • 📥 <b>Скачивание видео и MP3</b> в 1 клик\n\n\
                     Выберите вариант подключения ниже:",
                )
            }
        }
        Lang::Uk => {
            if is_premium {
                format!(
                    "✨ <b>Ваш статус: Premium активний</b> (до {expiry_str})\n\n\
                     🔍 Пошук відео: <b>активний</b> (/search або текст у чаті)\n\
                     💬 Читання коментарів: <b>активно</b>\n\
                     📺 Відстеження каналів: <b>{sub_count}/{limit}</b>\n\n\
                     <i>Ви можете подовжити підписку зірками Telegram Stars або промокодом:</i>",
                )
            } else {
                format!(
                    "⭐ <b>Підписка Premium</b>\n\n\
                     З Premium вам доступні ексклюзивні можливості:\n\
                     • 🔍 <b>Пошук відео за назвою</b> через /search та в інлайн-режимі\n\
                     • 💬 <b>Топ-коментарі до відео</b> зі зручною пагінацією\n\
                     • 📺 <b>Відстеження YouTube-каналів</b> (до {limit} каналів)\n\
                     • ⚡ <b>Миттєві сповіщення</b> про нові відео\n\
                     • 📥 <b>Завантаження відео та MP3</b> в 1 клік\n\n\
                     Оберіть варіант підключення нижче:",
                )
            }
        }
        Lang::Es => {
            if is_premium {
                format!(
                    "✨ <b>Tu estado: Premium Activo</b> (hasta {expiry_str})\n\n\
                     🔍 Búsqueda de videos: <b>activa</b> (/search o texto en chat)\n\
                     💬 Lectura de comentarios: <b>activa</b>\n\
                     📺 Seguimiento de canales: <b>{sub_count}/{limit}</b>\n\n\
                     <i>Puedes renovar con Telegram Stars o código promocional:</i>",
                )
            } else {
                format!(
                    "⭐ <b>Suscripción Premium</b>\n\n\
                     Con Premium obtienes funciones exclusivas:\n\
                     • 🔍 <b>Buscar videos por título</b> vía /search y modo inline\n\
                     • 💬 <b>Mejores comentarios</b> con paginación\n\
                     • 📺 <b>Seguir canales de YouTube</b> (hasta {limit} canales)\n\
                     • ⚡ <b>Notificaciones instantáneas</b> de nuevos videos\n\
                     • 📥 <b>Descargas en 1 clic</b>\n\n\
                     Elige una opción a continuación:",
                )
            }
        }
        _ => {
            if is_premium {
                format!(
                    "✨ <b>Your status: Premium Active</b> (until {expiry_str})\n\n\
                     🔍 Video search: <b>active</b> (/search or text in chat)\n\
                     💬 Video comments: <b>active</b>\n\
                     📺 Tracked channels: <b>{sub_count}/{limit}</b>\n\n\
                     <i>You can extend your subscription with Telegram Stars or promo code:</i>",
                )
            } else {
                format!(
                    "⭐ <b>Premium Subscription</b>\n\n\
                     With Premium you unlock exclusive features:\n\
                     • 🔍 <b>Search videos by title</b> via /search and inline mode\n\
                     • 💬 <b>Top video comments</b> with pagination\n\
                     • 📺 <b>Track YouTube channels</b> (up to {limit} channels)\n\
                     • ⚡ <b>Instant notifications</b> on new releases\n\
                     • 📥 <b>1-click video and MP3 downloads</b>\n\n\
                     Choose an option below:",
                )
            }
        }
    };

    let mut rows: Vec<Vec<Value>> = Vec::new();

    // 1. Welcome promo button if not yet used
    if !has_welcome {
        let welcome_btn = match lang {
            Lang::Ru => "🎁 Активировать 30 дней бесплатно",
            Lang::Uk => "🎁 Активувати 30 днів безкоштовно",
            Lang::Es => "🎁 Activar 30 días gratis",
            _ => "🎁 Activate 30 days free",
        };
        rows.push(vec![json!({ "text": welcome_btn, "callback_data": "pr:start30" })]);
    }

    // 2. Stars purchase options
    let price_1m = app.cfg.stars_price_1m;
    let price_3m = app.cfg.stars_price_3m;
    let (btn_1m, btn_3m) = if is_premium {
        match lang {
            Lang::Ru => {
                (format!("➕ Продлить на 1 мес. — {price_1m} ⭐️"), format!("➕ Продлить на 3 мес. — {price_3m} ⭐️"))
            }
            Lang::Uk => {
                (format!("➕ Подовжити на 1 міс. — {price_1m} ⭐️"), format!("➕ Подовжити на 3 міс. — {price_3m} ⭐️"))
            }
            Lang::Es => (format!("➕ Renovar 1 mes — {price_1m} ⭐️"), format!("➕ Renovar 3 meses — {price_3m} ⭐️")),
            _ => (format!("➕ Extend 1 month — {price_1m} ⭐️"), format!("➕ Extend 3 months — {price_3m} ⭐️")),
        }
    } else {
        match lang {
            Lang::Ru => (format!("⭐ 1 месяц — {price_1m} ⭐️"), format!("⭐ 3 месяца — {price_3m} ⭐️")),
            Lang::Uk => (format!("⭐ 1 місяць — {price_1m} ⭐️"), format!("⭐ 3 місяці — {price_3m} ⭐️")),
            Lang::Es => (format!("⭐ 1 mes — {price_1m} ⭐️"), format!("⭐ 3 meses — {price_3m} ⭐️")),
            _ => (format!("⭐ 1 month — {price_1m} ⭐️"), format!("⭐ 3 months — {price_3m} ⭐️")),
        }
    };
    rows.push(vec![
        json!({ "text": btn_1m, "callback_data": "pr:buy:30" }),
        json!({ "text": btn_3m, "callback_data": "pr:buy:90" }),
    ]);

    // 3. Promo code button
    let promo_btn = match lang {
        Lang::Ru => "🎟 Ввести промокод",
        Lang::Uk => "🎟 Ввести промокод",
        Lang::Es => "🎟 Canjear código",
        _ => "🎟 Enter promo code",
    };
    rows.push(vec![json!({ "text": promo_btn, "callback_data": "pr:code_hint" })]);

    if is_premium {
        let off_btn = match lang {
            Lang::Ru => "Отключить Premium",
            Lang::Uk => "Вимкнути Premium",
            Lang::Es => "Desactivar Premium",
            _ => "Disable Premium",
        };
        rows.push(vec![json!({ "text": off_btn, "callback_data": "pr:off" })]);
    }

    (text, json!({ "inline_keyboard": rows }))
}

fn premium_upsell_keyboard(app: &App, user_id: i64, lang: Lang) -> Value {
    if !app.store.has_user_activated_welcome(user_id) {
        let btn = match lang {
            Lang::Ru => "🎁 Активировать 30 дней бесплатно",
            Lang::Uk => "🎁 Активувати 30 днів безкоштовно",
            Lang::Es => "🎁 Activar 30 días gratis",
            _ => "🎁 Activate 30 days free",
        };
        json!({
            "inline_keyboard": [[{ "text": btn, "callback_data": "pr:start30" }]]
        })
    } else {
        let price_1m = app.cfg.stars_price_1m;
        let btn_1m = match lang {
            Lang::Ru => format!("⭐ 1 месяц — {price_1m} ⭐️"),
            Lang::Uk => format!("⭐ 1 місяць — {price_1m} ⭐️"),
            Lang::Es => format!("⭐ 1 mes — {price_1m} ⭐️"),
            _ => format!("⭐ 1 month — {price_1m} ⭐️"),
        };
        json!({
            "inline_keyboard": [
                [{ "text": btn_1m, "callback_data": "pr:buy:30" }]
            ]
        })
    }
}

fn subscriptions_menu(app: &App, user_id: i64, lang: Lang) -> (String, Value) {
    let subs = app.store.get_user_subscriptions(user_id);
    let limit = app.cfg.premium_limit;
    let is_premium = app.store.is_user_premium(user_id);

    if subs.is_empty() {
        let text = match lang {
            Lang::Ru => {
                if !is_premium {
                    "📺 <b>У вас нет активных подписок</b>\n\nФункция отслеживания каналов доступна с Premium.\nАктивируйте его через команду /premium!"
                } else {
                    "📺 <b>У вас пока нет активных подписок</b>\n\nЧтобы подписаться, просто отправьте боту ссылку на YouTube-канал (например, <code>https://youtube.com/@канал</code>)!"
                }
            }
            _ => {
                if !is_premium {
                    "📺 <b>No active subscriptions</b>\n\nChannel tracking is available with Premium.\nActivate it via /premium!"
                } else {
                    "📺 <b>No active subscriptions yet</b>\n\nTo subscribe, just send a YouTube channel link (e.g. <code>https://youtube.com/@channel</code>)!"
                }
            }
        };
        return (text.to_string(), json!({ "inline_keyboard": [] }));
    }

    let header = match lang {
        Lang::Ru => format!(
            "📺 <b>Ваши подписки ({}/{}):</b>\n• Нажмите 🎬, чтобы посмотреть последние видео\n• Нажмите ❌, чтобы отписаться",
            subs.len(),
            limit
        ),
        Lang::Uk => format!(
            "📺 <b>Ваші підписки ({}/{}):</b>\n• Натисніть 🎬, щоб подивитися останні відео\n• Натисніть ❌, щоб відписатися",
            subs.len(),
            limit
        ),
        Lang::Es => format!(
            "📺 <b>Tus suscripciones ({}/{}):</b>\n• Haz clic en 🎬 para ver los últimos videos\n• Haz clic en ❌ para cancelar suscripción",
            subs.len(),
            limit
        ),
        _ => format!(
            "📺 <b>Your subscriptions ({}/{}):</b>\n• Click 🎬 to view latest channel videos\n• Click ❌ to unsubscribe",
            subs.len(),
            limit
        ),
    };

    let mut buttons = Vec::new();
    for (channel_id, title) in subs {
        let label = format!("🎬 {}", util::truncate_chars(&title, 22));
        buttons.push(vec![
            json!({
                "text": label,
                "callback_data": format!("sub:vid:{}", channel_id)
            }),
            json!({
                "text": "❌",
                "callback_data": format!("sub:unsub:{}", channel_id)
            }),
        ]);
    }

    let close_subs_btn = match lang {
        Lang::Ru => "✖️ Закрыть подписки",
        Lang::Uk => "✖️ Закрити підписки",
        Lang::Es => "✖️ Cerrar suscripciones",
        _ => "✖️ Close subscriptions",
    };
    buttons.push(vec![json!({
        "text": close_subs_btn,
        "callback_data": "del"
    })]);

    (header, json!({ "inline_keyboard": buttons }))
}

async fn handle_menu_action(app: &Arc<App>, _msg: &Message, ctx: &Ctx, action: MenuAction) {
    let (chat, thread) = (ctx.chat, ctx.thread);
    match action {
        MenuAction::Search => {
            let is_premium = app.store.is_user_premium(ctx.user);
            if is_premium {
                let prompt = match ctx.lang {
                    Lang::Ru => {
                        "🔍 <b>Поиск видео на YouTube</b>\n\n\
                         Введите поисковый запрос текстом прямо в этот чат (например: <code>Queen</code> или <code>стендап</code>).\n\n\
                         Бот найдёт видео и покажет карточки с кнопками быстрого скачивания!"
                    },
                    Lang::Uk => {
                        "🔍 <b>Пошук відео на YouTube</b>\n\n\
                         Введіть пошуковий запит текстом прямо в цей чат (наприклад: <code>Queen</code>).\n\n\
                         Бот знайде відео та надішле картки з кнопками швидкого завантаження!"
                    }
                    Lang::Es => {
                        "🔍 <b>Búsqueda de videos en YouTube</b>\n\n\
                         Escribe tu búsqueda por texto directamente en este chat (por ejemplo: <code>Queen</code>).\n\n\
                         ¡El bot encontrará los videos con opciones de descarga rápida!"
                    }
                    _ => {
                        "🔍 <b>YouTube Video Search</b>\n\n\
                         Send your search query directly into this chat (e.g. <code>Queen</code>).\n\n\
                         The bot will find videos and send cards with instant download options!"
                    },
                };
                app.say(chat, thread, prompt, None).await;
            } else {
                let prompt = match ctx.lang {
                    Lang::Ru => "⭐ <b>Поиск видео по YouTube доступен с Premium</b>\n\nС Premium вы можете искать и сразу скачивать любые ролики прямо в Telegram!",
                    Lang::Uk => "⭐ <b>Пошук відео на YouTube доступний з Premium</b>\n\nЗ Premium ви можете шукати та завантажувати ролики прямо в Telegram!",
                    Lang::Es => "⭐ <b>La búsqueda de videos en YouTube está disponible con Premium</b>",
                    _ => "⭐ <b>YouTube Video Search is available with Premium</b>",
                };
                let kb = premium_upsell_keyboard(app, ctx.user, ctx.lang);
                app.say(chat, thread, prompt, Some(kb)).await;
            }
        }
        MenuAction::Subscriptions => {
            let (text, kb) = subscriptions_menu(app, ctx.user, ctx.lang);
            app.say(chat, thread, &text, Some(kb)).await;
        }
        MenuAction::Premium => {
            let (text, kb) = premium_menu(app, ctx.user, ctx.lang);
            app.say(chat, thread, &text, Some(kb)).await;
        }
        MenuAction::Settings => {
            let (text, kb) = settings_menu(app, ctx.user, ctx.chat, ctx.lang);
            app.say(chat, thread, &text, Some(kb)).await;
        }
    }
}

async fn handle_channel_subscription_request(app: &Arc<App>, ctx: &Ctx, url: &str, reply_to: Option<i64>) {
    let user_id = ctx.user;
    let (chat, thread) = (ctx.chat, ctx.thread);
    let lang = ctx.lang;

    // 1. Check if user is premium
    if !app.store.is_user_premium(user_id) {
        let prompt = match lang {
            Lang::Ru => "⭐ <b>Отслеживание каналов доступно только с Premium</b>\n\n\
                         С Premium бот будет автоматически присылать новые ролики с ваших любимых каналов с кнопками скачивания в 1 клик.",
            Lang::Uk => "⭐ <b>Відстеження каналів доступне лише з Premium</b>\n\n\
                         З Premium бот автоматично надсилатиме нові ролики з улюблених каналів.",
            Lang::Es => "⭐ <b>El seguimiento de canales requiere Premium</b>\n\n\
                         Con Premium el bot enviará nuevos videos automáticamente.",
            _ => "⭐ <b>Channel subscriptions require Premium</b>\n\n\
                  With Premium, the bot will automatically send new videos from your favorite channels.",
        };
        let kb = premium_upsell_keyboard(app, user_id, lang);
        let _ = app.bot.send_message(chat, thread, prompt, Some(kb), reply_to).await;
        return;
    }

    // 2. Check channel limit
    let current_count = app.store.count_user_subscriptions(user_id);
    if current_count >= app.cfg.premium_limit {
        let text = match lang {
            Lang::Ru => format!(
                "❌ <b>Достигнут лимит подписок ({})</b>\n\nУдалите ненужные каналы через /subscriptions, чтобы добавить новый.",
                app.cfg.premium_limit
            ),
            _ => format!(
                "❌ <b>Subscription limit reached ({})</b>\n\nRemove unneeded channels via /subscriptions to add a new one.",
                app.cfg.premium_limit
            ),
        };
        let _ = app.bot.send_message(chat, thread, &text, None, reply_to).await;
        return;
    }

    // 3. Status message: resolving channel
    let resolving_text = match lang {
        Lang::Ru => "🔍 Определяю канал...",
        _ => "🔍 Resolving channel...",
    };
    let status_msg = app.bot.send_message(chat, thread, resolving_text, None, reply_to).await.ok();

    // 4. Resolve channel
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(12)).build().unwrap_or_default();
    match crate::subs::resolver::resolve_channel(&http, url).await {
        Ok(info) => {
            match app.store.add_subscription(user_id, &info.channel_id, &info.title, info.latest_video_id.as_deref()) {
                Ok(is_new_to_system) => {
                    // If channel had 0 subscribers and WebSub is enabled, subscribe to Google Hub
                    if is_new_to_system {
                        if let Some(pub_url) = &app.cfg.websub_public_url {
                            let pub_url = pub_url.clone();
                            let ch_id = info.channel_id.clone();
                            let sec = app.cfg.websub_secret.clone();
                            let http_clone = http.clone();
                            tokio::spawn(async move {
                                let _ = crate::subs::websub::send_hub_request(
                                    &http_clone,
                                    &pub_url,
                                    &ch_id,
                                    "subscribe",
                                    sec.as_deref(),
                                    864000,
                                )
                                .await;
                            });
                        }
                    }

                    let success_text = match lang {
                        Lang::Ru => format!(
                            "✅ <b>Вы успешно подписались на канал:</b>\n📺 <b>{}</b>\n\n\
                             Бот уведомит вас, как только выйдет новое видео!\n\n\
                             Список ваших подписок: /subscriptions",
                            util::html_escape(&info.title)
                        ),
                        _ => format!(
                            "✅ <b>Successfully subscribed to channel:</b>\n📺 <b>{}</b>\n\n\
                             The bot will notify you as soon as a new video is published!\n\n\
                             Manage your subscriptions: /subscriptions",
                            util::html_escape(&info.title)
                        ),
                    };

                    if let Some(sm) = status_msg {
                        let _ = app.bot.edit_message(chat, sm.message_id, &success_text, None).await;
                    } else {
                        let _ = app.bot.send_message(chat, thread, &success_text, None, reply_to).await;
                    }
                }
                Err(e) => {
                    let err_text = format!("❌ Ошибка сохранения подписки: {e}");
                    if let Some(sm) = status_msg {
                        let _ = app.bot.edit_message(chat, sm.message_id, &err_text, None).await;
                    }
                }
            }
        }
        Err(e) => {
            log::warn!("Channel resolve failed: {e}");
            let fail_text = match lang {
                Lang::Ru => "❌ Не удалось определить канал по ссылке.\nУбедитесь, что ссылка ведет на канал YouTube (например, <code>https://youtube.com/@название_канала</code>).",
                _ => "❌ Could not resolve YouTube channel from link.\nPlease make sure the link points to a channel (e.g. <code>https://youtube.com/@channel_name</code>).",
            };
            if let Some(sm) = status_msg {
                let _ = app.bot.edit_message(chat, sm.message_id, fail_text, None).await;
            } else {
                let _ = app.bot.send_message(chat, thread, fail_text, None, reply_to).await;
            }
        }
    }
}

#[derive(Clone)]
struct CardSession {
    created_at: std::time::Instant,
    items: Vec<crate::search::SearchItem>,
    msg_ids: Vec<i64>,
    prefix: &'static str,
    lang: Lang,
}

static CARD_SESSIONS: std::sync::Mutex<Option<std::collections::HashMap<String, CardSession>>> =
    std::sync::Mutex::new(None);
static SESSION_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
const CARD_SESSION_TTL: std::time::Duration = std::time::Duration::from_secs(900);

fn new_session_token() -> String {
    let n = SESSION_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mix = (n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ crate::state::now_secs()) & 0xFF_FFFF;
    format!("{:06x}", mix)
}

fn save_card_session(token: String, session: CardSession) {
    if let Ok(mut guard) = CARD_SESSIONS.lock() {
        let map = guard.get_or_insert_with(std::collections::HashMap::new);
        if map.len() > 100 {
            map.retain(|_, s| s.created_at.elapsed() < CARD_SESSION_TTL);
        }
        map.insert(token, session);
    }
}

fn get_card_session(token: &str) -> Option<CardSession> {
    let mut guard = CARD_SESSIONS.lock().ok()?;
    let map = guard.as_mut()?;
    if let Some(session) = map.get(token) {
        if session.created_at.elapsed() < CARD_SESSION_TTL {
            return Some(session.clone());
        }
    }
    None
}

fn render_card_content(item: &crate::search::SearchItem) -> (String, String) {
    let dur =
        if !item.duration_str.is_empty() { format!(" • ⏱ <b>{}</b>", item.duration_str) } else { String::new() };
    let caption = format!(
        "🎬 <a href=\"{}\"><b>{}</b></a>\n👤 <i>{}</i>{}\n",
        item.url,
        util::html_escape(&item.title),
        util::html_escape(&item.uploader),
        dur
    );
    let thumb_url = format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", item.id);
    (caption, thumb_url)
}

fn render_card_markup(
    item: &crate::search::SearchItem,
    lang: Lang,
    page_nav: Option<(usize, usize, &str, &str)>,
) -> Value {
    let (dl_video_btn, dl_audio_btn, comm_btn) = match lang {
        Lang::Ru => ("🎬 Скачать", "🎵 MP3", "💬 Комментарии"),
        Lang::Uk => ("🎬 Завантажити", "🎵 MP3", "💬 Коментарі"),
        Lang::Es => ("🎬 Descargar", "🎵 MP3", "💬 Comentarios"),
        _ => ("🎬 Download", "🎵 MP3", "💬 Comments"),
    };

    let mut rows = vec![
        vec![
            json!({ "text": dl_video_btn, "callback_data": format!("s:dl:v:{}", item.id) }),
            json!({ "text": dl_audio_btn, "callback_data": format!("s:dl:a:{}", item.id) }),
        ],
        vec![json!({ "text": comm_btn, "callback_data": format!("comm:{}:0", item.id) })],
    ];

    if let Some((page, total_pages, prefix, token)) = page_nav {
        if total_pages > 1 {
            let prev_page = if page > 0 { page - 1 } else { total_pages - 1 };
            let next_page = if page + 1 < total_pages { page + 1 } else { 0 };

            rows.push(vec![
                json!({ "text": "◀️", "callback_data": format!("{prefix}:p:{prev_page}:{token}") }),
                json!({ "text": format!("{}/{}", page + 1, total_pages), "callback_data": "noop" }),
                json!({ "text": "▶️", "callback_data": format!("{prefix}:p:{next_page}:{token}") }),
            ]);
        }

        let close_btn = match (lang, prefix) {
            (Lang::Ru, "s") => "✖️ Закрыть поиск",
            (Lang::Ru, _) => "✖️ Закрыть видео",
            (Lang::Uk, "s") => "✖️ Закрити пошук",
            (Lang::Uk, _) => "✖️ Закрити відео",
            (Lang::Es, "s") => "✖️ Cerrar búsqueda",
            (Lang::Es, _) => "✖️ Cerrar videos",
            (_, "s") => "✖️ Close search",
            (_, _) => "✖️ Close videos",
        };

        rows.push(vec![json!({ "text": close_btn, "callback_data": format!("{prefix}:close:{token}") })]);
    }

    json!({ "inline_keyboard": rows })
}

async fn handle_search_command(app: &Arc<App>, msg: &Message, ctx: &Ctx, args: &str) {
    let user_id = ctx.user;
    let (chat, thread) = (ctx.chat, ctx.thread);
    let lang = ctx.lang;
    let query = args.trim();

    // 1. Check if user is premium
    if !app.store.is_user_premium(user_id) {
        let prompt = match lang {
            Lang::Ru => "⭐ <b>Поиск по видео доступен только с Premium</b>\n\n\
                         С Premium вы можете искать любые видео по названию прямо в Telegram и скачивать их в 1 клик.",
            Lang::Uk => "⭐ <b>Пошук відео доступний лише з Premium</b>\n\n\
                         З Premium ви можете шукати будь-які відео за назвою прямо в Telegram та завантажувати їх в 1 клік.",
            Lang::Es => "⭐ <b>La búsqueda de videos solo está disponible con Premium</b>\n\n\
                         Con Premium puedes buscar videos por título directamente en Telegram y descargarlos en 1 clic.",
            _ => "⭐ <b>Video search is only available with Premium</b>\n\n\
                  With Premium you can search any video by title directly in Telegram and download with 1 click.",
        };
        let kb = premium_upsell_keyboard(app, user_id, lang);
        let _ = app.bot.send_message(chat, thread, prompt, Some(kb), Some(msg.message_id)).await;
        return;
    }

    // 2. Check query
    if query.is_empty() {
        let hint = match lang {
            Lang::Ru => "🔍 <b>Поиск видео</b>\n\nИспользование: <code>/search &lt;запрос&gt;</code>\nПример: <code>/search funny cats</code>",
            Lang::Uk => "🔍 <b>Пошук відео</b>\n\nВикористання: <code>/search &lt;запит&gt;</code>\nПриклад: <code>/search funny cats</code>",
            Lang::Es => "🔍 <b>Búsqueda de videos</b>\n\nUso: <code>/search &lt;búsqueda&gt;</code>\nEjemplo: <code>/search funny cats</code>",
            _ => "🔍 <b>Video Search</b>\n\nUsage: <code>/search &lt;query&gt;</code>\nExample: <code>/search funny cats</code>",
        };
        app.say(chat, thread, hint, None).await;
        return;
    }

    let searching_text = match lang {
        Lang::Ru => format!("🔍 <i>Ищем видео по запросу «{}»...</i>", util::html_escape(query)),
        Lang::Uk => format!("🔍 <i>Шукаємо відео за запитом «{}»...</i>", util::html_escape(query)),
        Lang::Es => format!("🔍 <i>Buscando videos para «{}»...</i>", util::html_escape(query)),
        _ => format!("🔍 <i>Searching videos for «{}»...</i>", util::html_escape(query)),
    };

    let sent = app.bot.send_message(chat, thread, &searching_text, None, Some(msg.message_id)).await;
    let status_msg_id = sent.ok().map(|m| m.message_id);

    let results = crate::search::search_youtube(&app.cfg, &app.search_slots, query, 9).await;

    match results {
        Ok(items) if !items.is_empty() => {
            if let Some(msg_id) = status_msg_id {
                let _ = app.bot.delete_message(chat, msg_id).await;
            }

            let token = new_session_token();
            let total_pages = items.len().div_ceil(3);
            let page_0_items: Vec<_> = items.iter().take(3).collect();
            let count = page_0_items.len();
            let mut sent_msg_ids = Vec::new();

            for (idx, item) in page_0_items.into_iter().enumerate() {
                let (caption, thumb_url) = render_card_content(item);
                let is_last = idx == count - 1;
                let page_nav = if is_last { Some((0, total_pages, "s", token.as_str())) } else { None };
                let markup = render_card_markup(item, lang, page_nav);

                match app.bot.send_photo(chat, thread, &thumb_url, &caption, Some(markup.clone()), None).await {
                    Ok(sent_msg) => {
                        sent_msg_ids.push(sent_msg.message_id);
                    }
                    Err(_) => {
                        if let Ok(sent_msg) = app.bot.send_message(chat, thread, &caption, Some(markup), None).await {
                            sent_msg_ids.push(sent_msg.message_id);
                        }
                    }
                }
            }

            save_card_session(
                token,
                CardSession { created_at: std::time::Instant::now(), items, msg_ids: sent_msg_ids, prefix: "s", lang },
            );
        }
        Ok(_) => {
            let not_found = match lang {
                Lang::Ru => format!("❌ По запросу «{}» ничего не найдено.", util::html_escape(query)),
                Lang::Uk => format!("❌ За запитом «{}» нічого не знайдено.", util::html_escape(query)),
                Lang::Es => format!("❌ No se encontraron resultados para «{}».", util::html_escape(query)),
                _ => format!("❌ No videos found for «{}».", util::html_escape(query)),
            };
            if let Some(msg_id) = status_msg_id {
                let _ = app.bot.edit_message(chat, msg_id, &not_found, None).await;
            } else {
                let _ = app.bot.send_message(chat, thread, &not_found, None, Some(msg.message_id)).await;
            }
        }
        Err(err) => {
            log::warn!("search error: {err}");
            let error_text = match lang {
                Lang::Ru => "⚠️ Не удалось выполнить поиск. Попробуйте другой запрос позже.",
                Lang::Uk => "⚠️ Не вдалося виконати пошук. Спробуйте інший запит пізніше.",
                Lang::Es => "⚠️ Error al realizar la búsqueda. Inténtalo de nuevo más tarde.",
                _ => "⚠️ Failed to execute search. Please try another query later.",
            };
            if let Some(msg_id) = status_msg_id {
                let _ = app.bot.edit_message(chat, msg_id, error_text, None).await;
            } else {
                let _ = app.bot.send_message(chat, thread, error_text, None, Some(msg.message_id)).await;
            }
        }
    }
}

async fn send_backup(app: &Arc<App>, admin_user_id: i64, origin_chat_id: i64, origin_thread_id: Option<i64>) {
    let now = state::now_secs();
    let file_name = format!("bot_backup_{now}.db");
    let backup_path = app.cfg.temp_dir.join(&file_name);
    let _ = std::fs::create_dir_all(&app.cfg.temp_dir);

    if let Err(e) = app.stats.backup(&backup_path) {
        log::error!("hot backup failed: {e}");
        if origin_chat_id == admin_user_id {
            app.say(admin_user_id, None, "❌ Не удалось создать бэкап базы данных.", None).await;
        } else {
            app.say(origin_chat_id, origin_thread_id, "❌ Не удалось создать бэкап базы данных.", None).await;
        }
        return;
    }

    let media = Media {
        kind: MediaKind::Document,
        chat_id: admin_user_id, // ALWAYS send to admin's private chat!
        thread_id: None,
        source: Source::Upload(&backup_path),
        file_name: &file_name,
        caption: Some("💾 <b>Резервная копия базы данных</b> (SQLite: <code>bot.db</code>)"),
        duration: None,
        width: None,
        height: None,
        title: None,
        performer: None,
        thumb: None,
        markup: None,
    };

    match app.bot.send_media(&media, None).await {
        Ok(_) => {
            if origin_chat_id != admin_user_id {
                app.say(
                    origin_chat_id,
                    origin_thread_id,
                    "📩 Бэкап базы данных отправлен вам в личные сообщения.",
                    None,
                )
                .await;
            }
        }
        Err(e) => {
            log::error!("failed to send backup to {admin_user_id}: {e}");
            let err_msg = format!("❌ Не удалось отправить бэкап в ЛС: {e}. Убедитесь, что вы запустили бота в личных сообщениях (/start).");
            if origin_chat_id == admin_user_id {
                app.say(admin_user_id, None, &err_msg, None).await;
            } else {
                app.say(origin_chat_id, origin_thread_id, &err_msg, None).await;
            }
        }
    }

    let _ = std::fs::remove_file(&backup_path);
}

fn render_system_status(app: &App) -> String {
    let uptime = util::fmt_duration(app.uptime().as_secs());
    let active = app.active_jobs();
    let cache_entries = app.store.cache_count();
    let mem = sys::process_memory_mb().map(|m| format!("{m} MB")).unwrap_or_else(|| "n/a".to_string());
    let disk = sys::free_space_mb(&app.cfg.temp_dir).map(|d| format!("{d} MB")).unwrap_or_else(|| "n/a".to_string());

    format!(
        "⚙️ <b>Системный статус и конфигурация</b>\n\n\
         • <b>Версия:</b> <code>tg-media-bot {}</code>\n\
         • <b>Uptime:</b> {}\n\
         • <b>Активные процессы:</b> {}\n\
         • <b>Записей в кэше:</b> {}\n\
         • <b>RAM (RSS):</b> {}\n\
         • <b>Свободно на диске:</b> {}\n\n\
         ⚙️ <b>Параметры и лимиты:</b>\n\
         • Параллельные загрузки: {} (на пользователя: {})\n\
         • Макс. размер файла: {}\n\
         • Макс. длительность: {}\n\
         • Таймаут: {}\n\
         • Разрешённые пользователи: {}\n\
         • Администраторы: {}",
        env!("CARGO_PKG_VERSION"),
        uptime,
        active,
        cache_entries,
        mem,
        disk,
        app.cfg.max_parallel,
        app.cfg.per_user,
        util::fmt_size(app.cfg.max_file_bytes),
        if app.cfg.max_duration > 0 {
            util::fmt_duration(app.cfg.max_duration)
        } else {
            "без ограничений".into()
        },
        util::fmt_duration(app.cfg.download_timeout),
        if app.cfg.allowed_users.is_empty() {
            "все (публичный)".into()
        } else {
            app.cfg.allowed_users.len().to_string()
        },
        if app.cfg.admin_users.is_empty() {
            "разрешённые пользователи".into()
        } else {
            app.cfg.admin_users.len().to_string()
        },
    )
}

fn status_menu(app: &App, user_id: i64, lang: Lang) -> (String, Value) {
    let t = lang.t();
    let jobs = app.user_jobs(user_id);
    let mut text = fill(t.status_header, &[("n", &jobs.len().to_string()), ("max", &app.cfg.user_queue.to_string())]);
    let mut ordered: Vec<_> = jobs.iter().collect();
    ordered.sort_by_key(|j| (*j.stage.borrow() == Stage::Queued, j.seq));

    let mut buttons: Vec<Value> = Vec::new();
    for job in ordered.iter().take(STATUS_LINES) {
        let stage = job.stage.borrow().clone();
        let title_line = match job.title() {
            Some(title) if !title.trim().is_empty() => {
                format!("  🎬 <b>{}</b>\n", util::html_escape(util::truncate_chars(&title, 80)))
            }
            _ => format!("  🎬 <i>{}</i>\n", t.st_title_pending),
        };
        text.push_str(&format!(
            "\n\n• <code>{}</code> {} · {}\n{}  🔗 {}",
            job.id,
            stage_label(t, &stage),
            job.quality.label(t),
            title_line,
            util::html_escape(util::truncate_chars(&job.url, 70))
        ));

        let btn_label = match job.title() {
            Some(title) if !title.trim().is_empty() => {
                format!("✖ {} · {}", job.id, util::truncate_chars(&title, 20))
            }
            _ => format!("✖ {}", job.id),
        };
        buttons.push(json!({
            "text": btn_label,
            "callback_data": format!("c:{}", job.id),
        }));
    }

    if jobs.len() > STATUS_LINES {
        text.push('\n');
        text.push_str(&fill(t.status_more, &[("n", &(jobs.len() - STATUS_LINES).to_string())]));
    }

    let mut rows: Vec<Vec<Value>> = buttons.into_iter().map(|b| vec![b]).collect();
    if jobs.len() > 1 {
        rows.push(vec![json!({
            "text": t.btn_cancel_all,
            "callback_data": "c:all",
        })]);
    }

    (text, json!({ "inline_keyboard": rows }))
}

fn quality_name(t: &Strings, q: Option<Quality>) -> String {
    q.map(|q| q.label(t)).unwrap_or_else(|| t.word_ask.to_string())
}

enum Access {
    Granted,
    Denied,
    /// Outside the chat's locked topic: stay silent.
    Ignored,
}

fn check_access(app: &App, msg: &Message, user: Option<&User>, is_topic_cmd: bool) -> Access {
    let chat = &msg.chat;
    if chat.is_group() && !is_topic_cmd {
        if let Some(locked) = app.store.topic(chat.id) {
            if msg.thread() != Some(locked) {
                return Access::Ignored;
            }
        }
    }
    if app.cfg.allowed_users.is_empty() {
        return Access::Granted;
    }
    if user.is_some_and(|u| app.cfg.allowed_users.contains(&u.id)) {
        if chat.is_group() && !app.store.is_active(chat.id) {
            log::info!("group {} activated by an allowed user", chat.id);
            app.store.activate(chat.id);
        }
        return Access::Granted;
    }
    if chat.is_group() && app.store.is_active(chat.id) {
        return Access::Granted;
    }
    Access::Denied
}

pub async fn handle(app: Arc<App>, update: Update) {
    if let Some(msg) = update.message {
        handle_message(&app, msg).await;
    } else if let Some(cb) = update.callback_query {
        handle_callback(&app, cb).await;
    } else if let Some(iq) = update.inline_query {
        handle_inline_query(&app, iq).await;
    } else if let Some(pcq) = update.pre_checkout_query {
        let bot = app.bot.clone();
        tokio::spawn(async move {
            let _ = bot.answer_pre_checkout_query(&pcq.id, true, None).await;
        });
    }
}

/// Split "/cmd@bot args" into ("cmd", args). None if addressed to another bot.
fn parse_command<'a>(text: &'a str, bot_username: &str) -> Option<(String, &'a str)> {
    let rest = text.strip_prefix('/')?;
    let (head, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let (name, target) = head.split_once('@').unwrap_or((head, ""));
    if !target.is_empty() && !target.eq_ignore_ascii_case(bot_username) {
        return None;
    }
    Some((name.to_ascii_lowercase(), args.trim()))
}

fn urls_of(msg: &Message, text: &str) -> Vec<String> {
    let mut urls = util::extract_urls(text, MAX_LINKS_PER_MESSAGE);
    for e in msg.entities.iter().chain(&msg.caption_entities) {
        if let Some(u) = e.url.as_deref().filter(|_| e.kind == "text_link") {
            if urls.len() < MAX_LINKS_PER_MESSAGE && util::is_valid_url(u) && !urls.iter().any(|x| x == u) {
                urls.push(u.to_string());
            }
        }
    }
    urls
}

/// Who asked, where, in which language.
struct Ctx {
    chat: i64,
    thread: Option<i64>,
    user: i64,
    lang: Lang,
}

/// Ask which quality to use for `urls` (one question for the whole message).
async fn ask_quality(app: &Arc<App>, ctx: &Ctx, urls: Vec<String>, user_msg_id: Option<i64>) {
    let t = ctx.lang.t();
    let mut duration_secs = None;
    let mut title_opt = None;

    if urls.len() == 1 {
        let url = &urls[0];
        let vkey = cachekey::video_key(url);
        let candidates = vec![cachekey::request_key(&vkey, Quality::Max), cachekey::request_key(&vkey, Quality::Audio)];
        if let Some(entry) = app.store.cache_find(&candidates) {
            if entry.duration > 0 {
                duration_secs = Some(entry.duration as u64);
            }
            if !entry.title.is_empty() {
                title_opt = Some(entry.title);
            }
        }

        if duration_secs.is_none() || title_opt.is_none() {
            let meta = app.meta.resolve_meta(url).await;
            if duration_secs.is_none() {
                duration_secs = meta.duration_secs;
            }
            if title_opt.is_none() {
                title_opt = meta.title;
            }
        }
    }

    let text = if urls.len() == 1 {
        if let Some(title) = title_opt {
            if let Some(dur) = duration_secs {
                format!(
                    "🎚 <b>{}</b>\n⏱ <b>{}</b>\n\n{}",
                    util::html_escape(&title),
                    util::fmt_duration(dur),
                    t.ask_quality
                )
            } else {
                format!("🎚 <b>{}</b>\n\n{}", util::html_escape(&title), t.ask_quality)
            }
        } else {
            t.ask_quality.to_string()
        }
    } else {
        fill(t.ask_quality_many, &[("n", &urls.len().to_string())])
    };

    let limit_bytes = app.cfg.max_file_bytes.min(2000 * 1024 * 1024);
    let token = app.stash_urls(ctx.user, urls, user_msg_id);
    let kb = picker_keyboard(t, &token, app.cfg.max_height, duration_secs, limit_bytes);
    app.say(ctx.chat, ctx.thread, &text, Some(kb)).await;
}

/// Queue `urls` with `quality` and report the result. `picker` is the
/// question message, if there was one: a single link turns it into its status
/// message, several links turn it into a summary.
async fn enqueue_all(
    app: &Arc<App>,
    ctx: &Ctx,
    urls: Vec<String>,
    quality: Quality,
    picker: Option<i64>,
    user_msg_id: Option<i64>,
) {
    let t = ctx.lang.t();
    let single = urls.len() == 1;
    let (mut added, mut skipped) = (0usize, 0usize);
    let mut problems: Vec<String> = Vec::new();
    let mut minimal = app.store.minimal(ctx.chat);

    if single {
        if let Some(msg) = picker {
            let _ = app.bot.delete_message(ctx.chat, msg).await;
        }
    }

    for url in urls {
        let req = Request {
            chat_id: ctx.chat,
            thread_id: ctx.thread,
            user_id: ctx.user,
            url,
            quality,
            lang: ctx.lang,
            status_msg: None,
            user_msg_id,
        };
        match app.enqueue(req) {
            Ok(job) => {
                added += 1;
                minimal = job.minimal;
            }
            Err(Refusal::UserQueueFull) => skipped += 1,
            Err(r) => {
                let text = r.text(t, 0, 0);
                if !problems.contains(&text) {
                    problems.push(text);
                }
            }
        }
    }

    if single && added == 1 {
        if minimal {
            if let Some(msg) = picker {
                let _ = app.bot.delete_message(ctx.chat, msg).await;
            }
        }
        return;
    }

    let mut lines = Vec::new();
    if added > 0 && !single && skipped == 0 {
        lines.push(fill(t.queued_n, &[("choice", &quality.label(t)), ("n", &added.to_string())]));
    }
    if skipped > 0 {
        let max = app.cfg.user_queue.to_string();
        if added == 0 {
            let queued = app.cfg.user_queue - app.user_room(ctx.user);
            lines.push(fill(t.user_queue_full, &[("n", &queued.to_string()), ("max", &max)]));
        } else {
            lines.push(fill(
                t.queue_partial,
                &[("n", &added.to_string()), ("skipped", &skipped.to_string()), ("max", &max)],
            ));
        }
    }
    lines.extend(problems);
    if lines.is_empty() || (minimal && skipped == 0 && lines.len() == 1 && added > 0) {
        if let Some(msg) = picker {
            let _ = app.bot.delete_message(ctx.chat, msg).await;
        }
        return;
    }
    let text = lines.join("\n");
    let summary_msg: Option<i64> = match picker {
        Some(msg) => {
            if app.bot.edit_message(ctx.chat, msg, &text, None).await.is_err() {
                app.bot.send_message(ctx.chat, ctx.thread, &text, None, user_msg_id).await.ok().map(|m| m.message_id)
            } else {
                Some(msg)
            }
        }
        None => app.bot.send_message(ctx.chat, ctx.thread, &text, None, user_msg_id).await.ok().map(|m| m.message_id),
    };
    if skipped == 0 && added > 0 {
        if let Some(msg_id) = summary_msg {
            let bot = app.bot.clone();
            let chat_id = ctx.chat;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let _ = bot.delete_message(chat_id, msg_id).await;
            });
        }
    }
}

async fn handle_successful_payment(
    app: &Arc<App>,
    ctx: &Ctx,
    user: &crate::telegram::User,
    payment: &crate::telegram::SuccessfulPayment,
) {
    if payment.currency == "XTR" {
        let days: u32 = match payment.invoice_payload.as_str() {
            "premium_90d" => 90,
            _ => 30,
        };
        let new_expires = app.store.add_premium_days(ctx.user, days);
        let date_str = crate::util::format_date_str(new_expires as i64);

        log::info!(
            "Payment received: user_id={} username={:?} amount={} {} payload={} (new expiry: {})",
            ctx.user,
            user.username,
            payment.total_amount,
            payment.currency,
            payment.invoice_payload,
            date_str
        );

        let text = match ctx.lang {
            Lang::Ru => format!(
                "🎉 <b>Спасибо за поддержку!</b>\n\n\
                 Ваша подписка Premium продлена на <b>{days} дней</b> (активна до <b>{date_str}</b>).\n\n\
                 Вам доступны поиск видео, чтение комментариев и отслеживание YouTube-каналов!"
            ),
            Lang::Uk => format!(
                "🎉 <b>Дякуємо за підтримку!</b>\n\n\
                 Ваша підписка Premium подовжена на <b>{days} днів</b> (активна до <b>{date_str}</b>).\n\n\
                 Вам доступні пошук відео, читання коментарів та відстеження YouTube-каналів!"
            ),
            Lang::Es => format!(
                "🎉 <b>¡Gracias por tu apoyo!</b>\n\n\
                 Tu suscripción Premium se ha extendido por <b>{days} días</b> (activa hasta <b>{date_str}</b>).\n\n\
                 ¡Tienes acceso a búsqueda de videos, comentarios y seguimiento de canales!"
            ),
            _ => format!(
                "🎉 <b>Thank you for your support!</b>\n\n\
                 Your Premium subscription has been extended by <b>{days} days</b> (active until <b>{date_str}</b>).\n\n\
                 Video search, comments, and channel tracking are now available!"
            ),
        };
        let _ = app.bot.send_message(ctx.chat, ctx.thread, &text, None, None).await;

        // Notify admins
        let admin_ids: Vec<i64> = if !app.cfg.admin_users.is_empty() {
            app.cfg.admin_users.iter().copied().collect()
        } else {
            app.cfg.allowed_users.iter().copied().collect()
        };

        if !admin_ids.is_empty() {
            let user_info = match (&user.username, &user.first_name) {
                (Some(uname), Some(fname)) => {
                    format!("{} (@{})", util::html_escape(fname), util::html_escape(uname))
                }
                (Some(uname), None) => format!("@{}", util::html_escape(uname)),
                (None, Some(fname)) => util::html_escape(fname),
                (None, None) => "—".to_string(),
            };

            let admin_msg = format!(
                "⭐ <b>Новая оплата Premium!</b>\n\n\
                 👤 <b>Пользователь:</b> {user_info}\n\
                 🆔 <b>ID:</b> <code>{}</code>\n\
                 💰 <b>Сумма:</b> {} ⭐ (Telegram Stars)\n\
                 ⏳ <b>Период:</b> +{days} дн. (до {date_str})\n\
                 🧾 <b>Тариф:</b> <code>{}</code>",
                ctx.user, payment.total_amount, payment.invoice_payload
            );

            for admin_id in admin_ids {
                app.say(admin_id, None, &admin_msg, None).await;
            }
        }
    }
}

async fn handle_message(app: &Arc<App>, msg: Message) {
    let Some(user) = msg.from.clone() else { return };
    if user.is_bot {
        return;
    }
    app.stats.record_user(user.id, user.username.as_deref(), user.first_name.as_deref());

    let lang = app.lang_for(msg.chat.id, Some(&user));
    let t = lang.t();
    let ctx = Ctx { chat: msg.chat.id, thread: msg.thread(), user: user.id, lang };

    if let Some(payment) = &msg.successful_payment {
        handle_successful_payment(app, &ctx, &user, payment).await;
        return;
    }

    let text = msg.text.as_deref().or(msg.caption.as_deref()).unwrap_or("").to_string();
    let command = parse_command(&text, &app.bot.username);
    if text.starts_with('/') && command.is_none() {
        return; // a command for some other bot in the group
    }
    let is_topic_cmd = command.as_ref().is_some_and(|(c, _)| c == "topic");

    match check_access(app, &msg, Some(&user), is_topic_cmd) {
        Access::Granted => {}
        Access::Ignored => return,
        Access::Denied => {
            log::warn!("denied user {} in chat {}", user.id, ctx.chat);
            // In groups, only answer people who actually talk to the bot.
            if !msg.chat.is_group() || command.is_some() {
                app.say(ctx.chat, ctx.thread, t.not_authorized, None).await;
            }
            return;
        }
    }

    if let Some((cmd, args)) = command {
        run_command(app, &msg, &ctx, &cmd, args).await;
        return;
    }

    if !msg.chat.is_group() {
        if let Some(action) = parse_menu_action(&text) {
            handle_menu_action(app, &msg, &ctx, action).await;
            return;
        }
    }

    let urls = urls_of(&msg, &text);
    if urls.is_empty() {
        let addressed = !msg.chat.is_group()
            || msg.reply_to_message.as_ref().and_then(|r| r.from.as_ref()).is_some_and(|f| f.id == app.bot.id)
            || (!app.bot.username.is_empty()
                && text.to_lowercase().contains(&format!("@{}", app.bot.username.to_lowercase())));
        if addressed && msg.text.is_some() {
            let query = text.trim();
            if !msg.chat.is_group() && app.store.is_user_premium(user.id) && query.len() >= 2 && !query.starts_with('/')
            {
                handle_search_command(app, &msg, &ctx, query).await;
                return;
            }
            app.say(ctx.chat, ctx.thread, t.nudge, None).await;
        }
        return;
    }

    if app.cfg.subs_enabled && !msg.chat.is_group() && urls.len() == 1 {
        let first_url = &urls[0];
        if crate::subs::resolver::is_channel_url(first_url) {
            handle_channel_subscription_request(app, &ctx, first_url, Some(msg.message_id)).await;
            return;
        }
    }

    let room = app.user_room(user.id);
    if room == 0 {
        let text = Refusal::UserQueueFull.text(t, app.cfg.user_queue, app.cfg.user_queue);
        app.say(ctx.chat, ctx.thread, &text, None).await;
        return;
    }
    match app.store.default_quality(user.id) {
        Some(q) => enqueue_all(app, &ctx, urls, q, None, Some(msg.message_id)).await,
        None => ask_quality(app, &ctx, urls, Some(msg.message_id)).await,
    }
}

async fn run_command(app: &Arc<App>, msg: &Message, ctx: &Ctx, cmd: &str, args: &str) {
    let t = ctx.lang.t();
    let (chat, thread) = (ctx.chat, ctx.thread);
    let mut words = args.split_whitespace();
    let first = words.next().map(str::to_ascii_lowercase);

    match cmd {
        "start" => {
            if let Some(token) = args.strip_prefix("dl_").filter(|s| !s.is_empty()) {
                if let Ok((urls, _)) = app.take_urls(token, ctx.user) {
                    if !urls.is_empty() {
                        let room = app.user_room(ctx.user);
                        if room == 0 {
                            let text = Refusal::UserQueueFull.text(t, app.cfg.user_queue, app.cfg.user_queue);
                            app.say(chat, thread, &text, None).await;
                            return;
                        }
                        match app.store.default_quality(ctx.user) {
                            Some(q) => enqueue_all(app, ctx, urls, q, None, None).await,
                            None => ask_quality(app, ctx, urls, None).await,
                        }
                        return;
                    }
                }
            }
            app.say(chat, thread, &format!("{}\n\n{LANG_PROMPT}", t.welcome), Some(lang_keyboard())).await;
            if !msg.chat.is_group() {
                let menu_hint = match ctx.lang {
                    Lang::Ru => "👇 Используйте меню снизу для быстрого доступа к функциям:",
                    Lang::Uk => "👇 Використовуйте меню знизу для швидкого доступу до функцій:",
                    Lang::Es => "👇 Usa el menú de abajo para acceder rápidamente a las funciones:",
                    _ => "👇 Use the menu below for quick navigation:",
                };
                let _ = app.bot.send_message(chat, thread, menu_hint, Some(main_menu_keyboard(ctx.lang)), None).await;
            }
        }
        "help" => {
            let mut limits = fill(
                t.help_limits,
                &[("size", &util::fmt_size(app.cfg.max_file_bytes)), ("height", &app.cfg.max_height.to_string())],
            );
            if app.cfg.max_duration > 0 {
                limits.push_str(&fill(t.help_limit_duration, &[("dur", &util::fmt_duration(app.cfg.max_duration))]));
            }
            app.say(chat, thread, &format!("{}\n\n{limits}", t.help), None).await;
        }
        "search" | "find" | "s" => {
            handle_search_command(app, msg, ctx, args).await;
        }
        "comments" | "comm" => {
            let urls = urls_of(msg, args);
            if urls.is_empty() {
                let hint = match ctx.lang {
                    Lang::Ru => "ℹ️ Чтобы прочитать топ-комментарии, укажите ссылку на видео:\n<code>/comments https://youtube.com/watch?v=...</code>",
                    Lang::Uk => "ℹ️ Щоб прочитати топ-коментарі, надішліть посилання на відео:\n<code>/comments https://youtube.com/watch?v=...</code>",
                    Lang::Es => "ℹ️ Para ver los mejores comentarios, proporciona un enlace al video:\n<code>/comments https://youtube.com/watch?v=...</code>",
                    _ => "ℹ️ To read top comments, provide a video link:\n<code>/comments https://youtube.com/watch?v=...</code>",
                };
                app.say(chat, thread, hint, None).await;
                return;
            }
            let url = &urls[0];
            let vid_opt =
                crate::cachekey::local_video_key(url).and_then(|k| k.strip_prefix("youtube:").map(str::to_string));
            let Some(vid) = vid_opt else {
                let fail = match ctx.lang {
                    Lang::Ru => "⚠️ Чтение комментариев поддерживается для YouTube-видео.",
                    Lang::Uk => "⚠️ Читання коментарів підтримується для YouTube-відео.",
                    Lang::Es => "⚠️ Los comentarios son compatibles con videos de YouTube.",
                    _ => "⚠️ Comments are supported for YouTube videos.",
                };
                app.say(chat, thread, fail, None).await;
                return;
            };

            let is_premium = app.store.is_user_premium(ctx.user);
            if !is_premium {
                let prompt = match ctx.lang {
                    Lang::Ru => "⭐ <b>Чтение комментариев доступно только с Premium</b>",
                    Lang::Uk => "⭐ <b>Читання коментарів доступне лише з Premium</b>",
                    Lang::Es => "⭐ <b>La lectura de comentarios solo está disponible con Premium</b>",
                    _ => "⭐ <b>Reading comments is only available with Premium</b>",
                };
                let kb = premium_upsell_keyboard(app, ctx.user, ctx.lang);
                let _ = app.bot.send_message(chat, thread, prompt, Some(kb), Some(msg.message_id)).await;
                return;
            }

            let loading = match ctx.lang {
                Lang::Ru => "💬 <i>Загружаю комментарии...</i>",
                Lang::Uk => "💬 <i>Завантажую коментарі...</i>",
                Lang::Es => "💬 <i>Cargando comentarios...</i>",
                _ => "💬 <i>Loading comments...</i>",
            };
            let status_msg_id = app
                .bot
                .send_message(chat, thread, loading, None, Some(msg.message_id))
                .await
                .ok()
                .map(|m| m.message_id);

            let app_clone = app.clone();
            let lang = ctx.lang;
            tokio::spawn(async move {
                match crate::comments::fetch_comments(&app_clone.cfg, &app_clone.comment_slots, &vid).await {
                    Ok((title, items)) => {
                        let (text, kb) = crate::comments::render_comments_page(&vid, title.as_deref(), &items, 0, lang);
                        if let Some(mid) = status_msg_id {
                            let _ = app_clone.bot.edit_message(chat, mid, &text, Some(kb)).await;
                        }
                    }
                    Err(e) => {
                        log::warn!("Fetch comments for {vid} failed: {e}");
                        let err_text = match lang {
                            Lang::Ru => "⚠️ Не удалось загрузить комментарии к этому видео (возможно, они отключены).",
                            Lang::Uk => "⚠️ Не вдалося завантажити коментарі до цього відео.",
                            Lang::Es => "⚠️ No se pudieron cargar los comentarios de este video.",
                            _ => "⚠️ Failed to load comments for this video.",
                        };
                        if let Some(mid) = status_msg_id {
                            let _ = app_clone.bot.edit_message(chat, mid, err_text, None).await;
                        }
                    }
                }
            });
        }
        "premium" => {
            let (text, kb) = premium_menu(app, ctx.user, ctx.lang);
            app.say(chat, thread, &text, Some(kb)).await;
        }
        "promo" | "code" => {
            let code = args.trim();
            if code.is_empty() {
                let hint = match ctx.lang {
                    Lang::Ru => "🎟 <b>Активация промокода</b>\n\nИспользование: <code>/promo ВАШ_ПРОМОКОД</code>\n\nЕсли у вас есть промокод на бесплатный период или подарок, отправьте его вместе с этой командой.",
                    Lang::Uk => "🎟 <b>Активація промокоду</b>\n\nВикористання: <code>/promo ВАШ_ПРОМОКОД</code>",
                    Lang::Es => "🎟 <b>Canjear código promocional</b>\n\nUso: <code>/promo TU_CODIGO</code>",
                    _ => "🎟 <b>Promo Code Activation</b>\n\nUsage: <code>/promo YOUR_CODE</code>",
                };
                app.say(chat, thread, hint, None).await;
                return;
            }

            match app.store.activate_promocode(ctx.user, code) {
                Ok(res) => {
                    let date_str = crate::util::format_date_str(res.expires_at as i64);
                    let success_msg = match ctx.lang {
                        Lang::Ru => format!(
                            "🎉 <b>Промокод «{}» успешно активирован!</b>\n\n\
                             Вам начислено <b>{} дней</b> подписки Premium.\n\
                             Подписка активна до: <b>{}</b>\n\n\
                             Вам доступны: поиск видео (/search), чтение комментариев и отслеживание каналов (/subscriptions)!",
                            res.code, res.duration_days, date_str
                        ),
                        Lang::Uk => format!(
                            "🎉 <b>Промокод «{}» успішно активовано!</b>\n\n\
                             Вам нараховано <b>{} днів</b> підписки Premium.\n\
                             Підписка активна до: <b>{}</b>",
                            res.code, res.duration_days, date_str
                        ),
                        Lang::Es => format!(
                            "🎉 <b>¡Código «{}» activado con éxito!</b>\n\n\
                             Has recibido <b>{} días</b> de suscripción Premium.\n\
                             Activa hasta: <b>{}</b>",
                            res.code, res.duration_days, date_str
                        ),
                        _ => format!(
                            "🎉 <b>Promo code «{}» successfully activated!</b>\n\n\
                             You received <b>{} days</b> of Premium subscription.\n\
                             Active until: <b>{}</b>",
                            res.code, res.duration_days, date_str
                        ),
                    };
                    app.say(chat, thread, &success_msg, None).await;
                }
                Err(crate::state::PromoError::NotFound) => {
                    let err_msg = match ctx.lang {
                        Lang::Ru => "❌ Промокод не найден. Проверьте правильность написания и попробуйте снова.",
                        Lang::Uk => "❌ Промокод не знайдено. Перевірте правильність написання.",
                        Lang::Es => "❌ Código promocional no encontrado. Verifica que esté bien escrito.",
                        _ => "❌ Promo code not found. Please check spelling and try again.",
                    };
                    app.say(chat, thread, err_msg, None).await;
                }
                Err(crate::state::PromoError::AlreadyUsedByYou) => {
                    let err_msg = match ctx.lang {
                        Lang::Ru => "⚠️ Вы уже активировали этот промокод ранее на своём аккаунте.",
                        Lang::Uk => "⚠️ Ви вже активували цей промокод раніше на своєму акаунті.",
                        Lang::Es => "⚠️ Ya has activado este código promocional anteriormente.",
                        _ => "⚠️ You have already activated this promo code on your account.",
                    };
                    app.say(chat, thread, err_msg, None).await;
                }
                Err(crate::state::PromoError::Exhausted) => {
                    let err_msg = match ctx.lang {
                        Lang::Ru => "⌛ К сожалению, лимит активаций этого промокода уже исчерпан.",
                        Lang::Uk => "⌛ На жаль, ліміт активацій цього промокоду вже вичерпано.",
                        Lang::Es => "⌛ Lamentablemente, se ha alcanzado el límite de usos de este código.",
                        _ => "⌛ Unfortunately, this promo code has reached its activation limit.",
                    };
                    app.say(chat, thread, err_msg, None).await;
                }
                Err(crate::state::PromoError::DatabaseError) => {
                    app.say(chat, thread, "❌ Database error. Please try again later.", None).await;
                }
            }
        }
        "subscriptions" | "subs" => {
            let (text, kb) = subscriptions_menu(app, ctx.user, ctx.lang);
            app.say(chat, thread, &text, Some(kb)).await;
        }
        "sub" | "subscribe" => {
            let urls = urls_of(msg, args);
            if urls.is_empty() {
                let hint = match ctx.lang {
                    Lang::Ru => "ℹ️ Чтобы подписаться на YouTube-канал, отправьте команду со ссылкой:\n<code>/sub https://youtube.com/@канал</code>",
                    _ => "ℹ️ To subscribe to a YouTube channel, send the command with a link:\n<code>/sub https://youtube.com/@channel</code>",
                };
                app.say(chat, thread, hint, None).await;
                return;
            }
            let url = &urls[0];
            if crate::subs::resolver::is_video_url(url) {
                let warn = match ctx.lang {
                    Lang::Ru => "⚠️ Это ссылка на видео, а не на канал. Чтобы подписаться на новые выпуски автора, отправьте ссылку на сам канал (например, <code>https://youtube.com/@имя_канала</code>).",
                    _ => "⚠️ This is a video link, not a channel link. To subscribe to new releases, send a channel link (e.g. <code>https://youtube.com/@channel</code>).",
                };
                app.say(chat, thread, warn, None).await;
                return;
            }
            if crate::subs::resolver::is_channel_url(url) {
                handle_channel_subscription_request(app, ctx, url, Some(msg.message_id)).await;
            } else {
                let fail = match ctx.lang {
                    Lang::Ru => "⚠️ Пожалуйста, укажите корректную ссылку на YouTube-канал (например, <code>https://youtube.com/@имя_канала</code>).",
                    _ => "⚠️ Please provide a valid YouTube channel link (e.g. <code>https://youtube.com/@channel_name</code>).",
                };
                app.say(chat, thread, fail, None).await;
            }
        }
        "lang" | "language" => app.say(chat, thread, LANG_PROMPT, Some(lang_keyboard())).await,
        "quality" => {
            let current = quality_name(t, app.store.default_quality(ctx.user));
            let text = fill(t.quality_current, &[("q", &current)]);
            app.say(chat, thread, &text, Some(default_keyboard(t, app.cfg.max_height))).await;
        }
        // Force the question even when a default quality is set.
        "formats" => {
            let urls = urls_of(msg, args);
            if urls.is_empty() {
                app.say(chat, thread, t.formats_usage, None).await;
            } else {
                ask_quality(app, ctx, urls, Some(msg.message_id)).await;
            }
        }
        "status" => {
            let jobs = app.user_jobs(ctx.user);
            if jobs.is_empty() {
                app.say(chat, thread, t.status_empty, None).await;
                return;
            }
            let (text, markup) = status_menu(app, ctx.user, ctx.lang);
            app.say(chat, thread, &text, Some(markup)).await;
        }
        "cancel" => {
            let id = match first.as_deref() {
                Some("all") => {
                    let n = app.cancel_all(ctx.user).await;
                    app.say(chat, thread, &fill(t.cancelled_n, &[("n", &n.to_string())]), None).await;
                    return;
                }
                Some(id) => id.to_string(),
                None => {
                    let jobs = app.user_jobs(ctx.user);
                    if jobs.len() != 1 {
                        app.say(chat, thread, t.cancel_usage, None).await;
                        return;
                    }
                    jobs[0].id.clone()
                }
            };
            match app.cancel(&id, ctx.user).await {
                Cancel::Done => {}
                Cancel::Denied => app.say(chat, thread, t.cancel_denied, None).await,
                Cancel::NotFound => {
                    let text = fill(t.cancel_fail, &[("id", &util::html_escape(&id))]);
                    app.say(chat, thread, &text, None).await;
                }
            }
        }
        "clear" | "clean" => {
            if msg.chat.is_group() && !app.cfg.allowed_users.is_empty() && !app.cfg.allowed_users.contains(&ctx.user) {
                app.say(chat, thread, t.not_authorized, None).await;
                return;
            }

            let count: usize = first.as_deref().and_then(|s| s.parse().ok()).unwrap_or(100).clamp(1, 500);
            let mut remaining = count;
            let mut current_id = msg.message_id;

            while remaining > 0 && current_id > 0 {
                let batch_size = remaining.min(100);
                let start_id = current_id.saturating_sub(batch_size as i64 - 1).max(1);
                let ids: Vec<i64> = (start_id..=current_id).rev().collect();
                if ids.is_empty() {
                    break;
                }
                let _ = app.bot.delete_messages(chat, &ids).await;
                if start_id <= 1 {
                    break;
                }
                current_id = start_id - 1;
                remaining -= ids.len();
            }

            if let Ok(confirm_msg) = app.bot.send_message(chat, thread, t.cleared, None, None).await {
                let bot = app.bot.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                    let _ = bot.delete_message(chat, confirm_msg.message_id).await;
                });
            }
        }
        "minimal" => match first.as_deref() {
            Some("on") => {
                app.store.set_minimal(chat, true);
                app.say(chat, thread, t.minimal_on, None).await;
            }
            Some("off") => {
                app.store.set_minimal(chat, false);
                app.say(chat, thread, t.minimal_off, None).await;
            }
            _ => {
                let state = if app.store.minimal(chat) { t.word_on } else { t.word_off };
                app.say(chat, thread, &fill(t.minimal_status, &[("state", state)]), None).await;
            }
        },
        "settings" | "menu" => {
            let (text, kb) = settings_menu(app, ctx.user, ctx.chat, ctx.lang);
            app.say(chat, thread, &text, Some(kb)).await;
        }
        "topic" => {
            if !msg.chat.is_group() {
                app.say(chat, thread, t.topic_groups_only, None).await;
                return;
            }
            match first.as_deref().unwrap_or("status") {
                "lock" => match msg.thread() {
                    None => app.say(chat, thread, t.topic_lock_outside, None).await,
                    Some(topic) => {
                        app.store.set_topic(chat, Some(topic));
                        app.say(chat, thread, t.topic_locked, None).await;
                    }
                },
                "unlock" => {
                    app.store.set_topic(chat, None);
                    app.say(chat, thread, t.topic_unlocked, None).await;
                }
                "status" => {
                    let text = match app.store.topic(chat) {
                        None => t.topic_none.to_string(),
                        Some(id) => fill(t.topic_status, &[("id", &id.to_string())]),
                    };
                    app.say(chat, thread, &text, None).await;
                }
                _ => app.say(chat, thread, t.topic_usage, None).await,
            }
        }
        "mystats" | "me" => {
            let text = app.stats.render_user_stats(ctx.user);
            app.say(chat, thread, &text, None).await;
        }
        "stats" | "admin" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    let text = format!(
                        "ℹ️ <i>Полная статистика бота доступна только администраторам.</i>\n\n{}",
                        app.stats.render_user_stats(ctx.user)
                    );
                    app.say(chat, thread, &text, None).await;
                }
                return;
            }
            let (text, kb) = match first.as_deref() {
                Some("top") => (app.stats.render_top_users_period(0), top_keyboard(0)),
                Some("plat" | "platforms") => (app.stats.render_platforms(), stats_keyboard()),
                Some("trend") => (app.stats.render_trend(), stats_keyboard()),
                Some("err" | "errors") => (app.stats.render_errors(), stats_keyboard()),
                Some("sys" | "system") => (render_system_status(app), stats_keyboard()),
                _ => (app.stats.render_summary(), stats_keyboard()),
            };
            app.say(chat, thread, &text, Some(kb)).await;
        }
        "version" | "ver" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let version = env!("CARGO_PKG_VERSION");
            let ytdlp_ver = crate::ytdlp::version(&app.cfg).await.unwrap_or_else(|| "n/a".into());
            let uptime = util::fmt_duration(app.uptime().as_secs());
            let text = format!(
                "🤖 <b>tg-media-bot</b> <code>v{version}</code>\n\
                 • <b>yt-dlp:</b> <code>{ytdlp_ver}</code>\n\
                 • <b>Uptime:</b> {}\n\
                 • <b>API:</b> {}",
                uptime,
                if app.cfg.local_api { "local Bot API" } else { "api.telegram.org" }
            );
            app.say(chat, thread, &text, None).await;
        }
        "backup" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            send_backup(app, ctx.user, chat, thread).await;
        }
        "digest" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let text = app.stats.render_daily_digest();
            app.say(chat, thread, &text, None).await;
        }
        "clean_cache" | "cleancache" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let count = app.store.clear_cache();
            app.say(chat, thread, &format!("🧹 Cache cleared: {count} entries removed."), None).await;
        }
        "prune_stats" | "prunestats" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let days: u64 = first.as_deref().and_then(|s| s.parse().ok()).unwrap_or(365);
            let removed = app.stats.prune_downloads(days);
            let text = format!("🧹 Удалено <b>{removed}</b> записей скачиваний старше <b>{days}</b> дней.");
            app.say(chat, thread, &text, None).await;
        }
        "broadcast" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            if args.trim().is_empty() {
                app.say(chat, thread, "Usage: /broadcast &lt;message&gt;", None).await;
                return;
            }
            let mut targets = std::collections::HashSet::new();
            for id in app.store.known_chats() {
                targets.insert(id);
            }
            for id in app.stats.all_user_ids() {
                targets.insert(id);
            }
            for id in &app.cfg.allowed_users {
                targets.insert(*id);
            }
            targets.insert(chat);
            targets.insert(ctx.user);

            let mut sent = 0usize;
            let mut failed = 0usize;
            for target_chat in targets {
                match app.bot.send_message(target_chat, None, args, None, None).await {
                    Ok(_) => sent += 1,
                    Err(_) => failed += 1,
                }
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
            app.say(chat, thread, &format!("📢 Broadcast completed: {sent} sent, {failed} failed."), None).await;
        }
        "addpromo" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.len() < 2 {
                app.say(chat, thread, "Usage: <code>/addpromo &lt;CODE&gt; &lt;DAYS&gt; [MAX_USES]</code>\nExample: <code>/addpromo NY2027 30 100</code> (0 = unlimited)", None).await;
                return;
            }
            let code = parts[0];
            let Ok(days) = parts[1].parse::<u32>() else {
                app.say(chat, thread, "❌ Invalid days number.", None).await;
                return;
            };
            let max_uses = parts.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(0);
            match app.store.create_promocode(code, days, max_uses) {
                Ok(true) => {
                    let limit_str = if max_uses == 0 {
                        "безлимитно".to_string()
                    } else {
                        format!("{max_uses} активаций")
                    };
                    let msg = format!("✅ Промокод <code>{code}</code> успешно создан!\n• Длительность: <b>{days} дней</b>\n• Лимит: <b>{limit_str}</b>");
                    app.say(chat, thread, &msg, None).await;
                }
                Ok(false) => {
                    app.say(chat, thread, &format!("⚠️ Промокод <code>{code}</code> уже существует!"), None).await;
                }
                Err(e) => {
                    app.say(chat, thread, &format!("❌ Ошибка создания промокода: {e}"), None).await;
                }
            }
        }
        "promos" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let list = app.store.list_promocodes();
            if list.is_empty() {
                app.say(chat, thread, "Список промокодов пуст.", None).await;
                return;
            }
            let mut text = String::from("🎟 <b>Список промокодов:</b>\n\n");
            for p in list {
                let limit = if p.max_uses == 0 { "∞".to_string() } else { p.max_uses.to_string() };
                text.push_str(&format!(
                    "• <code>{}</code> — <b>{} дн.</b> (использовано: {}/{})\n",
                    p.code, p.duration_days, p.used_count, limit
                ));
            }
            app.say(chat, thread, &text, None).await;
        }
        "giftall" => {
            if !app.is_admin(ctx.user) {
                if !msg.chat.is_group() {
                    app.say(chat, thread, t.not_authorized, None).await;
                }
                return;
            }
            let parts: Vec<&str> = args.split_whitespace().collect();
            if parts.is_empty() {
                app.say(chat, thread, "Usage: <code>/giftall &lt;DAYS&gt; [HOLIDAY_MESSAGE]</code>\nExample: <code>/giftall 14 С праздником! Дарим 2 недели Premium!</code>", None).await;
                return;
            }
            let Ok(days) = parts[0].parse::<u32>() else {
                app.say(chat, thread, "❌ Неверное количество дней.", None).await;
                return;
            };
            let custom_msg = if parts.len() > 1 {
                args.split_once(char::is_whitespace).map(|(_, rest)| rest.trim()).unwrap_or("")
            } else {
                ""
            };

            let user_ids = app.stats.all_user_ids();
            let total_users = user_ids.len();
            let gifted = app.store.gift_premium_to_users(&user_ids, days);

            app.say(
                chat,
                thread,
                &format!("🎁 <b>Начислено {days} дней Premium</b> для <b>{gifted}</b> пользователей (из {total_users} найденных).\n\nЗапускаю рассылку уведомлений..."),
                None,
            ).await;

            let app_clone = app.clone();
            let admin_chat = chat;
            let admin_thread = thread;
            let note = custom_msg.to_string();
            tokio::spawn(async move {
                let mut sent = 0usize;
                let mut failed = 0usize;
                let broadcast_text = if note.is_empty() {
                    format!("🎉 <b>Праздничный подарок!</b>\n\nВам автоматически начислено <b>{days} дней</b> Premium подписки бесплатно!\n\nПроверить статус: /premium")
                } else {
                    format!("🎉 <b>Праздничный подарок!</b>\n\nВам автоматически начислено <b>{days} дней</b> Premium подписки!\n\n<i>{note}</i>\n\nПроверить статус: /premium")
                };

                for uid in user_ids {
                    match app_clone.bot.send_message(uid, None, &broadcast_text, None, None).await {
                        Ok(_) => sent += 1,
                        Err(_) => failed += 1,
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                }

                let report = format!("✅ <b>Рассылка подарка завершена!</b>\n• Доставлено: {sent}\n• Ошибок (бот заблокирован): {failed}");
                let _ = app_clone.bot.send_message(admin_chat, admin_thread, &report, None, None).await;
            });
        }
        _ => {
            if !msg.chat.is_group() {
                app.say(chat, thread, t.nudge, None).await;
            }
        }
    }
}

async fn handle_callback(app: &Arc<App>, cb: CallbackQuery) {
    app.stats.record_user(cb.from.id, cb.from.username.as_deref(), cb.from.first_name.as_deref());
    let data = cb.data.clone().unwrap_or_default();
    let Some(msg) = cb.message.as_ref() else {
        app.bot.answer_callback(&cb.id, None, false).await;
        return;
    };
    let lang = app.lang_for(msg.chat.id, Some(&cb.from));
    let t = lang.t();

    if !matches!(check_access(app, msg, Some(&cb.from), true), Access::Granted) {
        app.bot.answer_callback(&cb.id, Some(t.not_authorized), true).await;
        return;
    }

    let (prefix, rest) = data.split_once(':').unwrap_or((data.as_str(), ""));
    match prefix {
        "st" => {
            if !app.is_admin(cb.from.id) {
                app.bot.answer_callback(&cb.id, Some(t.not_authorized), true).await;
                return;
            }
            if rest == "backup" {
                app.bot.answer_callback(&cb.id, Some("Создаю бэкап базы данных..."), false).await;
                send_backup(app, cb.from.id, msg.chat.id, msg.thread()).await;
                return;
            }
            app.bot.answer_callback(&cb.id, None, false).await;
            let (text, kb) = if let Some(days_str) = rest.strip_prefix("top:") {
                let days = days_str.parse::<u64>().unwrap_or(0);
                (app.stats.render_top_users_period(days), top_keyboard(days))
            } else if rest == "top" {
                (app.stats.render_top_users_period(0), top_keyboard(0))
            } else {
                let text = match rest {
                    "platforms" => app.stats.render_platforms(),
                    "trend" => app.stats.render_trend(),
                    "errors" => app.stats.render_errors(),
                    "system" => render_system_status(app),
                    _ => app.stats.render_summary(),
                };
                (text, stats_keyboard())
            };
            if let Err(e) = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await {
                log::debug!("edit stats failed: {e}");
            }
        }
        "l" => {
            let Some(new) = Lang::from_code(rest) else {
                app.bot.answer_callback(&cb.id, None, false).await;
                return;
            };
            app.store.set_lang(msg.chat.id, new);
            app.bot.answer_callback(&cb.id, Some(new.button()), false).await;
            let text = fill(new.t().lang_set, &[("lang", new.button())]);
            let back_btn = match new {
                Lang::Ru => "« Назад в настройки",
                Lang::Uk => "« Назад до налаштувань",
                Lang::Es => "« Volver a ajustes",
                _ => "« Back to settings",
            };
            let kb = json!({
                "inline_keyboard": [[{ "text": back_btn, "callback_data": "cfg:back" }]]
            });
            if let Err(e) = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await {
                log::debug!("edit after language change failed: {e}");
            }
            // Private chats also get the command menu in the chosen language.
            if !msg.chat.is_group() {
                let commands = command_list(new.t());
                if let Err(e) = app.bot.set_commands(&commands, None, Some(msg.chat.id)).await {
                    log::debug!("setMyCommands for chat failed: {e}");
                }
                let updated_menu_hint = match new {
                    Lang::Ru => "✨ Меню обновлено на русском языке",
                    Lang::Uk => "✨ Меню оновлено українською мовою",
                    Lang::Es => "✨ Menú actualizado en español",
                    _ => "✨ Menu updated in English",
                };
                let _ = app
                    .bot
                    .send_message(msg.chat.id, msg.thread(), updated_menu_hint, Some(main_menu_keyboard(new)), None)
                    .await;
            }
        }
        // Answer to "which quality?".
        "q" => {
            let (token, code) = rest.split_once(':').unwrap_or((rest, ""));
            let Some(quality) = Quality::parse(code) else {
                app.bot.answer_callback(&cb.id, None, false).await;
                return;
            };
            let (urls, user_msg_id) = match app.take_urls(token, cb.from.id) {
                Ok(res) => res,
                Err(true) => {
                    app.bot.answer_callback(&cb.id, Some(t.not_yours), true).await;
                    return;
                }
                Err(false) => {
                    app.bot.answer_callback(&cb.id, Some(t.picker_expired), true).await;
                    return;
                }
            };
            app.bot.answer_callback(&cb.id, None, false).await;
            let ctx = Ctx { chat: msg.chat.id, thread: msg.thread(), user: cb.from.id, lang };
            enqueue_all(app, &ctx, urls, quality, Some(msg.message_id), user_msg_id).await;
        }
        // /quality: remember a default.
        "d" => {
            let choice = if rest == "ask" { None } else { Quality::parse(rest) };
            if rest != "ask" && choice.is_none() {
                app.bot.answer_callback(&cb.id, None, false).await;
                return;
            }
            app.store.set_default_quality(cb.from.id, choice);
            app.bot.answer_callback(&cb.id, None, false).await;
            let text = fill(t.quality_set, &[("q", &quality_name(t, choice))]);
            let back_btn = match lang {
                Lang::Ru => "« Назад в настройки",
                Lang::Uk => "« Назад до налаштувань",
                Lang::Es => "« Volver a ajustes",
                _ => "« Back to settings",
            };
            let kb = json!({
                "inline_keyboard": [[{ "text": back_btn, "callback_data": "cfg:back" }]]
            });
            if let Err(e) = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await {
                log::debug!("edit after quality change failed: {e}");
            }
        }
        "c" => {
            let is_status_list = msg.text.as_deref().is_some_and(|txt| txt.starts_with("📊"));
            if rest == "all" {
                let n = app.cancel_all(cb.from.id).await;
                let text = fill(t.cancelled_n, &[("n", &n.to_string())]);
                app.bot.answer_callback(&cb.id, Some(&text), false).await;
                if is_status_list {
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, t.status_empty, None).await;
                }
                return;
            }
            match app.cancel(rest, cb.from.id).await {
                Cancel::Done => {
                    app.bot.answer_callback(&cb.id, Some(t.cancelled), false).await;
                    if is_status_list {
                        let remaining = app.user_jobs(cb.from.id);
                        if remaining.is_empty() {
                            let _ = app.bot.edit_message(msg.chat.id, msg.message_id, t.status_empty, None).await;
                        } else {
                            let (text, markup) = status_menu(app, cb.from.id, lang);
                            let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(markup)).await;
                        }
                    }
                }
                Cancel::Denied => app.bot.answer_callback(&cb.id, Some(t.cancel_denied), true).await,
                Cancel::NotFound => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    if is_status_list {
                        let remaining = app.user_jobs(cb.from.id);
                        if remaining.is_empty() {
                            let _ = app.bot.edit_message(msg.chat.id, msg.message_id, t.status_empty, None).await;
                        } else {
                            let (text, markup) = status_menu(app, cb.from.id, lang);
                            let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(markup)).await;
                        }
                    }
                }
            }
        }
        "pr" => {
            if rest == "start30" {
                match app.store.activate_promocode(cb.from.id, "START30") {
                    Ok(res) => {
                        let date_str = crate::util::format_date_str(res.expires_at as i64);
                        let alert = match lang {
                            Lang::Ru => format!("✨ Промокод активирован! Premium на 30 дней (до {date_str})."),
                            Lang::Uk => format!("✨ Промокод активовано! Premium на 30 днів (до {date_str})."),
                            Lang::Es => format!("✨ ¡Código activado! Premium por 30 días (hasta {date_str})."),
                            _ => format!("✨ Promo code activated! 30 days Premium (until {date_str})."),
                        };
                        app.bot.answer_callback(&cb.id, Some(&alert), true).await;
                    }
                    Err(crate::state::PromoError::AlreadyUsedByYou) => {
                        let alert = match lang {
                            Lang::Ru => "Вы уже активировали этот промокод ранее.",
                            Lang::Uk => "Ви вже активували цей промокод раніше.",
                            Lang::Es => "Ya has activado este código promocional anteriormente.",
                            _ => "You have already used this promo code.",
                        };
                        app.bot.answer_callback(&cb.id, Some(alert), true).await;
                    }
                    Err(_) => {
                        app.bot.answer_callback(&cb.id, Some("Error activating code"), false).await;
                    }
                }
                let (text, kb) = premium_menu(app, cb.from.id, lang);
                let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
            } else if let Some(days_str) = rest.strip_prefix("buy:") {
                let days: u32 = days_str.parse().unwrap_or(30);
                let (title, desc, payload, amount) = match days {
                    90 => (
                        match lang {
                            Lang::Ru => "Подписка Premium на 3 месяца",
                            Lang::Uk => "Підписка Premium на 3 місяці",
                            Lang::Es => "Suscripción Premium 3 meses",
                            _ => "Premium Subscription 3 months",
                        },
                        match lang {
                            Lang::Ru => "Поиск видео, чтение комментариев и отслеживание YouTube-каналов на 90 дней",
                            Lang::Uk => "Пошук відео, коментарі та відстеження YouTube-каналів на 90 днів",
                            Lang::Es => "Búsqueda de videos, comentarios y seguimiento de canales por 90 días",
                            _ => "Video search, comments, and channel tracking for 90 days",
                        },
                        "premium_90d",
                        app.cfg.stars_price_3m,
                    ),
                    _ => (
                        match lang {
                            Lang::Ru => "Подписка Premium на 1 месяц",
                            Lang::Uk => "Підписка Premium на 1 місяць",
                            Lang::Es => "Suscripción Premium 1 mes",
                            _ => "Premium Subscription 1 month",
                        },
                        match lang {
                            Lang::Ru => "Поиск видео, чтение комментариев и отслеживание YouTube-каналов на 30 дней",
                            Lang::Uk => "Пошук відео, коментарі та відстеження YouTube-каналів на 30 днів",
                            Lang::Es => "Búsqueda de videos, comentarios y seguimiento de canales por 30 días",
                            _ => "Video search, comments, and channel tracking for 30 days",
                        },
                        "premium_30d",
                        app.cfg.stars_price_1m,
                    ),
                };

                let prices = vec![serde_json::json!({
                    "label": title,
                    "amount": amount,
                })];

                let _ = app.bot.send_invoice(msg.chat.id, title, desc, payload, "XTR", &prices).await;
                app.bot.answer_callback(&cb.id, None, false).await;
            } else if rest == "buy_disc:30" {
                let amount = app.cfg.stars_discount_1m;
                let (title, desc) = match lang {
                    Lang::Ru => (
                        "Продление Premium на 1 месяц (скидка!)",
                        "Специальная цена на продление: поиск, комментарии и подписки на 30 дней",
                    ),
                    Lang::Uk => {
                        ("Подовження Premium на 1 місяць (знижка!)", "Спеціальна ціна на подовження на 30 днів")
                    }
                    Lang::Es => ("Renovación Premium 1 mes (¡descuento!)", "Precio especial de renovación por 30 días"),
                    _ => ("Premium Renewal 1 month (discount!)", "Special renewal price for 30 days"),
                };
                let prices = vec![serde_json::json!({
                    "label": title,
                    "amount": amount,
                })];
                let _ = app.bot.send_invoice(msg.chat.id, title, desc, "premium_30d_discount", "XTR", &prices).await;
                app.bot.answer_callback(&cb.id, None, false).await;
            } else if rest == "code_hint" {
                let hint = match lang {
                    Lang::Ru => "🎟 Чтобы активировать промокод, отправьте боту команду:\n/promo ВАШ_ПРОМОКОД",
                    Lang::Uk => "🎟 Щоб активувати промокод, надішліть боту команду:\n/promo ВАШ_ПРОМОКОД",
                    Lang::Es => "🎟 Para canjear un código, envía el comando:\n/promo TU_CODIGO",
                    _ => "🎟 To activate a promo code, send the command:\n/promo YOUR_CODE",
                };
                app.bot.answer_callback(&cb.id, Some(hint), true).await;
            } else if rest == "off" {
                app.store.set_user_premium(cb.from.id, false, 0);
                let alert = match lang {
                    Lang::Ru => "Premium отключен",
                    Lang::Uk => "Premium вимкнено",
                    Lang::Es => "Premium desactivado",
                    _ => "Premium disabled",
                };
                app.bot.answer_callback(&cb.id, Some(alert), false).await;
                let (text, kb) = premium_menu(app, cb.from.id, lang);
                let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
            } else {
                let is_on = rest == "on";
                app.store.set_user_premium(cb.from.id, is_on, 0);
                let alert = match (lang, is_on) {
                    (Lang::Ru, true) => "✨ Premium активирован!",
                    (Lang::Ru, false) => "Premium отключен",
                    (_, true) => "✨ Premium activated!",
                    (_, false) => "Premium disabled",
                };
                app.bot.answer_callback(&cb.id, Some(alert), false).await;
                let (text, kb) = premium_menu(app, cb.from.id, lang);
                let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
            }
        }
        "s" | "sub" => {
            let parts: Vec<&str> = rest.split(':').collect();
            match parts.as_slice() {
                ["unsub", channel_id] => {
                    let removed = app.store.remove_subscription(cb.from.id, channel_id).unwrap_or(false);
                    if removed {
                        if let Some(pub_url) = &app.cfg.websub_public_url {
                            let pub_url = pub_url.clone();
                            let ch_id = channel_id.to_string();
                            let sec = app.cfg.websub_secret.clone();
                            tokio::spawn(async move {
                                let http = reqwest::Client::new();
                                let _ = crate::subs::websub::send_hub_request(
                                    &http,
                                    &pub_url,
                                    &ch_id,
                                    "unsubscribe",
                                    sec.as_deref(),
                                    864000,
                                )
                                .await;
                            });
                        }
                    }
                    let alert = match lang {
                        Lang::Ru => "🔕 Вы отписались от этого канала",
                        _ => "🔕 Unsubscribed from channel",
                    };
                    app.bot.answer_callback(&cb.id, Some(alert), false).await;
                    let (text, kb) = subscriptions_menu(app, cb.from.id, lang);
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
                }
                ["dl", kind, video_id] => {
                    let quality = if *kind == "a" {
                        Quality::Audio
                    } else {
                        app.store.default_quality(cb.from.id).unwrap_or(Quality::Max)
                    };
                    let video_url = format!("https://www.youtube.com/watch?v={video_id}");
                    let alert = match (lang, *kind) {
                        (Lang::Ru, "a") => "🎵 Аудио добавлено в очередь!",
                        (Lang::Ru, _) => "📥 Видео добавлено в очередь!",
                        (Lang::Uk, "a") => "🎵 Аудіо додано до черги!",
                        (Lang::Uk, _) => "📥 Відео додано до черги!",
                        (Lang::Es, "a") => "🎵 ¡Audio añadido a la cola!",
                        (Lang::Es, _) => "📥 ¡Video añadido a la cola!",
                        (_, "a") => "🎵 Audio queued!",
                        (_, _) => "📥 Video queued!",
                    };
                    app.bot.answer_callback(&cb.id, Some(alert), false).await;

                    let ctx = Ctx { chat: msg.chat.id, thread: msg.thread(), user: cb.from.id, lang };
                    enqueue_all(app, &ctx, vec![video_url], quality, None, None).await;
                }
                ["vid", channel_id] => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    let loading = match lang {
                        Lang::Ru => "📺 <i>Загружаю последние видео канала...</i>",
                        Lang::Uk => "📺 <i>Завантажую останні відео каналу...</i>",
                        Lang::Es => "📺 <i>Cargando últimos videos del canal...</i>",
                        _ => "📺 <i>Loading latest channel videos...</i>",
                    };
                    let status_msg =
                        app.bot.send_message(msg.chat.id, msg.thread(), loading, None, Some(msg.message_id)).await.ok();
                    let status_mid = status_msg.map(|m| m.message_id);

                    let app_clone = app.clone();
                    let ch_id = channel_id.to_string();
                    let chat_id = msg.chat.id;
                    let thread_id = msg.thread();

                    tokio::spawn(async move {
                        let http = reqwest::Client::new();
                        // Fetch up to 9 latest videos (3 pages of 3)
                        match crate::subs::resolver::fetch_channel_latest_videos(&http, &ch_id, 9).await {
                            Ok((channel_title, entries)) if !entries.is_empty() => {
                                if let Some(mid) = status_mid {
                                    let _ = app_clone.bot.delete_message(chat_id, mid).await;
                                }

                                let items: Vec<crate::search::SearchItem> = entries
                                    .into_iter()
                                    .map(|e| crate::search::SearchItem {
                                        id: e.video_id.clone(),
                                        url: e.link,
                                        title: e.title,
                                        uploader: channel_title.clone(),
                                        duration_secs: 0,
                                        duration_str: String::new(),
                                        thumb_url: Some(format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", e.video_id)),
                                    })
                                    .collect();

                                let token = new_session_token();
                                let total_pages = items.len().div_ceil(3);
                                let page_0_items: Vec<_> = items.iter().take(3).collect();
                                let count = page_0_items.len();
                                let mut sent_msg_ids = Vec::new();

                                for (idx, item) in page_0_items.into_iter().enumerate() {
                                    let (caption, thumb_url) = render_card_content(item);
                                    let is_last = idx == count - 1;
                                    let page_nav =
                                        if is_last { Some((0, total_pages, "sub", token.as_str())) } else { None };
                                    let markup = render_card_markup(item, lang, page_nav);

                                    match app_clone
                                        .bot
                                        .send_photo(
                                            chat_id,
                                            thread_id,
                                            &thumb_url,
                                            &caption,
                                            Some(markup.clone()),
                                            None,
                                        )
                                        .await
                                    {
                                        Ok(sent_msg) => {
                                            sent_msg_ids.push(sent_msg.message_id);
                                        }
                                        Err(_) => {
                                            if let Ok(sent_msg) = app_clone
                                                .bot
                                                .send_message(chat_id, thread_id, &caption, Some(markup), None)
                                                .await
                                            {
                                                sent_msg_ids.push(sent_msg.message_id);
                                            }
                                        }
                                    }
                                }

                                save_card_session(
                                    token,
                                    CardSession {
                                        created_at: std::time::Instant::now(),
                                        items,
                                        msg_ids: sent_msg_ids,
                                        prefix: "sub",
                                        lang,
                                    },
                                );
                            }
                            Ok(_) => {
                                let empty_text = match lang {
                                    Lang::Ru => "ℹ️ У этого канала пока нет опубликованных видео.",
                                    Lang::Uk => "ℹ️ У цього каналу поки немає опублікованих відео.",
                                    Lang::Es => "ℹ️ Este canal aún no tiene videos publicados.",
                                    _ => "ℹ️ No published videos found for this channel.",
                                };
                                if let Some(mid) = status_mid {
                                    let _ = app_clone.bot.edit_message(chat_id, mid, empty_text, None).await;
                                }
                            }
                            Err(e) => {
                                log::warn!("fetch channel latest videos failed: {e}");
                                let err_text = match lang {
                                    Lang::Ru => "⚠️ Не удалось загрузить видео канала. Попробуйте позже.",
                                    Lang::Uk => "⚠️ Не вдалося завантажити відео каналу. Спробуйте пізніше.",
                                    Lang::Es => "⚠️ Error al cargar los videos del canal. Inténtalo más tarde.",
                                    _ => "⚠️ Failed to fetch channel videos. Please try again later.",
                                };
                                if let Some(mid) = status_mid {
                                    let _ = app_clone.bot.edit_message(chat_id, mid, err_text, None).await;
                                }
                            }
                        }
                    });
                }
                ["p", page_str, token] => {
                    let page: usize = page_str.parse().unwrap_or(0);
                    let session = match get_card_session(token) {
                        Some(s) => s,
                        None => {
                            let alert = match lang {
                                Lang::Ru => "⚠️ Время сессии поиска истекло. Повторите запрос.",
                                Lang::Uk => "⚠️ Час сесії пошуку минув. Повторіть запит.",
                                Lang::Es => "⚠️ La sesión de búsqueda ha expirado. Repite la consulta.",
                                _ => "⚠️ Search session expired. Please search again.",
                            };
                            app.bot.answer_callback(&cb.id, Some(alert), false).await;
                            return;
                        }
                    };

                    app.bot.answer_callback(&cb.id, None, false).await;

                    let items = &session.items;
                    let total_pages = items.len().div_ceil(3);
                    if page >= total_pages {
                        return;
                    }

                    let start_idx = page * 3;
                    let page_items: Vec<_> = items.iter().skip(start_idx).take(3).collect();
                    let num_page_items = page_items.len();
                    let chat_id = msg.chat.id;

                    for (idx, &mid) in session.msg_ids.iter().enumerate() {
                        if idx < num_page_items {
                            let item = page_items[idx];
                            let (caption, thumb_url) = render_card_content(item);
                            let is_last = idx == num_page_items - 1;
                            let page_nav =
                                if is_last { Some((page, total_pages, session.prefix, *token)) } else { None };
                            let markup = render_card_markup(item, session.lang, page_nav);

                            let _ = app
                                .bot
                                .edit_message_media(chat_id, mid, "photo", &thumb_url, &caption, Some(markup))
                                .await;
                        }
                    }
                }
                ["close", token] => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    if let Some(session) = get_card_session(token) {
                        for mid in session.msg_ids {
                            let _ = app.bot.delete_message(msg.chat.id, mid).await;
                        }
                    } else {
                        let _ = app.bot.delete_message(msg.chat.id, msg.message_id).await;
                    }
                }
                _ => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                }
            }
        }
        "comm" => {
            let parts: Vec<&str> = rest.split(':').collect();
            let video_id = parts.first().copied().unwrap_or_default();
            let page: usize = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);

            if video_id.is_empty() {
                app.bot.answer_callback(&cb.id, None, false).await;
                return;
            }

            let is_premium = app.store.is_user_premium(cb.from.id);
            if !is_premium {
                let alert = match lang {
                    Lang::Ru => "⭐ Чтение комментариев доступно только с Premium! Включите его в /premium",
                    Lang::Uk => "⭐ Читання коментарів доступне лише з Premium! Увімкніть його у /premium",
                    Lang::Es => "⭐ ¡La lectura de comentarios solo está disponible con Premium! Actívalo en /premium",
                    _ => "⭐ Reading comments is only available with Premium! Activate it in /premium",
                };
                app.bot.answer_callback(&cb.id, Some(alert), true).await;
                return;
            }

            app.bot.answer_callback(&cb.id, None, false).await;

            let is_existing_comments_view = msg.text.as_deref().is_some_and(|t| t.starts_with("💬"));
            let status_msg_id = if !is_existing_comments_view {
                let loading = match lang {
                    Lang::Ru => "💬 <i>Загружаю комментарии...</i>",
                    Lang::Uk => "💬 <i>Завантажую коментарі...</i>",
                    Lang::Es => "💬 <i>Cargando comentarios...</i>",
                    _ => "💬 <i>Loading comments...</i>",
                };
                app.bot
                    .send_message(msg.chat.id, msg.thread(), loading, None, Some(msg.message_id))
                    .await
                    .ok()
                    .map(|m| m.message_id)
            } else {
                Some(msg.message_id)
            };

            let app_clone = app.clone();
            let vid = video_id.to_string();
            let chat_id = msg.chat.id;
            let thread_id = msg.thread();

            tokio::spawn(async move {
                match crate::comments::fetch_comments(&app_clone.cfg, &app_clone.comment_slots, &vid).await {
                    Ok((title, items)) => {
                        let (text, kb) =
                            crate::comments::render_comments_page(&vid, title.as_deref(), &items, page, lang);
                        if let Some(mid) = status_msg_id {
                            let _ = app_clone.bot.edit_message(chat_id, mid, &text, Some(kb)).await;
                        } else {
                            let _ = app_clone.bot.send_message(chat_id, thread_id, &text, Some(kb), None).await;
                        }
                    }
                    Err(e) => {
                        log::warn!("Fetch comments for {vid} failed: {e}");
                        let err_text = match lang {
                            Lang::Ru => "⚠️ Не удалось загрузить комментарии к этому видео (возможно, они отключены).",
                            Lang::Uk => "⚠️ Не вдалося завантажити коментарі до цього відео.",
                            Lang::Es => "⚠️ No se pudieron cargar los comentarios de este video.",
                            _ => "⚠️ Failed to load comments for this video.",
                        };
                        if let Some(mid) = status_msg_id {
                            let _ = app_clone.bot.edit_message(chat_id, mid, err_text, None).await;
                        }
                    }
                }
            });
        }
        "cfg" => {
            let back_btn = match lang {
                Lang::Ru => "« Назад в настройки",
                Lang::Uk => "« Назад до налаштувань",
                Lang::Es => "« Volver a ajustes",
                _ => "« Back to settings",
            };
            match rest {
                "q" => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    let cur_q = quality_name(t, app.store.default_quality(cb.from.id));
                    let title = match lang {
                        Lang::Ru => format!("🎚 <b>Выберите качество по умолчанию</b>\nТекущее: <b>{cur_q}</b>"),
                        Lang::Uk => format!("🎚 <b>Оберіть якість за замовчуванням</b>\nПоточна: <b>{cur_q}</b>"),
                        Lang::Es => format!("🎚 <b>Selecciona la calidad predeterminada</b>\nActual: <b>{cur_q}</b>"),
                        _ => format!("🎚 <b>Select default quality</b>\nCurrent: <b>{cur_q}</b>"),
                    };
                    let mut rows = quality_rows(t, app.cfg.max_height, None, u64::MAX, |code| format!("d:{code}"));
                    rows.push(vec![json!({ "text": t.btn_ask, "callback_data": "d:ask" })]);
                    rows.push(vec![json!({ "text": back_btn, "callback_data": "cfg:back" })]);
                    let kb = json!({ "inline_keyboard": rows });
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &title, Some(kb)).await;
                }
                "lang" => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    let title = match lang {
                        Lang::Ru => "🌐 <b>Выберите язык интерфейса</b>",
                        Lang::Uk => "🌐 <b>Оберіть мову інтерфейсу</b>",
                        Lang::Es => "🌐 <b>Selecciona el idioma</b>",
                        _ => "🌐 <b>Select interface language</b>",
                    };
                    let mut rows: Vec<Vec<Value>> = Lang::ALL
                        .chunks(2)
                        .map(|chunk| {
                            chunk
                                .iter()
                                .map(|l| json!({ "text": l.button(), "callback_data": format!("l:{}", l.code()) }))
                                .collect()
                        })
                        .collect();
                    rows.push(vec![json!({ "text": back_btn, "callback_data": "cfg:back" })]);
                    let kb = json!({ "inline_keyboard": rows });
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, title, Some(kb)).await;
                }
                "min" => {
                    let cur = app.store.minimal(msg.chat.id);
                    app.store.set_minimal(msg.chat.id, !cur);
                    let alert = match (lang, !cur) {
                        (Lang::Ru, true) => "🤫 Минимальный режим включён",
                        (Lang::Ru, false) => "🤫 Минимальный режим выключен",
                        (Lang::Uk, true) => "🤫 Мінімальний режим увімкнено",
                        (Lang::Uk, false) => "🤫 Мінімальний режим вимкнено",
                        (Lang::Es, true) => "🤫 Modo mínimo activado",
                        (Lang::Es, false) => "🤫 Modo mínimo desactivado",
                        (_, true) => "🤫 Minimal mode enabled",
                        (_, false) => "🤫 Minimal mode disabled",
                    };
                    app.bot.answer_callback(&cb.id, Some(alert), false).await;
                    let (text, kb) = settings_menu(app, cb.from.id, msg.chat.id, lang);
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
                }
                "me" => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    let text = app.stats.render_user_stats(cb.from.id);
                    let kb = json!({
                        "inline_keyboard": [[{ "text": back_btn, "callback_data": "cfg:back" }]]
                    });
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
                }
                "clear" => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    let mut current_id = msg.message_id;
                    let mut remaining = 100;
                    while remaining > 0 && current_id > 0 {
                        let batch_size = remaining.min(100);
                        let start_id = current_id.saturating_sub(batch_size as i64 - 1).max(1);
                        let ids: Vec<i64> = (start_id..=current_id).rev().collect();
                        if ids.is_empty() {
                            break;
                        }
                        let _ = app.bot.delete_messages(msg.chat.id, &ids).await;
                        if start_id <= 1 {
                            break;
                        }
                        current_id = start_id - 1;
                        remaining -= ids.len();
                    }
                    if let Ok(confirm_msg) =
                        app.bot.send_message(msg.chat.id, msg.thread(), t.cleared, None, None).await
                    {
                        let bot = app.bot.clone();
                        let chat_id = msg.chat.id;
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                            let _ = bot.delete_message(chat_id, confirm_msg.message_id).await;
                        });
                    }
                }
                _ => {
                    app.bot.answer_callback(&cb.id, None, false).await;
                    let (text, kb) = settings_menu(app, cb.from.id, msg.chat.id, lang);
                    let _ = app.bot.edit_message(msg.chat.id, msg.message_id, &text, Some(kb)).await;
                }
            }
        }
        "del" => {
            app.bot.answer_callback(&cb.id, None, false).await;
            let _ = app.bot.delete_message(msg.chat.id, msg.message_id).await;
        }
        "noop" => {
            app.bot.answer_callback(&cb.id, None, false).await;
        }
        _ => app.bot.answer_callback(&cb.id, None, false).await,
    }
}

async fn handle_inline_query(app: &Arc<App>, iq: InlineQuery) {
    app.stats.record_user(iq.from.id, iq.from.username.as_deref(), iq.from.first_name.as_deref());
    let lang = app.lang_for(iq.from.id, Some(&iq.from));
    let t = lang.t();

    if !app.cfg.allowed_users.is_empty() && !app.cfg.allowed_users.contains(&iq.from.id) {
        log::warn!("denied inline query from user {}", iq.from.id);
        let article = json!({
            "type": "article",
            "id": "unauth",
            "title": t.not_authorized,
            "input_message_content": {
                "message_text": t.not_authorized,
            }
        });
        let _ = app.bot.answer_inline_query(&iq.id, &[article], Some(300), true, None, None).await;
        return;
    }

    let query = iq.query.trim();
    if query.is_empty() {
        let article = json!({
            "type": "article",
            "id": "prompt",
            "title": t.inline_prompt_title,
            "description": t.inline_prompt_desc,
            "input_message_content": {
                "message_text": format!("@{}", app.bot.username),
            }
        });
        let _ = app
            .bot
            .answer_inline_query(&iq.id, &[article], Some(300), true, Some(t.inline_switch_pm), Some("inline"))
            .await;
        return;
    }

    let urls = util::extract_urls(query, 1);
    let Some(url) = urls.first().cloned() else {
        // Query has no URLs -> Video Search (Premium feature)
        let is_premium = app.store.is_user_premium(iq.from.id);
        if !is_premium {
            let (title, desc, btn_text) = match lang {
                Lang::Ru => (
                    "⭐ Поиск видео доступен с Premium",
                    "Нажмите кнопку ниже, чтобы активировать Premium в боте",
                    "⭐ Активировать Premium",
                ),
                Lang::Uk => (
                    "⭐ Пошук відео доступний з Premium",
                    "Натисніть кнопку нижче, щоб активувати Premium в боті",
                    "⭐ Активувати Premium",
                ),
                Lang::Es => (
                    "⭐ Búsqueda disponible con Premium",
                    "Haz clic en el botón de abajo para activar Premium",
                    "⭐ Activar Premium",
                ),
                _ => (
                    "⭐ Video search requires Premium",
                    "Click the button below to activate Premium in the bot",
                    "⭐ Activate Premium",
                ),
            };
            let article = json!({
                "type": "article",
                "id": "prem_search_required",
                "title": title,
                "description": desc,
                "input_message_content": {
                    "message_text": "/premium",
                }
            });
            let _ =
                app.bot.answer_inline_query(&iq.id, &[article], Some(10), true, Some(btn_text), Some("premium")).await;
            return;
        }

        // User is Premium!
        let mut results: Vec<Value> = Vec::new();

        // 1. Search local SQLite cache (instant delivery by file_id)
        let cached = app.store.cache_search(query, 5);
        for (i, entry) in cached.into_iter().enumerate() {
            let credit = (!app.bot.username.is_empty()).then(|| fill(t.caption_credit, &[("bot", &app.bot.username)]));
            let caption = crate::app::build_caption(&entry.title, "", credit.as_deref());
            let title = if entry.title.is_empty() { "Cached video" } else { &entry.title };
            let res = match entry.kind.as_str() {
                "audio" => json!({
                    "type": "audio",
                    "id": format!("c_audio_{i}_{}", entry.file_id),
                    "audio_file_id": entry.file_id,
                    "caption": caption,
                    "parse_mode": "HTML",
                }),
                "document" => json!({
                    "type": "document",
                    "id": format!("c_doc_{i}_{}", entry.file_id),
                    "document_file_id": entry.file_id,
                    "title": title,
                    "caption": caption,
                    "parse_mode": "HTML",
                }),
                _ => json!({
                    "type": "video",
                    "id": format!("c_vid_{i}_{}", entry.file_id),
                    "video_file_id": entry.file_id,
                    "title": format!("⚡ {title}"),
                    "caption": caption,
                    "parse_mode": "HTML",
                }),
            };
            results.push(res);
        }

        // 2. Search YouTube
        if results.len() < 5 {
            let yt_limit = 5 - results.len();
            if let Ok(yt_items) = crate::search::search_youtube(&app.cfg, &app.search_slots, query, yt_limit).await {
                for (i, item) in yt_items.into_iter().enumerate() {
                    let desc = if !item.duration_str.is_empty() {
                        format!("{} • {}", item.uploader, item.duration_str)
                    } else {
                        item.uploader.clone()
                    };
                    let mut article = json!({
                        "type": "article",
                        "id": format!("yt_{i}_{}", item.id),
                        "title": item.title,
                        "description": desc,
                        "input_message_content": {
                            "message_text": item.url,
                        }
                    });
                    if let Some(thumb) = &item.thumb_url {
                        article["thumbnail_url"] = json!(thumb);
                        article["thumb_url"] = json!(thumb);
                    }
                    results.push(article);
                }
            }
        }

        if results.is_empty() {
            let (no_res_title, no_res_desc) = match lang {
                Lang::Ru => ("Ничего не найдено", "Попробуйте изменить поисковый запрос"),
                Lang::Uk => ("Нічого не знайдено", "Спробуйте змінити пошуковий запит"),
                Lang::Es => ("Sin resultados", "Intenta con otro término de búsqueda"),
                _ => ("No results found", "Try a different search query"),
            };
            let article = json!({
                "type": "article",
                "id": "empty_search",
                "title": no_res_title,
                "description": no_res_desc,
                "input_message_content": {
                    "message_text": format!("🔍 {query}"),
                }
            });
            results.push(article);
        }

        let _ = app.bot.answer_inline_query(&iq.id, &results, Some(300), true, None, None).await;
        return;
    };

    if app.cfg.block_private && util::is_private_url(&url) {
        let article = json!({
            "type": "article",
            "id": "private",
            "title": t.private_url,
            "input_message_content": {
                "message_text": t.private_url,
            }
        });
        let _ = app.bot.answer_inline_query(&iq.id, &[article], Some(300), true, None, None).await;
        return;
    }

    let video_key = cachekey::video_key(&url);
    let mut candidate_keys =
        vec![cachekey::request_key(&video_key, Quality::Max), cachekey::delivered_key(&video_key, Kind::Video, None)];
    for &h in &Quality::HEIGHTS {
        candidate_keys.push(cachekey::request_key(&video_key, Quality::Height(h)));
        candidate_keys.push(cachekey::delivered_key(&video_key, Kind::Video, Some(h)));
    }
    candidate_keys.push(cachekey::request_key(&video_key, Quality::Audio));
    candidate_keys.push(cachekey::delivered_key(&video_key, Kind::Audio, None));

    if let Some(entry) = app.store.cache_find(&candidate_keys) {
        let credit = (!app.bot.username.is_empty()).then(|| fill(t.caption_credit, &[("bot", &app.bot.username)]));
        let caption = crate::app::build_caption(&entry.title, &url, credit.as_deref());
        let result = match entry.kind.as_str() {
            "audio" => json!({
                "type": "audio",
                "id": "cached_audio",
                "audio_file_id": entry.file_id,
                "caption": caption,
                "parse_mode": "HTML",
            }),
            "document" => json!({
                "type": "document",
                "id": "cached_doc",
                "document_file_id": entry.file_id,
                "title": if entry.title.is_empty() { "Document" } else { &entry.title },
                "caption": caption,
                "parse_mode": "HTML",
            }),
            _ => json!({
                "type": "video",
                "id": "cached_video",
                "video_file_id": entry.file_id,
                "title": if entry.title.is_empty() { "Video" } else { &entry.title },
                "caption": caption,
                "parse_mode": "HTML",
            }),
        };
        if let Err(e) = app.bot.answer_inline_query(&iq.id, &[result], Some(300), true, None, None).await {
            log::warn!("answerInlineQuery failed: {e}");
        }
    } else {
        // Cache miss: stash URL for instant download when user clicks the switch_pm button
        let token = app.stash_urls(iq.from.id, vec![url.clone()], None);
        let param = format!("dl_{token}");
        let escaped_url = util::html_escape(&url);
        let article = json!({
            "type": "article",
            "id": "miss",
            "title": t.inline_not_found_title,
            "description": t.inline_not_found_desc,
            "input_message_content": {
                "message_text": format!("<code>{escaped_url}</code>"),
                "parse_mode": "HTML",
            }
        });
        if let Err(e) = app
            .bot
            .answer_inline_query(&iq.id, &[article], Some(10), true, Some(t.inline_switch_pm), Some(&param))
            .await
        {
            log::warn!("answerInlineQuery failed: {e}");
        }
    }
}

pub async fn check_and_notify_expiring_premium(app: &Arc<App>) {
    let now = crate::state::now_secs();

    // 1. Уведомление за 24 часа со скидкой
    let to_warn = app.store.get_users_to_warn_expiry(now, 24 * 3600);
    for (user_id, expires_at, activated_at) in to_warn {
        let is_day = crate::state::is_user_daytime(activated_at, now);
        let time_left = expires_at.saturating_sub(now);
        // Отправляем, если сейчас дневное время пользователя ИЛИ если осталось меньше 2 часов
        if is_day || time_left <= 2 * 3600 {
            let lang = app.lang_for(user_id, None);
            let date_str = crate::util::format_date_str(expires_at as i64);
            let discount_price = app.cfg.stars_discount_1m;
            let standard_price = app.cfg.stars_price_1m;

            let text = match lang {
                Lang::Ru => format!(
                    "⏳ <b>Ваша подписка Premium скоро истекает ({date_str})!</b>\n\n\
                     Продлите её прямо сейчас по специальной цене всего за <b>{discount_price} ⭐️</b> (вместо {standard_price} ⭐️)!\n\n\
                     Оставшееся время не сгорит, а суммируется к новому сроку."
                ),
                Lang::Uk => format!(
                    "⏳ <b>Ваша підписка Premium скоро закінчується ({date_str})!</b>\n\n\
                     Подовжте її прямо зараз за спеціальною ціною всього за <b>{discount_price} ⭐️</b> (замість {standard_price} ⭐️)!\n\n\
                     Залишок часу не згорить, а додасться до нового терміну."
                ),
                Lang::Es => format!(
                    "⏳ <b>¡Tu suscripción Premium vencerá pronto ({date_str})!</b>\n\n\
                     Renuévala ahora por solo <b>{discount_price} ⭐️</b> (en lugar de {standard_price} ⭐️).\n\n\
                     ¡El tiempo restante se sumará!"
                ),
                _ => format!(
                    "⏳ <b>Your Premium subscription expires soon ({date_str})!</b>\n\n\
                     Renew it now for only <b>{discount_price} ⭐️</b> (regular {standard_price} ⭐️)!\n\n\
                     The remaining time will be stacked."
                ),
            };

            let btn_text = match lang {
                Lang::Ru => format!("🔥 Продлить за {discount_price} ⭐️ со скидкой"),
                Lang::Uk => format!("🔥 Подовжити за {discount_price} ⭐️ зі знижкою"),
                Lang::Es => format!("🔥 Renovar por {discount_price} ⭐️ con descuento"),
                _ => format!("🔥 Renew for {discount_price} ⭐️ discounted"),
            };

            let kb = json!({
                "inline_keyboard": [
                    [{ "text": btn_text, "callback_data": "pr:buy_disc:30" }]
                ]
            });

            if app.bot.send_message(user_id, None, &text, Some(kb), None).await.is_ok() {
                app.store.mark_user_expiry_warned(user_id);
            }
        }
    }

    // 2. Уведомление в момент истечения
    let expired = app.store.get_users_expired_to_notify(now);
    for (user_id, activated_at) in expired {
        if crate::state::is_user_daytime(activated_at, now) {
            let lang = app.lang_for(user_id, None);
            let text = match lang {
                Lang::Ru => "⌛ <b>Срок действия вашей подписки Premium подошел к концу.</b>\n\n\
                             Отслеживание YouTube-каналов временно приостановлено. Вы можете возобновить подписку в любой момент в /premium.",
                Lang::Uk => "⌛ <b>Термін дії вашої підписки Premium закінчився.</b>\n\n\
                             Відстеження YouTube-каналів тимчасово призупинено. Ви можете відновити підписку в будь-який момент у /premium.",
                Lang::Es => "⌛ <b>Tu suscripción Premium ha finalizado.</b>\n\n\
                             El seguimiento de canales está en pausa. Puedes reactivarla en cualquier momento en /premium.",
                _ => "⌛ <b>Your Premium subscription has expired.</b>\n\n\
                      Channel tracking has been paused. You can renew at any time in /premium.",
            };

            let renew_btn = match lang {
                Lang::Ru => "⭐ Возобновить Premium",
                Lang::Uk => "⭐ Відновити Premium",
                Lang::Es => "⭐ Reactivar Premium",
                _ => "⭐ Renew Premium",
            };

            let kb = json!({
                "inline_keyboard": [
                    [{ "text": renew_btn, "callback_data": "pr:code_hint" }]
                ]
            });

            if app.bot.send_message(user_id, None, text, Some(kb), None).await.is_ok() {
                app.store.mark_user_expired_notified(user_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands() {
        assert_eq!(parse_command("/start", "mybot"), Some(("start".to_string(), "")));
        assert_eq!(
            parse_command("/Formats@MyBot  https://x.com ", "mybot"),
            Some(("formats".to_string(), "https://x.com"))
        );
        assert_eq!(parse_command("/start@otherbot", "mybot"), None);
        assert_eq!(parse_command("hello", "mybot"), None);
    }

    #[test]
    fn keyboards_fit_callback_limits() {
        for kb in [
            lang_keyboard(),
            picker_keyboard(Lang::Uk.t(), "ffffffabc", 1080, None, u64::MAX),
            default_keyboard(Lang::Es.t(), 1080),
            stats_keyboard(),
            top_keyboard(0),
        ] {
            for row in kb["inline_keyboard"].as_array().unwrap() {
                for b in row.as_array().unwrap() {
                    assert!(b["callback_data"].as_str().unwrap().len() <= 64);
                }
            }
        }
        let all = picker_keyboard(Lang::En.t(), "t", 480, None, u64::MAX).to_string();
        assert!(all.contains("q:t:480") && all.contains("q:t:360") && all.contains("q:t:audio"));
        assert!(all.contains("q:t:max") && !all.contains("q:t:720"));
        assert!(default_keyboard(Lang::En.t(), 1080).to_string().contains("d:ask"));

        // Formula filtering for long videos (limit: 2000 MB)
        let limit_2gb = 2000 * 1024 * 1024;
        let short_kb = picker_keyboard(Lang::En.t(), "t", 1440, Some(600), limit_2gb).to_string();
        assert!(short_kb.contains("q:t:1440"));
        assert!(short_kb.contains("q:t:1080"));

        let mid_kb = picker_keyboard(Lang::En.t(), "t", 1440, Some(2100), limit_2gb).to_string();
        assert!(!mid_kb.contains("q:t:1440"));
        assert!(mid_kb.contains("q:t:1080"));

        let long_kb = picker_keyboard(Lang::En.t(), "t", 1440, Some(4500), limit_2gb).to_string();
        assert!(!long_kb.contains("q:t:1440"));
        assert!(!long_kb.contains("q:t:1080"));
        assert!(long_kb.contains("q:t:720"));
        assert_eq!(command_list(Lang::Ru.t()).len(), 14);
        assert!(command_list(Lang::Ru.t()).iter().any(|(cmd, _)| *cmd == "promo"));
        assert!(command_list(Lang::Ru.t()).iter().any(|(cmd, _)| *cmd == "search"));
        let lq = crate::app::lower_quality_keyboard(Lang::En.t(), "tok", Quality::Height(1080)).unwrap();
        let lq_str = lq.to_string();
        assert!(
            lq_str.contains("q:tok:720")
                && lq_str.contains("q:tok:480")
                && lq_str.contains("q:tok:360")
                && lq_str.contains("q:tok:audio")
        );
        assert!(crate::app::lower_quality_keyboard(Lang::En.t(), "tok", Quality::Audio).is_none());
    }

    #[tokio::test]
    async fn status_menu_renders_and_fits_limits() {
        let temp_dir = std::env::temp_dir().join(format!("tgmb-bot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&temp_dir);
        let env = crate::config::Env::from_pairs(&[("BOT_TOKEN", "123:abc"), ("DATA_DIR", temp_dir.to_str().unwrap())]);
        let cfg = crate::config::Config::from_env(&env).unwrap();
        let bot = crate::telegram::Bot::new(&cfg).unwrap();
        let store = crate::state::Store::load(&cfg.data_dir);
        let plugins = crate::plugins::Plugins::load(None);
        let app = Arc::new(App::new(cfg, bot, store, plugins));

        let req = Request {
            chat_id: 10,
            thread_id: None,
            user_id: 42,
            url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
            quality: Quality::Max,
            lang: Lang::Ru,
            status_msg: None,
            user_msg_id: None,
        };
        let job = app.enqueue(req).unwrap();
        job.set_title("Rick Astley - Never Gonna Give You Up");

        let (text, kb) = status_menu(&app, 42, Lang::Ru);
        assert!(text.contains("Rick Astley - Never Gonna Give You Up"));
        assert!(text.contains(&job.id));
        assert!(text.contains("📊 <b>Твоя очередь</b> (1/100)"));

        for row in kb["inline_keyboard"].as_array().unwrap() {
            for b in row.as_array().unwrap() {
                assert!(b["callback_data"].as_str().unwrap().len() <= 64);
                assert!(b["callback_data"].as_str().unwrap().starts_with("c:"));
            }
        }

        // Test start parameter dl_<token> stashing and consumption
        let token = app.stash_urls(42, vec!["https://youtu.be/test12345".into()], None);
        assert!(!token.is_empty());
        let (stashed_urls, _) = app.take_urls(&token, 42).expect("token should be valid");
        assert_eq!(stashed_urls, vec!["https://youtu.be/test12345"]);

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_menu_actions() {
        assert_eq!(parse_menu_action("🔍 Поиск"), Some(MenuAction::Search));
        assert_eq!(parse_menu_action("search"), Some(MenuAction::Search));
        assert_eq!(parse_menu_action("🔍 Buscar"), Some(MenuAction::Search));
        assert_eq!(parse_menu_action("📺 Мои подписки"), Some(MenuAction::Subscriptions));
        assert_eq!(parse_menu_action("📺 Mis suscripciones"), Some(MenuAction::Subscriptions));
        assert_eq!(parse_menu_action("⭐ Premium"), Some(MenuAction::Premium));
        assert_eq!(parse_menu_action("⚙️ Настройки"), Some(MenuAction::Settings));
        assert_eq!(parse_menu_action("⚙️ Ajustes"), Some(MenuAction::Settings));
        assert_eq!(parse_menu_action("some random text"), None);

        let ru_kb = main_menu_keyboard(Lang::Ru);
        assert_eq!(ru_kb["keyboard"].as_array().unwrap().len(), 2);
    }
}
