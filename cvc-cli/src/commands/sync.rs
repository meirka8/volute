use anyhow::{bail, Context, Result};
use cvc_core::{
    db::CvcStore,
    models::{FutureSharePolicy, InteractionId, PublicationState, TombstoneReasonCode},
    privacy, sync,
};
use git2::Repository;
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    env,
    io::{IsTerminal, Write},
    path::Path,
};

fn remote(repo: &Repository, requested: Option<&str>) -> Result<String> {
    if let Some(name) = requested {
        return Ok(name.to_owned());
    }
    let remotes = repo.remotes()?;
    Ok(if remotes.iter().any(|x| x == Some("origin")) {
        "origin".into()
    } else {
        remotes.get(0).unwrap_or("origin").into()
    })
}
fn open(cwd: &Path) -> Result<(Repository, CvcStore)> {
    let repo = Repository::open(cwd).context("Failed to open git repository")?;
    let store =
        CvcStore::open_initialized(cvc_core::privacy::common_git_dir(&repo).join("cvc/index.db"))?;
    Ok((repo, store))
}
pub async fn privacy_status(requested: Option<&str>) -> Result<()> {
    let (repo, _) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let s = privacy::privacy_status_for_fingerprint(&repo, &destination.fingerprint)?;
    println!("capture_acknowledged: {}\nsharing_consented: {}\nauto_push: {}\nremote: {}\ndestination_fingerprint: {}", s.capture_acknowledged, s.sharing_consented, s.auto_push, name, s.remote_fingerprint);
    Ok(())
}
pub async fn acknowledge_capture() -> Result<()> {
    let (repo, _) = open(&env::current_dir()?)?;
    typed_acknowledgement("I UNDERSTAND LOCAL CAPTURE")?;
    privacy::acknowledge_capture(&repo)?;
    println!("Capture acknowledgement saved locally; it is never synced.");
    Ok(())
}
pub async fn acknowledge_sharing(requested: Option<&str>) -> Result<()> {
    let (repo, _) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let s = privacy::privacy_status_for_fingerprint(&repo, &destination.fingerprint)?;
    println!("Acknowledging immutable publication to remote '{}' (fingerprint {}). This does not enable automatic push.", name, s.remote_fingerprint);
    typed_acknowledgement(&format!("I AUTHORIZE SHARING {}", s.remote_fingerprint))?;
    privacy::acknowledge_sharing_destination(&repo, &destination)?;
    Ok(())
}
fn typed_acknowledgement(challenge: &str) -> Result<()> {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("acknowledgement requires an interactive TTY");
    }
    print!("Type exactly '{challenge}' to continue: ");
    std::io::stdout().flush()?;
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer)?;
    if answer.trim_end_matches(['\r', '\n']) != challenge {
        bail!("acknowledgement challenge did not match");
    }
    Ok(())
}
pub async fn set_auto_push(value: &str, requested: Option<&str>) -> Result<()> {
    let enabled = match value {
        "on" => true,
        "off" => false,
        _ => bail!("auto-push must be 'on' or 'off'"),
    };
    let (repo, _) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    typed_acknowledgement(&format!(
        "I AUTHORIZE AUTO PUSH {} {}",
        if enabled { "ON" } else { "OFF" },
        destination.fingerprint
    ))?;
    privacy::set_auto_push_destination(&repo, &destination, enabled)?;
    println!(
        "Auto-push {} for remote '{}'.",
        if enabled { "enabled" } else { "disabled" },
        name
    );
    Ok(())
}
pub async fn share(id: &str, future: bool, push: bool, requested: Option<&str>) -> Result<()> {
    let cwd = env::current_dir()?;
    let (repo, store) = open(&cwd)?;
    // Sharing must never silently select a different authority. Its sole
    // implicit destination is origin; an explicit --remote is authoritative.
    let name = requested.unwrap_or("origin");
    let destination = privacy::remote_destination(&repo, name)?;
    let ids = store.share_snapshot(id)?;
    let closure = ids.iter().map(|x| x.as_str()).collect::<String>();
    let token = hex::encode(Sha256::digest(closure.as_bytes()));
    typed_acknowledgement(&format!(
        "I SHARE {id} {} {} {}",
        destination.fingerprint,
        ids.len(),
        &token[..12]
    ))?;
    store.share_exact_snapshot(
        id,
        &destination.fingerprint,
        &ids,
        if future {
            FutureSharePolicy::Shared
        } else {
            FutureSharePolicy::Private
        },
    )?;
    let count = ids.len();
    println!(
        "{} turn(s) marked shared; future turns are {}.",
        count,
        if future { "shared" } else { "private" }
    );
    if push {
        push_destination(&repo, &store, &destination, false)?;
    }
    Ok(())
}
pub async fn unshare(id: &str, requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    typed_acknowledgement(&format!("I UNSHARE {id} {}", destination.fingerprint))?;
    println!(
        "{} unpublished turn(s) made private.",
        store.unshare_conversation_for_remote(id, &destination.fingerprint)?
    );
    Ok(())
}
pub async fn delete_local(id: &str) -> Result<()> {
    let (_, store) = open(&env::current_dir()?)?;
    let id: InteractionId = id.parse().context("invalid interaction id")?;
    store.tombstone_local(&id, TombstoneReasonCode::UserRequested, None)?;
    println!("Local CVC projection suppressed. This does not erase remote Git objects or history.");
    Ok(())
}
fn write_redaction_plan(path: &Path, plan: &cvc_core::RedactionPlan) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        bail!("plan directory does not exist");
    }
    let bytes = serde_json::to_vec_pretty(plan)?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|x| x.to_str())
            .unwrap_or("redaction-plan"),
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        options.mode(0o600);
    }
    let mut file = options.open(&temporary)?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub async fn redact(
    id: &str,
    remote_name: &str,
    plan_path: &Path,
    apply_local: bool,
) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let id: InteractionId = id.parse().context("invalid interaction id")?;
    let destination = privacy::remote_destination(&repo, remote_name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    // Fresh authenticated advertisement/fetch is both the baseline proof and the
    // stale-plan lease. This command deliberately has no push path.
    sync::fetch_and_pull_destination(&repo, &store, &destination)?;
    let baseline = format!("refs/remotes/{}/cvc/main", destination.name);
    if repo.find_reference(&baseline).is_err() {
        bail!("remote has no cvc baseline to redact");
    }
    // This may only be planned after a tombstone is visible in the fetched
    // baseline. Create the pending tombstone first; the next push projects it.
    // A hard-rewrite plan remains deliberately unavailable until that proof exists.
    if !store
        .tombstones_for_projection(&destination.fingerprint)?
        .iter()
        .any(|t| t.interaction_id == id)
    {
        typed_acknowledgement(&format!("I REDACT {id} {}", destination.fingerprint))?;
        store.authorize_and_tombstone_remote(
            &id,
            &destination.fingerprint,
            TombstoneReasonCode::UserRequested,
        )?;
        println!(
            "Remote tombstone is pending; run `cvc push --manual --remote {}` then rerun redact to build the local-only rewrite plan.",
            destination.name
        );
        return Ok(());
    }
    let candidate =
        sync::build_hard_redaction_plan(&repo, Some(&baseline), &destination.fingerprint, &id)?;
    let plan = candidate.plan.clone();
    typed_acknowledgement(&format!(
        "I PLAN HARD REDACTION {} {} {} {}",
        id,
        destination.fingerprint,
        plan.expected_remote_tip.as_deref().unwrap_or("ABSENT"),
        &plan.replacement_commit[..12]
    ))?;
    write_redaction_plan(plan_path, &plan)?;
    if apply_local {
        sync::apply_hard_redaction_locally(&repo, &plan)?;
        println!("Applied replacement to local refs/cvc/main only.");
    }
    // CandidateRef drops here and removes its temporary ref on success, errors,
    // cancellation, and unwinding. The plan records it only for auditability.
    drop(candidate);
    println!("Hard-redaction plan written to {}.", plan_path.display());
    eprintln!("WARNING: Rotate credentials first if they may be exposed.");
    eprintln!("WARNING: Local current-ref unlinking is not physical deletion; clones, forks, reflogs, caches, backups, and host objects may retain data.");
    eprintln!("WARNING: Remote rewrite is unsupported pending atomic force-with-lease transport.");
    Ok(())
}
pub async fn verify_redaction_plan(path: &Path, remote_name: &str) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let destination = privacy::remote_destination(&repo, remote_name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    let plan: cvc_core::RedactionPlan = serde_json::from_slice(&fs::read(path)?)?;
    if plan.destination_fingerprint != destination.fingerprint {
        bail!("plan destination does not match remote");
    }
    sync::fetch_and_pull_destination(&repo, &store, &destination)?;
    let tracking = format!("refs/remotes/{}/cvc/main", destination.name);
    if sync::verify_redaction_plan(&repo, &plan, &tracking)? {
        println!("Plan is current; no refs were changed.");
        Ok(())
    } else {
        bail!("plan is stale: remote cvc tip changed")
    }
}
pub async fn push(requested: Option<&str>, manual: bool) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    // Bare push is intentionally safe for legacy hook lines.  Humans must opt in
    // to manual publication; hooks use `auto_push` below.
    let destination = privacy::remote_destination(&repo, &name)?;
    push_destination(&repo, &store, &destination, !manual)
}
pub async fn auto_push_remote(cwd: &Path, requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(cwd)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let status = privacy::privacy_status_for_fingerprint(&repo, &destination.fingerprint)?;
    if !status.sharing_consented || !status.auto_push {
        eprintln!("CVC: auto publication disabled for '{}'.", name);
        return Ok(());
    }
    push_destination(&repo, &store, &destination, true)
}
fn push_destination(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
    automatic: bool,
) -> Result<()> {
    let _operation_lock = privacy::destination_operation_lock(repo, &destination.fingerprint)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let status = privacy::privacy_status_for_fingerprint(repo, &destination.fingerprint)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    if !status.sharing_consented {
        bail!("sharing consent is required for remote '{}'; run cvc privacy acknowledge-sharing --remote {}", destination.name, destination.name);
    }
    if automatic && !status.auto_push {
        return Ok(());
    }
    reconcile_destination(repo, store, destination)?;
    // Fetch first: this exact tracking ref is the only permissible projection baseline.
    sync::fetch_and_pull_destination(repo, store, destination)?;
    let baseline = format!("refs/remotes/{}/cvc/main", destination.name);
    let projection = sync::push_projection_to_ref(
        repo,
        store,
        // fetch has proved absence by removing any stale tracking ref first.
        if repo.find_reference(&baseline).is_ok() {
            &baseline
        } else {
            ""
        },
        &status.remote_fingerprint,
    )?;
    let (oid, candidate, ids) = match projection {
        sync::ProjectionResult::NoChanges => {
            println!(
                "No shared CVC interactions pending for '{}'.",
                destination.name
            );
            return Ok(());
        }
        sync::ProjectionResult::Candidate {
            oid,
            candidate,
            ids,
        } => (oid, candidate, ids),
    };
    if !automatic {
        typed_acknowledgement(&format!(
            "I PUBLISH {} {} {}",
            destination.fingerprint,
            &oid.to_string()[..12],
            ids.len()
        ))?;
    }
    store.mark_publication(&ids, &status.remote_fingerprint, PublicationState::Pending)?;
    let result = match sync::push_temp_ref(repo, destination, candidate.ref_name()) {
        Ok(()) => {
            store.mark_publication(
                &ids,
                &status.remote_fingerprint,
                PublicationState::Published,
            )?;
            store.mark_remote_tombstones_published(&status.remote_fingerprint)?;
            println!(
                "Published {} shared interaction(s) to '{}'.",
                ids.len(),
                destination.name
            );
            Ok(())
        }
        Err(e) => {
            // A failed transport is ambiguous: do not make content mutable until a
            // later reconciliation proves the destination did not receive it.
            store.mark_publication(&ids, &status.remote_fingerprint, PublicationState::Unknown)?;
            Err(e.into())
        }
    };
    // `candidate` cleans up on success, errors, TTY cancellation, and unwinding.
    drop(candidate);
    result
}
fn reconcile_destination(
    repo: &Repository,
    store: &CvcStore,
    destination: &privacy::RemoteDestination,
) -> Result<()> {
    let uncertain = store.uncertain_publication_ids(&destination.fingerprint)?;
    sync::fetch_and_pull_destination(repo, store, destination)?;
    // fetch/pull atomically records every node observed at the exact fetched
    // baseline as Published. Anything still uncertain was proven absent.
    store.clear_uncertain_publications(&uncertain, &destination.fingerprint)?;
    Ok(())
}
pub async fn reconcile(requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let name = remote(&repo, requested)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    reconcile_destination(&repo, &store, &destination)?;
    println!("Reconciled publication state for '{}'.", name);
    Ok(())
}
pub async fn pull() -> Result<()> {
    let (repo, mut store) = open(&env::current_dir()?)?;
    let name = remote(&repo, None)?;
    let destination = privacy::remote_destination(&repo, &name)?;
    let _operation_lock = privacy::destination_operation_lock(&repo, &destination.fingerprint)?;
    let n = sync::fetch_and_pull_destination(&repo, &store, &destination)?;
    let _ =
        cvc_core::squash::scan_for(&repo, &mut store, false, std::time::Duration::from_secs(30))?;
    println!("Pulled {n} interaction(s) from '{name}'.");
    Ok(())
}

pub async fn observe_range(base: &str, tip: &str, requested: Option<&str>) -> Result<()> {
    let (repo, store) = open(&env::current_dir()?)?;
    let base = repo.revparse_single(base)?.peel_to_commit()?.id();
    let tip = repo.revparse_single(tip)?.peel_to_commit()?.id();
    let authorization = if let Some(name) = requested {
        let destination = privacy::remote_destination(&repo, name)?;
        typed_acknowledgement(&format!(
            "I AUTHORIZE RANGE {} {} {}",
            base, tip, destination.fingerprint
        ))?;
        Some(destination.fingerprint)
    } else {
        None
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let range = cvc_core::squash::observe_explicit_range_with_abort(
        &repo,
        &store,
        base,
        tip,
        None,
        requested,
        authorization.as_deref(),
        || std::time::Instant::now() >= deadline,
    )?;
    println!("Observed exact range {}.", range.range_id);
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::write_redaction_plan;
    use cvc_core::{InteractionId, RedactionPlan};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn redaction_plan_is_written_0600() {
        let directory =
            std::env::temp_dir().join(format!("cvc-plan-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("plan.json");
        let plan = RedactionPlan {
            format: "cvc.redaction-plan/v1".into(),
            version: 1,
            repository_fingerprint: "repo".into(),
            destination_fingerprint: "destination".into(),
            target_id: InteractionId::new(),
            expected_remote_tip: None,
            replacement_commit: "a".repeat(40),
            temporary_ref: "refs/cvc/candidate-test".into(),
            removed_nodes: 0,
            removed_by_commit_entries: 0,
            removed_link_entries: 0,
            unrelated_entries_retained: 0,
            tombstone_oid: "b".repeat(40),
            created_at: chrono::Utc::now(),
            warning: "test".into(),
        };
        write_redaction_plan(&path, &plan).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
