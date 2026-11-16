//! Persistent bot state: per-chat settings, per-user default quality, the
//! file_id cache, titles cache and saved jobs.
//!
//! Powered by a high-performance, tuned SQLite database (`bot.db`) with
//! in-memory write-through caching for chat and quality settings.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::cachekey;
use crate::i18n::Lang;
use crate::types::Quality;

/// Upper bound on cache keys; the oldest tenth is dropped when exceeded.
pub const MAX_CACHE: usize = 20_000;
/// Upper bound on cached titles; the oldest tenth is dropped when exceeded.
pub const MAX_TITLES: usize = 10_000;

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
pub struct ChatCfg {
    pub lang: Option<Lang>,
    pub minimal: bool,
    pub topic: Option<i64>,
    pub active: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct CacheEntry {
    /// "video" | "audio" | "document"
    pub kind: String,
    pub file_id: String,
    pub title: String,
    pub performer: String,
    pub duration: u32,
    pub size: u64,
    pub ts: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct SavedJob {
    pub id: String,
    pub user_id: i64,
    pub chat_id: i64,
    pub thread_id: Option<i64>,
    pub url: String,
    pub quality: String,
    pub lang: Lang,
    pub status_msg: Option<i64>,
    pub user_msg_id: Option<i64>,
    pub created_at: u64,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct TitleEntry {
    pub title: String,
    pub thumb_url: Option<String>,
    pub ts: u64,
}

#[derive(Debug, Clone)]
pub struct PromocodeInfo {
    pub code: String,
    pub duration_days: u32,
    pub max_uses: usize,
    pub used_count: usize,
    #[allow(dead_code)]
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromoError {
    NotFound,
    AlreadyUsedByYou,
    Exhausted,
    DatabaseError,
}

#[derive(Debug, Clone)]
pub struct ActivatePromoResult {
    pub code: String,
    pub duration_days: u32,
    pub expires_at: u64,
}

pub struct Store {
    #[allow(dead_code)]
    db_path: PathBuf,
    conn: Mutex<Connection>,
    chats: Mutex<HashMap<i64, ChatCfg>>,
    quality: Mutex<HashMap<i64, String>>,
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn configure_sqlite(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -16000;        -- 16 MB page cache in RAM
         PRAGMA mmap_size = 268435456;      -- 256 MB memory-mapped I/O
         PRAGMA temp_store = MEMORY;        -- temp tables & indices in memory
         PRAGMA busy_timeout = 5000;        -- 5s timeout on lock contention
         PRAGMA foreign_keys = ON;
         PRAGMA auto_vacuum = INCREMENTAL;  -- keep DB file compact over time",
    )?;
    Ok(())
}

fn init_tables(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS chats (
             chat_id INTEGER PRIMARY KEY,
             lang TEXT,
             minimal INTEGER NOT NULL DEFAULT 0,
             topic INTEGER,
             active INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS user_quality (
             user_id INTEGER PRIMARY KEY,
             quality TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS media_cache (
             key TEXT PRIMARY KEY,
             kind TEXT NOT NULL,
             file_id TEXT NOT NULL,
             title TEXT NOT NULL DEFAULT '',
             performer TEXT NOT NULL DEFAULT '',
             duration INTEGER NOT NULL DEFAULT 0,
             size INTEGER NOT NULL DEFAULT 0,
             ts INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_media_cache_file_id ON media_cache(file_id);
         CREATE INDEX IF NOT EXISTS idx_media_cache_ts ON media_cache(ts);

         CREATE TABLE IF NOT EXISTS titles_cache (
             key TEXT PRIMARY KEY,
             title TEXT NOT NULL,
             thumb_url TEXT,
             ts INTEGER NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_titles_cache_ts ON titles_cache(ts);

         CREATE TABLE IF NOT EXISTS saved_jobs (
             id TEXT PRIMARY KEY,
             user_id INTEGER NOT NULL,
             chat_id INTEGER NOT NULL,
             thread_id INTEGER,
             url TEXT NOT NULL,
             quality TEXT NOT NULL,
             lang TEXT NOT NULL,
             status_msg INTEGER,
             user_msg_id INTEGER,
             created_at INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS user_premium (
             user_id INTEGER PRIMARY KEY,
             is_premium INTEGER NOT NULL DEFAULT 0,
             expires_at INTEGER NOT NULL DEFAULT 0,
             activated_at INTEGER NOT NULL DEFAULT 0,
             warned_at INTEGER NOT NULL DEFAULT 0,
             expired_notified_at INTEGER NOT NULL DEFAULT 0
         );

         CREATE TABLE IF NOT EXISTS tracked_channels (
             channel_id TEXT PRIMARY KEY,
             title TEXT NOT NULL DEFAULT '',
             last_video_id TEXT,
             last_published_at INTEGER NOT NULL DEFAULT 0,
             etag TEXT,
             last_modified TEXT,
             lease_expires_at INTEGER NOT NULL DEFAULT 0,
             last_checked_at INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS idx_tracked_channels_checked ON tracked_channels(last_checked_at);
         CREATE INDEX IF NOT EXISTS idx_tracked_channels_lease ON tracked_channels(lease_expires_at);

         CREATE TABLE IF NOT EXISTS user_subscriptions (
             user_id INTEGER NOT NULL,
             channel_id TEXT NOT NULL,
             is_paused INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL,
             PRIMARY KEY (user_id, channel_id)
         );
         CREATE INDEX IF NOT EXISTS idx_user_subs_channel ON user_subscriptions(channel_id);
         CREATE INDEX IF NOT EXISTS idx_user_subs_user ON user_subscriptions(user_id);

         CREATE TABLE IF NOT EXISTS sent_notifications (
             user_id INTEGER NOT NULL,
             video_id TEXT NOT NULL,
             sent_at INTEGER NOT NULL,
             PRIMARY KEY (user_id, video_id)
         );
         CREATE INDEX IF NOT EXISTS idx_sent_notifications_ts ON sent_notifications(sent_at);

         CREATE TABLE IF NOT EXISTS promocodes (
             code TEXT PRIMARY KEY COLLATE NOCASE,
             duration_days INTEGER NOT NULL,
             max_uses INTEGER NOT NULL DEFAULT 0,
             used_count INTEGER NOT NULL DEFAULT 0,
             created_at INTEGER NOT NULL
         );

         CREATE TABLE IF NOT EXISTS promocode_activations (
             code TEXT NOT NULL COLLATE NOCASE,
             user_id INTEGER NOT NULL,
             activated_at INTEGER NOT NULL,
             PRIMARY KEY (code, user_id)
         );
         CREATE INDEX IF NOT EXISTS idx_promocode_activations_user ON promocode_activations(user_id);",
    )?;

    let now = now_secs() as i64;
    let _ = conn.execute("ALTER TABLE user_premium ADD COLUMN activated_at INTEGER NOT NULL DEFAULT 0", []);
    let _ = conn.execute("ALTER TABLE user_premium ADD COLUMN warned_at INTEGER NOT NULL DEFAULT 0", []);
    let _ = conn.execute("ALTER TABLE user_premium ADD COLUMN expired_notified_at INTEGER NOT NULL DEFAULT 0", []);

    let _ = conn.execute(
        "INSERT OR IGNORE INTO promocodes (code, duration_days, max_uses, used_count, created_at)
         VALUES ('START30', 30, 0, 0, ?1)",
        params![now],
    );

    Ok(())
}

impl Store {
    pub fn load(data_dir: &Path) -> Store {
        let _ = std::fs::create_dir_all(data_dir);
        let db_path = data_dir.join("bot.db");
        let conn = Connection::open(&db_path).expect("failed to open bot.db");
        configure_sqlite(&conn).expect("failed to configure sqlite pragma");
        init_tables(&conn).expect("failed to initialize sqlite tables");

        // Migrate from old state.json or pre-state.json legacy files if present
        Self::migrate_if_needed(&conn, data_dir);

        // Preload in-memory write-through caches for zero-latency lookups
        let mut chats = HashMap::new();
        if let Ok(mut stmt) = conn.prepare("SELECT chat_id, lang, minimal, topic, active FROM chats") {
            if let Ok(rows) = stmt.query_map([], |r| {
                let lang_str: Option<String> = r.get(1)?;
                let lang = lang_str.as_deref().and_then(Lang::from_code);
                let minimal: i64 = r.get(2)?;
                let topic: Option<i64> = r.get(3)?;
                let active: i64 = r.get(4)?;
                Ok((r.get::<_, i64>(0)?, ChatCfg { lang, minimal: minimal != 0, topic, active: active != 0 }))
            }) {
                for item in rows.flatten() {
                    chats.insert(item.0, item.1);
                }
            }
        }

        let mut quality = HashMap::new();
        if let Ok(mut stmt) = conn.prepare("SELECT user_id, quality FROM user_quality") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))) {
                for item in rows.flatten() {
                    quality.insert(item.0, item.1);
                }
            }
        }

        Store { db_path, conn: Mutex::new(conn), chats: Mutex::new(chats), quality: Mutex::new(quality) }
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub fn in_memory() -> Store {
        let conn = Connection::open_in_memory().expect("in-memory db");
        configure_sqlite(&conn).expect("sqlite pragma");
        init_tables(&conn).expect("sqlite tables");
        Store {
            db_path: PathBuf::from(":memory:"),
            conn: Mutex::new(conn),
            chats: Mutex::new(HashMap::new()),
            quality: Mutex::new(HashMap::new()),
        }
    }

    fn migrate_if_needed(conn: &Connection, data_dir: &Path) {
        let json_path = data_dir.join("state.json");
        if json_path.exists() {
            if let Ok(bytes) = std::fs::read(&json_path) {
                if let Ok(data) = serde_json::from_slice::<LegacyData>(&bytes) {
                    log::info!("migrating existing state.json into bot.db…");
                    if let Ok(tx) = conn.unchecked_transaction() {
                        for (chat_id, cfg) in data.chats {
                            let lang_str = cfg.lang.map(|l| l.code().to_string());
                            let _ = tx.execute(
                                "INSERT OR REPLACE INTO chats (chat_id, lang, minimal, topic, active) VALUES (?1, ?2, ?3, ?4, ?5)",
                                params![chat_id, lang_str, if cfg.minimal { 1 } else { 0 }, cfg.topic, if cfg.active { 1 } else { 0 }],
                            );
                        }
                        for (user_id, q) in data.quality {
                            let _ = tx.execute(
                                "INSERT OR REPLACE INTO user_quality (user_id, quality) VALUES (?1, ?2)",
                                params![user_id, q],
                            );
                        }
                        for user_id in data.audio_users {
                            let _ = tx.execute(
                                "INSERT OR IGNORE INTO user_quality (user_id, quality) VALUES (?1, 'audio')",
                                params![user_id],
                            );
                        }
                        for (key, entry) in data.cache {
                            let _ = tx.execute(
                                "INSERT OR REPLACE INTO media_cache (key, kind, file_id, title, performer, duration, size, ts)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                                params![key, entry.kind, entry.file_id, entry.title, entry.performer, entry.duration, entry.size as i64, entry.ts as i64],
                            );
                        }
                        for (key, title) in data.titles {
                            let _ = tx.execute(
                                "INSERT OR REPLACE INTO titles_cache (key, title, thumb_url, ts)
                                 VALUES (?1, ?2, ?3, ?4)",
                                params![key, title.title, title.thumb_url, title.ts as i64],
                            );
                        }
                        for (id, job) in data.jobs {
                            let _ = tx.execute(
                                "INSERT OR REPLACE INTO saved_jobs (id, user_id, chat_id, thread_id, url, quality, lang, status_msg, user_msg_id, created_at)
                                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                                params![id, job.user_id, job.chat_id, job.thread_id, job.url, job.quality, job.lang.code(), job.status_msg, job.user_msg_id, job.created_at as i64],
                            );
                        }
                        if tx.commit().is_ok() {
                            log::info!("state.json successfully migrated to bot.db");
                            let _ = std::fs::rename(&json_path, data_dir.join("state.json.migrated"));
                        }
                    }
                }
            }
        }

        // Migrate pre-state.json legacy json files if any
        let d = import_legacy(data_dir);
        if !d.chats.is_empty() || !d.cache.is_empty() {
            if let Ok(tx) = conn.unchecked_transaction() {
                for (chat_id, cfg) in d.chats {
                    let lang_str = cfg.lang.map(|l| l.code().to_string());
                    let _ = tx.execute(
                        "INSERT OR IGNORE INTO chats (chat_id, lang, minimal, topic, active) VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![chat_id, lang_str, if cfg.minimal { 1 } else { 0 }, cfg.topic, if cfg.active { 1 } else { 0 }],
                    );
                }
                for (key, entry) in d.cache {
                    let _ = tx.execute(
                        "INSERT OR IGNORE INTO media_cache (key, kind, file_id, title, performer, duration, size, ts)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                        params![
                            key,
                            entry.kind,
                            entry.file_id,
                            entry.title,
                            entry.performer,
                            entry.duration,
                            entry.size as i64,
                            entry.ts as i64
                        ],
                    );
                }
                let _ = tx.commit();
            }
        }
    }

    fn edit_chat(&self, chat_id: i64, f: impl FnOnce(&mut ChatCfg)) {
        let mut chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        let entry = chats.entry(chat_id).or_default();
        let before = entry.clone();
        f(entry);
        if *entry != before {
            let updated = entry.clone();
            if updated == ChatCfg::default() {
                chats.remove(&chat_id);
                if let Ok(conn) = self.conn.lock() {
                    let _ = conn.execute("DELETE FROM chats WHERE chat_id = ?1", params![chat_id]);
                }
            } else {
                let lang_str = updated.lang.map(|l| l.code().to_string());
                if let Ok(conn) = self.conn.lock() {
                    let _ = conn.execute(
                        "INSERT INTO chats (chat_id, lang, minimal, topic, active)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(chat_id) DO UPDATE SET
                             lang = excluded.lang,
                             minimal = excluded.minimal,
                             topic = excluded.topic,
                             active = excluded.active",
                        params![
                            chat_id,
                            lang_str,
                            if updated.minimal { 1 } else { 0 },
                            updated.topic,
                            if updated.active { 1 } else { 0 }
                        ],
                    );
                }
            }
        }
    }

    pub fn lang(&self, chat_id: i64) -> Option<Lang> {
        let chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        chats.get(&chat_id).and_then(|c| c.lang)
    }

    pub fn set_lang(&self, chat_id: i64, lang: Lang) {
        self.edit_chat(chat_id, |c| c.lang = Some(lang));
    }

    pub fn minimal(&self, chat_id: i64) -> bool {
        let chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        chats.get(&chat_id).is_some_and(|c| c.minimal)
    }

    pub fn set_minimal(&self, chat_id: i64, on: bool) {
        self.edit_chat(chat_id, |c| c.minimal = on);
    }

    pub fn topic(&self, chat_id: i64) -> Option<i64> {
        let chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        chats.get(&chat_id).and_then(|c| c.topic)
    }

    pub fn set_topic(&self, chat_id: i64, topic: Option<i64>) {
        self.edit_chat(chat_id, |c| c.topic = topic);
    }

    pub fn is_active(&self, chat_id: i64) -> bool {
        let chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        chats.get(&chat_id).is_some_and(|c| c.active)
    }

    pub fn activate(&self, chat_id: i64) {
        self.edit_chat(chat_id, |c| c.active = true);
    }

    /// The user's remembered choice; `None` means "ask every time".
    pub fn default_quality(&self, user_id: i64) -> Option<Quality> {
        let q = self.quality.lock().unwrap_or_else(|e| e.into_inner());
        q.get(&user_id).and_then(|c| Quality::parse(c))
    }

    pub fn set_default_quality(&self, user_id: i64, q: Option<Quality>) {
        let mut quality = self.quality.lock().unwrap_or_else(|e| e.into_inner());
        let changed = match q {
            Some(q) => quality.insert(user_id, q.code()) != Some(q.code()),
            None => quality.remove(&user_id).is_some(),
        };
        if changed {
            if let Ok(conn) = self.conn.lock() {
                match q {
                    Some(q) => {
                        let _ = conn.execute(
                            "INSERT INTO user_quality (user_id, quality) VALUES (?1, ?2)
                             ON CONFLICT(user_id) DO UPDATE SET quality = excluded.quality",
                            params![user_id, q.code()],
                        );
                    }
                    None => {
                        let _ = conn.execute("DELETE FROM user_quality WHERE user_id = ?1", params![user_id]);
                    }
                }
            }
        }
    }

    /// First hit among `keys`, in order.
    pub fn cache_find(&self, keys: &[String]) -> Option<CacheEntry> {
        let Ok(conn) = self.conn.lock() else { return None };
        let mut stmt = conn
            .prepare_cached(
                "SELECT kind, file_id, title, performer, duration, size, ts FROM media_cache WHERE key = ?1 LIMIT 1",
            )
            .ok()?;

        for key in keys {
            let res: Option<CacheEntry> = stmt
                .query_row([key], |r| {
                    let sz: i64 = r.get(5)?;
                    let ts: i64 = r.get(6)?;
                    Ok(CacheEntry {
                        kind: r.get(0)?,
                        file_id: r.get(1)?,
                        title: r.get(2)?,
                        performer: r.get(3)?,
                        duration: r.get(4)?,
                        size: sz.max(0) as u64,
                        ts: ts.max(0) as u64,
                    })
                })
                .optional()
                .unwrap_or(None);

            if res.is_some() {
                return res;
            }
        }
        None
    }

    /// Search cached media by title or performer.
    pub fn cache_search(&self, query: &str, limit: usize) -> Vec<CacheEntry> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let pattern = format!("%{}%", query.trim());
        let mut stmt = match conn.prepare_cached(
            "SELECT kind, file_id, title, performer, duration, size, ts
             FROM media_cache
             WHERE title LIKE ?1 OR performer LIKE ?1
             GROUP BY file_id
             ORDER BY ts DESC
             LIMIT ?2",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };

        let rows = stmt.query_map(params![pattern, limit as i64], |r| {
            let sz: i64 = r.get(5)?;
            let ts: i64 = r.get(6)?;
            Ok(CacheEntry {
                kind: r.get(0)?,
                file_id: r.get(1)?,
                title: r.get(2)?,
                performer: r.get(3)?,
                duration: r.get(4)?,
                size: sz.max(0) as u64,
                ts: ts.max(0) as u64,
            })
        });

        match rows {
            Ok(iter) => iter.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Store one upload under several keys (request + delivered aliases).
    pub fn cache_put(&self, keys: &[String], mut entry: CacheEntry) {
        if keys.is_empty() {
            return;
        }
        entry.ts = now_secs();
        let Ok(mut conn) = self.conn.lock() else { return };
        let Ok(tx) = conn.transaction() else { return };
        {
            let stmt = tx
                .prepare_cached(
                    "INSERT INTO media_cache (key, kind, file_id, title, performer, duration, size, ts)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(key) DO UPDATE SET
                         kind = excluded.kind,
                         file_id = excluded.file_id,
                         title = excluded.title,
                         performer = excluded.performer,
                         duration = excluded.duration,
                         size = excluded.size,
                         ts = excluded.ts",
                )
                .ok();

            if let Some(mut stmt) = stmt {
                for key in keys {
                    let _ = stmt.execute(params![
                        key,
                        entry.kind,
                        entry.file_id,
                        entry.title,
                        entry.performer,
                        entry.duration,
                        entry.size as i64,
                        entry.ts as i64,
                    ]);
                }
            }
        }

        // Evict oldest 10% if limit exceeded
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM media_cache", [], |r| r.get(0)).unwrap_or(0);
        if count > MAX_CACHE as i64 {
            let to_remove = (MAX_CACHE / 10).max(1) as i64;
            let _ = tx.execute(
                "DELETE FROM media_cache WHERE key IN (SELECT key FROM media_cache ORDER BY ts ASC LIMIT ?1)",
                params![to_remove],
            );
        }

        let _ = tx.commit();
    }

    /// Drop every key pointing at a file_id that Telegram no longer accepts.
    pub fn cache_evict_file(&self, file_id: &str) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute("DELETE FROM media_cache WHERE file_id = ?1", params![file_id]);
    }

    pub fn cache_count(&self) -> usize {
        let Ok(conn) = self.conn.lock() else { return 0 };
        conn.query_row("SELECT COUNT(*) FROM media_cache", [], |r| r.get(0)).unwrap_or(0)
    }

    /// First hit among `keys`, returning (title, thumb_url).
    pub fn find_cached_meta(&self, keys: &[String]) -> Option<(String, Option<String>)> {
        for key in keys {
            if let Some(res) = self.meta_find(key) {
                if !res.0.trim().is_empty() {
                    return Some(res);
                }
            }
            if let Some(entry) = self.cache_find(std::slice::from_ref(key)) {
                if !entry.title.trim().is_empty() {
                    return Some((entry.title, None));
                }
            }
        }
        None
    }

    /// First hit among `keys`, checking both title cache and media cache.
    #[allow(dead_code)]
    pub fn find_cached_title(&self, keys: &[String]) -> Option<String> {
        self.find_cached_meta(keys).map(|(t, _)| t)
    }

    /// Look up metadata in the evicting title cache. Updates access time (LRU).
    pub fn meta_find(&self, key: &str) -> Option<(String, Option<String>)> {
        let Ok(conn) = self.conn.lock() else { return None };
        let res: Option<(String, Option<String>)> = conn
            .query_row("SELECT title, thumb_url FROM titles_cache WHERE key = ?1", [key], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .optional()
            .unwrap_or(None);

        if res.is_some() {
            let now = now_secs() as i64;
            let _ = conn.execute("UPDATE titles_cache SET ts = ?1 WHERE key = ?2", params![now, key]);
        }
        res
    }

    /// Look up a title in the evicting title cache. Updates access time (LRU).
    #[allow(dead_code)]
    pub fn title_find(&self, key: &str) -> Option<String> {
        self.meta_find(key).map(|(t, _)| t)
    }

    /// Store metadata in the evicting title cache.
    pub fn meta_put(&self, key: &str, title: &str, thumb_url: Option<&str>) {
        let trimmed = title.trim();
        if key.is_empty() || trimmed.is_empty() {
            return;
        }
        let ts = now_secs() as i64;
        let thumb = thumb_url.map(|s| s.trim()).filter(|s| !s.is_empty());

        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO titles_cache (key, title, thumb_url, ts)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(key) DO UPDATE SET
                 title = excluded.title,
                 thumb_url = excluded.thumb_url,
                 ts = excluded.ts",
            params![key, trimmed, thumb, ts],
        );

        let count: i64 = conn.query_row("SELECT COUNT(*) FROM titles_cache", [], |r| r.get(0)).unwrap_or(0);
        if count > MAX_TITLES as i64 {
            let to_remove = (MAX_TITLES / 10).max(1) as i64;
            let _ = conn.execute(
                "DELETE FROM titles_cache WHERE key IN (SELECT key FROM titles_cache ORDER BY ts ASC LIMIT ?1)",
                params![to_remove],
            );
        }
    }

    /// Store a title in the evicting title cache.
    pub fn title_put(&self, key: &str, title: &str) {
        let existing_thumb = self.meta_find(key).and_then(|(_, th)| th);
        self.meta_put(key, title, existing_thumb.as_deref());
    }

    pub fn clear_cache(&self) -> usize {
        let Ok(conn) = self.conn.lock() else { return 0 };
        let count1: usize = conn.query_row("SELECT COUNT(*) FROM media_cache", [], |r| r.get(0)).unwrap_or(0);
        let count2: usize = conn.query_row("SELECT COUNT(*) FROM titles_cache", [], |r| r.get(0)).unwrap_or(0);
        let _ = conn.execute_batch("DELETE FROM media_cache; DELETE FROM titles_cache;");
        count1 + count2
    }

    pub fn save_active_job(&self, job: SavedJob) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute(
            "INSERT INTO saved_jobs (id, user_id, chat_id, thread_id, url, quality, lang, status_msg, user_msg_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
                 status_msg = excluded.status_msg",
            params![
                job.id,
                job.user_id,
                job.chat_id,
                job.thread_id,
                job.url,
                job.quality,
                job.lang.code(),
                job.status_msg,
                job.user_msg_id,
                job.created_at as i64,
            ],
        );
    }

    pub fn update_active_job_status_msg(&self, id: &str, status_msg: i64) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute("UPDATE saved_jobs SET status_msg = ?1 WHERE id = ?2", params![status_msg, id]);
    }

    pub fn remove_active_job(&self, id: &str) {
        let Ok(conn) = self.conn.lock() else { return };
        let _ = conn.execute("DELETE FROM saved_jobs WHERE id = ?1", params![id]);
    }

    pub fn take_active_jobs(&self) -> Vec<SavedJob> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let mut list = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT id, user_id, chat_id, thread_id, url, quality, lang, status_msg, user_msg_id, created_at FROM saved_jobs",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| {
                let lang_str: String = r.get(6)?;
                let lang = Lang::from_code(&lang_str).unwrap_or(Lang::En);
                let cr: i64 = r.get(9)?;
                Ok(SavedJob {
                    id: r.get(0)?,
                    user_id: r.get(1)?,
                    chat_id: r.get(2)?,
                    thread_id: r.get(3)?,
                    url: r.get(4)?,
                    quality: r.get(5)?,
                    lang,
                    status_msg: r.get(7)?,
                    user_msg_id: r.get(8)?,
                    created_at: cr.max(0) as u64,
                })
            }) {
                for item in rows.flatten() {
                    list.push(item);
                }
            }
        }
        let _ = conn.execute("DELETE FROM saved_jobs", []);
        list
    }

    pub fn known_chats(&self) -> Vec<i64> {
        let mut set = std::collections::HashSet::new();
        let chats = self.chats.lock().unwrap_or_else(|e| e.into_inner());
        set.extend(chats.keys().copied());
        let quality = self.quality.lock().unwrap_or_else(|e| e.into_inner());
        set.extend(quality.keys().copied());
        if let Ok(conn) = self.conn.lock() {
            if let Ok(mut stmt) = conn.prepare(
                "SELECT chat_id FROM chats UNION SELECT user_id FROM user_quality UNION SELECT chat_id FROM saved_jobs",
            ) {
                if let Ok(rows) = stmt.query_map([], |r| r.get::<_, i64>(0)) {
                    for id in rows.flatten() {
                        set.insert(id);
                    }
                }
            }
        }
        set.into_iter().collect()
    }

    /// Passive WAL checkpoint and vacuum freed pages
    pub fn flush(&self) {
        if let Ok(conn) = self.conn.lock() {
            let _ = conn.execute_batch("PRAGMA wal_checkpoint(PASSIVE); PRAGMA incremental_vacuum;");
        }
    }

    /// Asynchronously checkpoint WAL without blocking caller
    pub async fn flush_async(&self) {
        // In SQLite with WAL mode, disk writes happen asynchronously and safely.
        // Periodic WAL checkpoints keep the WAL file small.
        self.flush();
    }

    // ================= YouTube Subscriptions & Premium =================

    pub fn is_user_premium(&self, user_id: i64) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT is_premium, expires_at FROM user_premium WHERE user_id = ?1", params![user_id], |r| {
            let is_prem: i64 = r.get(0)?;
            let expires: i64 = r.get(1)?;
            let now = now_secs() as i64;
            Ok(is_prem != 0 && (expires == 0 || expires > now))
        })
        .unwrap_or(false)
    }

    #[allow(dead_code)]
    pub fn get_user_premium_info(&self, user_id: i64) -> (bool, u64) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT is_premium, expires_at FROM user_premium WHERE user_id = ?1", params![user_id], |r| {
            let is_prem: i64 = r.get(0)?;
            let expires: i64 = r.get(1)?;
            Ok((is_prem != 0, expires as u64))
        })
        .unwrap_or((false, 0))
    }

    pub fn set_user_premium(&self, user_id: i64, is_premium: bool, expires_at: u64) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let _ = conn.execute(
            "INSERT INTO user_premium (user_id, is_premium, expires_at, activated_at, warned_at, expired_notified_at)
             VALUES (?1, ?2, ?3, ?4, 0, 0)
             ON CONFLICT(user_id) DO UPDATE SET
                is_premium = excluded.is_premium,
                expires_at = excluded.expires_at,
                activated_at = CASE WHEN user_premium.activated_at = 0 THEN excluded.activated_at ELSE user_premium.activated_at END,
                warned_at = 0,
                expired_notified_at = 0",
            params![user_id, if is_premium { 1 } else { 0 }, expires_at as i64, now],
        );
        if is_premium {
            let _ = conn.execute("UPDATE user_subscriptions SET is_paused = 0 WHERE user_id = ?1", params![user_id]);
        }
    }

    /// Returns users whose premium expires in <= max_future_secs (e.g. 24h) and who haven't been warned yet.
    /// (user_id, expires_at, activated_at)
    pub fn get_users_to_warn_expiry(&self, now: u64, max_future_secs: u64) -> Vec<(i64, u64, u64)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT user_id, expires_at, activated_at FROM user_premium
             WHERE is_premium = 1 AND expires_at > ?1 AND expires_at <= ?2 AND warned_at = 0",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let max_exp = (now + max_future_secs) as i64;
        let rows = stmt.query_map(params![now as i64, max_exp], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64, r.get::<_, i64>(2)? as u64))
        });
        rows.map(|iter| iter.flatten().collect()).unwrap_or_default()
    }

    pub fn mark_user_expiry_warned(&self, user_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let _ = conn.execute("UPDATE user_premium SET warned_at = ?1 WHERE user_id = ?2", params![now, user_id]);
    }

    /// Returns users whose premium has expired (expires_at <= now) and who haven't received expiry notification yet.
    /// (user_id, activated_at)
    pub fn get_users_expired_to_notify(&self, now: u64) -> Vec<(i64, u64)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT user_id, activated_at FROM user_premium
             WHERE is_premium = 1 AND expires_at > 0 AND expires_at <= ?1 AND expired_notified_at = 0",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![now as i64], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? as u64))
        });
        rows.map(|iter| iter.flatten().collect()).unwrap_or_default()
    }

    pub fn mark_user_expired_notified(&self, user_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let _ = conn.execute(
            "UPDATE user_premium SET expired_notified_at = ?1, is_premium = 0 WHERE user_id = ?2",
            params![now, user_id],
        );
        let _ = conn.execute("UPDATE user_subscriptions SET is_paused = 1 WHERE user_id = ?1", params![user_id]);
    }

    pub fn add_premium_days(&self, user_id: i64, days: u32) -> u64 {
        let now = now_secs();
        let (is_prem, cur_expires) = self.get_user_premium_info(user_id);
        let start_ts = if is_prem && cur_expires > now { cur_expires } else { now };
        let new_expires = start_ts + (days as u64) * 86400;
        self.set_user_premium(user_id, true, new_expires);
        new_expires
    }

    pub fn gift_premium_to_users(&self, user_ids: &[i64], days: u32) -> usize {
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = match conn.transaction() {
            Ok(tx) => tx,
            Err(_) => return 0,
        };
        let now = now_secs() as i64;
        let add_secs = (days as i64) * 86400;
        let mut count = 0;
        for &uid in user_ids {
            let cur: (i64, i64) = tx
                .query_row(
                    "SELECT is_premium, expires_at FROM user_premium WHERE user_id = ?1",
                    params![uid],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap_or((0, 0));
            let start = if cur.0 != 0 && cur.1 > now { cur.1 } else { now };
            let new_exp = start + add_secs;
            let res = tx.execute(
                "INSERT INTO user_premium (user_id, is_premium, expires_at) VALUES (?1, 1, ?2)
                 ON CONFLICT(user_id) DO UPDATE SET is_premium = 1, expires_at = ?2",
                params![uid, new_exp],
            );
            if res.is_ok() {
                let _ = tx.execute("UPDATE user_subscriptions SET is_paused = 0 WHERE user_id = ?1", params![uid]);
                count += 1;
            }
        }
        let _ = tx.commit();
        count
    }

    pub fn create_promocode(&self, code: &str, duration_days: u32, max_uses: usize) -> Result<bool, rusqlite::Error> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let res = conn.execute(
            "INSERT INTO promocodes (code, duration_days, max_uses, used_count, created_at)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![code.trim(), duration_days, max_uses as i64, now],
        );
        match res {
            Ok(_) => Ok(true),
            Err(rusqlite::Error::SqliteFailure(e, _)) if e.extended_code == 1555 || e.extended_code == 2067 => Ok(false),
            Err(e) => Err(e),
        }
    }

    pub fn list_promocodes(&self) -> Vec<PromocodeInfo> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT code, duration_days, max_uses, used_count, created_at FROM promocodes ORDER BY created_at DESC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok(PromocodeInfo {
                code: r.get(0)?,
                duration_days: r.get(1)?,
                max_uses: r.get::<_, i64>(2)? as usize,
                used_count: r.get::<_, i64>(3)? as usize,
                created_at: r.get::<_, i64>(4)? as u64,
            })
        });
        rows.map(|iter| iter.flatten().collect()).unwrap_or_default()
    }

    pub fn has_user_activated_code(&self, user_id: i64, code: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT 1 FROM promocode_activations WHERE user_id = ?1 AND code = ?2 COLLATE NOCASE",
            params![user_id, code.trim()],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub fn has_user_activated_welcome(&self, user_id: i64) -> bool {
        self.has_user_activated_code(user_id, "START30")
    }

    pub fn activate_promocode(&self, user_id: i64, code: &str) -> Result<ActivatePromoResult, PromoError> {
        let trimmed = code.trim();
        if trimmed.is_empty() {
            return Err(PromoError::NotFound);
        }

        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction().map_err(|_| PromoError::DatabaseError)?;

        // 1. Fetch promocode
        let promo: Result<(String, u32, i64, i64), _> = tx.query_row(
            "SELECT code, duration_days, max_uses, used_count FROM promocodes WHERE code = ?1 COLLATE NOCASE",
            params![trimmed],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        );
        let (exact_code, duration_days, max_uses, used_count) = match promo {
            Ok(data) => data,
            Err(rusqlite::Error::QueryReturnedNoRows) => return Err(PromoError::NotFound),
            Err(_) => return Err(PromoError::DatabaseError),
        };

        // 2. Check max uses
        if max_uses > 0 && used_count >= max_uses {
            return Err(PromoError::Exhausted);
        }

        // 3. Check if user already activated this code
        let already: Result<i64, _> = tx.query_row(
            "SELECT 1 FROM promocode_activations WHERE user_id = ?1 AND code = ?2 COLLATE NOCASE",
            params![user_id, trimmed],
            |r| r.get(0),
        );
        if already.is_ok() {
            return Err(PromoError::AlreadyUsedByYou);
        }

        // 4. Record activation
        let now = now_secs() as i64;
        tx.execute(
            "INSERT INTO promocode_activations (code, user_id, activated_at) VALUES (?1, ?2, ?3)",
            params![exact_code, user_id, now],
        )
        .map_err(|_| PromoError::DatabaseError)?;

        // 5. Increment used_count
        tx.execute(
            "UPDATE promocodes SET used_count = used_count + 1 WHERE code = ?1 COLLATE NOCASE",
            params![trimmed],
        )
        .map_err(|_| PromoError::DatabaseError)?;

        // 6. Calculate new premium expiry and update user_premium
        let cur_prem: (i64, i64) = tx
            .query_row(
                "SELECT is_premium, expires_at FROM user_premium WHERE user_id = ?1",
                params![user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));
        let start_ts = if cur_prem.0 != 0 && cur_prem.1 > now { cur_prem.1 } else { now };
        let new_expires = (start_ts + (duration_days as i64) * 86400) as u64;

        tx.execute(
            "INSERT INTO user_premium (user_id, is_premium, expires_at) VALUES (?1, 1, ?2)
             ON CONFLICT(user_id) DO UPDATE SET is_premium = 1, expires_at = ?2",
            params![user_id, new_expires as i64],
        )
        .map_err(|_| PromoError::DatabaseError)?;

        let _ = tx.execute("UPDATE user_subscriptions SET is_paused = 0 WHERE user_id = ?1", params![user_id]);

        tx.commit().map_err(|_| PromoError::DatabaseError)?;

        Ok(ActivatePromoResult {
            code: exact_code,
            duration_days,
            expires_at: new_expires,
        })
    }

    pub fn count_user_subscriptions(&self, user_id: i64) -> usize {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT COUNT(*) FROM user_subscriptions WHERE user_id = ?1", params![user_id], |r| {
            r.get::<_, i64>(0)
        })
        .unwrap_or(0) as usize
    }

    pub fn get_user_subscriptions(&self, user_id: i64) -> Vec<(String, String)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = match conn.prepare(
            "SELECT s.channel_id, c.title FROM user_subscriptions s
             JOIN tracked_channels c ON s.channel_id = c.channel_id
             WHERE s.user_id = ?1 ORDER BY s.created_at ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![user_id], |r| Ok((r.get(0)?, r.get(1)?)));
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn add_subscription(
        &self,
        user_id: i64,
        channel_id: &str,
        title: &str,
        latest_video_id: Option<&str>,
    ) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;

        let existed: bool = conn
            .query_row("SELECT 1 FROM tracked_channels WHERE channel_id = ?1", params![channel_id], |_| Ok(true))
            .unwrap_or(false);

        conn.execute(
            "INSERT INTO tracked_channels (channel_id, title, last_video_id, last_checked_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(channel_id) DO UPDATE SET title = CASE WHEN excluded.title != '' THEN excluded.title ELSE tracked_channels.title END",
            params![channel_id, title, latest_video_id, now],
        ).map_err(|e| format!("db error: {e}"))?;

        conn.execute(
            "INSERT OR REPLACE INTO user_subscriptions (user_id, channel_id, is_paused, created_at)
             VALUES (?1, ?2, 0, ?3)",
            params![user_id, channel_id, now],
        )
        .map_err(|e| format!("db error: {e}"))?;

        Ok(!existed)
    }

    pub fn remove_subscription(&self, user_id: i64, channel_id: &str) -> Result<bool, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.execute(
            "DELETE FROM user_subscriptions WHERE user_id = ?1 AND channel_id = ?2",
            params![user_id, channel_id],
        )
        .map_err(|e| format!("db error: {e}"))?;

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM user_subscriptions WHERE channel_id = ?1", params![channel_id], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        if remaining == 0 {
            let _ = conn.execute("DELETE FROM tracked_channels WHERE channel_id = ?1", params![channel_id]);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn get_channel_subscribers(&self, channel_id: &str) -> Vec<i64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let mut stmt = match conn.prepare(
            "SELECT s.user_id FROM user_subscriptions s
             LEFT JOIN user_premium p ON s.user_id = p.user_id
             WHERE s.channel_id = ?1 AND s.is_paused = 0
               AND (p.is_premium = 1 AND (p.expires_at = 0 OR p.expires_at > ?2))",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![channel_id, now], |r| r.get(0));
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn mark_notification_sent(&self, user_id: i64, video_id: &str) -> bool {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        conn.execute(
            "INSERT INTO sent_notifications (user_id, video_id, sent_at) VALUES (?1, ?2, ?3)",
            params![user_id, video_id, now],
        )
        .is_ok()
    }

    pub fn pause_user_subscriptions(&self, user_id: i64) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let _ = conn.execute("UPDATE user_subscriptions SET is_paused = 1 WHERE user_id = ?1", params![user_id]);
    }

    pub fn get_next_channel_for_rss(
        &self,
        checked_before: u64,
    ) -> Option<(String, Option<String>, Option<String>, Option<String>)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row(
            "SELECT channel_id, etag, last_modified, last_video_id FROM tracked_channels
             WHERE last_checked_at < ?1
             ORDER BY last_checked_at ASC LIMIT 1",
            params![checked_before as i64],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .unwrap_or(None)
    }

    pub fn get_oldest_channel_check_time(&self) -> Option<u64> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT MIN(last_checked_at) FROM tracked_channels", [], |r| r.get::<_, Option<i64>>(0))
            .unwrap_or(None)
            .map(|t| t.max(0) as u64)
    }

    pub fn touch_channel_checked(&self, channel_id: &str) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let _ = conn.execute(
            "UPDATE tracked_channels SET last_checked_at = ?1 WHERE channel_id = ?2",
            params![now, channel_id],
        );
    }

    pub fn update_channel_rss(
        &self,
        channel_id: &str,
        etag: Option<&str>,
        last_modified: Option<&str>,
        last_video_id: Option<&str>,
    ) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let now = now_secs() as i64;
        let _ = conn.execute(
            "UPDATE tracked_channels SET last_checked_at = ?1, etag = ?2, last_modified = ?3,
             last_video_id = COALESCE(?4, last_video_id)
             WHERE channel_id = ?5",
            params![now, etag, last_modified, last_video_id, channel_id],
        );
    }

    pub fn get_channels_to_renew(&self, before_timestamp: u64) -> Vec<String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let mut stmt = match conn.prepare("SELECT channel_id FROM tracked_channels WHERE lease_expires_at < ?1") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![before_timestamp as i64], |r| r.get(0));
        rows.map(|r| r.flatten().collect()).unwrap_or_default()
    }

    pub fn update_channel_lease(&self, channel_id: &str, lease_expires_at: u64) {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let _ = conn.execute(
            "UPDATE tracked_channels SET lease_expires_at = ?1 WHERE channel_id = ?2",
            params![lease_expires_at as i64, channel_id],
        );
    }
}

#[derive(Deserialize, Default)]
struct LegacyData {
    #[serde(default)]
    chats: HashMap<i64, ChatCfg>,
    #[serde(default)]
    quality: HashMap<i64, String>,
    #[serde(default)]
    audio_users: Vec<i64>,
    #[serde(default)]
    cache: HashMap<String, CacheEntry>,
    #[serde(default)]
    titles: HashMap<String, TitleEntry>,
    #[serde(default)]
    jobs: HashMap<String, SavedJob>,
}

#[derive(Deserialize)]
struct OldEntry {
    kind: String,
    file_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    performer: String,
    #[serde(default)]
    duration: u32,
}

fn import_legacy(dir: &Path) -> LegacyData {
    let mut data = LegacyData::default();
    if let Ok(text) = std::fs::read_to_string(dir.join("allowed_chats.json")) {
        if let Ok(list) = serde_json::from_str::<Vec<i64>>(&text) {
            for id in list {
                data.chats.entry(id).or_default().active = true;
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("minimal_mode.json")) {
        if let Ok(list) = serde_json::from_str::<Vec<i64>>(&text) {
            for id in list {
                data.chats.entry(id).or_default().minimal = true;
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("topic_locks.json")) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, i64>>(&text) {
            for (chat, topic) in map {
                if let Ok(id) = chat.parse::<i64>() {
                    data.chats.entry(id).or_default().topic = Some(topic);
                }
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join("media_cache.json")) {
        if let Ok(map) = serde_json::from_str::<HashMap<String, OldEntry>>(&text) {
            for (k, v) in map {
                let entry = CacheEntry {
                    kind: v.kind,
                    file_id: v.file_id,
                    title: v.title,
                    performer: v.performer,
                    duration: v.duration,
                    size: 0,
                    ts: 0,
                };
                if let Some((_, url)) = k.split_once('\n') {
                    let vkey = cachekey::video_key(url);
                    let q = if entry.kind == "audio" { Quality::Audio } else { Quality::Max };
                    data.cache.insert(cachekey::request_key(&vkey, q), entry);
                }
            }
        }
    }
    data
}

/// Returns true if `now` is within +/- 6 hours of user's active hour-of-day.
pub fn is_user_daytime(activated_at: u64, now: u64) -> bool {
    if activated_at == 0 {
        let utc_hour = (now % 86400) / 3600;
        return (7..=20).contains(&utc_hour);
    }
    let act_tod = (activated_at % 86400) as i64;
    let now_tod = (now % 86400) as i64;
    let diff = (now_tod - act_tod).abs();
    let circular_diff = diff.min(86400 - diff);
    circular_diff <= 6 * 3600
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tgmb-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(id: &str) -> CacheEntry {
        CacheEntry {
            kind: "video".into(),
            file_id: id.into(),
            title: "T".into(),
            performer: String::new(),
            duration: 9,
            size: 1024,
            ts: 0,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = temp_dir("roundtrip");
        let store = Store::load(&dir);
        assert_eq!(store.lang(1), None);
        store.set_lang(1, Lang::Ru);
        store.set_minimal(-100, true);
        store.set_topic(-100, Some(7));
        store.activate(-100);
        store.set_default_quality(5, Some(Quality::Height(720)));
        store.set_default_quality(6, Some(Quality::Audio));
        store.set_default_quality(6, None);
        store.cache_put(&["a|v@max".into(), "a|v=720".into()], entry("F"));
        store.flush();

        let again = Store::load(&dir);
        assert_eq!(again.lang(1), Some(Lang::Ru));
        assert!(again.minimal(-100) && again.is_active(-100));
        assert_eq!(again.topic(-100), Some(7));
        assert_eq!(again.default_quality(5), Some(Quality::Height(720)));
        assert_eq!(again.default_quality(6), None);
        assert_eq!(again.cache_find(&["nope".into(), "a|v=720".into()]).unwrap().file_id, "F");

        again.set_topic(-100, None);
        again.cache_evict_file("F");
        assert!(again.cache_find(&["a|v@max".into(), "a|v=720".into()]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn migrates_audio_users() {
        let dir = temp_dir("audio-users");
        std::fs::write(dir.join("state.json"), r#"{"audio_users": [7], "cache": {}}"#).unwrap();
        let store = Store::load(&dir);
        assert_eq!(store.default_quality(7), Some(Quality::Audio));
        store.flush();
        assert!(dir.join("state.json.migrated").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_import() {
        let dir = temp_dir("legacy");
        std::fs::write(dir.join("allowed_chats.json"), "[-1001, -1002]").unwrap();
        std::fs::write(dir.join("minimal_mode.json"), "[-1002]").unwrap();
        std::fs::write(dir.join("topic_locks.json"), r#"{"-1001": 42}"#).unwrap();
        std::fs::write(
            dir.join("media_cache.json"),
            r#"{"auto\nhttps://youtu.be/dQw4w9WgXcQ": {"kind": "video", "file_id": "X", "duration": 12},
                "audio\nhttps://a.com/s": {"kind": "audio", "file_id": "Y", "title": "S", "performer": "P", "duration": 3}}"#,
        )
        .unwrap();
        let store = Store::load(&dir);
        assert!(store.is_active(-1001) && store.is_active(-1002));
        assert!(store.minimal(-1002) && !store.minimal(-1001));
        assert_eq!(store.topic(-1001), Some(42));
        // Imported by video id: any spelling of the link hits.
        let yt = cachekey::video_key("https://www.youtube.com/watch?v=dQw4w9WgXcQ&si=x");
        assert_eq!(store.cache_find(&[cachekey::request_key(&yt, Quality::Max)]).unwrap().file_id, "X");
        let s = cachekey::video_key("https://a.com/s");
        assert_eq!(store.cache_find(&[cachekey::request_key(&s, Quality::Audio)]).unwrap().performer, "P");
        store.flush();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_is_bounded() {
        let dir = temp_dir("bounded");
        let store = Store::load(&dir);
        for i in 0..(MAX_CACHE + 50) {
            store.cache_put(&[format!("url:https://a.com/{i}|v@max")], entry(&i.to_string()));
        }
        assert!(store.cache_count() <= MAX_CACHE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_cache_is_bounded_and_evicts() {
        let dir = temp_dir("title-bounded");
        let store = Store::load(&dir);
        for i in 0..(MAX_TITLES + 50) {
            store.title_put(&format!("key_{i}"), &format!("Title {i}"));
        }
        // The most recently added entries must be present
        assert_eq!(store.title_find(&format!("key_{}", MAX_TITLES + 40)), Some(format!("Title {}", MAX_TITLES + 40)));

        store.meta_put("yt:123", "Cool Video", Some("https://example.com/thumb.jpg"));
        assert_eq!(
            store.find_cached_meta(&["yt:123".to_string()]),
            Some(("Cool Video".to_string(), Some("https://example.com/thumb.jpg".to_string())))
        );
        assert_eq!(store.find_cached_title(&["yt:123".to_string()]), Some("Cool Video".to_string()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_jobs_lifecycle() {
        let dir = temp_dir("saved-jobs");
        let store = Store::load(&dir);
        let job = SavedJob {
            id: "job123".into(),
            user_id: 100,
            chat_id: 200,
            thread_id: None,
            url: "https://example.com/video".into(),
            quality: "720".into(),
            lang: Lang::En,
            status_msg: Some(300),
            user_msg_id: Some(400),
            created_at: 1000,
        };
        store.save_active_job(job);
        store.update_active_job_status_msg("job123", 305);

        // Reload from disk
        let reloaded = Store::load(&dir);
        let jobs = reloaded.take_active_jobs();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "job123");
        assert_eq!(jobs[0].status_msg, Some(305));

        // After take_active_jobs, store has no active jobs
        assert!(reloaded.take_active_jobs().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn subscriptions_and_premium_lifecycle() {
        let dir = temp_dir("subs-premium");
        let store = Store::load(&dir);

        // 1. Premium toggle
        assert!(!store.is_user_premium(100));
        store.set_user_premium(100, true, 0);
        assert!(store.is_user_premium(100));

        // 2. Subscriptions
        assert_eq!(store.count_user_subscriptions(100), 0);
        let is_new = store.add_subscription(100, "UC12345", "Test Channel", Some("vid1")).unwrap();
        assert!(is_new); // first time in system
        assert_eq!(store.count_user_subscriptions(100), 1);

        // Add second user to same channel
        store.set_user_premium(200, true, 0);
        let is_new2 = store.add_subscription(200, "UC12345", "Test Channel", Some("vid1")).unwrap();
        assert!(!is_new2); // channel already existed in system

        let subscribers = store.get_channel_subscribers("UC12345");
        assert_eq!(subscribers.len(), 2);
        assert!(subscribers.contains(&100) && subscribers.contains(&200));

        // 3. Deduplication: mark_notification_sent
        assert!(store.mark_notification_sent(100, "vid2"));
        assert!(!store.mark_notification_sent(100, "vid2")); // duplicate returns false!

        // 4. Pause subscriptions on bot block
        store.pause_user_subscriptions(100);
        let active_subs = store.get_channel_subscribers("UC12345");
        assert_eq!(active_subs, vec![200]); // 100 is paused, only 200 is returned

        // Unpause on premium re-set
        store.set_user_premium(100, true, 0);
        assert_eq!(store.get_channel_subscribers("UC12345").len(), 2);

        // 5. Unsubscribe
        let last_left1 = store.remove_subscription(100, "UC12345").unwrap();
        assert!(!last_left1); // 200 still subscribed
        let last_left2 = store.remove_subscription(200, "UC12345").unwrap();
        assert!(last_left2); // 0 subscribers left, returns true for WebSub unsubscription!

        // 6. Cooldown query
        store.add_subscription(100, "UC999", "Another Channel", None).unwrap();
        let now = now_secs();
        assert!(store.get_next_channel_for_rss(now.saturating_sub(600)).is_none());
        assert!(store.get_next_channel_for_rss(now + 1).is_some());
        assert!(store.get_oldest_channel_check_time().is_some());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_search_by_title_and_performer() {
        let dir = temp_dir("cache-search");
        let store = Store::load(&dir);

        let e1 = CacheEntry {
            kind: "video".into(),
            file_id: "file_vid_1".into(),
            title: "Rust Language Tutorial for Beginners".into(),
            performer: "Ferris".into(),
            duration: 120,
            size: 1024,
            ts: 100,
        };
        let e2 = CacheEntry {
            kind: "audio".into(),
            file_id: "file_aud_1".into(),
            title: "Bohemian Rhapsody".into(),
            performer: "Queen".into(),
            duration: 354,
            size: 2048,
            ts: 200,
        };

        store.cache_put(&["key1".into()], e1);
        store.cache_put(&["key2".into()], e2);

        // Search by title substring (case-insensitive)
        let res1 = store.cache_search("tutorial", 5);
        assert_eq!(res1.len(), 1);
        assert_eq!(res1[0].file_id, "file_vid_1");

        // Search by performer
        let res2 = store.cache_search("Queen", 5);
        assert_eq!(res2.len(), 1);
        assert_eq!(res2[0].file_id, "file_aud_1");

        // Search non-existent
        let res3 = store.cache_search("nonexistent query", 5);
        assert!(res3.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn promocode_and_gift_lifecycle() {
        let dir = temp_dir("promo-gift-test");
        let store = Store::load(&dir);

        // START30 is seeded automatically
        assert!(!store.has_user_activated_welcome(500));
        let res = store.activate_promocode(500, "start30").expect("should activate START30");
        assert_eq!(res.duration_days, 30);
        assert!(store.is_user_premium(500));
        assert!(store.has_user_activated_welcome(500));

        // Can't activate same code twice
        let err = store.activate_promocode(500, "START30").unwrap_err();
        assert_eq!(err, PromoError::AlreadyUsedByYou);

        // Create limited promo: max 1 use
        store.create_promocode("GIFT100", 100, 1).unwrap();
        let p_res = store.activate_promocode(500, "gift100").expect("stacks on existing");
        assert_eq!(p_res.duration_days, 100);
        // User now has 30 + 100 days of premium
        let (is_p, exp) = store.get_user_premium_info(500);
        assert!(is_p);
        assert!(exp > now_secs() + 125 * 86400);

        // Another user tries GIFT100 -> exhausted
        let err2 = store.activate_promocode(600, "GIFT100").unwrap_err();
        assert_eq!(err2, PromoError::Exhausted);

        // Gift all
        let gifted = store.gift_premium_to_users(&[500, 600, 700], 7);
        assert_eq!(gifted, 3);
        assert!(store.is_user_premium(600));
        assert!(store.is_user_premium(700));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn expiry_warning_and_daytime_check() {
        let dir = temp_dir("expiry-test");
        let store = Store::load(&dir);
        let now = now_secs();

        // Daytime check: same hour is daytime
        assert!(is_user_daytime(now, now));
        // 4 hours difference is daytime
        assert!(is_user_daytime(now, now + 4 * 3600));
        // 12 hours difference (opposite half of day, e.g. 03:00 vs 15:00) is night
        assert!(!is_user_daytime(now, now + 12 * 3600));

        // User expires in 12 hours
        store.set_user_premium(1001, true, now + 12 * 3600);
        let warn_list = store.get_users_to_warn_expiry(now, 24 * 3600);
        assert!(warn_list.iter().any(|(uid, _, _)| *uid == 1001));

        store.mark_user_expiry_warned(1001);
        let warn_list2 = store.get_users_to_warn_expiry(now, 24 * 3600);
        assert!(!warn_list2.iter().any(|(uid, _, _)| *uid == 1001));

        // User already expired
        store.set_user_premium(1002, true, now.saturating_sub(60));
        let exp_list = store.get_users_expired_to_notify(now);
        assert!(exp_list.iter().any(|(uid, _)| *uid == 1002));

        store.mark_user_expired_notified(1002);
        let exp_list2 = store.get_users_expired_to_notify(now);
        assert!(!exp_list2.iter().any(|(uid, _)| *uid == 1002));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
