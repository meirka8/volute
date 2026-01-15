use cvc_core::db::CvcStore;
use std::path::PathBuf;
use std::sync::Mutex;

pub struct AppState {
    pub store: Mutex<Option<CvcStore>>,
    pub root_path: Mutex<Option<PathBuf>>,
    // Add pending turns or session info here
    pub pending_prompt: Mutex<Option<String>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            store: Mutex::new(None),
            root_path: Mutex::new(None),
            pending_prompt: Mutex::new(None),
        }
    }
}
