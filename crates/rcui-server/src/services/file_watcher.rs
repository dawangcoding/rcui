use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use notify::RecursiveMode;
use notify_debouncer_mini::{DebouncedEvent, new_debouncer};
use tokio::sync::mpsc;

use crate::services::project_scanner;
use crate::state::{AppState, BroadcastMessage};

/// Start watching provider directories for session file changes.
/// Spawns a background task that detects changes and broadcasts updates.
pub fn start_file_watcher(state: Arc<AppState>) {
    tokio::spawn(async move {
        if let Err(e) = run_watcher(state).await {
            tracing::error!("File watcher error: {e}");
        }
    });
}

async fn run_watcher(state: Arc<AppState>) -> anyhow::Result<()> {
    let home = dirs::home_dir().unwrap_or_default();

    let watch_dirs: Vec<(PathBuf, &str)> = vec![
        (home.join(".claude").join("projects"), "claude"),
        (home.join(".cursor").join("chats"), "cursor"),
        (home.join(".codex").join("sessions"), "codex"),
        (home.join(".gemini").join("tmp"), "gemini"),
    ];

    let (tx, mut rx) = mpsc::channel::<DebouncedEvent>(256);

    let mut debouncer = new_debouncer(
        Duration::from_millis(300),
        move |events: Result<Vec<DebouncedEvent>, notify::Error>| {
            if let Ok(events) = events {
                for event in events {
                    let _ = tx.blocking_send(event);
                }
            }
        },
    )?;

    for (dir, provider) in &watch_dirs {
        if dir.exists() {
            if let Err(e) = debouncer.watcher().watch(dir, RecursiveMode::Recursive) {
                tracing::warn!("Cannot watch {dir:?} ({provider}): {e}");
            } else {
                tracing::info!("Watching {dir:?} for {provider} changes");
            }
        }
    }

    // Keep the debouncer alive
    let _debouncer = debouncer;

    // Track if a project rescan is already running
    let mut rescan_in_progress = false;

    while let Some(event) = rx.recv().await {
        if rescan_in_progress {
            continue;
        }

        let changed_file = event.path.to_string_lossy().to_string();

        // Determine which provider changed based on path
        let watch_provider = watch_dirs
            .iter()
            .find(|(dir, _)| event.path.starts_with(dir))
            .map(|(_, p)| p.to_string())
            .unwrap_or_else(|| "unknown".to_string());

        tracing::debug!(
            "File change detected: {changed_file} (provider: {watch_provider})"
        );

        // Clear cache and rescan
        rescan_in_progress = true;
        tracing::debug!("Clearing project cache and starting rescan");
        project_scanner::clear_project_directory_cache();

        let projects = project_scanner::get_projects(&state.db, None).await;
        let project_count = projects.len();
        let projects_json: Vec<serde_json::Value> = projects
            .iter()
            .map(|p| serde_json::to_value(p).unwrap_or_default())
            .collect();

        // Update cache
        {
            let mut cache = state.project_cache.write().await;
            *cache = Some(projects_json);
        }
        tracing::debug!(project_count, "Rescan complete, cache updated");

        // Broadcast to connected clients
        let _ = state.broadcast_tx.send(BroadcastMessage::ProjectsUpdated {
            change_type: "change".to_string(),
            changed_file: Some(changed_file),
            watch_provider: Some(watch_provider),
        });

        rescan_in_progress = false;
    }

    Ok(())
}
