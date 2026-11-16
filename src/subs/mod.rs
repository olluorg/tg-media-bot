//! YouTube subscriptions module: WebSub, RSS fallback, resolver, and notifications.

pub mod notify;
pub mod resolver;
pub mod rss;
pub mod types;
pub mod websub;

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc::unbounded_channel;

use crate::app::App;

pub fn start_subscription_subsystem(app: Arc<App>) {
    if !app.cfg.subs_enabled {
        log::info!("YouTube subscriptions subsystem is disabled");
        return;
    }

    let (notify_tx, notify_rx) = unbounded_channel();

    // 1. Start notification dispatcher worker
    let app_notify = app.clone();
    tokio::spawn(async move {
        notify::run_notify_worker(app_notify, notify_rx).await;
    });

    // 2. Start WebSub server if public URL is configured
    if let Some(pub_url) = &app.cfg.websub_public_url {
        let bind_addr = app.cfg.websub_bind_addr.clone();
        let state =
            Arc::new(websub::WebSubState { secret: app.cfg.websub_secret.clone(), notify_tx: notify_tx.clone() });
        let router = websub::router(state);

        log::info!("Starting WebSub HTTP server on {} (public URL: {})", bind_addr, pub_url);
        tokio::spawn(async move {
            match tokio::net::TcpListener::bind(&bind_addr).await {
                Ok(listener) => {
                    if let Err(e) = axum::serve(listener, router).await {
                        log::error!("WebSub HTTP server error: {e}");
                    }
                }
                Err(e) => {
                    log::error!("Failed to bind WebSub HTTP server to {}: {}", bind_addr, e);
                }
            }
        });
    } else {
        log::info!("WEBSUB_URL not set; running YouTube subscriptions in RSS-only mode");
    }

    // 3. Start RSS fallback worker
    let app_rss = app.clone();
    let http = reqwest::Client::builder().timeout(Duration::from_secs(15)).build().unwrap_or_default();
    let rss_cfg = rss::RssWorkerConfig {
        check_interval: Duration::from_secs(app.cfg.rss_check_interval_secs),
        websub_callback: app.cfg.websub_public_url.clone(),
        websub_secret: app.cfg.websub_secret.clone(),
    };
    tokio::spawn(async move {
        rss::run_rss_worker(app_rss, http, notify_tx, rss_cfg).await;
    });
}
