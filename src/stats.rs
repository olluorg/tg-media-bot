//! Analytics & usage statistics stored in an embedded SQLite database.
//!
//! Includes asynchronous non-blocking event collection, covering indices,
//! sparkline trend visualization, error breakdown, retention metrics, and hot backups.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64
}

pub fn fmt_traffic(bytes: u64) -> String {
    let mb = bytes as f64 / 1_048_576.0;
    if mb >= 1024.0 * 1024.0 {
        format!("{:.2} TB", mb / (1024.0 * 1024.0))
    } else if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else if mb >= 1.0 {
        format!("{mb:.1} MB")
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub fn sparkline(values: &[i64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    const BARS: [char; 8] = [' ', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = values.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return values.iter().map(|_| ' ').collect();
    }
    values
        .iter()
        .map(|&v| {
            let idx = ((v.max(0) as f64 / max as f64) * 7.0).round() as usize;
            BARS[idx.min(7)]
        })
        .collect()
}

pub fn detect_platform(url: &str) -> &'static str {
    let lower = url.to_lowercase();
    if lower.contains("youtube.com") || lower.contains("youtu.be") {
        "YouTube"
    } else if lower.contains("tiktok.com") {
        "TikTok"
    } else if lower.contains("instagram.com") {
        "Instagram"
    } else if lower.contains("twitter.com") || lower.contains("x.com") {
        "X (Twitter)"
    } else if lower.contains("reddit.com") || lower.contains("redd.it") {
        "Reddit"
    } else if lower.contains("vk.com") || lower.contains("vkvideo.ru") {
        "VK"
    } else if lower.contains("facebook.com") || lower.contains("fb.watch") {
        "Facebook"
    } else if lower.contains("twitch.tv") {
        "Twitch"
    } else if lower.contains("pinterest.com") || lower.contains("pin.it") {
        "Pinterest"
    } else if lower.contains("soundcloud.com") {
        "SoundCloud"
    } else if lower.contains("vimeo.com") {
        "Vimeo"
    } else if lower.contains("bilibili.com") {
        "Bilibili"
    } else if lower.contains("threads.net") {
        "Threads"
    } else if lower.contains("rutube.ru") {
        "Rutube"
    } else if lower.contains("ok.ru") {
        "Odnoklassniki"
    } else {
        "Other"
    }
}

#[derive(Debug, Default, Clone)]
pub struct SummaryStats {
    pub total_users: i64,
    pub new_today: i64,
    pub new_week: i64,
    pub new_month: i64,

    pub dau: i64,
    pub wau: i64,
    pub mau: i64,

    pub total_downloads: i64,
    pub total_bytes: u64,
    pub total_videos: i64,
    pub total_audios: i64,

    pub today_downloads: i64,
    pub today_bytes: u64,
    pub week_downloads: i64,
    pub week_bytes: u64,
    pub month_downloads: i64,
    pub month_bytes: u64,

    pub cached_downloads: i64,
    pub cached_percent: f32,

    pub avg_per_active_user: f64,
    pub median_per_user: i64,
    pub retention_rate: f32,
}

#[derive(Debug, Clone)]
pub struct TopUser {
    pub user_id: i64,
    pub username: Option<String>,
    pub first_name: Option<String>,
    pub download_count: i64,
    pub total_bytes: u64,
    #[allow(dead_code)]
    pub last_seen: i64,
}

#[derive(Debug, Clone)]
pub struct UserPersonalStats {
    #[allow(dead_code)]
    pub user_id: i64,
    pub first_seen: i64,
    pub total_downloads: i64,
    pub total_videos: i64,
    pub total_audios: i64,
    pub total_bytes: u64,
    pub rank: i64,
    pub total_users: i64,
    pub top_platforms: Vec<PlatformStat>,
}

#[derive(Debug, Clone)]
pub struct PlatformStat {
    pub name: String,
    pub count: i64,
    pub percent: f32,
}

#[derive(Debug, Clone)]
pub struct DailyStat {
    pub date: String,
    pub count: i64,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ErrorStat {
    pub reason: String,
    pub count: i64,
    pub percent: f32,
}

pub enum StatEvent {
    User {
        user_id: i64,
        username: Option<String>,
        first_name: Option<String>,
    },
    Download {
        user_id: i64,
        url: String,
        media_kind: String,
        file_size: u64,
        duration: u32,
        is_cached: bool,
        status: String,
        error_reason: Option<String>,
    },
}

pub struct StatsDb {
    conn: Arc<Mutex<Connection>>,
    tx: UnboundedSender<StatEvent>,
}

fn sync_write_user(conn: &Connection, user_id: i64, username: Option<&str>, first_name: Option<&str>) {
    let now = now_secs();
    let _ = conn.execute(
        "INSERT INTO users (user_id, username, first_name, first_seen, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(user_id) DO UPDATE SET
             username = COALESCE(?2, username),
             first_name = COALESCE(?3, first_name),
             last_seen = ?4;",
        params![user_id, username, first_name, now],
    );
}

#[allow(clippy::too_many_arguments)]
fn sync_write_download(
    conn: &Connection,
    user_id: i64,
    url: &str,
    media_kind: &str,
    file_size: u64,
    duration: u32,
    is_cached: bool,
    status: &str,
    error_reason: Option<&str>,
) {
    let now = now_secs();
    let extractor = detect_platform(url);
    let _ = conn.execute(
        "INSERT INTO downloads (user_id, created_at, url, extractor, media_kind, file_size, duration, is_cached, status, error_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10);",
        params![
            user_id,
            now,
            url,
            extractor,
            media_kind,
            file_size as i64,
            duration as i64,
            if is_cached { 1 } else { 0 },
            status,
            error_reason,
        ],
    );
}

impl StatsDb {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        if let Some(parent) = path.parent() {
            let old_stats = parent.join("stats.db");
            if old_stats.exists() && old_stats != path {
                if let Ok(count) = conn.query_row::<i64, _, _>("SELECT COUNT(*) FROM users", [], |r| r.get(0)) {
                    if count == 0 {
                        let sql = format!(
                            "ATTACH DATABASE '{}' AS old_stats;
                             INSERT OR IGNORE INTO users SELECT * FROM old_stats.users;
                             INSERT OR IGNORE INTO downloads SELECT * FROM old_stats.downloads;
                             DETACH DATABASE old_stats;",
                            old_stats.to_string_lossy().replace('\'', "''")
                        );
                        let _ = conn.execute_batch(&sql);
                        let _ = std::fs::rename(&old_stats, parent.join("stats.db.migrated"));
                    }
                }
            }
        }
        Self::init(conn)
    }

    pub fn in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, rusqlite::Error> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA cache_size = -16000;
             PRAGMA mmap_size = 268435456;
             PRAGMA temp_store = MEMORY;
             PRAGMA busy_timeout = 5000;

             CREATE TABLE IF NOT EXISTS users (
                 user_id INTEGER PRIMARY KEY,
                 username TEXT,
                 first_name TEXT,
                 first_seen INTEGER NOT NULL,
                 last_seen INTEGER NOT NULL
             );

             CREATE TABLE IF NOT EXISTS downloads (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 user_id INTEGER NOT NULL,
                 created_at INTEGER NOT NULL,
                 url TEXT NOT NULL,
                 extractor TEXT NOT NULL,
                 media_kind TEXT NOT NULL,
                 file_size INTEGER NOT NULL,
                 duration INTEGER NOT NULL,
                 is_cached INTEGER NOT NULL,
                 status TEXT NOT NULL,
                 error_reason TEXT
             );

             -- Base indices
             CREATE INDEX IF NOT EXISTS idx_downloads_created_at ON downloads(created_at);
             CREATE INDEX IF NOT EXISTS idx_downloads_user_id ON downloads(user_id);
             CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status);
             CREATE INDEX IF NOT EXISTS idx_downloads_extractor ON downloads(extractor);

             -- High performance composite covering indices
             CREATE INDEX IF NOT EXISTS idx_dl_status_created ON downloads(status, created_at);
             CREATE INDEX IF NOT EXISTS idx_dl_status_user ON downloads(status, user_id);
             CREATE INDEX IF NOT EXISTS idx_dl_status_extractor ON downloads(status, extractor);",
        )?;

        // Upgrade schema if error_reason column wasn't there
        let _ = conn.execute_batch("ALTER TABLE downloads ADD COLUMN error_reason TEXT;");

        let conn_arc = Arc::new(Mutex::new(conn));
        let (tx, mut rx) = unbounded_channel::<StatEvent>();

        // Start non-blocking background queue worker if inside tokio runtime
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let worker_conn = conn_arc.clone();
            handle.spawn(async move {
                while let Some(event) = rx.recv().await {
                    let mut batch = vec![event];
                    while let Ok(next) = rx.try_recv() {
                        batch.push(next);
                        if batch.len() >= 50 {
                            break;
                        }
                    }
                    if let Ok(conn) = worker_conn.lock() {
                        for ev in batch {
                            match ev {
                                StatEvent::User { user_id, username, first_name } => {
                                    sync_write_user(&conn, user_id, username.as_deref(), first_name.as_deref());
                                }
                                StatEvent::Download {
                                    user_id,
                                    url,
                                    media_kind,
                                    file_size,
                                    duration,
                                    is_cached,
                                    status,
                                    error_reason,
                                } => {
                                    sync_write_download(
                                        &conn,
                                        user_id,
                                        &url,
                                        &media_kind,
                                        file_size,
                                        duration,
                                        is_cached,
                                        &status,
                                        error_reason.as_deref(),
                                    );
                                }
                            }
                        }
                    }
                }
            });
        }

        Ok(StatsDb { conn: conn_arc, tx })
    }

    pub fn record_user(&self, user_id: i64, username: Option<&str>, first_name: Option<&str>) {
        let ev =
            StatEvent::User { user_id, username: username.map(String::from), first_name: first_name.map(String::from) };
        if self.tx.send(ev).is_err() {
            if let Ok(conn) = self.conn.lock() {
                sync_write_user(&conn, user_id, username, first_name);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_download(
        &self,
        user_id: i64,
        url: &str,
        media_kind: &str,
        file_size: u64,
        duration: u32,
        is_cached: bool,
        status: &str,
        error_reason: Option<&str>,
    ) {
        let ev = StatEvent::Download {
            user_id,
            url: url.to_string(),
            media_kind: media_kind.to_string(),
            file_size,
            duration,
            is_cached,
            status: status.to_string(),
            error_reason: error_reason.map(String::from),
        };
        if self.tx.send(ev).is_err() {
            if let Ok(conn) = self.conn.lock() {
                sync_write_download(
                    &conn,
                    user_id,
                    url,
                    media_kind,
                    file_size,
                    duration,
                    is_cached,
                    status,
                    error_reason,
                );
            }
        }
    }

    pub fn summary(&self) -> SummaryStats {
        let Ok(conn) = self.conn.lock() else { return SummaryStats::default() };
        let now = now_secs();
        let day_ago = now - 86400;
        let week_ago = now - 7 * 86400;
        let month_ago = now - 30 * 86400;

        let total_users: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap_or(0);
        let new_today: i64 =
            conn.query_row("SELECT COUNT(*) FROM users WHERE first_seen >= ?1", [day_ago], |r| r.get(0)).unwrap_or(0);
        let new_week: i64 =
            conn.query_row("SELECT COUNT(*) FROM users WHERE first_seen >= ?1", [week_ago], |r| r.get(0)).unwrap_or(0);
        let new_month: i64 =
            conn.query_row("SELECT COUNT(*) FROM users WHERE first_seen >= ?1", [month_ago], |r| r.get(0)).unwrap_or(0);

        let dau: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT user_id) FROM downloads WHERE created_at >= ?1 AND status = 'success'",
                [day_ago],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let wau: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT user_id) FROM downloads WHERE created_at >= ?1 AND status = 'success'",
                [week_ago],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let mau: i64 = conn
            .query_row(
                "SELECT COUNT(DISTINCT user_id) FROM downloads WHERE created_at >= ?1 AND status = 'success'",
                [month_ago],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let (total_downloads, total_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(file_size), 0) FROM downloads WHERE status = 'success'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));

        let total_videos: i64 = conn
            .query_row("SELECT COUNT(*) FROM downloads WHERE status = 'success' AND media_kind = 'video'", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);
        let total_audios: i64 = conn
            .query_row("SELECT COUNT(*) FROM downloads WHERE status = 'success' AND media_kind = 'audio'", [], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        let (today_downloads, today_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(file_size), 0) FROM downloads WHERE created_at >= ?1 AND status = 'success'",
                [day_ago],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));

        let (week_downloads, week_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(file_size), 0) FROM downloads WHERE created_at >= ?1 AND status = 'success'",
                [week_ago],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));

        let (month_downloads, month_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(file_size), 0) FROM downloads WHERE created_at >= ?1 AND status = 'success'",
                [month_ago],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));

        let cached_downloads: i64 = conn
            .query_row("SELECT COUNT(*) FROM downloads WHERE status = 'success' AND is_cached = 1", [], |r| r.get(0))
            .unwrap_or(0);

        let cached_percent =
            if total_downloads > 0 { (cached_downloads as f32 / total_downloads as f32) * 100.0 } else { 0.0 };

        // User engagement: mean & median
        let mut user_counts = Vec::new();
        if let Ok(mut stmt) =
            conn.prepare("SELECT COUNT(*) as c FROM downloads WHERE status = 'success' GROUP BY user_id ORDER BY c ASC")
        {
            if let Ok(rows) = stmt.query_map([], |r| r.get::<_, i64>(0)) {
                for r in rows.flatten() {
                    user_counts.push(r);
                }
            }
        }

        let distinct_active = user_counts.len() as f64;
        let avg_per_active_user = if distinct_active > 0.0 { total_downloads as f64 / distinct_active } else { 0.0 };

        let median_per_user = if user_counts.is_empty() {
            0
        } else if user_counts.len() % 2 == 1 {
            user_counts[user_counts.len() / 2]
        } else {
            let mid = user_counts.len() / 2;
            (user_counts[mid - 1] + user_counts[mid]) / 2
        };

        // 7-day retention rate: users joined 7-14 days ago who returned in last 7 days
        let cohort_start = now - 14 * 86400;
        let cohort_end = now - 7 * 86400;
        let retention_rate = conn
            .query_row(
                "SELECT 
                    COUNT(DISTINCT u.user_id),
                    COUNT(DISTINCT d.user_id)
                 FROM users u
                 LEFT JOIN downloads d ON u.user_id = d.user_id AND d.created_at >= ?2 AND d.status = 'success'
                 WHERE u.first_seen >= ?1 AND u.first_seen < ?2",
                params![cohort_start, cohort_end],
                |r| {
                    let total_cohort: i64 = r.get(0)?;
                    let returned: i64 = r.get(1)?;
                    if total_cohort > 0 {
                        Ok((returned as f32 / total_cohort as f32) * 100.0)
                    } else {
                        Ok(0.0)
                    }
                },
            )
            .unwrap_or(0.0);

        SummaryStats {
            total_users,
            new_today,
            new_week,
            new_month,
            dau,
            wau,
            mau,
            total_downloads,
            total_bytes: total_bytes.max(0) as u64,
            total_videos,
            total_audios,
            today_downloads,
            today_bytes: today_bytes.max(0) as u64,
            week_downloads,
            week_bytes: week_bytes.max(0) as u64,
            month_downloads,
            month_bytes: month_bytes.max(0) as u64,
            cached_downloads,
            cached_percent,
            avg_per_active_user,
            median_per_user,
            retention_rate,
        }
    }

    pub fn top_users(&self, limit: usize) -> Vec<TopUser> {
        self.top_users_period(0, limit)
    }

    pub fn top_users_period(&self, days: u64, limit: usize) -> Vec<TopUser> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let mut result = Vec::new();
        let since = if days > 0 { now_secs() - (days * 86400) as i64 } else { 0 };

        let sql = "SELECT e.user_id, u.username, u.first_name, COUNT(e.id) as cnt, \
                   COALESCE(SUM(e.file_size), 0) as total_sz, MAX(e.created_at) as last_act \
                   FROM downloads e \
                   LEFT JOIN users u ON e.user_id = u.user_id \
                   WHERE e.status = 'success' AND e.created_at >= ?1 \
                   GROUP BY e.user_id \
                   ORDER BY cnt DESC \
                   LIMIT ?2";

        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map(params![since, limit as i64], |r| {
                let sz: i64 = r.get(4)?;
                Ok(TopUser {
                    user_id: r.get(0)?,
                    username: r.get(1)?,
                    first_name: r.get(2)?,
                    download_count: r.get(3)?,
                    total_bytes: sz.max(0) as u64,
                    last_seen: r.get(5)?,
                })
            }) {
                for item in rows.flatten() {
                    result.push(item);
                }
            }
        }

        result
    }

    pub fn top_platforms(&self, days: u64, limit: usize) -> Vec<PlatformStat> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let mut result = Vec::new();
        let since = if days > 0 { now_secs() - (days * 86400) as i64 } else { 0 };

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM downloads WHERE created_at >= ?1 AND status = 'success'", [since], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        let sql = "SELECT extractor, COUNT(*) as cnt \
                   FROM downloads \
                   WHERE created_at >= ?1 AND status = 'success' \
                   GROUP BY extractor \
                   ORDER BY cnt DESC \
                   LIMIT ?2";

        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map(params![since, limit as i64], |r| {
                let name: String = r.get(0)?;
                let count: i64 = r.get(1)?;
                let percent = if total > 0 { (count as f32 / total as f32) * 100.0 } else { 0.0 };
                Ok(PlatformStat { name, count, percent })
            }) {
                for item in rows.flatten() {
                    result.push(item);
                }
            }
        }

        result
    }

    pub fn daily_trend(&self, days: usize) -> Vec<DailyStat> {
        let Ok(conn) = self.conn.lock() else { return Vec::new() };
        let now = now_secs();
        let since = now - (days as i64 * 86400);

        let mut map = std::collections::BTreeMap::new();
        // Pre-fill days so days with 0 downloads still exist
        for i in (0..days).rev() {
            let day_ts = now - (i as i64 * 86400);
            let date = format_day_label(day_ts);
            map.insert(date, (0i64, 0u64));
        }

        let sql = "SELECT strftime('%d.%m', datetime(created_at, 'unixepoch', 'localtime')) as day_lbl, \
                          COUNT(*) as cnt, \
                          COALESCE(SUM(file_size), 0) as sz \
                   FROM downloads \
                   WHERE created_at >= ?1 AND status = 'success' \
                   GROUP BY day_lbl";

        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map([since], |r| {
                let sz: i64 = r.get(2)?;
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, sz.max(0) as u64))
            }) {
                for (date, count, bytes) in rows.flatten() {
                    map.insert(date, (count, bytes));
                }
            }
        }

        map.into_iter().map(|(date, (count, bytes))| DailyStat { date, count, bytes }).collect()
    }

    pub fn error_breakdown(&self, days: u64, limit: usize) -> (i64, i64, Vec<ErrorStat>) {
        let Ok(conn) = self.conn.lock() else { return (0, 0, Vec::new()) };
        let since = if days > 0 { now_secs() - (days * 86400) as i64 } else { 0 };

        let total_errors: i64 = conn
            .query_row("SELECT COUNT(*) FROM downloads WHERE created_at >= ?1 AND status = 'error'", [since], |r| {
                r.get(0)
            })
            .unwrap_or(0);

        let total_attempts: i64 =
            conn.query_row("SELECT COUNT(*) FROM downloads WHERE created_at >= ?1", [since], |r| r.get(0)).unwrap_or(0);

        let mut list = Vec::new();
        let sql = "SELECT COALESCE(error_reason, 'Ошибка yt-dlp / сети') as reason, COUNT(*) as cnt \
                   FROM downloads \
                   WHERE created_at >= ?1 AND status = 'error' \
                   GROUP BY reason \
                   ORDER BY cnt DESC \
                   LIMIT ?2";

        if let Ok(mut stmt) = conn.prepare(sql) {
            if let Ok(rows) = stmt.query_map(params![since, limit as i64], |r| {
                let reason: String = r.get(0)?;
                let count: i64 = r.get(1)?;
                let percent = if total_errors > 0 { (count as f32 / total_errors as f32) * 100.0 } else { 0.0 };
                Ok(ErrorStat { reason, count, percent })
            }) {
                for item in rows.flatten() {
                    list.push(item);
                }
            }
        }

        (total_errors, total_attempts, list)
    }

    pub fn backup(&self, target_path: &Path) -> Result<(), rusqlite::Error> {
        let Ok(conn) = self.conn.lock() else {
            return Err(rusqlite::Error::ExecuteReturnedResults);
        };
        if target_path.exists() {
            let _ = std::fs::remove_file(target_path);
        }
        let sql = format!("VACUUM INTO '{}';", target_path.to_string_lossy().replace('\'', "''"));
        conn.execute_batch(&sql)?;
        Ok(())
    }

    pub fn render_summary(&self) -> String {
        let s = self.summary();
        let trend = self.daily_trend(7);
        let counts: Vec<i64> = trend.iter().map(|d| d.count).collect();
        let spark = sparkline(&counts);

        let mut spark_line = String::new();
        if !spark.is_empty() {
            spark_line = format!("• Тренд за 7 дней: <code>[{}]</code>\n", spark);
        }

        format!(
            "📊 <b>Статистика использования</b>\n\n\
             👥 <b>Аудитория:</b>\n\
             • Всего уникальных: <b>{}</b>\n\
             • Активные (DAU / WAU / MAU): <b>{}</b> / <b>{}</b> / <b>{}</b>\n\
             • Новые пользователи (24ч / 7д / 30д): <b>{}</b> / <b>{}</b> / <b>{}</b>\n\
             • Удержание за неделю (Retention): <b>{:.1}%</b>\n\n\
             📥 <b>Скачивания (число | трафик):</b>\n\
             {}\
             • За сегодня: <b>{}</b> · <b>{}</b>\n\
             • За 7 дней: <b>{}</b> · <b>{}</b>\n\
             • За 30 дней: <b>{}</b> · <b>{}</b>\n\
             • За всё время: <b>{}</b> · <b>{}</b>\n\
               └ Видео: <b>{}</b> · Аудио: <b>{}</b>\n\n\
             📈 <b>Вовлеченность:</b>\n\
             • В среднем на активного: <b>{:.1}</b> скачиваний\n\
             • Медиана: <b>{}</b> скачиваний\n\
             • Из кэша (мгновенно): <b>{}</b> (<b>{:.1}%</b>)",
            s.total_users,
            s.dau,
            s.wau,
            s.mau,
            s.new_today,
            s.new_week,
            s.new_month,
            s.retention_rate,
            spark_line,
            s.today_downloads,
            fmt_traffic(s.today_bytes),
            s.week_downloads,
            fmt_traffic(s.week_bytes),
            s.month_downloads,
            fmt_traffic(s.month_bytes),
            s.total_downloads,
            fmt_traffic(s.total_bytes),
            s.total_videos,
            s.total_audios,
            s.avg_per_active_user,
            s.median_per_user,
            s.cached_downloads,
            s.cached_percent,
        )
    }

    pub fn render_trend(&self) -> String {
        let trend = self.daily_trend(7);
        if trend.is_empty() {
            return "📈 <b>Активность за неделю</b>\n\n<i>Данных пока недостаточно.</i>".to_string();
        }

        let counts: Vec<i64> = trend.iter().map(|d| d.count).collect();
        let spark = sparkline(&counts);

        let mut out = format!("📈 <b>Динамика скачиваний за последние 7 дней:</b>\n<code>[{}]</code>\n\n", spark);
        for d in trend {
            out.push_str(&format!(
                "• <b>{}</b>: <b>{}</b> скачиваний · <b>{}</b>\n",
                d.date,
                d.count,
                fmt_traffic(d.bytes),
            ));
        }
        out
    }

    pub fn render_errors(&self) -> String {
        let (total_errors, total_attempts, list) = self.error_breakdown(30, 8);
        if total_errors == 0 {
            return "⚠️ <b>Ошибки и отказы (за 30 дней)</b>\n\n<i>🎉 За последние 30 дней ошибок не зафиксировано!</i>"
                .to_string();
        }

        let err_rate = if total_attempts > 0 { (total_errors as f32 / total_attempts as f32) * 100.0 } else { 0.0 };

        let mut out = format!(
            "⚠️ <b>Ошибки и отказы (за 30 дней):</b>\n\n\
             Всего ошибок: <b>{}</b> (доля сбоев: <b>{:.1}%</b>)\n\n",
            total_errors, err_rate
        );

        for (i, e) in list.iter().enumerate() {
            out.push_str(&format!("{}. <b>{}</b>: {} (<b>{:.1}%</b>)\n", i + 1, e.reason, e.count, e.percent,));
        }
        out
    }

    #[allow(dead_code)]
    pub fn render_top_users(&self) -> String {
        self.render_top_users_period(0)
    }

    pub fn render_top_users_period(&self, days: u64) -> String {
        let list = self.top_users_period(days, 10);
        let period_label = match days {
            1 => "за сегодня",
            7 => "за 7 дней",
            30 => "за 30 дней",
            _ => "за всё время",
        };
        if list.is_empty() {
            return format!(
                "🏆 <b>Топ пользователей ({period_label})</b>\n\n<i>Пока нет завершённых загрузок за этот период.</i>"
            );
        }

        let mut out = format!("🏆 <b>Топ-10 активных пользователей ({period_label}):</b>\n\n");
        for (i, u) in list.iter().enumerate() {
            let medal = match i {
                0 => "🥇",
                1 => "🥈",
                2 => "🥉",
                _ => "•",
            };
            let name = if let Some(un) = &u.username {
                format!("@{un}")
            } else if let Some(fn_name) = &u.first_name {
                crate::util::html_escape(fn_name)
            } else {
                format!("User <code>{}</code>", u.user_id)
            };

            out.push_str(&format!(
                "{} <b>{}</b> (<code>{}</code>): <b>{}</b> скачиваний · <b>{}</b>\n",
                medal,
                name,
                u.user_id,
                u.download_count,
                fmt_traffic(u.total_bytes),
            ));
        }
        out
    }

    pub fn get_user_stats(&self, user_id: i64) -> UserPersonalStats {
        let Ok(conn) = self.conn.lock() else {
            return UserPersonalStats {
                user_id,
                first_seen: 0,
                total_downloads: 0,
                total_videos: 0,
                total_audios: 0,
                total_bytes: 0,
                rank: 1,
                total_users: 1,
                top_platforms: Vec::new(),
            };
        };

        let first_seen: i64 =
            conn.query_row("SELECT first_seen FROM users WHERE user_id = ?1", [user_id], |r| r.get(0)).unwrap_or(0);

        let (total_downloads, total_bytes): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(file_size), 0) FROM downloads WHERE user_id = ?1 AND status = 'success'",
                [user_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap_or((0, 0));

        let total_videos: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM downloads WHERE user_id = ?1 AND status = 'success' AND media_kind = 'video'",
                [user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let total_audios: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM downloads WHERE user_id = ?1 AND status = 'success' AND media_kind = 'audio'",
                [user_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        let total_users: i64 = conn.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0)).unwrap_or(1);

        let rank: i64 = if total_downloads > 0 {
            conn.query_row(
                "SELECT COUNT(*) + 1 FROM (
                     SELECT user_id, COUNT(*) as cnt
                     FROM downloads
                     WHERE status = 'success'
                     GROUP BY user_id
                     HAVING cnt > ?1
                 )",
                [total_downloads],
                |r| r.get(0),
            )
            .unwrap_or(1)
        } else {
            total_users
        };

        let mut top_platforms = Vec::new();
        if let Ok(mut stmt) = conn.prepare(
            "SELECT extractor, COUNT(*) as cnt \
             FROM downloads \
             WHERE user_id = ?1 AND status = 'success' \
             GROUP BY extractor \
             ORDER BY cnt DESC \
             LIMIT 3",
        ) {
            if let Ok(rows) = stmt.query_map([user_id], |r| {
                let name: String = r.get(0)?;
                let count: i64 = r.get(1)?;
                let percent = if total_downloads > 0 { (count as f32 / total_downloads as f32) * 100.0 } else { 0.0 };
                Ok(PlatformStat { name, count, percent })
            }) {
                for item in rows.flatten() {
                    top_platforms.push(item);
                }
            }
        }

        UserPersonalStats {
            user_id,
            first_seen,
            total_downloads,
            total_videos,
            total_audios,
            total_bytes: total_bytes.max(0) as u64,
            rank,
            total_users,
            top_platforms,
        }
    }

    pub fn render_user_stats(&self, user_id: i64) -> String {
        let stats = self.get_user_stats(user_id);
        if stats.total_downloads == 0 {
            return "👤 <b>Твоя статистика использования</b>\n\n\
                    <i>Ты пока ещё ничего не скачал через этого бота. Отправь любую ссылку на видео!</i>"
                .to_string();
        }

        let first_seen_str = format_date_str(stats.first_seen);
        let mut plat_str = String::new();
        if !stats.top_platforms.is_empty() {
            plat_str.push_str("\n🌐 <b>Любимые платформы:</b>\n");
            for (i, p) in stats.top_platforms.iter().enumerate() {
                plat_str.push_str(&format!(
                    "{}. <b>{}</b> — <b>{:.0}%</b> ({} шт.)\n",
                    i + 1,
                    p.name,
                    p.percent,
                    p.count
                ));
            }
        }

        format!(
            "👤 <b>Твоя статистика использования</b>\n\n\
             📥 <b>Скачивания:</b>\n\
             • Всего загрузок: <b>{}</b> (видео: {}, аудио: {})\n\
             • Скачанный объём: <b>{}</b>\n\
             • С нами с: <b>{}</b>\n\
             {}\n\
             🏆 <b>Место в рейтинге бота:</b> <b>#{}</b> (из {})",
            stats.total_downloads,
            stats.total_videos,
            stats.total_audios,
            fmt_traffic(stats.total_bytes),
            first_seen_str,
            plat_str,
            stats.rank,
            stats.total_users,
        )
    }

    pub fn prune_downloads(&self, older_than_days: u64) -> usize {
        let Ok(conn) = self.conn.lock() else { return 0 };
        let since = now_secs() - (older_than_days * 86400) as i64;
        let removed: usize =
            conn.execute("DELETE FROM downloads WHERE created_at < ?1", params![since]).unwrap_or_default();
        if removed > 0 {
            let _ = conn.execute_batch("PRAGMA incremental_vacuum;");
        }
        removed
    }

    pub fn render_platforms(&self) -> String {
        let list = self.top_platforms(30, 8);
        if list.is_empty() {
            return "🌐 <b>Популярные платформы (за 30 дней)</b>\n\n<i>Пока нет загрузок за этот период.</i>"
                .to_string();
        }

        let mut out = String::from("🌐 <b>Популярные платформы (за последние 30 дней):</b>\n\n");
        for (i, p) in list.iter().enumerate() {
            out.push_str(&format!("{}. <b>{}</b> — {} (<b>{:.1}%</b>)\n", i + 1, p.name, p.count, p.percent,));
        }
        out
    }

    pub fn render_daily_digest(&self) -> String {
        let s = self.summary();
        let top_plat = self.top_platforms(1, 3);
        let top_user = self.top_users(1);

        let mut plat_summary = String::new();
        if !top_plat.is_empty() {
            plat_summary.push_str("\n🌐 <b>Топ платформ за сутки:</b>\n");
            for p in top_plat {
                plat_summary.push_str(&format!("• {}: <b>{:.0}%</b>\n", p.name, p.percent));
            }
        }

        let mut user_summary = String::new();
        if let Some(u) = top_user.first() {
            let name = if let Some(un) = &u.username {
                format!("@{un}")
            } else if let Some(fn_name) = &u.first_name {
                crate::util::html_escape(fn_name)
            } else {
                format!("ID {}", u.user_id)
            };
            user_summary = format!("\n🏆 <b>Лидер дня:</b> {} ({} скачиваний)\n", name, u.download_count);
        }

        format!(
            "☀️ <b>Ежедневная сводка бота</b>\n\n\
             👥 <b>Пользователи:</b>\n\
             • Активных за сутки (DAU): <b>{}</b>\n\
             • Новых пользователей: <b>+{}</b>\n\n\
             📥 <b>Скачивания за 24ч:</b>\n\
             • Успешно: <b>{}</b> медиа\n\
             • Трафик: <b>{}</b>\n\
             • Из кэша: <b>{:.1}%</b>\
             {}{}",
            s.dau,
            s.new_today,
            s.today_downloads,
            fmt_traffic(s.today_bytes),
            s.cached_percent,
            plat_summary,
            user_summary
        )
    }
}

fn format_day_label(ts: i64) -> String {
    // Simple dd.mm from unix timestamp
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    // Calculate month and day roughly or using standard formula
    // Days since unix epoch 1970-01-01
    let z = days + 719468;
    let era = z / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{d:02}.{m:02}")
}

fn format_date_str(ts: i64) -> String {
    let secs = ts.max(0) as u64;
    let days = secs / 86400;
    let z = days + 719468;
    let era = z / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + (era as i64) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{d:02}.{m:02}.{year}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparkline_render() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0, 0, 0]), "   ");
        let s = sparkline(&[10, 50, 100]);
        assert_eq!(s.chars().count(), 3);
        assert!(s.chars().nth(0).unwrap() < s.chars().nth(2).unwrap());
    }

    #[test]
    fn traffic_formatting() {
        assert_eq!(fmt_traffic(500), "500 B");
        assert_eq!(fmt_traffic(1536), "1.5 KB");
        assert_eq!(fmt_traffic(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(fmt_traffic(2 * 1024 * 1024 * 1024), "2.00 GB");
    }

    #[test]
    fn stats_lifecycle() {
        let db = StatsDb::in_memory().unwrap();
        db.record_user(101, Some("alice"), Some("Alice"));
        db.record_user(102, None, Some("Bob"));

        db.record_download(101, "https://youtube.com/watch?v=1", "video", 50_000_000, 120, false, "success", None);
        db.record_download(101, "https://tiktok.com/@test/video/1", "video", 10_000_000, 30, false, "success", None);
        db.record_download(102, "https://instagram.com/reel/1", "video", 20_000_000, 15, true, "success", None);
        db.record_download(102, "https://youtube.com/watch?v=2", "video", 0, 0, false, "error", Some("Too large"));

        let s = db.summary();
        assert_eq!(s.total_users, 2);
        assert_eq!(s.total_downloads, 3);
        assert_eq!(s.cached_downloads, 1);
        assert_eq!(s.total_bytes, 80_000_000);
        assert_eq!(s.dau, 2);

        let top = db.top_users(5);
        assert_eq!(top.len(), 2);
        assert_eq!(top[0].user_id, 101);
        assert_eq!(top[0].download_count, 2);

        let platforms = db.top_platforms(30, 5);
        assert_eq!(platforms.len(), 3);

        let (errors, attempts, list) = db.error_breakdown(30, 5);
        assert_eq!(errors, 1);
        assert_eq!(attempts, 4);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].reason, "Too large");

        let user101 = db.get_user_stats(101);
        assert_eq!(user101.total_downloads, 2);
        assert_eq!(user101.rank, 1);
        assert_eq!(user101.top_platforms.len(), 2);

        let user102 = db.get_user_stats(102);
        assert_eq!(user102.total_downloads, 1);
        assert_eq!(user102.rank, 2);

        let user_empty = db.get_user_stats(999);
        assert_eq!(user_empty.total_downloads, 0);

        let top_today = db.top_users_period(1, 5);
        assert_eq!(top_today.len(), 2);

        let removed = db.prune_downloads(365);
        assert_eq!(removed, 0); // No downloads older than 1 year
    }
}
