use cvc_core::git;
use cvc_core::models::InteractionId;
use git2::{Repository, Signature};
use std::fs::File;
use std::io::Write;
use tempfile::TempDir;

#[test]
fn test_git_context_snapshot() -> anyhow::Result<()> {
    // 1. Setup Temp Repo
    let temp_dir = TempDir::new()?;
    let repo = Repository::init(temp_dir.path())?;

    // Create a file and commit it
    let file_path = temp_dir.path().join("test.txt");
    {
        let mut file = File::create(&file_path)?;
        write!(file, "Initial content")?;
    }

    let mut index = repo.index()?;
    index.add_path(std::path::Path::new("test.txt"))?;
    let oid = index.write_tree()?;
    let signature = Signature::now("Test User", "test@example.com")?;
    let tree = repo.find_tree(oid)?;
    repo.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "Initial commit",
        &tree,
        &[],
    )?;

    // 2. Modify file (Dirty state)
    {
        let mut file = File::create(&file_path)?;
        write!(file, "Modified content")?;
    }

    // 3. Snapshot Context
    let inter_id = InteractionId::new();
    let items = git::snapshot_context(&repo, &inter_id, &["test.txt".to_string()])?;

    assert_eq!(items.len(), 1);
    let item = &items[0];
    assert!(item.dirty_patch.is_some());
    assert!(item.git_blob_sha.is_some()); // Should have base blob

    // 4. New untracked file
    let new_file_path = temp_dir.path().join("new.txt");
    {
        let mut file = File::create(&new_file_path)?;
        write!(file, "New file")?;
    }

    let items_new = git::snapshot_context(&repo, &inter_id, &["new.txt".to_string()])?;
    assert_eq!(items_new.len(), 1);
    assert!(items_new[0].dirty_patch.is_some());
    // git_blob_sha might be None for untracked new files depending on impl, checking logic...
    // The impl says for untracked file, it tries to diff.

    Ok(())
}
