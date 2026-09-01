use cvc_core::db::CvcStore;
use cvc_core::repository::RepositoryLayout;
use dashmap::DashMap;
use std::sync::Mutex;

/// The server deliberately has one repository/worktree binding.  Multi-root
/// workspaces need separate stores and policy contexts and are not supported.
pub struct BoundRepository {
    pub layout: RepositoryLayout,
    pub store: CvcStore,
}

pub struct AppState {
    pub binding: Mutex<Option<BoundRepository>>,
    // Map of Turn ID -> Prompt
    pub pending_turns: DashMap<String, String>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            binding: Mutex::new(None),
            pending_turns: DashMap::new(),
        }
    }
}
