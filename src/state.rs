//! Persistent bot state: per-chat settings, per-user default quality and the
//! file_id cache. One JSON file, written atomically and at most every few
//! seconds.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::cachekey;
use crate::i18n::Lang;
use crate::types::Quality;

/// Upper bound on cache keys; the oldest tenth is dropped when exceeded.
/// An entry is ~200 bytes, so this is a few MB at most.
const MAX_CACHE: usize = 20_000;

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Debug)]
struct ChatCfg {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lang: Option<Lang>,
    #[serde(default, skip_serializing_if = "is_false")]
    minimal: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    topic: Option<i64>,
    /// Group activated by an allowed user: its members may use the bot.
    #[serde(default, skip_serializing_if = "is_false")]
    active: bool,
}

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct CacheEntry {
    /// "video" | "audio" | "document"
    pub kind: String,
    pub file_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub performer: String,
    #[serde(default)]
    pub duration: u32,
    #[serde(default)]
    pub ts: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct Data {
    #[serde(default)]
    chats: HashMap<i64, ChatCfg>,
    /// user -> default quality code ("max", "720", "audio"); absent = ask.
    #[serde(default)]
    quality: HashMap<i64, String>,
    /// Pre-`quality` format; migrated on load.
    #[serde(default, skip_serializing)]
    audio_users: Vec<i64>,
    #[serde(default)]
    cache: HashMap<String, CacheEntry>,
}

pub struct Store {
    path: PathBuf,
    data: Mutex<Data>,
    dirty: AtomicBool,
}

pub fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Store {
    pub fn load(data_dir: &Path) -> Store {
        let path = data_dir.join("state.json");
        let (mut data, mut dirty) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Data>(&bytes) {
                Ok(d) => (d, false),
                Err(e) => {
                    // Keep the broken file around instead of silently overwriting it.
                    let backup = data_dir.join("state.json.corrupt");
                    log::error!("state file is corrupt ({e}); moved to {}", backup.display());
                    let _ = std::fs::rename(&path, backup);
                    (Data::default(), false)
                }
            },
            Err(_) => {
                let d = import_legacy(data_dir);
                let found = !d.chats.is_empty() || !d.cache.is_empty();
                (d, found)
            }
        };
        for user in std::mem::take(&mut data.audio_users) {
            data.quality.entry(user).or_insert_with(|| "audio".to_string());
            dirty = true;
        }
        Store { path, data: Mutex::new(data), dirty: AtomicBool::new(dirty) }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Data) -> R) -> R {
        let mut guard = self.data.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut guard)
    }

    fn edit_chat(&self, chat_id: i64, f: impl FnOnce(&mut ChatCfg)) {
        self.with(|d| {
            let entry = d.chats.entry(chat_id).or_default();
            let before = entry.clone();
            f(entry);
            if *entry != before {
                self.dirty.store(true, Ordering::Relaxed);
            }
            if *entry == ChatCfg::default() {
                d.chats.remove(&chat_id);
            }
        });
    }

    pub fn lang(&self, chat_id: i64) -> Option<Lang> {
        self.with(|d| d.chats.get(&chat_id).and_then(|c| c.lang))
    }

    pub fn set_lang(&self, chat_id: i64, lang: Lang) {
        self.edit_chat(chat_id, |c| c.lang = Some(lang));
    }

    pub fn minimal(&self, chat_id: i64) -> bool {
        self.with(|d| d.chats.get(&chat_id).is_some_and(|c| c.minimal))
    }

    pub fn set_minimal(&self, chat_id: i64, on: bool) {
        self.edit_chat(chat_id, |c| c.minimal = on);
    }

    pub fn topic(&self, chat_id: i64) -> Option<i64> {
        self.with(|d| d.chats.get(&chat_id).and_then(|c| c.topic))
    }

    pub fn set_topic(&self, chat_id: i64, topic: Option<i64>) {
        self.edit_chat(chat_id, |c| c.topic = topic);
    }

    pub fn is_active(&self, chat_id: i64) -> bool {
        self.with(|d| d.chats.get(&chat_id).is_some_and(|c| c.active))
    }

    pub fn activate(&self, chat_id: i64) {
        self.edit_chat(chat_id, |c| c.active = true);
    }

    /// The user's remembered choice; `None` means "ask every time".
    pub fn default_quality(&self, user_id: i64) -> Option<Quality> {
        self.with(|d| d.quality.get(&user_id).and_then(|c| Quality::parse(c)))
    }

    pub fn set_default_quality(&self, user_id: i64, q: Option<Quality>) {
        self.with(|d| {
            let changed = match q {
                Some(q) => d.quality.insert(user_id, q.code()) != Some(q.code()),
                None => d.quality.remove(&user_id).is_some(),
            };
            if changed {
                self.dirty.store(true, Ordering::Relaxed);
            }
        });
    }

    /// First hit among `keys`, in order.
    pub fn cache_find(&self, keys: &[String]) -> Option<CacheEntry> {
        self.with(|d| keys.iter().find_map(|k| d.cache.get(k).cloned()))
    }

    /// Store one upload under several keys (request + delivered aliases).
    pub fn cache_put(&self, keys: &[String], mut entry: CacheEntry) {
        if keys.is_empty() {
            return;
        }
        entry.ts = now_secs();
        self.with(|d| {
            for key in keys {
                d.cache.insert(key.clone(), entry.clone());
            }
            if d.cache.len() > MAX_CACHE {
                let mut oldest: Vec<(u64, String)> = d.cache.iter().map(|(k, e)| (e.ts, k.clone())).collect();
                oldest.sort_unstable();
                for (_, key) in oldest.into_iter().take(MAX_CACHE / 10) {
                    d.cache.remove(&key);
                }
            }
        });
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Drop every key pointing at a file_id that Telegram no longer accepts.
    pub fn cache_evict_file(&self, file_id: &str) {
        let removed = self.with(|d| {
            let before = d.cache.len();
            d.cache.retain(|_, e| e.file_id != file_id);
            before != d.cache.len()
        });
        if removed {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Write to disk if anything changed since the last flush.
    pub fn flush(&self) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let json = self.with(|d| serde_json::to_vec(d));
        let result = json.map_err(|e| e.to_string()).and_then(|bytes| {
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, &self.path)).map_err(|e| e.to_string())
        });
        if let Err(e) = result {
            log::error!("could not save state to {}: {e}", self.path.display());
            self.dirty.store(true, Ordering::Relaxed);
        }
    }
}

/// One-time import of the JSON files written by the original Python bot, so an
/// existing `/data` volume keeps its groups, settings and file_id cache.
fn import_legacy(dir: &Path) -> Data {
    let mut data = Data::default();
    let read = |name: &str| -> Option<Value> { serde_json::from_slice(&std::fs::read(dir.join(name)).ok()?).ok() };
    let ids = |v: Value| -> Vec<i64> {
        v.as_array().map(|a| a.iter().filter_map(Value::as_i64).collect()).unwrap_or_default()
    };

    if let Some(v) = read("allowed_chats.json") {
        for id in ids(v) {
            data.chats.entry(id).or_default().active = true;
        }
    }
    if let Some(v) = read("minimal_mode.json") {
        for id in ids(v) {
            data.chats.entry(id).or_default().minimal = true;
        }
    }
    if let Some(Value::Object(map)) = read("topic_locks.json") {
        for (chat, topic) in map {
            if let (Ok(chat), Some(topic)) = (chat.parse::<i64>(), topic.as_i64()) {
                data.chats.entry(chat).or_default().topic = Some(topic);
            }
        }
    }
    if let Some(Value::Object(map)) = read("media_cache.json") {
        for (key, entry) in map {
            let Some((fmt, url)) = key.split_once('\n') else { continue };
            // The Python bot only ever cached "best" video or audio.
            let q = if fmt == "audio" { Quality::Audio } else { Quality::Max };
            if let Ok(entry) = serde_json::from_value::<CacheEntry>(entry) {
                data.cache.insert(cachekey::request_key(&cachekey::video_key(url), q), entry);
            }
        }
    }
    if !data.chats.is_empty() || !data.cache.is_empty() {
        log::info!("imported state of the Python bot: {} chats, {} cached files", data.chats.len(), data.cache.len());
    }
    data
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
        assert!(!std::fs::read_to_string(dir.join("state.json")).unwrap().contains("audio_users"));
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
        assert!(dir.join("state.json").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cache_is_bounded() {
        let dir = temp_dir("bounded");
        let store = Store::load(&dir);
        for i in 0..(MAX_CACHE + 50) {
            store.cache_put(&[format!("url:https://a.com/{i}|v@max")], entry(&i.to_string()));
        }
        assert!(store.with(|d| d.cache.len()) <= MAX_CACHE);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
