//! Speech-to-text service backed by `voicy-core` (GigaAM-v3 INT8 + Silero VAD).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use tokio::sync::{OnceCell, Semaphore};
use voicy_core::{setup, Stt, TranscribeOptions};

pub struct SttService {
    cache_dir: PathBuf,
    dirs_init: OnceLock<()>,
    engine: OnceCell<Arc<Stt>>,
    /// Keep at most 1 active inference at a time to protect VPS RAM (<= 800 MB).
    gate: Semaphore,
}

impl SttService {
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir, dirs_init: OnceLock::new(), engine: OnceCell::new(), gate: Semaphore::new(1) }
    }

    fn init_dirs(&self) {
        self.dirs_init.get_or_init(|| {
            if std::env::var_os("VOICY_CACHE").is_none() {
                let _ = voicy_core::use_dirs(&self.cache_dir, None);
            }
        });
    }

    /// Preload ONNX Runtime, Silero VAD and GigaAM-v3 INT8 at startup so the first
    /// voice message is transcribed immediately without cold-start delay.
    pub async fn warmup(&self) -> Result<(), String> {
        self.get().await.map(|_| ())
    }

    async fn get(&self) -> Result<Arc<Stt>, String> {
        self.init_dirs();
        self.engine
            .get_or_try_init(|| async {
                let gigaam_model = voicy_core::cache_dir().join("models").join("gigaam").join("v3_e2e_ctc.int8.onnx");
                if !gigaam_model.is_file() {
                    log::info!("voicy-core: downloading CPU ONNX Runtime, Silero VAD and GigaAM-v3 INT8...");
                    tokio::task::spawn_blocking(|| {
                        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                        rt.block_on(setup::run_with("all", true, true))
                    })
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| format!("{e:#}"))?;
                }
                let stt = tokio::task::spawn_blocking(|| Stt::load_gigaam(None, None, None))
                    .await
                    .map_err(|e| e.to_string())?
                    .map_err(|e| format!("{e:#}"))?;
                Ok::<_, String>(Arc::new(stt))
            })
            .await
            .cloned()
    }

    /// Transcribe audio bytes (ogg/opus, mp3, wav, flac, m4a, mp4/video_note) to Russian text.
    pub async fn transcribe_bytes(&self, bytes: Vec<u8>) -> Result<String, String> {
        let _permit = self.gate.acquire().await.map_err(|_| "stt gate closed".to_string())?;
        let stt = self.get().await?;
        let opts = TranscribeOptions { language: Some("ru".into()), ..Default::default() };
        let tr = match stt.clone().transcribe_file_async(bytes.clone(), opts.clone()).await {
            Ok(tr) => tr,
            Err(first_err) => {
                // MP4/MOV (e.g. Telegram video_note) with moov atom at EOF cannot be demuxed from pipe:0;
                // extract 16 kHz mono WAV from a seekable temp file via ffmpeg and retry.
                let wav =
                    extract_wav_via_ffmpeg(&bytes).await.map_err(|e| format!("{first_err:#}; ffmpeg fallback: {e}"))?;
                stt.transcribe_file_async(wav, opts).await.map_err(|e| format!("{e:#}"))?
            }
        };
        Ok(tr.text.trim().to_string())
    }
}

async fn extract_wav_via_ffmpeg(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir();
    let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    let tmp = dir.join(format!("tgmb-stt-{}-{unique}.bin", std::process::id()));
    tokio::fs::write(&tmp, bytes).await.map_err(|e| format!("write temp: {e}"))?;
    let ffmpeg = std::env::var("FFMPEG_PATH").unwrap_or_else(|_| "ffmpeg".into());
    let out = tokio::process::Command::new(&ffmpeg)
        .args(["-v", "error", "-y", "-i"])
        .arg(&tmp)
        .args(["-vn", "-ac", "1", "-ar", "16000", "-f", "wav", "pipe:1"])
        .output()
        .await;
    let _ = tokio::fs::remove_file(&tmp).await;
    let out = out.map_err(|e| format!("spawn ffmpeg: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("ffmpeg exited {}: {}", out.status, err.trim()));
    }
    Ok(out.stdout)
}
