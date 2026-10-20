//! Fail-closed capture sanitization and repository-local capture policy.
use crate::models::{
    ArtifactLink, CaptureSource, ContextItem, Conversation, Interaction, InteractionId,
    ToolExecution,
};
use crate::stimulus;
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::ops::Range;
use std::path::PathBuf;
use std::path::{Component, Path};
use std::sync::OnceLock;
use thiserror::Error;
use uuid::Uuid;

/// The longest single value the scrubber takes.
pub(crate) const MAX_TEXT: usize = 1024 * 1024;
const MAX_POLICY: usize = 64 * 1024;
const MAX_DIRECTIVE: usize = 4096;
const MAX_REGEXES: usize = 64;
const MAX_LITERALS: usize = 128;
const MAX_MATCHES: usize = 256;
/// The most context items and tool executions one capture may hold.
pub(crate) const MAX_CONTEXT: usize = 256;
pub(crate) const MAX_TOOLS: usize = 256;
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

fn privacy_path(repo: &git2::Repository) -> Result<PathBuf> {
    Ok(crate::repository::common_git_dir(repo)
        .map_err(|e| PrivacyError::Policy(e.to_string()))?
        .join("cvc")
        .join("privacy.json"))
}
/// Legacy best-effort common directory helper retained for API compatibility.
/// New production code must use [`crate::repository::RepositoryLayout`] or its
/// fallible [`crate::repository::common_git_dir`] helper instead.
#[deprecated(note = "use cvc_core::repository::RepositoryLayout or common_git_dir")]
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
    let path = privacy_path(repo)?;
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
    let path = privacy_path(repo)?;
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
    let path = privacy_path(repo)?;
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
    let path = privacy_path(repo)?;
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

/// An existing CVC redaction marker.
const REDACTION_MARKER: &str = r"\[CVC_REDACTED:v1:[a-z_]+:[0-9a-fA-F-]{16,}\]";

fn redaction_marker_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| re(REDACTION_MARKER))
}

/// The byte ranges of the redaction markers in `text`, in order. Bounding code
/// keeps these whole: a cut inside one would leave a fragment that is neither
/// text nor marker.
pub(crate) fn redaction_marker_spans(text: &str) -> Vec<Range<usize>> {
    redaction_marker_regex()
        .find_iter(text)
        .map(|found| found.range())
        .collect()
}

/// The built-in detectors in application order, compiled once.
fn builtin_detectors() -> &'static [(&'static str, Regex)] {
    static DETECTORS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    DETECTORS.get_or_init(|| {
        [
            (
                "pem",
                r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----.*?-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----",
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
        ]
        .into_iter()
        .map(|(kind, pattern)| (kind, re(pattern)))
        .collect()
    })
}

/// A private-key header line alone, whether or not its END line follows. The
/// OpenPGP armor form ends in `PRIVATE KEY BLOCK`.
fn pem_header_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| re(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----"))
}

/// A private-key END line alone, whether or not its header precedes it.
fn pem_footer_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| re(r"-----END [A-Z0-9 ]*PRIVATE KEY(?: BLOCK)?-----"))
}

// ---- a key block with only one end -------------------------------------------------
//
// A complete block (header, body, END line) is replaced as a whole by the `pem`
// detector. A key can also arrive with one end missing: cut by a size bound or by
// the tool that printed it (a header, then its body, then nothing), or shown from
// the middle (`tail`, a ranged read, a diff: a body and then the END line). The
// key material is then the run of base64 lines next to the lone line, and that run
// is redacted with the line. Nothing else is: prose that mentions a header
// ("must start with -----BEGIN RSA PRIVATE KEY-----") keeps the rest of its text,
// and a header or END line with no key material beside it is left as it is.

/// The characters of the base64 text that PEM, OpenSSH and OpenPGP key bodies are
/// written in.
fn is_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')
}

/// A line of a key body is at least this long, except the last one.
const KEY_LINE_MIN: usize = 16;
/// Armor and legacy-encryption header lines (`Version: ...`, `DEK-Info: ...`)
/// that may sit between a header and its body: at most this many, this long.
const KEY_PREAMBLE_LINES: usize = 12;
const KEY_PREAMBLE_LINE_BYTES: usize = 256;

/// The length of the line break that starts at `at`: `\n` or `\r\n`, or the same
/// written out as the characters backslash and `n`, as it is in text that was
/// itself escaped (a log line, a JSON string shown raw). 0 when there is none.
fn break_at(bytes: &[u8], at: usize) -> usize {
    let rest = &bytes[at..];
    if rest.starts_with(b"\r\n") {
        2
    } else if rest.starts_with(b"\n") {
        1
    } else if rest.starts_with(b"\\r\\n") {
        4
    } else if rest.starts_with(b"\\n") {
        2
    } else {
        0
    }
}

/// The length of the line break that ends exactly at `at`; 0 when there is none.
fn break_before(bytes: &[u8], at: usize) -> usize {
    let head = &bytes[..at];
    if head.ends_with(b"\r\n") {
        2
    } else if head.ends_with(b"\n") {
        1
    } else if head.ends_with(b"\\r\\n") {
        4
    } else if head.ends_with(b"\\n") {
        2
    } else {
        0
    }
}

/// Whether `line` is key text: only base64 characters, and at least one letter
/// or digit (so a rule of `=` signs is not).
fn is_key_line(line: &[u8]) -> bool {
    !line.is_empty()
        && line.iter().all(|byte| is_key_byte(*byte))
        && line.iter().any(u8::is_ascii_alphanumeric)
}

/// The longest line number a tool prints in front of a line.
const LINE_NUMBER_DIGITS: usize = 10;
/// How far before an END line its own line may start (a line number and a
/// marker with the spaces around them).
const LINE_PREFIX_MAX: usize = 32;

/// How much of the start of `line` (which has no leading spaces) is what a tool
/// puts in front of the lines it prints, with the spaces after it: a line number
/// (`59\t`, `59→`, `59|`, `59:`, as the Read tool, `cat -n` and `grep -n` print
/// them) or the one marker a diff or a quotation uses (`-`, `+`, `>`). 0 when
/// there is none.
fn line_prefix_len(line: &[u8]) -> usize {
    let spaces_after = |from: usize| {
        from + line[from..]
            .iter()
            .take_while(|byte| matches!(**byte, b' ' | b'\t'))
            .count()
    };
    let digits = line.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if (1..=LINE_NUMBER_DIGITS).contains(&digits) {
        let rest = &line[digits..];
        let separator = if rest.starts_with("→".as_bytes()) {
            "→".len()
        } else if rest.starts_with(b"\t") || rest.starts_with(b"|") || rest.starts_with(b":") {
            1
        } else {
            0
        };
        if separator > 0 {
            return spaces_after(digits + separator);
        }
    }
    match line.first() {
        Some(b'-' | b'+' | b'>') => spaces_after(1),
        _ => 0,
    }
}

/// Where the line that has an END line at `at` starts, when nothing but spaces
/// and a line prefix stands in front of the END line on its line. `None` when
/// something else does (the END line is quoted in prose), and when the line starts
/// further back than a prefix can reach.
fn line_start_before(bytes: &[u8], at: usize) -> Option<usize> {
    let mut start = at;
    let floor = at.saturating_sub(LINE_PREFIX_MAX);
    while start > floor && break_before(bytes, start) == 0 {
        start -= 1;
    }
    if break_before(bytes, start) == 0 && start != 0 {
        return None;
    }
    let before = &bytes[start..at];
    let indent = before
        .iter()
        .take_while(|byte| matches!(**byte, b' ' | b'\t'))
        .count();
    let rest = &before[indent..];
    (rest.is_empty() || line_prefix_len(rest) == rest.len()).then_some(start)
}

/// One line split the way the key scanners read it.
struct LineParts {
    /// Where the line's content starts, after its indentation.
    content_start: usize,
    /// What follows its line prefix (a number or marker), without trailing spaces.
    text: Range<usize>,
}

/// Splits the line `bytes[start..stop]`.
fn line_parts(bytes: &[u8], start: usize, stop: usize) -> LineParts {
    let mut content_start = start;
    while content_start < stop && matches!(bytes[content_start], b' ' | b'\t') {
        content_start += 1;
    }
    let text_start = content_start + line_prefix_len(&bytes[content_start..stop]);
    let mut text_end = stop;
    while text_end > text_start && matches!(bytes[text_end - 1], b' ' | b'\t') {
        text_end -= 1;
    }
    LineParts {
        content_start,
        text: text_start..text_end,
    }
}

/// A quote that can wrap key text: a JSON or YAML string, a shell word.
fn is_quote(byte: u8) -> bool {
    matches!(byte, b'"' | b'\'' | b'`')
}

/// How much of the start of `line` is base64 text that a quote follows: the last
/// line of a key inside a quoted string (`...QXJl="`). 0 when there is none.
fn key_text_before_quote(line: &[u8]) -> usize {
    let length = line.iter().take_while(|byte| is_key_byte(**byte)).count();
    let quoted = line.get(length).is_some_and(|byte| is_quote(*byte));
    if quoted && line[..length].iter().any(u8::is_ascii_alphanumeric) {
        length
    } else {
        0
    }
}

/// How much of the end of `line` is base64 text that a quote precedes: the first
/// line of a key inside a quoted string (`"out": "MIIEvQ...`). It must be as long
/// as a body line, so a short quoted word never counts. 0 when there is none.
fn key_text_after_quote(line: &[u8]) -> usize {
    let length = line
        .iter()
        .rev()
        .take_while(|byte| is_key_byte(**byte))
        .count();
    let quoted = length < line.len() && is_quote(line[line.len() - length - 1]);
    if quoted && length >= KEY_LINE_MIN {
        length
    } else {
        0
    }
}

/// An armor or legacy-encryption header line: `Name: value`.
fn is_preamble_line(line: &[u8]) -> bool {
    if line.len() > KEY_PREAMBLE_LINE_BYTES {
        return false;
    }
    match line.iter().position(|byte| *byte == b':') {
        Some(colon) if colon > 0 && colon <= 40 => {
            line[0].is_ascii_alphabetic()
                && line[..colon]
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
                && line.get(colon + 1) == Some(&b' ')
        }
        _ => false,
    }
}

/// `at` moved forward over spaces and tabs.
fn skip_blanks(bytes: &[u8], mut at: usize) -> usize {
    while at < bytes.len() && matches!(bytes[at], b' ' | b'\t') {
        at += 1;
    }
    at
}

/// Where the line whose text starts at `from` ends, when that is within `limit`
/// bytes; `None` when the line is longer.
fn line_end_within(bytes: &[u8], from: usize, limit: usize) -> Option<usize> {
    let mut at = from;
    while at < bytes.len() && break_at(bytes, at) == 0 {
        if at - from >= limit {
            return None;
        }
        at += 1;
    }
    Some(at)
}

/// Where the key material that follows a header ending at `from` ends: the end
/// of the last line of the base64 run after it, which is the end of the text
/// when the run reaches it, and `from` itself when no key material follows.
///
/// Each line is read only as far as it can be key text (its prefix, then the
/// base64 characters), never to its end, so that the work is linear in the text
/// however many headers it holds.
fn key_run_end(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut end = from;
    // The first line is the rest of the header's own line.
    let mut line_start = from;
    let mut body_lines = 0usize;
    let mut preamble = 0usize;
    loop {
        let content_start = skip_blanks(bytes, line_start);
        let text_start = content_start + line_prefix_len(&bytes[content_start..]);
        let run = bytes[text_start..]
            .iter()
            .take_while(|byte| is_key_byte(**byte))
            .count();
        let run_end = text_start + run;
        let after = skip_blanks(bytes, run_end);
        let to_line_end = after == bytes.len() || break_at(bytes, after) > 0;
        let is_key = to_line_end && is_key_line(&bytes[text_start..run_end]);
        let quoted = key_text_before_quote(&bytes[text_start..]);
        if is_key && run >= KEY_LINE_MIN {
            body_lines += 1;
            end = run_end;
        } else if is_key && body_lines > 0 {
            // The short last line of a body, or an OpenPGP checksum line.
            end = run_end;
            break;
        } else if quoted >= KEY_LINE_MIN || (quoted > 0 && body_lines > 0) {
            // The last line of a key that sits in a quoted string: the run ends
            // where the string does.
            end = text_start + quoted;
            break;
        } else if body_lines == 0 && preamble < KEY_PREAMBLE_LINES {
            // Armor headers and blank lines may come before the body; they are
            // part of the run only if a body follows.
            let Some(stop) = line_end_within(bytes, text_start, KEY_PREAMBLE_LINE_BYTES) else {
                break;
            };
            let mut text_end = stop;
            while text_end > text_start && matches!(bytes[text_end - 1], b' ' | b'\t') {
                text_end -= 1;
            }
            let line = &bytes[text_start..text_end];
            if stop >= bytes.len() || !(line.is_empty() || is_preamble_line(line)) {
                break;
            }
            preamble += 1;
            let next = break_at(bytes, stop);
            line_start = stop + next;
            continue;
        } else {
            break;
        }
        let next = break_at(bytes, after);
        if next == 0 {
            break;
        }
        line_start = after + next;
    }
    end
}

/// Where the key material before an END line that starts at `footer` begins, when
/// the END line starts its line and key text precedes it.
fn key_run_start(text: &str, footer: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    // The END line must start its line, after at most a line number or a marker.
    let line_begin = line_start_before(bytes, footer)?;
    let mut cursor = line_begin.checked_sub(break_before(bytes, line_begin))?;
    if cursor == line_begin {
        // The text starts with the END line: no key text before it.
        return None;
    }
    let mut start = None;
    loop {
        // The line that ends at `cursor`.
        let mut begin = cursor;
        while begin > 0 && break_before(bytes, begin) == 0 {
            begin -= 1;
        }
        let parts = line_parts(bytes, begin, cursor);
        let text = &bytes[parts.text.clone()];
        // The line next to the END line may be short (the last of the body), and
        // so may the first line of a text that was cut from the middle of a body.
        let accepted =
            is_key_line(text) && (text.len() >= KEY_LINE_MIN || start.is_none() || begin == 0);
        if !accepted {
            // The first line of a key that sits in a quoted string starts after
            // the quote, so only the text after it is key material.
            let quoted = key_text_after_quote(text);
            if quoted > 0 {
                start = Some(parts.text.end - quoted);
            }
            break;
        }
        start = Some(parts.content_start);
        if begin == 0 {
            break;
        }
        cursor = begin - break_before(bytes, begin);
    }
    start
}

/// The regions of `text` that hold key material next to a lone header or END line.
fn orphan_pem_regions(text: &str) -> Vec<Range<usize>> {
    let mut regions: Vec<Range<usize>> = Vec::new();
    for header in pem_header_regex().find_iter(text) {
        let end = key_run_end(text, header.end());
        if end > header.end() {
            regions.push(header.start()..end);
        }
    }
    for footer in pem_footer_regex().find_iter(text) {
        if let Some(start) = key_run_start(text, footer.start()) {
            regions.push(start..footer.end());
        }
    }
    regions.sort_by_key(|region| region.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(regions.len());
    for region in regions {
        match merged.last_mut() {
            Some(last) if region.start <= last.end => last.end = last.end.max(region.end),
            _ => merged.push(region),
        }
    }
    merged
}

/// Whether `text` holds key material next to a private-key header or END line
/// that has no partner, the case [`scrub`] redacts. A complete block does not
/// count, and neither does a header or END line with no key material beside it.
pub fn has_unterminated_private_key_block(text: &str) -> bool {
    let complete_blocks = builtin_detectors()
        .iter()
        .find(|(kind, _)| *kind == "pem")
        .map(|(_, regex)| regex);
    match complete_blocks {
        Some(regex) => !orphan_pem_regions(&regex.replace_all(text, "")).is_empty(),
        None => !orphan_pem_regions(text).is_empty(),
    }
}

/// Redacts the key material next to a lone header or END line, with the line.
/// Complete blocks were replaced before this runs. Like every detector, it takes
/// at most [`MAX_MATCHES`] of them.
fn redact_orphan_pem(text: String) -> Result<String> {
    let regions = orphan_pem_regions(&text);
    if regions.is_empty() {
        return Ok(text);
    }
    if regions.len() > MAX_MATCHES {
        return Err(PrivacyError::Policy("too many secret matches".into()));
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for region in regions {
        out.push_str(&text[at..region.start]);
        out.push_str(&marker("pem"));
        at = region.end;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// Bounded high-confidence replacement. Existing markers are deliberately preserved.
pub fn scrub(input: &str) -> Result<String> {
    if input.len() > MAX_TEXT {
        return Err(PrivacyError::TooLarge);
    }
    let mut protected = Vec::new();
    let staged = redaction_marker_regex()
        .replace_all(input, |m: &regex::Captures<'_>| {
            protected.push(m[0].to_owned());
            format!("\u{0}CVC{}\u{0}", protected.len() - 1)
        })
        .into_owned();
    let mut out = staged;
    for (kind, regex) in builtin_detectors() {
        let count = regex.find_iter(&out).count();
        if count > MAX_MATCHES {
            return Err(PrivacyError::Policy("too many secret matches".into()));
        }
        out = regex
            .replace_all(&out, |_: &regex::Captures<'_>| marker(kind))
            .into_owned();
        if *kind == "pem" {
            out = redact_orphan_pem(out)?;
        }
    }
    // Every placeholder back to its marker in one pass: a text can hold as many
    // markers as fit in it, and one pass for each would be quadratic.
    Ok(placeholder_regex()
        .replace_all(&out, |found: &regex::Captures<'_>| {
            found[1]
                .parse::<usize>()
                .ok()
                .and_then(|index| protected.get(index))
                .map_or_else(|| found[0].to_owned(), Clone::clone)
        })
        .into_owned())
}

/// A placeholder [`scrub`] puts where a redaction marker was while it runs.
fn placeholder_regex() -> &'static Regex {
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX.get_or_init(|| re(r"\x00CVC([0-9]+)\x00"))
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
    static REGEX: OnceLock<Regex> = OnceLock::new();
    REGEX
        .get_or_init(|| re(&format!("^{REDACTION_MARKER}$")))
        .is_match(value)
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
    ///
    /// A rule never matches inside a redaction marker: not one the built-in
    /// detectors inserted, nor one an earlier rule did. Each marker is set aside
    /// as a single placeholder character while the rules run, so a rule can still
    /// match across it (and redact it together with the text around it, as it
    /// redacts anything else in its way) but cannot rewrite a part of one. A rule
    /// that happens to match characters a marker is made of (`regex:[0-9a-f]{8}`
    /// matches inside the id of every marker) would otherwise redact the markers
    /// themselves, and scrubbing text again would not leave it as it was. Rules
    /// that match nothing but markers, or nothing at all, redact no text and are
    /// not counted.
    pub fn scrub(&self, input: &str) -> Result<String> {
        let scrubbed = scrub(input)?;
        if self.literals.is_empty() && self.regexes.is_empty() {
            return Ok(scrubbed);
        }
        let mut staged = Staged::new(
            &scrubbed,
            self.literals
                .iter()
                .map(String::as_str)
                .chain(self.regexes.iter().map(Regex::as_str)),
        )?;
        let mut matches = 0usize;
        for literal in &self.literals {
            let mut ranges = Vec::new();
            let mut at = 0;
            while let Some(found) = staged.text[at..].find(literal.as_str()) {
                matches += 1;
                if matches > MAX_MATCHES {
                    return Err(PrivacyError::Policy("too many policy matches".into()));
                }
                let start = at + found;
                at = start + literal.len();
                ranges.push(start..at);
            }
            staged.replace(&ranges, "policy_literal");
        }
        for regex in &self.regexes {
            let mut ranges = Vec::new();
            for found in regex.find_iter(&staged.text) {
                if !staged.holds_text(found.range()) {
                    continue;
                }
                matches += 1;
                if matches > MAX_MATCHES {
                    return Err(PrivacyError::Policy("too many policy matches".into()));
                }
                ranges.push(found.range());
            }
            staged.replace(&ranges, "policy_regex");
        }
        staged.finish()
    }
}

/// Text with its redaction markers set aside, for the policy rules to work on:
/// each marker is one placeholder character in `text`, and `markers` holds them
/// in the order their placeholders appear.
struct Staged {
    text: String,
    markers: Vec<String>,
    placeholder: char,
}

/// The private-use characters a placeholder is taken from.
const PLACEHOLDER_FIRST: u32 = 0xE000;
const PLACEHOLDER_LAST: u32 = 0xF8FF;

impl Staged {
    /// Sets the markers in `scrubbed` aside. The placeholder is a character that
    /// neither the text nor a rule (`sources`) contains.
    fn new<'a>(scrubbed: &str, sources: impl Iterator<Item = &'a str>) -> Result<Self> {
        let mut used = vec![false; (PLACEHOLDER_LAST - PLACEHOLDER_FIRST + 1) as usize];
        let mut note = |text: &str| {
            for found in text.chars() {
                let point = found as u32;
                if (PLACEHOLDER_FIRST..=PLACEHOLDER_LAST).contains(&point) {
                    used[(point - PLACEHOLDER_FIRST) as usize] = true;
                }
            }
        };
        note(scrubbed);
        sources.for_each(&mut note);
        let placeholder = used
            .iter()
            .position(|used| !used)
            .and_then(|index| char::from_u32(PLACEHOLDER_FIRST + index as u32))
            .ok_or_else(|| PrivacyError::Policy("no free placeholder character".into()))?;
        let mut text = String::with_capacity(scrubbed.len());
        let mut markers = Vec::new();
        let mut at = 0;
        for span in redaction_marker_spans(scrubbed) {
            text.push_str(&scrubbed[at..span.start]);
            text.push(placeholder);
            markers.push(scrubbed[span.clone()].to_owned());
            at = span.end;
        }
        text.push_str(&scrubbed[at..]);
        Ok(Self {
            text,
            markers,
            placeholder,
        })
    }

    /// Whether `range` of the text holds anything but markers.
    fn holds_text(&self, range: Range<usize>) -> bool {
        self.text[range].chars().any(|c| c != self.placeholder)
    }

    /// Replaces each of `ranges` (ascending, apart) with a new marker of `kind`.
    /// The markers inside a range go with it.
    fn replace(&mut self, ranges: &[Range<usize>], kind: &str) {
        if ranges.is_empty() {
            return;
        }
        let mut old = std::mem::take(&mut self.markers).into_iter();
        let mut text = String::with_capacity(self.text.len());
        let mut markers = Vec::new();
        let mut carry = |segment: &str, markers: &mut Vec<String>, keep: bool| {
            for c in segment.chars() {
                if c == self.placeholder {
                    let marker = old.next();
                    if keep {
                        markers.extend(marker);
                    }
                }
            }
        };
        let mut at = 0;
        for range in ranges {
            let before = &self.text[at..range.start];
            text.push_str(before);
            carry(before, &mut markers, true);
            carry(&self.text[range.clone()], &mut markers, false);
            text.push(self.placeholder);
            markers.push(marker(kind));
            at = range.end;
        }
        let rest = &self.text[at..];
        text.push_str(rest);
        carry(rest, &mut markers, true);
        self.text = text;
        self.markers = markers;
    }

    /// The text with every marker back in place of its placeholder.
    fn finish(self) -> Result<String> {
        let mut markers = self.markers.into_iter();
        let mut out = String::with_capacity(self.text.len());
        for c in self.text.chars() {
            if c == self.placeholder {
                out.push_str(
                    &markers
                        .next()
                        .ok_or_else(|| PrivacyError::Policy("marker staging failed".into()))?,
                );
            } else {
                out.push(c);
            }
        }
        Ok(out)
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
    /// Scrubs `input` with the built-in detectors and this snapshot's rules,
    /// the way every capture field is scrubbed.
    pub fn scrub(&self, input: &str) -> Result<String> {
        self.0.scrub(input)
    }
    /// The same for a JSON document such as tool arguments: every string and
    /// key is scrubbed, and the values under a credential key are replaced.
    pub fn scrub_json(&self, input: &str) -> Result<String> {
        scrub_json_with_policy(input, &self.0)
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
    /// Worktree-origin fingerprint scoping automatic link eligibility.  Every
    /// production capture happens in a discovered worktree, so the typed
    /// constructors require it; only sync imports store no origin.
    pub(crate) capture_worktree: Option<String>,
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
                capture_worktree: String,
            ) -> Self {
                Self(Capture {
                    conversation,
                    interaction,
                    context_items,
                    tool_executions,
                    source: $source,
                    policy,
                    capture_worktree: Some(capture_worktree),
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
capture_type!(ClaudeCodeCapture, CaptureSource::ClaudeCode);

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
        // Imported nodes were not captured by any local worktree. Without an
        // origin they remain broadly eligible, exactly like legacy rows.
        capture_worktree: None,
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
    // A harness stimulus is scrubbed part by part (see `stimulus`); any other
    // text, including a stimulus whose boundary does not verify, as one string.
    capture.interaction.user_prompt = stimulus::scrub_stimulus(
        &capture.interaction.id,
        &capture.interaction.user_prompt,
        &|text| policy.scrub(text),
    )?;
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

// ---- a capture that cannot be prepared ----------------------------------------------
//
// `prepare` refuses a whole capture when one of its fields cannot be scrubbed. For
// a transcript that must not mean the session is never recorded: the ingest's
// cursor only moves when the whole batch is stored, so one response that cannot be
// prepared would hold every response after it back, on every later run. A
// harness capture that cannot be prepared is stored as a stand-in instead.

/// What a response's text is replaced by in a stand-in.
pub(crate) const WITHHELD_RESPONSE_NOTE: &str =
    "[cvc: withheld this response: it could not be scrubbed within the limits]";

/// What the capture boundary made of one harness capture.
pub(crate) enum HarnessCapture {
    /// Prepared as it was.
    Prepared(Capture),
    /// Could not be prepared. `stand_in` is stored instead, and `reason` says why.
    Withheld { stand_in: Capture, reason: String },
    /// Could not be prepared, and neither could a stand-in: nothing is stored.
    /// `id` and `parent_id` are what the capture was, so that the chain can skip it.
    Lost {
        id: InteractionId,
        parent_id: Option<InteractionId>,
        reason: String,
    },
}

/// Why `prepare` refused a capture, in words that are safe to print and store:
/// they name the limit that was hit and quote nothing.
fn refusal_reason(error: &PrivacyError) -> String {
    match error {
        PrivacyError::TooLarge => "a value exceeds the scrubber's size limit".into(),
        PrivacyError::Policy(reason) => reason.clone(),
    }
}

impl Capture {
    /// What stands in for this capture when it cannot be prepared: the same
    /// interaction (its id and place in the conversation, its time, author and
    /// stimulus), no response, and no tool executions or context items; the note
    /// that stands for the response is put in once it has been prepared.
    /// `withhold_stimulus` replaces the stimulus too.
    fn stand_in(&self, withhold_stimulus: bool) -> Capture {
        let held = &self.interaction;
        Capture {
            conversation: self.conversation.clone(),
            interaction: Interaction {
                id: held.id.clone(),
                conversation_id: held.conversation_id.clone(),
                parent_id: held.parent_id.clone(),
                timestamp: held.timestamp,
                author: held.author.clone(),
                user_prompt: if withhold_stimulus {
                    stimulus::withheld_stimulus(&held.id)
                } else {
                    held.user_prompt.clone()
                },
                model_name: None,
                model_cot: None,
                // The note goes in after the boundary has run: it is fixed text
                // of ours, which no policy should be able to refuse or rewrite.
                model_response: None,
                // An identifier that is not safe is what `prepare` refuses, and
                // nothing depends on this one here.
                source_request_id: held
                    .source_request_id
                    .clone()
                    .filter(|id| is_safe_identifier(id)),
            },
            context_items: Vec::new(),
            tool_executions: Vec::new(),
            source: self.source,
            policy: self.policy.clone(),
            capture_worktree: self.capture_worktree.clone(),
        }
    }
}

/// [`prepare`] for a capture an agent-harness adapter built from a transcript.
/// One that cannot be prepared is stored as a stand-in, so that it cannot keep
/// the responses after it from being recorded: first one that keeps the stimulus
/// and says where the response was withheld, then, if even that cannot be
/// prepared, one with a withheld stimulus. A capture whose stand-ins cannot be
/// prepared either (its conversation id is not a safe identifier, or the policy
/// refuses a title) is lost, and the caller says so.
pub(crate) fn prepare_harness(capture: Capture) -> HarnessCapture {
    let stand_ins = [capture.stand_in(false), capture.stand_in(true)];
    let (id, parent_id) = (
        capture.interaction.id.clone(),
        capture.interaction.parent_id.clone(),
    );
    let error = match prepare(capture) {
        Ok(prepared) => return HarnessCapture::Prepared(prepared),
        Err(error) => error,
    };
    for stand_in in stand_ins {
        if let Ok(mut prepared) = prepare(stand_in) {
            prepared.interaction.model_response = Some(WITHHELD_RESPONSE_NOTE.to_owned());
            return HarnessCapture::Withheld {
                stand_in: prepared,
                reason: refusal_reason(&error),
            };
        }
    }
    HarnessCapture::Lost {
        id,
        parent_id,
        reason: refusal_reason(&error),
    }
}

pub(crate) fn is_safe_identifier(value: &str) -> bool {
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
    interaction.user_prompt =
        stimulus::scrub_stimulus(&interaction.id, &interaction.user_prompt, &scrub)?;
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
