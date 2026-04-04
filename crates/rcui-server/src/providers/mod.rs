pub mod types;
pub mod utils;
pub mod claude;
pub mod cursor;
pub mod codex;
pub mod gemini;

use std::collections::HashMap;

use async_trait::async_trait;
use serde_json::Value;

use crate::error::AppError;
use types::{FetchHistoryOptions, FetchHistoryResult, NormalizedMessage, SessionProvider};

/// Adapter for reading persisted session history.
#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Read persisted session messages from disk/database.
    async fn fetch_history(
        &self,
        session_id: &str,
        opts: FetchHistoryOptions,
    ) -> Result<FetchHistoryResult, AppError>;

    /// Normalize a provider-specific event into NormalizedMessage(s).
    fn normalize_message(&self, raw: &Value, session_id: &str) -> Vec<NormalizedMessage>;
}

/// Registry holding all provider adapters.
pub struct ProviderRegistry {
    adapters: HashMap<SessionProvider, Box<dyn ProviderAdapter>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            adapters: HashMap::new(),
        }
    }

    pub fn register(&mut self, provider: SessionProvider, adapter: Box<dyn ProviderAdapter>) {
        self.adapters.insert(provider, adapter);
    }

    pub fn get(&self, provider: &SessionProvider) -> Option<&dyn ProviderAdapter> {
        self.adapters.get(provider).map(|a| a.as_ref())
    }
}
