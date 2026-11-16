//! Types and structures for YouTube subscriptions.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedEntry {
    pub video_id: String,
    pub channel_id: String,
    pub title: String,
    pub link: String,
    pub author_name: String,
    pub published_at: u64,
}

#[derive(Debug, Clone)]
pub struct NewVideoNotification {
    pub channel_id: String,
    pub channel_title: String,
    pub video_id: String,
    pub video_title: String,
    #[allow(dead_code)]
    pub published_at: u64,
}

#[derive(Debug, Clone)]
pub struct ChannelInfo {
    pub channel_id: String,
    pub title: String,
    pub latest_video_id: Option<String>,
}
