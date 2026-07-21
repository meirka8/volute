//! Anchored range observation and exact, retryable squash recognition.
use crate::changeset;
use crate::db::{CvcStore, DbError, RetryDisposition};
use crate::models::*;
use git2::{Oid, Repository, Sort};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};
use thiserror::Error;

pub const MAX_SCAN_COMMITS: usize = 128;

/// Opaque CAS input. Fields are crate-visible only for the DB transaction;
/// external callers cannot fabricate squash derivations.
pub(crate) struct ValidatedSquashPlan {
    worktree: String,
    symbolic_ref: String,
    expected_cursor: String,
    candidate: String,
    expected_parent: String,
    expected_head: String,
    range: RangeEvidence,
    sources: Vec<RangeSourceSnapshot>,
    events: Vec<DerivationEvent>,
}
pub(crate) struct ValidatedSquashPlanView<'a> {
    pub(crate) worktree: &'a str,
    pub(crate) symbolic_ref: &'a str,
    pub(crate) expected_cursor: &'a str,
    pub(crate) candidate: &'a str,
    pub(crate) expected_parent: &'a str,
    pub(crate) expected_head: &'a str,
    pub(crate) range: &'a RangeEvidence,
    pub(crate) sources: &'a [RangeSourceSnapshot],
    pub(crate) events: &'a [DerivationEvent],
}

#[derive(Error, Debug)]
pub enum SquashError {
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    #[error("db: {0}")]
    Db(#[from] DbError),
    #[error("changeset: {0}")]
    Changeset(#[from] changeset::ChangesetError),
    #[error("invalid range: {0}")]
    Invalid(&'static str),
    #[error("squash operation deadline exceeded")]
    Deadline,
}

fn repository_identity(repo: &Repository) -> String {
    let mut h = Sha256::new();
    h.update(b"cvc.repository/local/v1\0");
    h.update(
        crate::privacy::common_git_dir(repo)
            .as_os_str()
            .as_encoded_bytes(),
    );
    hex::encode(h.finalize())
}
fn source_id(snapshot: &SourceSnapshot) -> String {
    match snapshot {
        SourceSnapshot::Legacy {
            interaction,
            commit,
            ..
        } => format!("legacy:{interaction}:{commit}"),
        SourceSnapshot::Event { event_id, .. } => event_id.clone(),
    }
}

impl ValidatedSquashPlan {
    pub(crate) fn db_view(&self) -> ValidatedSquashPlanView<'_> {
        ValidatedSquashPlanView {
            worktree: &self.worktree,
            symbolic_ref: &self.symbolic_ref,
            expected_cursor: &self.expected_cursor,
            candidate: &self.candidate,
            expected_parent: &self.expected_parent,
            expected_head: &self.expected_head,
            range: &self.range,
            sources: &self.sources,
            events: &self.events,
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn build(
        repo: &Repository,
        store: &CvcStore,
        worktree: &str,
        symbolic_ref: &str,
        expected_cursor: &str,
        candidate: &str,
        expected_parent: &str,
        expected_head: &str,
        range: &RangeEvidence,
    ) -> Result<Self, SquashError> {
        let candidate_oid = Oid::from_str(candidate)?;
        let commit = repo.find_commit(candidate_oid)?;
        if commit.parent_count() != 1 || commit.parent_id(0)?.to_string() != expected_parent {
            return Err(SquashError::Invalid("candidate topology changed"));
        }
        let change = changeset::identify(repo, Some(&commit.parent(0)?.tree()?), &commit.tree()?)?;
        if change.algorithm != range.changeset_algorithm || change.digest != range.changeset_digest
        {
            return Err(SquashError::Invalid(
                "candidate changeset differs from range",
            ));
        }
        let mut persisted = store
            .trusted_ranges_for_changeset(change.algorithm, &change.digest)?
            .into_iter()
            .filter(|(candidate, _)| candidate.range_id == range.range_id)
            .collect::<Vec<_>>();
        if persisted.len() != 1 {
            return Err(SquashError::Invalid("trusted range changed"));
        }
        let (persisted_range, sources) = persisted
            .pop()
            .ok_or(SquashError::Invalid("trusted range missing"))?;
        if sources.is_empty() {
            return Err(SquashError::Invalid("range has no source snapshots"));
        }
        if persisted_range != *range {
            return Err(SquashError::Invalid("range payload changed"));
        }
        let mut interactions: Vec<_> = sources
            .iter()
            .map(|source| source.interaction_id.clone())
            .collect();
        interactions.sort_by_key(ToString::to_string);
        interactions.dedup();
        let mut events = Vec::new();
        for interaction in interactions {
            let mut ids: Vec<_> = sources
                .iter()
                .filter(|source| source.interaction_id == interaction)
                .map(|source| source.source_id.clone())
                .collect();
            ids.sort();
            ids.dedup();
            let mut event = DerivationEvent {
                event_id: String::new(),
                interaction_id: interaction,
                target_commit: CommitSha::new(candidate.to_owned()),
                relation: DerivationRelation::SquashExact,
                evidence: Evidence {
                    version: 1,
                    kind: EvidenceKind::LocallyObserved,
                },
                origin: DerivationOrigin::LocalHook,
                source_event_ids: ids,
                old_oid: None,
                new_oid: None,
                range_id: Some(range.range_id.clone()),
                linked_by: None,
            };
            event.event_id = event.canonical_id();
            events.push(event);
        }
        Self::validate_events(candidate, range, &sources, &events)?;
        Ok(Self {
            worktree: worktree.into(),
            symbolic_ref: symbolic_ref.into(),
            expected_cursor: expected_cursor.into(),
            candidate: candidate.into(),
            expected_parent: expected_parent.into(),
            expected_head: expected_head.into(),
            range: persisted_range,
            sources,
            events,
        })
    }

    fn validate_events(
        candidate: &str,
        range: &RangeEvidence,
        sources: &[RangeSourceSnapshot],
        events: &[DerivationEvent],
    ) -> Result<(), SquashError> {
        if sources.is_empty() {
            return Err(SquashError::Invalid("empty squash source set"));
        }
        use std::collections::BTreeMap;
        let mut expected: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for source in sources {
            expected
                .entry(source.interaction_id.to_string())
                .or_default()
                .push(source.source_id.clone());
        }
        for ids in expected.values_mut() {
            ids.sort();
            ids.dedup();
        }
        let mut actual: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for event in events {
            if !event.verify_id()
                || event.target_commit.as_str() != candidate
                || event.relation != DerivationRelation::SquashExact
                || event.evidence.version != 1
                || event.evidence.kind != EvidenceKind::LocallyObserved
                || event.origin != DerivationOrigin::LocalHook
                || event.old_oid.is_some()
                || event.new_oid.is_some()
                || event.range_id.as_deref() != Some(range.range_id.as_str())
                || !event.sources_are_canonical()
                || actual
                    .insert(
                        event.interaction_id.to_string(),
                        event.source_event_ids.clone(),
                    )
                    .is_some()
            {
                return Err(SquashError::Invalid("invalid squash event partition"));
            }
        }
        if actual != expected {
            return Err(SquashError::Invalid("incomplete squash event partition"));
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
pub fn observe_explicit_range(
    repo: &Repository,
    store: &CvcStore,
    base: Oid,
    tip: Oid,
    source_ref: Option<&str>,
    source_remote: Option<&str>,
    authorized_remote: Option<&str>,
) -> Result<RangeEvidence, SquashError> {
    observe_range_with_abort(
        repo,
        store,
        base,
        tip,
        RangeObservationOrigin::Explicit,
        source_ref,
        source_remote,
        authorized_remote,
        || false,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn observe_explicit_range_with_abort<F: FnMut() -> bool>(
    repo: &Repository,
    store: &CvcStore,
    base: Oid,
    tip: Oid,
    source_ref: Option<&str>,
    source_remote: Option<&str>,
    authorized_remote: Option<&str>,
    abort: F,
) -> Result<RangeEvidence, SquashError> {
    observe_range_with_abort(
        repo,
        store,
        base,
        tip,
        RangeObservationOrigin::Explicit,
        source_ref,
        source_remote,
        authorized_remote,
        abort,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn observe_pre_push_range_with_abort<F: FnMut() -> bool>(
    repo: &Repository,
    store: &CvcStore,
    base: Oid,
    tip: Oid,
    source_ref: Option<&str>,
    source_remote: Option<&str>,
    authorized_remote: Option<&str>,
    abort: F,
) -> Result<RangeEvidence, SquashError> {
    observe_range_with_abort(
        repo,
        store,
        base,
        tip,
        RangeObservationOrigin::PrePush,
        source_ref,
        source_remote,
        authorized_remote,
        abort,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn observe_range_with_abort<F: FnMut() -> bool>(
    repo: &Repository,
    store: &CvcStore,
    base: Oid,
    tip: Oid,
    origin: RangeObservationOrigin,
    source_ref: Option<&str>,
    source_remote: Option<&str>,
    authorized_remote: Option<&str>,
    mut abort: F,
) -> Result<RangeEvidence, SquashError> {
    if abort() {
        return Err(SquashError::Deadline);
    }
    let base_commit = repo.find_commit(base)?;
    let tip_commit = repo.find_commit(tip)?;
    if base == tip || !repo.graph_descendant_of(tip, base)? {
        return Err(SquashError::Invalid(
            "base must be a strict ancestor of tip",
        ));
    }
    let bases = repo.merge_bases(base, tip)?;
    if bases.len() != 1 || bases.first() != Some(&base) {
        return Err(SquashError::Invalid(
            "range requires a unique merge base equal to base",
        ));
    }
    let mut walk = repo.revwalk()?;
    walk.set_sorting(Sort::TOPOLOGICAL | Sort::REVERSE)?;
    walk.push(tip)?;
    walk.hide(base)?;
    let mut commits = Vec::new();
    for oid in walk {
        if abort() {
            return Err(SquashError::Deadline);
        }
        if commits.len() >= MAX_RANGE_COMMITS {
            return Err(SquashError::Invalid("commit membership bound"));
        }
        commits.push(oid?);
    }
    if commits.is_empty() {
        return Err(SquashError::Invalid("empty range"));
    }
    let change = changeset::identify_with_abort(
        repo,
        Some(&base_commit.tree()?),
        &tip_commit.tree()?,
        &mut abort,
    )?;
    let members = commits
        .iter()
        .map(|oid| RangeMember {
            commit_oid: CommitSha::new(oid.to_string()),
        })
        .collect();
    let mut sources = Vec::new();
    for oid in &commits {
        if abort() {
            return Err(SquashError::Deadline);
        }
        let sha = CommitSha::new(oid.to_string());
        for interaction in store.get_interactions_for_commit(&sha)? {
            for snapshot in store.rewrite_source_snapshots(&interaction.id, &sha)? {
                sources.push(RangeSourceSnapshot {
                    interaction_id: interaction.id.clone(),
                    source_id: source_id(&snapshot),
                    snapshot,
                });
            }
        }
    }
    sources.sort_by(|a, b| {
        (a.interaction_id.as_str(), a.source_id.as_str())
            .cmp(&(b.interaction_id.as_str(), b.source_id.as_str()))
    });
    sources.dedup_by(|a, b| a.interaction_id == b.interaction_id && a.source_id == b.source_id);
    let mut range = RangeEvidence {
        range_id: String::new(),
        format: "cvc.range-evidence/v1".into(),
        version: 1,
        repository_identity: repository_identity(repo),
        object_format: "sha1".into(),
        base_oid: CommitSha::new(base.to_string()),
        tip_oid: CommitSha::new(tip.to_string()),
        base_tree_oid: base_commit.tree_id().to_string(),
        result_tree_oid: tip_commit.tree_id().to_string(),
        commits: members,
        changeset_algorithm: change.algorithm.into(),
        changeset_digest: change.digest,
    };
    range.range_id = range.canonical_id();
    let key = format!(
        "{}:{}:{}",
        match origin {
            RangeObservationOrigin::Explicit => "explicit",
            RangeObservationOrigin::PrePush => "pre-push",
        },
        source_remote.unwrap_or("local"),
        source_ref.unwrap_or("detached")
    );
    store.observe_range(
        &range,
        &sources,
        origin,
        &key,
        source_ref,
        source_remote,
        authorized_remote,
    )?;
    Ok(range)
}

fn branch_key(repo: &Repository) -> Result<(String, String), SquashError> {
    let head = repo.head()?;
    let name = head
        .name()
        .ok_or(SquashError::Invalid("detached HEAD"))?
        .to_owned();
    let worktree = hex::encode(Sha256::digest(repo.path().as_os_str().as_encoded_bytes()));
    Ok((worktree, name))
}

pub fn scan(
    repo: &Repository,
    store: &mut CvcStore,
    immediate_head: bool,
) -> Result<usize, SquashError> {
    scan_for(repo, store, immediate_head, Duration::from_secs(30))
}
pub fn scan_for(
    repo: &Repository,
    store: &mut CvcStore,
    immediate_head: bool,
    budget: Duration,
) -> Result<usize, SquashError> {
    let end = Instant::now() + budget;
    scan_with_abort(repo, store, immediate_head, || Instant::now() >= end)
}

pub fn scan_with_abort<F: FnMut() -> bool>(
    repo: &Repository,
    store: &mut CvcStore,
    immediate_head: bool,
    mut abort: F,
) -> Result<usize, SquashError> {
    if abort() {
        return Err(SquashError::Deadline);
    }
    let (worktree, symbolic) = branch_key(repo)?;
    let head = repo.head()?.peel_to_commit()?.id();
    let head_text = head.to_string();
    let cursor = store.scan_cursor(&worktree, &symbolic)?;
    let mut discovered = Vec::new();
    match cursor.as_deref() {
        None if immediate_head => {
            let c = repo.find_commit(head)?;
            discovered.push((
                head_text.clone(),
                if c.parent_count() == 1 {
                    c.parent_id(0)?.to_string()
                } else {
                    String::new()
                },
                c.parent_count() == 1,
            ));
        }
        None => {}
        Some(old) => {
            let old = Oid::from_str(old)?;
            if old != head {
                let mut at = head;
                let mut first_parent = Vec::new();
                while at != old && first_parent.len() < MAX_SCAN_COMMITS {
                    if abort() {
                        return Err(SquashError::Deadline);
                    }
                    let c = repo.find_commit(at)?;
                    let supported = c.parent_count() == 1;
                    let parent = if supported {
                        c.parent_id(0)?.to_string()
                    } else {
                        String::new()
                    };
                    first_parent.push((at.to_string(), parent, supported));
                    if c.parent_count() == 0 {
                        break;
                    }
                    at = c.parent_id(0)?;
                }
                if at == old {
                    first_parent.reverse();
                    discovered = first_parent;
                } else {
                    // A general-ancestry match through a merge side parent is
                    // not continuity. Reset to at most the current one-parent
                    // HEAD and advance the cursor atomically, avoiding a wedge.
                    let c = repo.find_commit(head)?;
                    if c.parent_count() == 1 {
                        discovered.push((head_text.clone(), c.parent_id(0)?.to_string(), true));
                    }
                }
            }
        }
    }
    let discovery_warning = match store.discover_squash_targets(
        &worktree,
        &symbolic,
        cursor.as_deref(),
        &head_text,
        &discovered,
    ) {
        Ok(true) => None,
        Ok(false) => return Err(SquashError::Invalid("cursor changed")),
        Err(error @ DbError::SquashQueueCapacity { .. })
            if error.retry_disposition() == RetryDisposition::Retryable =>
        {
            Some(error)
        }
        Err(error) => return Err(error.into()),
    };
    let mut total = 0;
    for (target_text, parent_text) in store.pending_squash_targets(&worktree, &symbolic)? {
        if abort() {
            return Err(SquashError::Deadline);
        }
        store.mark_squash_attempt(&worktree, &symbolic, &target_text)?;
        let target = match Oid::from_str(&target_text)
            .ok()
            .and_then(|oid| repo.find_commit(oid).ok())
        {
            Some(v) => v,
            None => continue,
        };
        if target.parent_count() != 1 || target.parent_id(0)?.to_string() != parent_text {
            continue;
        }
        let fp = changeset::identify_with_abort(
            repo,
            Some(&target.parent(0)?.tree()?),
            &target.tree()?,
            &mut abort,
        )?;
        let matches = store.trusted_ranges_for_changeset(fp.algorithm, &fp.digest)?;
        if matches.len() != 1 {
            continue;
        }
        let (range, _) = matches
            .into_iter()
            .next()
            .ok_or(SquashError::Invalid("range vanished"))?;
        if range.commits.iter().any(|m| {
            Oid::from_str(m.commit_oid.as_str())
                .ok()
                .and_then(|oid| repo.find_commit(oid).ok())
                .is_none()
        }) {
            continue;
        }
        let plan = ValidatedSquashPlan::build(
            repo,
            store,
            &worktree,
            &symbolic,
            &head_text,
            &target_text,
            &parent_text,
            &head_text,
            &range,
        )?;
        total += store.apply_squash_plan(repo, &plan)?;
    }
    if let Some(error) = discovery_warning {
        return Err(error.into());
    }
    Ok(total)
}

#[cfg(test)]
mod plan_tests {
    use super::*;
    use git2::Signature;
    fn source(interaction: &InteractionId, id: &str) -> RangeSourceSnapshot {
        RangeSourceSnapshot {
            interaction_id: interaction.clone(),
            source_id: id.into(),
            snapshot: SourceSnapshot::Legacy {
                interaction: interaction.to_string(),
                commit: "a".repeat(40),
                link_type: "generated".into(),
                linked_by: None,
                authorization_rows: vec![],
            },
        }
    }
    fn event(
        interaction: &InteractionId,
        ids: Vec<&str>,
        range: &RangeEvidence,
    ) -> DerivationEvent {
        let mut event = DerivationEvent {
            event_id: String::new(),
            interaction_id: interaction.clone(),
            target_commit: CommitSha::new("b".repeat(40)),
            relation: DerivationRelation::SquashExact,
            evidence: Evidence {
                version: 1,
                kind: EvidenceKind::LocallyObserved,
            },
            origin: DerivationOrigin::LocalHook,
            source_event_ids: ids.into_iter().map(str::to_owned).collect(),
            old_oid: None,
            new_oid: None,
            range_id: Some(range.range_id.clone()),
            linked_by: None,
        };
        event.event_id = event.canonical_id();
        event
    }
    fn range() -> RangeEvidence {
        RangeEvidence {
            range_id: "c".repeat(64),
            format: "cvc.range-evidence/v1".into(),
            version: 1,
            repository_identity: "d".repeat(64),
            object_format: "sha1".into(),
            base_oid: CommitSha::new("1".repeat(40)),
            tip_oid: CommitSha::new("2".repeat(40)),
            base_tree_oid: "3".repeat(40),
            result_tree_oid: "4".repeat(40),
            commits: vec![RangeMember {
                commit_oid: CommitSha::new("2".repeat(40)),
            }],
            changeset_algorithm: changeset::ALGORITHM.into(),
            changeset_digest: "e".repeat(64),
        }
    }
    #[test]
    fn event_partition_is_complete_unsplit_and_unique() {
        let range = range();
        assert!(ValidatedSquashPlan::validate_events(&"b".repeat(40), &range, &[], &[]).is_err());
        let a = InteractionId::new();
        let b = InteractionId::new();
        let sources = vec![source(&a, "a1"), source(&a, "a2"), source(&b, "b1")];
        let good = vec![
            event(&a, vec!["a1", "a2"], &range),
            event(&b, vec!["b1"], &range),
        ];
        assert!(
            ValidatedSquashPlan::validate_events(&"b".repeat(40), &range, &sources, &good).is_ok()
        );
        let cases = vec![
            vec![],
            vec![event(&a, vec!["a1"], &range), event(&b, vec!["b1"], &range)],
            vec![
                event(&a, vec!["a1"], &range),
                event(&a, vec!["a2"], &range),
                event(&b, vec!["b1"], &range),
            ],
            vec![good[0].clone(), good[0].clone(), good[1].clone()],
            {
                let extra = InteractionId::new();
                vec![
                    good[0].clone(),
                    good[1].clone(),
                    event(&extra, vec!["x"], &range),
                ]
            },
        ];
        for invalid in cases {
            assert!(ValidatedSquashPlan::validate_events(
                &"b".repeat(40),
                &range,
                &sources,
                &invalid
            )
            .is_err());
        }
    }

    #[test]
    fn digest_mismatch_and_source_race_leave_target_pending() -> anyhow::Result<()> {
        fn commit(
            repo: &Repository,
            reference: Option<&str>,
            parent: Option<Oid>,
            body: &[u8],
        ) -> anyhow::Result<Oid> {
            let mut builder = repo.treebuilder(None)?;
            builder.insert("a", repo.blob(body)?, git2::FileMode::Blob.into())?;
            let tree = repo.find_tree(builder.write()?)?;
            let sig = Signature::now("test", "test@example.test")?;
            let parent = parent.map(|oid| repo.find_commit(oid)).transpose()?;
            let parents: Vec<_> = parent.iter().collect();
            Ok(repo.commit(reference, &sig, &sig, "test", &tree, &parents)?)
        }
        let temp = tempfile::TempDir::new()?;
        let repo = Repository::init(temp.path())?;
        let db_path = temp.path().join("index.db");
        let mut store = CvcStore::open(&db_path)?;
        let base = commit(&repo, Some("refs/heads/main"), None, b"base")?;
        let feature = commit(&repo, Some("refs/heads/feature"), Some(base), b"feature")?;
        let interaction = Interaction {
            id: InteractionId::new(),
            conversation_id: "race".into(),
            parent_id: None,
            timestamp: chrono::Utc::now(),
            author: Author::Human,
            user_prompt: "race".into(),
            model_name: None,
            model_cot: None,
            model_response: None,
            source_request_id: None,
        };
        store.capture_mcp(crate::privacy::McpCapture::new(
            Conversation {
                id: "race".into(),
                title: "race".into(),
                created_at: interaction.timestamp,
            },
            interaction.clone(),
            vec![],
            vec![],
            crate::privacy::PreparedPolicy::built_ins_only(),
        ))?;
        store.link_automatic_interaction_batch_trusted(
            &[(&interaction.id, "generated")],
            &CommitSha::new(feature.to_string()),
            None,
        )?;
        let range = observe_explicit_range(&repo, &store, base, feature, None, None, None)?;
        let wrong = commit(&repo, None, Some(base), b"wrong")?;
        assert!(ValidatedSquashPlan::build(
            &repo,
            &store,
            "w",
            "refs/heads/main",
            &wrong.to_string(),
            &wrong.to_string(),
            &base.to_string(),
            &wrong.to_string(),
            &range
        )
        .is_err());
        let target = commit(&repo, None, Some(base), b"feature")?;
        repo.reference("refs/heads/main", target, true, "test")?;
        repo.set_head("refs/heads/main")?;
        let worktree = hex::encode(Sha256::digest(repo.path().as_os_str().as_encoded_bytes()));
        store.discover_squash_targets(
            &worktree,
            "refs/heads/main",
            None,
            &target.to_string(),
            &[(target.to_string(), base.to_string(), true)],
        )?;
        let plan = ValidatedSquashPlan::build(
            &repo,
            &store,
            &worktree,
            "refs/heads/main",
            &target.to_string(),
            &target.to_string(),
            &base.to_string(),
            &target.to_string(),
            &range,
        )?;
        let db = rusqlite::Connection::open(&db_path)?;
        let fake = InteractionId::new();
        let snapshot = SourceSnapshot::Legacy {
            interaction: fake.to_string(),
            commit: feature.to_string(),
            link_type: "generated".into(),
            linked_by: None,
            authorization_rows: vec![],
        };
        db.execute("INSERT INTO range_interaction_sources(range_id,interaction_id,source_id,snapshot) VALUES(?1,?2,'race',?3)",rusqlite::params![range.range_id,fake.to_string(),serde_json::to_string(&snapshot)?])?;
        assert!(store.apply_squash_plan(&repo, &plan).is_err());
        let status: String = db.query_row(
            "SELECT status FROM pending_squash_targets WHERE target_commit=?1",
            [target.to_string()],
            |row| row.get(0),
        )?;
        assert_eq!(status, "pending");
        let events: i64 = db.query_row(
            "SELECT COUNT(*) FROM derivation_events WHERE relation='squash_exact'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(events, 0);
        Ok(())
    }
}
