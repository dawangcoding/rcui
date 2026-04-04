use std::path::PathBuf;

use serde::Deserialize;
use tracing::info;

#[derive(Debug, Clone, Deserialize)]
pub struct AppConfig {
    pub server_port: u16,
    pub host: String,
    pub database_path: PathBuf,
    pub workspaces_root: PathBuf,
    pub context_window: u32,
    pub is_platform: bool,
    pub api_key: Option<String>,
    pub static_dir: Option<PathBuf>,
}

impl AppConfig {
    pub fn from_env() -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));

        let database_path = std::env::var("DATABASE_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.join(".rcui").join("auth.db"));

        let workspaces_root = std::env::var("WORKSPACES_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home.clone());

        let server_port = std::env::var("SERVER_PORT")
            .or_else(|_| std::env::var("PORT"))
            .unwrap_or_else(|_| "3001".to_string())
            .parse()
            .unwrap_or(3001);

        let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());

        let context_window = std::env::var("CONTEXT_WINDOW")
            .unwrap_or_else(|_| "160000".to_string())
            .parse()
            .unwrap_or(160000);

        let is_platform = std::env::var("VITE_IS_PLATFORM")
            .unwrap_or_else(|_| "false".to_string())
            .to_lowercase()
            == "true";

        let api_key = std::env::var("API_KEY").ok().filter(|s| !s.is_empty());

        let static_dir = std::env::var("STATIC_DIR")
            .ok()
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);

        info!(
            server_port,
            %host,
            ?database_path,
            is_platform,
            has_api_key = api_key.is_some(),
            ?static_dir,
            "Configuration loaded"
        );

        Self {
            server_port,
            host,
            database_path,
            workspaces_root,
            context_window,
            is_platform,
            api_key,
            static_dir,
        }
    }
}
