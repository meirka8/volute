use cvc_core::db::CvcStore;
use dashmap::DashMap;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub store: Mutex<Option<CvcStore>>,
    pub root_path: Mutex<Option<PathBuf>>,
    // Map of Turn ID -> Prompt
    pub pending_turns: DashMap<String, String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(None),
            root_path: Mutex::new(None),
            pending_turns: DashMap::new(),
        }
    }
}
