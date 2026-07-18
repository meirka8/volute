//! Fail-closed capture sanitization and repository-local capture policy.
use crate::models::{
    ArtifactLink, CaptureSource, ContextItem, Conversation, Interaction, ToolExecution,
};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::path::{Component, Path};
use thiserror::Error;
use uuid::Uuid;

const MAX_TEXT: usize = 1024 * 1024;
const MAX_POLICY: usize = 64 * 1024;
const MAX_DIRECTIVE: usize = 4096;
const MAX_REGEXES: usize = 64;
const MAX_LITERALS: usize = 128;
const MAX_MATCHES: usize = 256;
const MAX_CONTEXT: usize = 256;
const MAX_TOOLS: usize = 256;
const MAX_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Error, Debug)]
pub enum PrivacyError {
    #[error("capture value exceeds limit")]
    TooLarge,
    #[error("invalid .thoughtignore: {0}")]
    Policy(String),
}
pub type Result<T> = std::result::Result<T, PrivacyError>;

const PRIVACY_NOTICE_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize, Debug, Default)]
struct PrivacyFile {
    version: u32,
    capture_acknowledged_version: Option<u32>,
    sharing: Vec<SharingConsent>,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct SharingConsent {
    fingerprint: String,
    notice_version: u32,
    auto_push: bool,
}
#[derive(Debug, Clone, Default)]
pub struct PrivacyStatus {
    pub capture_acknowledged: bool,
    pub sharing_consented: bool,
    pub auto_push: bool,
    pub remote_fingerprint: String,
}

/// An immutable description of the destination selected for one operation.  Do not
/// re-read the git config after this is created: `pushurl` is deliberately allowed
/// to differ from the fetch URL.
#[derive(Debug, Clone)]
pub struct RemoteDestination {
    pub name: String,
    pub effective_url: String,
    pub fingerprint: String,
    pub ref_name: String,
}

pub fn remote_destination(repo: &git2::Repository, remote_name: &str) -> Result<RemoteDestination> {
    let remote = repo
        .find_remote(remote_name)
        .map_err(|_| PrivacyError::Policy("remote is not configured".into()))?;
    let url = remote
        .pushurl()
        .or_else(|| remote.url())
        .ok_or_else(|| PrivacyError::Policy("remote has no push URL".into()))?
        .trim()
        .to_owned();
    if url.is_empty() {
        return Err(PrivacyError::Policy("remote has an empty push URL".into()));
    }
    let ref_name = "refs/cvc/main".to_owned();
    let fingerprint = hex::encode(Sha256::digest(
        format!("remote-url:{url}\nref:{ref_name}").as_bytes(),
    ));
    Ok(RemoteDestination {
        name: remote_name.to_owned(),
        effective_url: url,
        fingerprint,
        ref_name,
    })
}

fn privacy_path(repo: &git2::Repository) -> PathBuf {
    common_git_dir(repo).join("cvc").join("privacy.json")
}
/// libgit2 0.19 does not expose git_common_dir. Git writes a relative `commondir`
/// file for linked worktrees; normal repositories simply use `Repository::path()`.
pub fn common_git_dir(repo: &git2::Repository) -> PathBuf {
    let git_dir = repo.path();
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(value) => {
            let candidate = Path::new(value.trim());
            if candidate.is_absolute() {
                candidate.to_path_buf()
            } else {
                git_dir.join(candidate)
            }
        }
        Err(_) => git_dir.to_path_buf(),
    }
}
pub fn remote_fingerprint(repo: &git2::Repository, remote_name: &str) -> Result<String> {
    Ok(remote_destination(repo, remote_name)?.fingerprint)
}
fn read_privacy(repo: &git2::Repository) -> Result<PrivacyFile> {
    let path = privacy_path(repo);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(x) => x,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(PrivacyFile {
                version: PRIVACY_NOTICE_VERSION,
                ..Default::default()
            })
        }
        Err(e) => return Err(PrivacyError::Policy(e.to_string())),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Err(PrivacyError::Policy(
            "privacy state is not a regular file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(PrivacyError::Policy(
                "privacy state permissions are insecure".into(),
            ));
        }
    }
    #[cfg(unix)]
    let bytes = {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let opened = file
            .metadata()
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if !opened.is_file()
            || opened.dev() != meta.dev()
            || opened.ino() != meta.ino()
            || opened.mode() & 0o077 != 0
        {
            return Err(PrivacyError::Policy(
                "privacy state changed while opening".into(),
            ));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        bytes
    };
    #[cfg(not(unix))]
    let bytes = std::fs::read(path).map_err(|e| PrivacyError::Policy(e.to_string()))?;
    let state: PrivacyFile = serde_json::from_slice(&bytes)
        .map_err(|_| PrivacyError::Policy("privacy state is malformed".into()))?;
    if state.version != PRIVACY_NOTICE_VERSION {
        return Err(PrivacyError::Policy(
            "privacy notice version is unsupported".into(),
        ));
    }
    Ok(state)
}
fn write_privacy(repo: &git2::Repository, state: &PrivacyFile) -> Result<()> {
    let path = privacy_path(repo);
    let parent = path
        .parent()
        .ok_or_else(|| PrivacyError::Policy("invalid privacy path".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| PrivacyError::Policy(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // The CVC directory is repository-private state. Tighten directories we
        // create before placing any state in them.
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let meta =
            std::fs::symlink_metadata(parent).map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if meta.file_type().is_symlink()
            || !meta.is_dir()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return Err(PrivacyError::Policy(
                "privacy directory ownership or permissions are insecure".into(),
            ));
        }
    }
    let tmp = parent.join(format!(".privacy-{}.tmp", Uuid::new_v4()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        serde_json::to_writer_pretty(&mut f, state)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        use std::io::Write;
        f.flush().map_err(|e| PrivacyError::Policy(e.to_string()))?;
        f.sync_all()
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(state).map_err(|e| PrivacyError::Policy(e.to_string()))?,
        )
        .map_err(|e| PrivacyError::Policy(e.to_string()))?;
    }
    std::fs::rename(tmp, &path).map_err(|e| PrivacyError::Policy(e.to_string()))?;
    #[cfg(unix)]
    {
        std::fs::File::open(parent)
            .and_then(|d| d.sync_all())
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
    }
    Ok(())
}
pub struct PrivacyLock(std::fs::File);
impl Drop for PrivacyLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}
fn privacy_lock(repo: &git2::Repository) -> Result<PrivacyLock> {
    let path = privacy_path(repo);
    let parent = path
        .parent()
        .ok_or_else(|| PrivacyError::Policy("invalid privacy path".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| PrivacyError::Policy(e.to_string()))?;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let meta =
            std::fs::symlink_metadata(parent).map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if meta.file_type().is_symlink()
            || meta.uid() != unsafe { libc::geteuid() }
            || meta.mode() & 0o077 != 0
        {
            return Err(PrivacyError::Policy("privacy directory is insecure".into()));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(parent.join("privacy.lock"))
        .map_err(|e| PrivacyError::Policy(e.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = file
            .metadata()
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err(PrivacyError::Policy("privacy lock is insecure".into()));
        }
    }
    fs2::FileExt::lock_exclusive(&file)
        .map_err(|e| PrivacyError::Policy(format!("cannot lock privacy state: {e}")))?;
    Ok(PrivacyLock(file))
}
/// Serializes the full publication state machine for one immutable destination.
/// The fingerprint is hex, so it is safe as a filename component.
pub fn destination_operation_lock(
    repo: &git2::Repository,
    fingerprint: &str,
) -> Result<PrivacyLock> {
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(PrivacyError::Policy(
            "invalid destination fingerprint".into(),
        ));
    }
    let path = privacy_path(repo);
    let parent = path
        .parent()
        .ok_or_else(|| PrivacyError::Policy("invalid privacy path".into()))?;
    std::fs::create_dir_all(parent).map_err(|e| PrivacyError::Policy(e.to_string()))?;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(parent.join(format!("publish-{fingerprint}.lock")))
        .map_err(|e| PrivacyError::Policy(e.to_string()))?;
    fs2::FileExt::lock_exclusive(&file)
        .map_err(|e| PrivacyError::Policy(format!("cannot lock destination: {e}")))?;
    Ok(PrivacyLock(file))
}
pub fn privacy_status(repo: &git2::Repository, remote: &str) -> Result<PrivacyStatus> {
    let destination = remote_destination(repo, remote)?;
    privacy_status_for_fingerprint(repo, &destination.fingerprint)
}
pub fn privacy_status_for_fingerprint(repo: &git2::Repository, fp: &str) -> Result<PrivacyStatus> {
    let state = read_privacy(repo)?;
    let consent = state
        .sharing
        .iter()
        .find(|x| x.fingerprint == fp && x.notice_version == PRIVACY_NOTICE_VERSION);
    Ok(PrivacyStatus {
        capture_acknowledged: state.capture_acknowledged_version == Some(PRIVACY_NOTICE_VERSION),
        sharing_consented: consent.is_some(),
        auto_push: consent.is_some_and(|x| x.auto_push),
        remote_fingerprint: fp.to_owned(),
    })
}
/// Passive collection is opt-in. Explicit local actions may still persist private
/// data, but background IDE observation must fail closed until this is true.
pub fn capture_acknowledged(repo: &git2::Repository) -> Result<bool> {
    Ok(read_privacy(repo)?.capture_acknowledged_version == Some(PRIVACY_NOTICE_VERSION))
}
pub fn acknowledge_capture(repo: &git2::Repository) -> Result<()> {
    let _lock = privacy_lock(repo)?;
    let mut s = read_privacy(repo)?;
    s.capture_acknowledged_version = Some(PRIVACY_NOTICE_VERSION);
    write_privacy(repo, &s)
}
pub fn acknowledge_sharing(repo: &git2::Repository, remote: &str) -> Result<()> {
    let destination = remote_destination(repo, remote)?;
    acknowledge_sharing_destination(repo, &destination)
}
pub fn acknowledge_sharing_destination(
    repo: &git2::Repository,
    destination: &RemoteDestination,
) -> Result<()> {
    let _lock = privacy_lock(repo)?;
    let mut s = read_privacy(repo)?;
    s.sharing
        .retain(|x| x.fingerprint != destination.fingerprint);
    s.sharing.push(SharingConsent {
        fingerprint: destination.fingerprint.clone(),
        notice_version: PRIVACY_NOTICE_VERSION,
        auto_push: false,
    });
    write_privacy(repo, &s)
}
pub fn set_auto_push(repo: &git2::Repository, remote: &str, enabled: bool) -> Result<()> {
    let destination = remote_destination(repo, remote)?;
    set_auto_push_destination(repo, &destination, enabled)
}
pub fn set_auto_push_destination(
    repo: &git2::Repository,
    destination: &RemoteDestination,
    enabled: bool,
) -> Result<()> {
    let _lock = privacy_lock(repo)?;
    let mut s = read_privacy(repo)?;
    let consent = s
        .sharing
        .iter_mut()
        .find(|x| {
            x.fingerprint == destination.fingerprint && x.notice_version == PRIVACY_NOTICE_VERSION
        })
        .ok_or_else(|| PrivacyError::Policy("sharing consent is required".into()))?;
    consent.auto_push = enabled;
    write_privacy(repo, &s)
}

fn marker(kind: &str) -> String {
    format!("[CVC_REDACTED:v1:{kind}:{}]", Uuid::new_v4())
}
fn re(p: &str) -> Regex {
    Regex::new(p).expect("built-in privacy regex is valid")
}

/// Bounded high-confidence replacement. Existing markers are deliberately preserved.
pub fn scrub(input: &str) -> Result<String> {
    if input.len() > MAX_TEXT {
        return Err(PrivacyError::TooLarge);
    }
    let marker_re = re(r"\[CVC_REDACTED:v1:[a-z_]+:[0-9a-fA-F-]{16,}\]");
    let mut protected = Vec::new();
    let staged = marker_re
        .replace_all(input, |m: &regex::Captures<'_>| {
            protected.push(m[0].to_owned());
            format!("\u{0}CVC{}\u{0}", protected.len() - 1)
        })
        .into_owned();
    let patterns = [
        (
            "pem",
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
        ),
        ("aws_access_key", r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b"),
        (
            "jwt",
            r"\beyJ[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\.[a-zA-Z0-9_-]{8,}\b",
        ),
        (
            "github_token",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,})\b",
        ),
        ("openai_token", r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b"),
        ("slack_token", r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b"),
        ("google_token", r"\bya29\.[A-Za-z0-9_-]{20,}\b"),
        (
            "authorization",
            r"(?i)\b(?:authorization\s*:\s*|bearer\s+)[A-Za-z0-9._~+/=-]{12,}",
        ),
        (
            "url_credentials",
            r"(?i)\b[a-z][a-z0-9+.-]*://[^\s/:@]+:[^\s@]+@",
        ),
        (
            "secret_assignment",
            r#"(?i)\b(?:aws_secret_access_key|api[_-]?key|secret|password|token)\s*[:=]\s*[^\s'"`]{8,}"#,
        ),
    ];
    let mut out = staged;
    for (kind, pattern) in patterns {
        let regex = re(pattern);
        let count = regex.find_iter(&out).count();
        if count > MAX_MATCHES {
            return Err(PrivacyError::Policy("too many secret matches".into()));
        }
        out = regex
            .replace_all(&out, |_: &regex::Captures<'_>| marker(kind))
            .into_owned();
    }
    for (i, value) in protected.iter().enumerate() {
        out = out.replace(&format!("\u{0}CVC{i}\u{0}"), value);
    }
    Ok(out)
}

pub fn scrub_json(input: &str) -> Result<String> {
    if input.len() > MAX_TEXT {
        return Err(PrivacyError::TooLarge);
    }
    match serde_json::from_str::<Value>(input) {
        Ok(mut value) => {
            scrub_value(&mut value)?;
            serde_json::to_string(&value).map_err(|e| PrivacyError::Policy(e.to_string()))
        }
        Err(_) => scrub(input),
    }
}
pub fn scrub_json_with_policy(input: &str, policy: &ThoughtIgnore) -> Result<String> {
    if input.len() > MAX_TEXT {
        return Err(PrivacyError::TooLarge);
    }
    match serde_json::from_str::<Value>(input) {
        Ok(mut value) => {
            scrub_value_with_policy(&mut value, policy)?;
            serde_json::to_string(&value).map_err(|e| PrivacyError::Policy(e.to_string()))
        }
        Err(_) => policy.scrub(input),
    }
}
fn scrub_value_with_policy(value: &mut Value, policy: &ThoughtIgnore) -> Result<()> {
    match value {
        Value::String(s) => *s = policy.scrub(s)?,
        Value::Array(values) => {
            for value in values {
                scrub_value_with_policy(value, policy)?;
            }
        }
        Value::Object(values) => {
            let old = std::mem::take(values);
            for (key, mut value) in old {
                let credential = is_credential_key(&key);
                let key = policy.scrub(&key)?;
                if credential {
                    redact_json_leaves(&mut value);
                } else {
                    scrub_value_with_policy(&mut value, policy)?;
                }
                values.insert(key, value);
            }
        }
        _ => {}
    };
    Ok(())
}
fn scrub_value(value: &mut Value) -> Result<()> {
    match value {
        Value::String(s) => *s = scrub(s)?,
        Value::Array(a) => {
            for v in a {
                scrub_value(v)?
            }
        }
        Value::Object(o) => {
            let old = std::mem::take(o);
            for (k, mut v) in old {
                let scrubbed_key = scrub(&k)?;
                if is_credential_key(&k) {
                    redact_json_leaves(&mut v);
                } else {
                    scrub_value(&mut v)?;
                }
                o.insert(scrubbed_key, v);
            }
        }
        _ => {}
    };
    Ok(())
}

/// A credential-bearing key taints its complete JSON subtree.  Preserve the
/// caller's shape so consumers retain schema compatibility, but no string leaf
/// in that subtree may retain the credential value.
fn redact_json_leaves(value: &mut Value) {
    match value {
        Value::String(value) if is_redaction_marker(value) => {}
        Value::String(value) => *value = marker("secret_assignment"),
        Value::Array(values) => values.iter_mut().for_each(redact_json_leaves),
        Value::Object(values) => values.values_mut().for_each(redact_json_leaves),
        _ => {}
    }
}

fn is_redaction_marker(value: &str) -> bool {
    re(r"^\[CVC_REDACTED:v1:[a-z_]+:[0-9a-fA-F-]{16,}\]$").is_match(value)
}

/// Verifies the precise JSON tool-arguments representation is already
/// credential-aware sanitized (rather than merely free of built-in token
/// patterns).
pub fn validate_sanitized_json(input: &str) -> Result<()> {
    if scrub_json(input)? != input {
        return Err(PrivacyError::Policy("unsanitized JSON value".into()));
    }
    Ok(())
}

fn is_credential_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    matches!(
        normalized.as_str(),
        "awssecretaccesskey"
            | "apikey"
            | "clientsecret"
            | "accesstoken"
            | "refreshtoken"
            | "privatekey"
            | "secret"
            | "password"
            | "passwd"
            | "pwd"
            | "token"
            | "authorization"
    )
}

#[derive(Default, Clone)]
pub struct ThoughtIgnore {
    paths: Vec<String>,
    literals: Vec<String>,
    regexes: Vec<Regex>,
}
impl ThoughtIgnore {
    fn load(root: &Path) -> Result<Self> {
        let file = root.join(".thoughtignore");
        // `exists` follows links and treats broken links as absent, silently
        // disabling a policy. Inspect the directory entry itself instead.
        let meta = match std::fs::symlink_metadata(&file) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default())
            }
            Err(error) => return Err(PrivacyError::Policy(error.to_string())),
        };
        if meta.file_type().is_symlink()
            || !meta.file_type().is_file()
            || meta.len() as usize > MAX_POLICY
        {
            return Err(PrivacyError::Policy(
                "policy is symlinked or oversized".into(),
            ));
        }
        let text = read_policy_file(&file, &meta)?;
        let mut p = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line.len() > MAX_DIRECTIVE {
                return Err(PrivacyError::Policy("directive oversized".into()));
            }
            if let Some(x) = line.strip_prefix("path:") {
                validate_path(x)?;
                p.paths.push(x.to_owned());
            } else if let Some(x) = line.strip_prefix("literal:") {
                if x.is_empty() || p.literals.len() == MAX_LITERALS {
                    return Err(PrivacyError::Policy("invalid or excessive literal".into()));
                }
                p.literals.push(x.to_owned());
            } else if let Some(x) = line.strip_prefix("regex:") {
                if p.regexes.len() == MAX_REGEXES || x.contains("(?") || x.len() > MAX_DIRECTIVE {
                    return Err(PrivacyError::Policy("unsafe regex".into()));
                };
                p.regexes.push(
                    Regex::new(x).map_err(|_| PrivacyError::Policy("malformed regex".into()))?,
                );
            } else {
                return Err(PrivacyError::Policy("unknown directive".into()));
            }
        }
        Ok(p)
    }
    pub fn ignores_path(&self, path: &str) -> bool {
        path == ".thoughtignore"
            || self
                .paths
                .iter()
                .any(|p| path == p || path.starts_with(&format!("{p}/")))
    }
    /// Repository rules add redactions; they never remove built-in redactions.
    pub fn scrub(&self, input: &str) -> Result<String> {
        let mut output = scrub(input)?;
        let mut matches = 0usize;
        for literal in &self.literals {
            while output.contains(literal) {
                matches += 1;
                if matches > MAX_MATCHES {
                    return Err(PrivacyError::Policy("too many policy matches".into()));
                }
                output = output.replacen(literal, &marker("policy_literal"), 1);
            }
        }
        for regex in &self.regexes {
            let count = regex.find_iter(&output).count();
            matches += count;
            if matches > MAX_MATCHES {
                return Err(PrivacyError::Policy("too many policy matches".into()));
            }
            output = regex
                .replace_all(&output, |_: &regex::Captures<'_>| marker("policy_regex"))
                .into_owned();
        }
        Ok(output)
    }
}

/// An opaque, single-read policy snapshot for one capture operation.  It is
/// intentionally not reloadable by capture code: callers that inspect paths
/// and later persist a capture hand the exact same value to the capture.
#[derive(Clone, Default)]
pub struct PreparedPolicy(ThoughtIgnore);
impl PreparedPolicy {
    pub fn load(root: &Path) -> Result<Self> {
        ThoughtIgnore::load(root).map(Self)
    }
    pub fn built_ins_only() -> Self {
        Self::default()
    }
    pub fn ignores_path(&self, path: &str) -> bool {
        self.0.ignores_path(path)
    }
}

fn read_policy_file(path: &Path, before: &std::fs::Metadata) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let opened = file
            .metadata()
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if !opened.is_file()
            || opened.len() as usize > MAX_POLICY
            || before.dev() != opened.dev()
            || before.ino() != opened.ino()
        {
            return Err(PrivacyError::Policy("policy changed while opening".into()));
        }
        use std::io::Read;
        let mut text = String::new();
        (&file)
            .take((MAX_POLICY + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if text.len() > MAX_POLICY {
            return Err(PrivacyError::Policy("policy oversized".into()));
        }
        let after_handle = file
            .metadata()
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let after_path =
            std::fs::symlink_metadata(path).map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if after_handle.dev() != opened.dev()
            || after_handle.ino() != opened.ino()
            || after_handle.len() != opened.len()
            || after_handle.mtime() != opened.mtime()
            || after_handle.ctime() != opened.ctime()
            || after_path.dev() != opened.dev()
            || after_path.ino() != opened.ino()
            || after_path.len() != opened.len()
            || after_path.mtime() != opened.mtime()
            || after_path.ctime() != opened.ctime()
            || text.len() as u64 != after_handle.len()
        {
            return Err(PrivacyError::Policy("policy changed while reading".into()));
        }
        Ok(text)
    }
    #[cfg(windows)]
    {
        // Windows lacks a portable O_NOFOLLOW equivalent in std. Compare the
        // directory entry and opened handle metadata; a race remains possible,
        // so reject anything that is not an unchanged regular file.
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT obtains the reparse point itself rather
        // than following it; subsequent regular-file checks reject links.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(0x0020_0000)
            .open(path)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let opened = file
            .metadata()
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        let after =
            std::fs::symlink_metadata(path).map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if !opened.is_file()
            || after.file_type().is_symlink()
            || opened.len() as usize > MAX_POLICY
            || before.len() != after.len()
            || before.modified().ok() != after.modified().ok()
        {
            return Err(PrivacyError::Policy("policy changed while opening".into()));
        }
        use std::io::Read;
        let mut text = String::new();
        (&file)
            .take((MAX_POLICY + 1) as u64)
            .read_to_string(&mut text)
            .map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if text.len() > MAX_POLICY {
            return Err(PrivacyError::Policy("policy oversized".into()));
        }
        let after =
            std::fs::symlink_metadata(path).map_err(|e| PrivacyError::Policy(e.to_string()))?;
        if after.file_type().is_symlink()
            || after.len() != opened.len()
            || after.modified().ok() != opened.modified().ok()
            || text.len() as u64 != after.len()
        {
            return Err(PrivacyError::Policy("policy changed while reading".into()));
        }
        Ok(text)
    }
    #[cfg(all(not(unix), not(windows)))]
    {
        let _ = (path, before);
        // There is no portable no-follow handle primitive here.  Treat an
        // existing policy as unsafe instead of pretending path metadata closes
        // the TOCTOU window.
        Err(PrivacyError::Policy(
            "secure policy loading unsupported on this platform".into(),
        ))
    }
}
fn validate_path(p: &str) -> Result<()> {
    let path = Path::new(p);
    if p.is_empty()
        || p != p.trim()
        || p.contains("//")
        || p.contains('\\')
        || p.starts_with("./")
        || p.contains("/./")
        || path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        Err(PrivacyError::Policy("unsafe path".into()))
    } else {
        Ok(())
    }
}

pub(crate) struct Capture {
    pub(crate) conversation: Conversation,
    pub(crate) interaction: Interaction,
    pub(crate) context_items: Vec<ContextItem>,
    pub(crate) tool_executions: Vec<ToolExecution>,
    pub(crate) source: CaptureSource,
    pub(crate) policy: PreparedPolicy,
}
macro_rules! capture_type {
    ($name:ident, $source:expr) => {
        pub struct $name(Capture);
        impl $name {
            pub fn new(
                conversation: Conversation,
                interaction: Interaction,
                context_items: Vec<ContextItem>,
                tool_executions: Vec<ToolExecution>,
                policy: PreparedPolicy,
            ) -> Self {
                Self(Capture {
                    conversation,
                    interaction,
                    context_items,
                    tool_executions,
                    source: $source,
                    policy,
                })
            }
            pub(crate) fn into_capture(self) -> Capture {
                self.0
            }
        }
    };
}
capture_type!(McpCapture, CaptureSource::Mcp);
capture_type!(CliRunCapture, CaptureSource::CliRun);
capture_type!(LspPassiveCapture, CaptureSource::VscodePassive);
capture_type!(LspExplicitCapture, CaptureSource::VscodeExplicit);

pub(crate) fn sync_import_capture(
    conversation: Conversation,
    interaction: Interaction,
    context_items: Vec<ContextItem>,
    tool_executions: Vec<ToolExecution>,
) -> Capture {
    Capture {
        conversation,
        interaction,
        context_items,
        tool_executions,
        source: CaptureSource::SyncImport,
        policy: PreparedPolicy::built_ins_only(),
    }
}
pub(crate) fn prepare(mut capture: Capture) -> Result<Capture> {
    if capture.context_items.len() > MAX_CONTEXT || capture.tool_executions.len() > MAX_TOOLS {
        return Err(PrivacyError::Policy(
            "capture collection exceeds limit".into(),
        ));
    }
    let bytes = capture.conversation.id.len()
        + capture.conversation.title.len()
        + capture.interaction.conversation_id.len()
        + capture.interaction.user_prompt.len()
        + capture
            .interaction
            .model_name
            .as_ref()
            .map_or(0, String::len)
        + capture
            .interaction
            .model_cot
            .as_ref()
            .map_or(0, String::len)
        + capture
            .interaction
            .model_response
            .as_ref()
            .map_or(0, String::len)
        + capture
            .context_items
            .iter()
            .map(|x| {
                x.file_path.len()
                    + x.git_blob_sha.as_ref().map_or(0, String::len)
                    + x.dirty_patch.as_ref().map_or(0, String::len)
            })
            .sum::<usize>()
        + capture
            .tool_executions
            .iter()
            .map(|x| x.tool_protocol.len() + x.tool_name.len() + x.arguments.len())
            .sum::<usize>();
    if bytes > MAX_CAPTURE_BYTES {
        return Err(PrivacyError::TooLarge);
    }
    let policy = &capture.policy.0;
    capture.conversation.title = policy.scrub(&capture.conversation.title)?;
    // IDs are relational keys and must remain stable across capture calls. A
    // randomized redaction marker cannot safely replace one; reject a sensitive
    // caller-provided ID rather than storing it or breaking its relationships.
    if !is_safe_identifier(&capture.conversation.id)
        || capture.interaction.conversation_id != capture.conversation.id
    {
        return Err(PrivacyError::Policy(
            "unsafe or inconsistent conversation id".into(),
        ));
    }
    capture.interaction.source_request_id = capture
        .interaction
        .source_request_id
        .map(|s| {
            if !is_safe_identifier(&s) {
                return Err(PrivacyError::Policy("unsafe source request id".into()));
            }
            scrub(&s)
        })
        .transpose()?;
    capture.interaction.user_prompt = policy.scrub(&capture.interaction.user_prompt)?;
    capture.interaction.model_name = capture
        .interaction
        .model_name
        .map(|s| policy.scrub(&s))
        .transpose()?;
    capture.interaction.model_cot = capture
        .interaction
        .model_cot
        .map(|s| policy.scrub(&s))
        .transpose()?;
    capture.interaction.model_response = capture
        .interaction
        .model_response
        .map(|s| policy.scrub(&s))
        .transpose()?;
    for x in &mut capture.context_items {
        if policy.ignores_path(&x.file_path) {
            x.file_path.clear();
            continue;
        }
        x.file_path = policy.scrub(&x.file_path)?;
        if let Some(sha) = &x.git_blob_sha {
            if !is_oid(sha) {
                return Err(PrivacyError::Policy("invalid git blob oid".into()));
            }
        }
        x.dirty_patch = x.dirty_patch.take().map(|s| policy.scrub(&s)).transpose()?;
    }
    capture.context_items.retain(|x| !x.file_path.is_empty());
    for x in &mut capture.tool_executions {
        x.tool_protocol = policy.scrub(&x.tool_protocol)?;
        x.tool_name = policy.scrub(&x.tool_name)?;
        x.arguments = scrub_json_with_policy(&x.arguments, policy)?;
    }
    Ok(capture)
}

fn is_safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}
fn is_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Defense-in-depth serialization boundary for data which may predate capture v1.
/// It deliberately has no repository policy: policy controls capture, while this
/// guard ensures no raw known-secret reaches a Git blob.
pub fn final_scrub_for_sync(
    interaction: &mut Interaction,
    context: &mut [ContextItem],
    tools: &mut [ToolExecution],
) -> Result<()> {
    interaction.conversation_id = scrub(&interaction.conversation_id)?;
    interaction.source_request_id = interaction
        .source_request_id
        .take()
        .map(|v| scrub(&v))
        .transpose()?;
    interaction.user_prompt = scrub(&interaction.user_prompt)?;
    interaction.model_name = interaction
        .model_name
        .take()
        .map(|v| scrub(&v))
        .transpose()?;
    interaction.model_cot = interaction
        .model_cot
        .take()
        .map(|v| scrub(&v))
        .transpose()?;
    interaction.model_response = interaction
        .model_response
        .take()
        .map(|v| scrub(&v))
        .transpose()?;
    for item in context {
        item.file_path = scrub(&item.file_path)?;
        item.dirty_patch = item.dirty_patch.take().map(|v| scrub(&v)).transpose()?;
    }
    for tool in tools {
        tool.tool_protocol = scrub(&tool.tool_protocol)?;
        tool.tool_name = scrub(&tool.tool_name)?;
        tool.arguments = scrub_json(&tool.arguments)?;
    }
    Ok(())
}

pub fn final_validate_links(links: &mut [ArtifactLink]) -> Result<()> {
    for link in links {
        if !is_oid(link.git_commit_hash.as_str()) {
            return Err(PrivacyError::Policy("invalid link oid".into()));
        }
        if !matches!(
            link.link_type.as_str(),
            "generated" | "temporal" | "verified"
        ) {
            return Err(PrivacyError::Policy("invalid link type".into()));
        }
        link.linked_by = link
            .linked_by
            .take()
            .map(|value| scrub(&value))
            .transpose()?;
    }
    Ok(())
}
