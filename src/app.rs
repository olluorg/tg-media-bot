//! Shared application state and the download job runner.
//!
//! Every user has a personal queue (up to `USER_QUEUE_SIZE` links) that is
//! worked through `RATE_LIMIT_PER_USER` at a time; those heads then compete
//! fairly (FIFO) for the global `MAX_PARALLEL_DOWNLOADS` slots, so one user
//! with 100 links cannot starve everybody else. A job only gets a status
//! message once its turn comes, so a long queue doesn't flood the chat.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::{watch, Semaphore};
use tokio::task::AbortHandle;

use crate::cachekey::{self, delivered_key, request_key};
use crate::config::Config;
use crate::i18n::{fill, Lang, Strings};
use crate::media::{self, Ready};
use crate::plugins::Plugins;
use crate::state::{CacheEntry, Store};
use crate::sys;
use crate::telegram::{Bot, Media, MediaKind, Message, Source, TgError, User};
use crate::types::{Quality, Stage};
use crate::util::{self, html_escape};
use crate::ytdlp::{self, DlError, Hint, Outcome, Probed, Reject};

const FLOOD_RETRIES: u32 = 3;
const FLOOD_MAX_WAIT: u64 = 300;
const CAPTION_MAX: usize = 1024;
const PENDING_TTL: Duration = Duration::from_secs(24 * 3600);
const PENDING_MAX: usize = 2000;

pub struct Job {
    pub id: String,
    /// Creation order, for listing.
    pub seq: u64,
    pub user_id: i64,
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub url: String,
    pub quality: Quality,
    pub lang: Lang,
    pub minimal: bool,
    /// Video identity from the link alone (`youtube:<id>` or `url:<url>`).
    pub video_key: String,
    /// Cache key of this request.
    pub key: String,
    pub stage: watch::Sender<Stage>,
    /// Bytes of the current upload that have left the process.
    pub sent: Arc<AtomicU64>,
}

struct Entry {
    job: Arc<Job>,
    abort: Option<AbortHandle>,
    status_msg: Option<i64>,
}

pub struct Request {
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub user_id: i64,
    pub url: String,
    pub quality: Quality,
    pub lang: Lang,
    /// Message to turn into this job's status (the quality question).
    pub status_msg: Option<i64>,
}

/// Why a link was not queued.
#[derive(Debug, PartialEq)]
pub enum Refusal {
    InvalidUrl,
    PrivateUrl,
    UserQueueFull,
    QueueFull,
    Duplicate,
}

impl Refusal {
    pub fn text(&self, t: &Strings, queued: usize, max: usize) -> String {
        match self {
            Refusal::InvalidUrl => t.invalid_url.to_string(),
            Refusal::PrivateUrl => t.private_url.to_string(),
            Refusal::UserQueueFull => fill(t.user_queue_full, &[("n", &queued.to_string()), ("max", &max.to_string())]),
            Refusal::QueueFull => t.queue_full.to_string(),
            Refusal::Duplicate => t.duplicate.to_string(),
        }
    }
}

pub enum Cancel {
    Done,
    NotFound,
    Denied,
}

/// A pending quality question: the links waiting for an answer.
struct Pending {
    user_id: i64,
    urls: Vec<String>,
    created: Instant,
}

pub struct App {
    pub cfg: Config,
    pub bot: Bot,
    pub store: Store,
    pub plugins: Plugins,
    /// Global download slots (MAX_PARALLEL_DOWNLOADS).
    pub slots: Semaphore,
    /// Per-user turn (RATE_LIMIT_PER_USER), created on demand.
    user_slots: Mutex<HashMap<i64, Arc<Semaphore>>>,
    jobs: Mutex<HashMap<String, Entry>>,
    pending: Mutex<HashMap<String, Pending>>,
    /// chat -> moment until which progress edits are skipped (flood control).
    edit_block: Mutex<HashMap<i64, Instant>>,
    seq: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Removes the job from the table and its temp dir however the run ends —
/// including being aborted by /cancel or shutdown.
struct JobGuard {
    app: Arc<App>,
    id: String,
    user_id: i64,
    dir: PathBuf,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        {
            let mut jobs = lock(&self.app.jobs);
            jobs.remove(&self.id);
            if !jobs.values().any(|e| e.job.user_id == self.user_id) {
                lock(&self.app.user_slots).remove(&self.user_id);
            }
        }
        if self.dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&self.dir) {
                log::warn!("could not remove {}: {e}", self.dir.display());
            }
        }
    }
}

enum JobError {
    Dl(DlError),
    NoSpace,
    TooBigToSend(u64),
    Upload { size: u64, err: TgError },
}

pub fn cancel_markup(t: &Strings, id: &str) -> Value {
    json!({ "inline_keyboard": [[{ "text": t.btn_cancel, "callback_data": format!("c:{id}") }]] })
}

fn build_caption(title: &str, url: &str) -> String {
    let url_html = format!("<code>{}</code>", html_escape(url));
    let title = title.trim();
    if title.is_empty() {
        return util::truncate_chars(&url_html, CAPTION_MAX).to_string();
    }
    // Entities are counted after parsing, so budget on the raw title length.
    let budget = CAPTION_MAX.saturating_sub(url.chars().count() + 4);
    if budget < 8 {
        return util::truncate_chars(&html_escape(title), CAPTION_MAX).to_string();
    }
    let title = util::truncate_chars(title, budget);
    format!("{}\n\n{url_html}", html_escape(title))
}

pub fn stage_label(t: &Strings, stage: &Stage) -> &'static str {
    match stage {
        Stage::Queued => t.st_queued,
        Stage::Waiting => t.st_waiting,
        Stage::Probing => t.st_probing,
        Stage::Downloading { .. } => t.st_downloading,
        Stage::Processing => t.st_processing,
        Stage::Transcoding { .. } => t.st_transcoding,
        Stage::Uploading { .. } => t.st_uploading,
    }
}

pub fn render_status(job: &Job) -> String {
    let t = job.lang.t();
    let stage = job.stage.borrow().clone();
    let mut text = format!("<b>{}</b> · {}", stage_label(t, &stage), job.quality.label(t));
    let bar = |pct: f32| format!("\n<code>{}</code> {pct:.0}%", util::progress_bar(pct));
    match stage {
        Stage::Downloading { pct, speed, eta } => {
            if let Some(p) = pct {
                text.push_str(&bar(p));
            }
            let mut meta = Vec::new();
            if let Some(s) = speed {
                meta.push(format!("{}/s", util::fmt_size(s as u64)));
            }
            if let Some(e) = eta.filter(|e| *e > 0) {
                meta.push(format!("~{}", util::fmt_duration(e)));
            }
            if !meta.is_empty() {
                text.push('\n');
                text.push_str(&meta.join(" · "));
            }
        }
        Stage::Transcoding { pct: Some(p) } => text.push_str(&bar(p)),
        Stage::Uploading { total } if total > 0 => {
            let sent = job.sent.load(Ordering::Relaxed).min(total);
            text.push_str(&bar(sent as f32 * 100.0 / total as f32));
            text.push_str(&format!("\n{} / {}", util::fmt_size(sent), util::fmt_size(total)));
        }
        _ => {}
    }
    text.push_str(&format!(
        "\n<code>{}</code> · <code>{}</code>",
        html_escape(util::truncate_chars(&job.url, 80)),
        job.id
    ));
    text
}

fn error_text(t: &Strings, cfg: &Config, err: &JobError, url: &str) -> String {
    let body = match err {
        JobError::Dl(DlError::Rejected(r)) => match r {
            Reject::Live => t.e_live.to_string(),
            Reject::NoMedia => t.e_no_media.to_string(),
            Reject::TooLong { secs } => fill(
                t.e_too_long,
                &[("dur", &util::fmt_duration(*secs)), ("max", &util::fmt_duration(cfg.max_duration))],
            ),
            Reject::TooLarge { bytes } => {
                fill(t.e_too_large, &[("size", &util::fmt_size(*bytes)), ("max", &util::fmt_size(cfg.max_file_bytes))])
            }
        },
        JobError::Dl(DlError::Failed(raw)) => match ytdlp::classify(raw) {
            Some(hint) => hint_text(t, hint).to_string(),
            None => format!("<code>{}</code>", html_escape(util::truncate_chars(raw, 300))),
        },
        JobError::Dl(DlError::Timeout) => fill(t.e_timeout, &[("min", &(cfg.download_timeout / 60).to_string())]),
        JobError::Dl(DlError::Internal(_)) => t.e_internal.to_string(),
        JobError::NoSpace => t.e_no_space.to_string(),
        JobError::TooBigToSend(size) => fill(
            t.e_upload_big,
            &[("size", &util::fmt_size(*size)), ("max", &util::fmt_size(cfg.upload_limit_bytes()))],
        ),
        JobError::Upload { size, .. } => fill(t.e_upload, &[("size", &util::fmt_size(*size))]),
    };
    // With a long queue the error must say which link it is about.
    format!("{}\n<code>{}</code>\n\n{body}", t.err_title, html_escape(util::truncate_chars(url, 120)))
}

fn hint_text(t: &Strings, hint: Hint) -> &'static str {
    match hint {
        Hint::Age => t.e_age,
        Hint::Bot => t.e_bot,
        Hint::Private => t.e_private,
        Hint::Members => t.e_members,
        Hint::Login => t.e_login,
        Hint::Format => t.e_format,
        Hint::Unavailable => t.e_unavailable,
        Hint::Unsupported => t.e_unsupported,
        Hint::NoMedia => t.e_no_media,
        Hint::NotFound => t.e_404,
        Hint::Forbidden => t.e_403,
        Hint::RateLimited => t.e_rate,
        Hint::Network => t.e_network,
        Hint::Geo => t.e_geo,
        Hint::Drm => t.e_drm,
        Hint::Live => t.e_live,
    }
}

fn cache_entry(msg: &Message, ready: &Ready) -> Option<CacheEntry> {
    let (kind, file) = if let Some(v) = &msg.video {
        ("video", v)
    } else if let Some(a) = &msg.audio {
        ("audio", a)
    } else {
        ("document", msg.document.as_ref()?)
    };
    Some(CacheEntry {
        kind: kind.to_string(),
        file_id: file.file_id.clone(),
        title: ready.title.clone(),
        performer: ready.performer.clone(),
        duration: ready.duration,
        ts: 0,
    })
}

/// Keys to check once the probe knows the video and the quality it will get.
fn probe_keys(job: &Job, p: &Probed) -> Vec<String> {
    let mut keys = Vec::with_capacity(4);
    if let Some(v) = &p.video_key {
        keys.push(request_key(v, job.quality));
        keys.push(delivered_key(v, p.kind, p.height));
    }
    keys.push(delivered_key(&job.video_key, p.kind, p.height));
    keys.dedup();
    keys
}

/// Every key an upload is stored under, so all of them hit next time.
fn store_keys(job: &Job, p: &Probed) -> Vec<String> {
    let mut keys = vec![job.key.clone()];
    keys.extend(probe_keys(job, p));
    keys.sort();
    keys.dedup();
    keys
}

impl App {
    pub fn new(cfg: Config, bot: Bot, store: Store, plugins: Plugins) -> App {
        let slots = Semaphore::new(cfg.max_parallel);
        App {
            cfg,
            bot,
            store,
            plugins,
            slots,
            user_slots: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
            edit_block: Mutex::new(HashMap::new()),
            seq: AtomicU64::new(0),
        }
    }

    /// Chat setting, else the user's Telegram language, else the default.
    pub fn lang_for(&self, chat_id: i64, user: Option<&User>) -> Lang {
        self.store
            .lang(chat_id)
            .or_else(|| Lang::guess(user.and_then(|u| u.language_code.as_deref())))
            .unwrap_or(self.cfg.default_lang)
    }

    pub async fn say(&self, chat_id: i64, thread_id: Option<i64>, text: &str, markup: Option<Value>) {
        if let Err(e) = self.bot.send_message(chat_id, thread_id, text, markup).await {
            log::warn!("sendMessage to {chat_id} failed: {e}");
        }
    }

    pub fn active_jobs(&self) -> usize {
        lock(&self.jobs).len()
    }

    /// The user's queue, oldest first.
    pub fn user_jobs(&self, user_id: i64) -> Vec<Arc<Job>> {
        let mut list: Vec<Arc<Job>> =
            lock(&self.jobs).values().filter(|e| e.job.user_id == user_id).map(|e| e.job.clone()).collect();
        list.sort_by_key(|j| j.seq);
        list
    }

    fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::Relaxed)
    }

    fn new_id(&self, n: u64) -> String {
        // Short, unguessable-enough, unique for the process lifetime.
        let mix = (n.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ crate::state::now_secs()) & 0xFF_FFFF;
        format!("{:06x}{}", mix, if n > 0xFF_FFFF { format!("{n:x}") } else { String::new() })
    }

    // -- quality question ---------------------------------------------------

    pub fn stash_urls(&self, user_id: i64, urls: Vec<String>) -> String {
        let mut pending = lock(&self.pending);
        pending.retain(|_, p| p.created.elapsed() < PENDING_TTL);
        if pending.len() >= PENDING_MAX {
            if let Some(oldest) = pending.iter().min_by_key(|(_, p)| p.created).map(|(k, _)| k.clone()) {
                pending.remove(&oldest);
            }
        }
        let token = self.new_id(self.next_seq());
        pending.insert(token.clone(), Pending { user_id, urls, created: Instant::now() });
        token
    }

    /// Take the links behind a quality question. `Err(true)` means the
    /// question belongs to someone else, `Err(false)` that it expired.
    pub fn take_urls(&self, token: &str, user_id: i64) -> Result<Vec<String>, bool> {
        let mut pending = lock(&self.pending);
        match pending.get(token) {
            None => Err(false),
            Some(p) if p.created.elapsed() >= PENDING_TTL => {
                pending.remove(token);
                Err(false)
            }
            // Only the person who sent the links answers for them.
            Some(p) if p.user_id != user_id => Err(true),
            Some(_) => Ok(pending.remove(token).map(|p| p.urls).unwrap_or_default()),
        }
    }

    /// How many more links this user may queue.
    pub fn user_room(&self, user_id: i64) -> usize {
        let mine = lock(&self.jobs).values().filter(|e| e.job.user_id == user_id).count();
        self.cfg.user_queue.saturating_sub(mine)
    }

    // -- jobs ---------------------------------------------------------------

    /// Validate and queue a download.
    pub fn enqueue(self: &Arc<Self>, req: Request) -> Result<Arc<Job>, Refusal> {
        if !util::is_valid_url(&req.url) {
            return Err(Refusal::InvalidUrl);
        }
        if self.cfg.block_private && util::is_private_url(&req.url) {
            return Err(Refusal::PrivateUrl);
        }
        let video_key = cachekey::video_key(&req.url);
        let key = request_key(&video_key, req.quality);
        let minimal = self.store.minimal(req.chat_id);

        let mut jobs = lock(&self.jobs);
        if jobs.values().filter(|e| e.job.user_id == req.user_id).count() >= self.cfg.user_queue {
            return Err(Refusal::UserQueueFull);
        }
        if jobs.len() >= self.cfg.max_queue {
            return Err(Refusal::QueueFull);
        }
        if jobs.values().any(|e| e.job.chat_id == req.chat_id && e.job.key == key) {
            return Err(Refusal::Duplicate);
        }
        let seq = self.next_seq();
        let mut id = self.new_id(seq);
        while jobs.contains_key(&id) {
            id = self.new_id(self.next_seq());
        }
        let (stage, _) = watch::channel(Stage::Queued);
        let job = Arc::new(Job {
            id: id.clone(),
            seq,
            user_id: req.user_id,
            chat_id: req.chat_id,
            thread_id: req.thread_id,
            url: req.url,
            quality: req.quality,
            lang: req.lang,
            minimal,
            video_key,
            key,
            stage,
            sent: Arc::new(AtomicU64::new(0)),
        });
        log::info!(
            "job {} queued: user={} chat={} quality={} url={}",
            job.id,
            job.user_id,
            job.chat_id,
            job.quality.code(),
            job.url
        );
        // Registered before the task exists, so even an instant cache hit
        // finds (and removes) its own entry.
        let status_msg = if minimal { None } else { req.status_msg };
        jobs.insert(id.clone(), Entry { job: job.clone(), abort: None, status_msg });
        let handle = tokio::spawn(run_job(self.clone(), job.clone()));
        if let Some(entry) = jobs.get_mut(&id) {
            entry.abort = Some(handle.abort_handle());
        }
        Ok(job)
    }

    fn status_msg(&self, id: &str) -> Option<i64> {
        lock(&self.jobs).get(id).and_then(|e| e.status_msg)
    }

    fn set_status_msg(&self, id: &str, msg: i64) {
        if let Some(e) = lock(&self.jobs).get_mut(id) {
            e.status_msg = Some(msg);
        }
    }

    fn may_cancel(&self, job: &Job, user_id: i64) -> bool {
        job.user_id == user_id || self.cfg.allowed_users.contains(&user_id)
    }

    /// Cancel a job. Its owner may cancel it, and so may allowlisted users.
    pub async fn cancel(&self, id: &str, user_id: i64) -> Cancel {
        let removed = {
            let mut jobs = lock(&self.jobs);
            match jobs.get(id) {
                None => return Cancel::NotFound,
                Some(e) if !self.may_cancel(&e.job, user_id) => return Cancel::Denied,
                Some(_) => jobs.remove(id),
            }
        };
        let Some(entry) = removed else { return Cancel::NotFound };
        // Dropping the task kills yt-dlp/ffmpeg and removes the temp dir.
        if let Some(abort) = entry.abort {
            abort.abort();
        }
        log::info!("job {id} cancelled by {user_id}");
        let t = entry.job.lang.t();
        if let Some(msg) = entry.status_msg {
            if let Err(e) = self.bot.edit_message(entry.job.chat_id, msg, t.cancelled, None).await {
                log::debug!("edit after cancel failed: {e}");
            }
        } else {
            self.say(entry.job.chat_id, entry.job.thread_id, t.cancelled, None).await;
        }
        Cancel::Done
    }

    /// Cancel the whole queue of a user. Returns how many were cancelled.
    pub async fn cancel_all(&self, user_id: i64) -> usize {
        let removed: Vec<Entry> = {
            let mut jobs = lock(&self.jobs);
            let ids: Vec<String> =
                jobs.iter().filter(|(_, e)| e.job.user_id == user_id).map(|(id, _)| id.clone()).collect();
            ids.iter().filter_map(|id| jobs.remove(id)).collect()
        };
        for entry in &removed {
            if let Some(abort) = &entry.abort {
                abort.abort();
            }
        }
        // Clear the buttons on their status messages; stop at flood control.
        for entry in &removed {
            if let Some(msg) = entry.status_msg {
                let t = entry.job.lang.t();
                if let Err(e) = self.bot.edit_message(entry.job.chat_id, msg, t.cancelled, None).await {
                    if e.retry_after().is_some() {
                        break;
                    }
                }
            }
        }
        log::info!("user {user_id} cancelled {} job(s)", removed.len());
        removed.len()
    }

    /// Abort everything (shutdown). Children are killed as tasks drop.
    pub fn abort_all(&self) {
        for entry in lock(&self.jobs).values() {
            if let Some(a) = &entry.abort {
                a.abort();
            }
        }
    }

    fn user_slot(&self, user_id: i64) -> Arc<Semaphore> {
        lock(&self.user_slots).entry(user_id).or_insert_with(|| Arc::new(Semaphore::new(self.cfg.per_user))).clone()
    }

    // -- status message -----------------------------------------------------

    async fn edit_status(&self, chat_id: i64, msg_id: i64, text: &str, markup: Option<Value>) {
        if lock(&self.edit_block).get(&chat_id).is_some_and(|until| Instant::now() < *until) {
            return;
        }
        if let Err(e) = self.bot.edit_message(chat_id, msg_id, text, markup).await {
            if let Some(secs) = e.retry_after() {
                lock(&self.edit_block).insert(chat_id, Instant::now() + Duration::from_secs(secs));
            }
            log::debug!("status edit failed: {e}");
        }
    }

    /// Send or edit, waiting out flood control instead of dropping the message.
    async fn deliver(&self, job: &Job, msg_id: Option<i64>, text: &str, markup: Option<Value>) -> Option<i64> {
        for _ in 0..=FLOOD_RETRIES {
            let result = match msg_id {
                Some(id) => self.bot.edit_message(job.chat_id, id, text, markup.clone()).await.map(|_| id),
                None => {
                    self.bot.send_message(job.chat_id, job.thread_id, text, markup.clone()).await.map(|m| m.message_id)
                }
            };
            match result {
                Err(e) if e.retry_after().is_some_and(|s| s <= FLOOD_MAX_WAIT) => {
                    tokio::time::sleep(Duration::from_secs(e.retry_after().unwrap_or(1) + 1)).await;
                }
                Err(e) => {
                    log::warn!("could not post status of job {}: {e}", job.id);
                    return None;
                }
                Ok(id) => return Some(id),
            }
        }
        None
    }

    async fn status_loop(&self, job: &Job, msg_id: i64) {
        let interval = Duration::from_secs(self.cfg.progress_interval);
        let markup = cancel_markup(job.lang.t(), &job.id);
        let mut rx = job.stage.subscribe();
        let mut last = String::new();
        loop {
            let text = render_status(job);
            if text != last {
                self.edit_status(job.chat_id, msg_id, &text, Some(markup.clone())).await;
                last = text;
            }
            tokio::time::sleep(interval).await;
            // Uploads report through a counter, not the channel: poll them.
            let uploading = matches!(*rx.borrow_and_update(), Stage::Uploading { .. });
            if !uploading && rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    // -- the pipeline -------------------------------------------------------

    async fn send_with_retry(&self, media: &Media<'_>, sent: &Arc<AtomicU64>) -> Result<Message, TgError> {
        let mut attempt = 0;
        loop {
            sent.store(0, Ordering::Relaxed);
            match self.bot.send_media(media, Some(sent.clone())).await {
                Err(e) if attempt < FLOOD_RETRIES && e.retry_after().is_some_and(|s| s <= FLOOD_MAX_WAIT) => {
                    attempt += 1;
                    let wait = e.retry_after().unwrap_or(1) + 1;
                    log::warn!("flood control: retrying upload in {wait}s");
                    tokio::time::sleep(Duration::from_secs(wait)).await;
                }
                other => return other,
            }
        }
    }

    /// Re-send an earlier upload. A dead file_id is evicted everywhere.
    async fn send_cached(&self, job: &Job, entry: &CacheEntry) -> bool {
        let kind = match entry.kind.as_str() {
            "video" => MediaKind::Video,
            "audio" => MediaKind::Audio,
            _ => MediaKind::Document,
        };
        let caption = (!job.minimal).then(|| build_caption(&entry.title, &job.url));
        let media = Media {
            kind,
            chat_id: job.chat_id,
            thread_id: job.thread_id,
            source: Source::FileId(&entry.file_id),
            file_name: "",
            caption: caption.as_deref(),
            duration: (entry.duration > 0).then_some(entry.duration),
            width: None,
            height: None,
            title: (!entry.title.is_empty()).then_some(entry.title.as_str()),
            performer: (!entry.performer.is_empty()).then_some(entry.performer.as_str()),
            thumb: None,
        };
        match self.send_with_retry(&media, &job.sent).await {
            Ok(_) => true,
            Err(e) => {
                log::info!("cached file_id unusable ({e}); dropping it");
                self.store.cache_evict_file(&entry.file_id);
                false
            }
        }
    }

    async fn upload(&self, job: &Job, ready: &Ready) -> Result<Message, TgError> {
        let caption = (!job.minimal).then(|| build_caption(&ready.title, &job.url));
        let source = if self.cfg.local_api_files { Source::LocalUri(&ready.path) } else { Source::Upload(&ready.path) };
        let mut media = Media {
            kind: ready.kind,
            chat_id: job.chat_id,
            thread_id: job.thread_id,
            source,
            file_name: &ready.file_name,
            caption: caption.as_deref(),
            duration: (ready.duration > 0).then_some(ready.duration),
            width: ready.width,
            height: ready.height,
            title: (!ready.title.is_empty()).then_some(ready.title.as_str()),
            performer: (!ready.performer.is_empty()).then_some(ready.performer.as_str()),
            thumb: ready.thumb.as_deref(),
        };
        job.stage.send_replace(Stage::Uploading { total: ready.size });
        match self.send_with_retry(&media, &job.sent).await {
            // Telegram refused it as a video/audio (odd codec, bad metadata):
            // a plain document almost always goes through.
            Err(e) if media.kind != MediaKind::Document && e.code() == Some(400) => {
                log::warn!("sending as {:?} failed ({e}); retrying as a document", media.kind);
                media.kind = MediaKind::Document;
                self.send_with_retry(&media, &job.sent).await
            }
            other => other,
        }
    }

    async fn pipeline(&self, job: &Job, dir: &Path) -> Result<(), JobError> {
        let _slot = match self.slots.try_acquire() {
            Ok(p) => p,
            Err(_) => {
                job.stage.send_replace(Stage::Waiting);
                self.slots.acquire().await.map_err(|_| JobError::Dl(DlError::Internal("shutting down".into())))?
            }
        };

        if let Some(free) = sys::free_space_mb(&self.cfg.temp_dir) {
            if free < self.cfg.min_free_mb {
                log::warn!("only {free} MB free in {}", self.cfg.temp_dir.display());
                return Err(JobError::NoSpace);
            }
        }
        std::fs::create_dir_all(dir).map_err(|e| JobError::Dl(DlError::Internal(e.to_string())))?;

        let (kind, cap) = (job.quality.kind(), job.quality.cap());
        let lookup = |p: &Probed| self.store.cache_find(&probe_keys(job, p));
        // Two rounds at most: a cache hit whose file_id turns out dead is
        // retried as a real download.
        for round in 0..2 {
            let no_lookup = |_: &Probed| None;
            let lookup: ytdlp::Lookup<'_> = if round == 0 { &lookup } else { &no_lookup };
            let outcome = ytdlp::fetch(&self.cfg, &self.plugins, &job.url, kind, cap, dir, &job.stage, lookup)
                .await
                .map_err(JobError::Dl)?;
            match outcome {
                Outcome::Cached(entry, probed) => {
                    if self.send_cached(job, &entry).await {
                        log::info!("job {} served from cache after probe ({:?})", job.id, probed.video_key);
                        self.store.cache_put(&store_keys(job, &probed), entry);
                        return Ok(());
                    }
                }
                Outcome::Downloaded(fetched) => {
                    let ready = media::prepare(&self.cfg, fetched, &job.stage).await;
                    log::info!(
                        "job {} downloaded: {} ({}, {:?})",
                        job.id,
                        ready.file_name,
                        util::fmt_size(ready.size),
                        ready.kind
                    );
                    if ready.size > self.cfg.upload_limit_bytes() {
                        return Err(JobError::TooBigToSend(ready.size));
                    }
                    let msg =
                        self.upload(job, &ready).await.map_err(|err| JobError::Upload { size: ready.size, err })?;
                    if let Some(entry) = cache_entry(&msg, &ready) {
                        self.store.cache_put(&store_keys(job, &ready.probed), entry);
                    }
                    return Ok(());
                }
            }
        }
        Err(JobError::Dl(DlError::Internal("cached file could not be re-sent".into())))
    }
}

async fn run_job(app: Arc<App>, job: Arc<Job>) {
    let dir = app.cfg.temp_dir.join(format!("job-{}", job.id));
    let _guard = JobGuard { app: app.clone(), id: job.id.clone(), user_id: job.user_id, dir: dir.clone() };
    let started = Instant::now();

    // 1. Same video (any spelling of the link) already sent: no queue at all.
    if let Some(entry) = app.store.cache_find(std::slice::from_ref(&job.key)) {
        if app.send_cached(&job, &entry).await {
            log::info!("job {} served from cache", job.id);
            if let Some(msg) = app.status_msg(&job.id) {
                let _ = app.bot.delete_message(job.chat_id, msg).await;
            }
            return;
        }
    }

    // 2. Wait for this user's turn.
    let Ok(_turn) = app.user_slot(job.user_id).acquire_owned().await else { return };

    // 3. Now the job is live: give it a status message.
    let mut status_msg = app.status_msg(&job.id);
    if status_msg.is_none() && !job.minimal {
        let markup = cancel_markup(job.lang.t(), &job.id);
        status_msg = app.deliver(&job, None, &render_status(&job), Some(markup)).await;
        if let Some(id) = status_msg {
            app.set_status_msg(&job.id, id);
        }
    }

    let result = match status_msg {
        Some(msg) => tokio::select! {
            r = app.pipeline(&job, &dir) => r,
            _ = app.status_loop(&job, msg) => unreachable!("status loop never ends"),
        },
        None => app.pipeline(&job, &dir).await,
    };

    match result {
        Ok(()) => {
            log::info!("job {} done in {:.0?}", job.id, started.elapsed());
            if let Some(msg) = status_msg {
                if let Err(e) = app.bot.delete_message(job.chat_id, msg).await {
                    log::debug!("could not delete status message: {e}");
                }
            }
        }
        Err(err) => {
            match &err {
                JobError::Dl(DlError::Internal(e)) => log::error!("job {} internal error: {e}", job.id),
                JobError::Upload { err, .. } => log::error!("job {} upload failed: {err}", job.id),
                JobError::Dl(e) => log::info!("job {} failed: {e:?}", job.id),
                JobError::NoSpace | JobError::TooBigToSend(_) => log::warn!("job {} refused", job.id),
            }
            let text = error_text(job.lang.t(), &app.cfg, &err, &job.url);
            app.deliver(&job, status_msg, &text, None).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Kind;

    #[test]
    fn captions() {
        assert_eq!(
            build_caption("A & B", "https://x.com/?a=1&b=2"),
            "A &amp; B\n\n<code>https://x.com/?a=1&amp;b=2</code>"
        );
        assert_eq!(build_caption("  ", "https://x.com"), "<code>https://x.com</code>");
        let long = "я".repeat(5000);
        let c = build_caption(&long, "https://x.com/v");
        let visible = c.replace("<code>", "").replace("</code>", "");
        assert!(visible.chars().count() <= CAPTION_MAX);
        assert!(c.ends_with("<code>https://x.com/v</code>"));
    }

    #[test]
    fn errors_are_localized() {
        let cfg = Config::from_env(&crate::config::Env::from_pairs(&[("BOT_TOKEN", "1:x")])).unwrap();
        let ru = Lang::Ru.t();
        let url = "https://a.com/v";
        let text = error_text(ru, &cfg, &JobError::Dl(DlError::Rejected(Reject::TooLong { secs: 4 * 3600 })), url);
        assert!(text.contains("4:00:00") && text.contains("3:00:00") && text.starts_with(ru.err_title));
        assert!(text.contains(url));
        let text = error_text(ru, &cfg, &JobError::Dl(DlError::Failed("ERROR: Private video".into())), url);
        assert!(text.contains(ru.e_private));
        let text = error_text(Lang::En.t(), &cfg, &JobError::Dl(DlError::Failed("weird <thing>".into())), url);
        assert!(text.contains("<code>weird &lt;thing&gt;</code>"));
    }

    fn job(url: &str, quality: Quality) -> Job {
        let video_key = cachekey::video_key(url);
        Job {
            id: "x".into(),
            seq: 0,
            user_id: 1,
            chat_id: 1,
            thread_id: None,
            url: url.into(),
            quality,
            lang: Lang::En,
            minimal: false,
            key: request_key(&video_key, quality),
            video_key,
            stage: watch::channel(Stage::Queued).0,
            sent: Arc::new(AtomicU64::new(0)),
        }
    }

    #[test]
    fn cache_keys_link_requests_to_deliveries() {
        // "Max" that had to drop to 720p to fit is reused by an explicit 720p
        // request for the same video under another spelling of the link.
        let first = job("https://www.youtube.com/watch?v=dQw4w9WgXcQ&si=a", Quality::Max);
        let p = Probed { video_key: Some("youtube:dQw4w9WgXcQ".into()), kind: Kind::Video, height: Some(720) };
        let stored = store_keys(&first, &p);
        assert!(stored.contains(&"youtube:dQw4w9WgXcQ|v@max".to_string()));
        assert!(stored.contains(&"youtube:dQw4w9WgXcQ|v=720".to_string()));

        let second = job("https://youtu.be/dQw4w9WgXcQ", Quality::Height(720));
        assert!(!stored.contains(&second.key), "explicit 720 is not known before probing");
        assert!(probe_keys(&second, &p).iter().any(|k| stored.contains(k)));

        // Unknown site: the URL is the identity until metadata names the video.
        let other = job("https://example.com/v?utm_source=x", Quality::Audio);
        assert_eq!(other.key, "url:https://example.com/v|a");
        let p = Probed { video_key: Some("example:42".into()), kind: Kind::Audio, height: None };
        assert_eq!(store_keys(&other, &p), vec!["example:42|a".to_string(), "url:https://example.com/v|a".to_string()]);
    }
}
