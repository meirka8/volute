//! Fail-closed capture sanitization and repository-local capture policy.
use crate::models::{
    ArtifactLink, CaptureSource, ContextItem, Conversation, Interaction, ToolExecution,
};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::path::{Component, Path};
use std::sync::OnceLock;
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
    /// The checkout and user the capture acknowledgement was given for: the
    /// [`capture_binding`] of the common Git directory and the user that
    /// completed `cvc privacy acknowledge-capture`. Absent from files written
    /// by cvc 0.7.0 and earlier, which recorded the notice version alone.
    #[serde(default)]
    capture_acknowledged_binding: Option<String>,
    sharing: Vec<SharingConsent>,
}
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
struct SharingConsent {
    fingerprint: String,
    notice_version: u32,
    auto_push: bool,
}
/// Where the repository-local capture acknowledgement stands for the checkout
/// and user asking. Only [`CaptureAcknowledgement::Acknowledged`] lets passive
/// capture run; the other two differ only in what to tell the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CaptureAcknowledgement {
    /// The current notice was acknowledged in this checkout (this common Git
    /// directory, at this path) by this user.
    Acknowledged,
    /// The current notice was never acknowledged in this repository.
    #[default]
    NotAcknowledged,
    /// An acknowledgement of the current notice is on record, but not for this
    /// checkout and user: it was given at another path or by another user (the
    /// repository was copied, moved or restored together with its `.git`), it
    /// was recorded by cvc 0.7.0 or earlier without saying for whom, or the
    /// current user cannot be identified. It grants nothing here; acknowledging
    /// again here replaces it.
    OtherCheckoutOrUser,
}
impl CaptureAcknowledgement {
    pub fn is_acknowledged(self) -> bool {
        self == Self::Acknowledged
    }
}
#[derive(Debug, Clone, Default)]
pub struct PrivacyStatus {
    /// `capture_acknowledgement.is_acknowledged()`.
    pub capture_acknowledged: bool,
    pub capture_acknowledgement: CaptureAcknowledgement,
    pub sharing_consented: bool,
    pub auto_push: bool,
    pub remote_fingerprint: String,
}

/// An immutable description of the destination selected for one operation.  Do not
/// re-read the git config after this is created: `pushurl` is deliberately allowed
/// to differ from the fetch URL.
///
/// `effective_url` may embed credentials. `Debug` therefore leaves it out, and
/// anything shown to a person uses [`display_location`] instead.
#[derive(Clone)]
pub struct RemoteDestination {
    pub name: String,
    pub effective_url: String,
    pub fingerprint: String,
    pub ref_name: String,
}

impl std::fmt::Debug for RemoteDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteDestination")
            .field("name", &self.name)
            .field("effective_url", &"<redacted>")
            .field("fingerprint", &self.fingerprint)
            .field("ref_name", &self.ref_name)
            .finish()
    }
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

/// Longest remote URL [`display_location`] will look at.
const MAX_LOCATION_INPUT: usize = 2048;

/// A form of a remote's effective push URL that is safe to put on a screen, or
/// `None` when no such form can be established. It is display-only: nothing may
/// be resolved, fetched or compared through it.
///
/// - `scheme://[userinfo@]host[:port]/path?query#fragment` becomes
///   `host[:port]/path`. The user and the password or token, the query and the
///   fragment are all dropped. It is shown only when the host shown is the host
///   git contacts (see below).
/// - scp-like `[user@]host:path` becomes `host:path`.
/// - a local path, or a `file://` URL, becomes the path.
/// - anything else yields `None`. That includes a string that does not parse; a
///   host that is not plain (a percent-encoded or non-ASCII one among them); a
///   `?` or `#` in a form that is not a URL; a transport-helper address
///   (`<transport>::<address>`, whose address is a command line, not a
///   location); any `@` left in what would be shown, which is where a credential
///   hides when the URL is mistyped (`https:/user:pw@host/r`, `user:pw/x@host:r`);
///   any control, format, line-separator or paragraph-separator character
///   (Unicode categories Cc, Cf, Zl and Zp), whether the string holds it or only
///   the part of it that is not shown; or a form in which the built-in secret
///   scrubber still finds a secret.
///
/// The host shown is checked against the one libgit2 contacts. The `url` crate
/// ends a URL's authority at `?`, `#` and `\`, but libgit2 ends it only at the
/// first `/` and takes the host after the *last* `@`, so
/// `https://github.com?@evil.example/o/r.git` parses as host `github.com` and is
/// contacted as host `evil.example`. A `\`, `?` or `#` in the authority, or a
/// host that does not read the same both ways, is therefore not shown.
///
/// The raw string is never returned as a fallback, and no error carries it.
/// This is leak prevention for what a terminal shows; it is not a promise that a
/// path component holds no sensitive text.
pub fn display_location(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty()
        || url.len() > MAX_LOCATION_INPUT
        || has_unsafe_display_char(url)
        || is_transport_helper(url)
    {
        return None;
    }
    let location = if url.contains("://") {
        url_location(url)?
    } else if is_windows_drive_path(url) {
        url.to_owned()
    } else if let Some((authority, path)) = split_scp_like(url) {
        scp_location(authority, path)?
    } else {
        url.to_owned()
    };
    // A parsed URL's query and fragment are already gone and its path is
    // percent-encoded, so a raw `?` or `#` can only come from a form that has
    // no query to strip (`host:path`, a local path). There it may well be one,
    // so nothing is shown.
    //
    // No `@` is shown either, whatever form it came out of. Every form in which
    // user information could survive to this point is one that is not
    // recognized as having any (a single slash after the scheme, a `/` inside
    // the user information of an scp-like remote, a drive-letter lookalike),
    // and git reads those as something else: `user:secret@host:path` is host
    // `user` and path `secret@host:path`.
    if location.is_empty()
        || location.contains(['?', '#', '@'])
        || has_unsafe_display_char(&location)
    {
        return None;
    }
    // Defense in depth: whatever the scrubber would redact is not shown at all.
    match scrub(&location) {
        Ok(scrubbed) if scrubbed == location => Some(location),
        _ => None,
    }
}

fn url_location(url: &str) -> Option<String> {
    // libgit2 reads a URL as `<scheme>://<authority>` where the scheme is the
    // unbroken run of scheme characters before the first `://` and the
    // authority ends at the first `/`. Whatever else the `url` crate would make
    // of the string is not what is contacted.
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty() || !scheme.bytes().all(is_scheme_byte) {
        return None;
    }
    // `\` is a path separator to the `url` crate in a URL with a special
    // scheme, and a character like any other to libgit2, so the path shown
    // would not be the path used. In the authority it also moves the end of the
    // host, as do `?` and `#`.
    let authority = rest.split('/').next().unwrap_or("");
    if url.contains('\\') || authority.contains(['?', '#']) {
        return None;
    }
    let parsed = url::Url::parse(url).ok()?;
    if parsed.scheme() == "file" {
        // A local file has no host to contact; `localhost` means the same.
        if !(authority.is_empty() || authority.eq_ignore_ascii_case("localhost")) {
            return None;
        }
        return parsed.to_file_path().ok()?.to_str().map(str::to_owned);
    }
    // The host as libgit2 takes it: after the last `@`, before the port.
    let host_port = authority.rsplit('@').next()?;
    let raw_host = match host_port.strip_prefix('[') {
        Some(_) => {
            let end = host_port.find(']')? + 1;
            let (host, rest) = host_port.split_at(end);
            if !(rest.is_empty() || rest.starts_with(':')) {
                return None;
            }
            host
        }
        None => host_port.split(':').next()?,
    };
    // Only a plain host can be told apart from its normalized form: a
    // percent-encoded, non-ASCII or numeric-looking one is not shown even when
    // it would turn out the same.
    let host = parsed.host_str().filter(|host| !host.is_empty())?;
    if !is_plain_host(raw_host) || !raw_host.eq_ignore_ascii_case(host) {
        return None;
    }
    let mut location = host.to_owned();
    if let Some(port) = parsed.port() {
        location.push(':');
        location.push_str(&port.to_string());
    }
    let path = parsed.path();
    if path != "/" {
        location.push_str(path);
    }
    Some(location)
}

/// Splits `[user@]host:path` the way git recognizes it: the first `:` outside a
/// bracketed IPv6 host, with no `/` before it.
fn split_scp_like(value: &str) -> Option<(&str, &str)> {
    let mut in_brackets = false;
    for (index, character) in value.char_indices() {
        match character {
            '[' => in_brackets = true,
            ']' => in_brackets = false,
            '/' | '\\' if !in_brackets => return None,
            ':' if !in_brackets => return Some((&value[..index], &value[index + 1..])),
            _ => {}
        }
    }
    None
}

fn scp_location(authority: &str, path: &str) -> Option<String> {
    // Only the host survives: everything up to the last `@` is user information.
    let host = authority.rsplit('@').next()?;
    if path.is_empty() || !is_plain_host(host) {
        return None;
    }
    Some(format!("{host}:{path}"))
}

fn is_plain_host(host: &str) -> bool {
    match host.strip_prefix('[') {
        Some(rest) => rest.strip_suffix(']').is_some_and(|inner| {
            !inner.is_empty()
                && inner
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() || matches!(c, ':' | '.'))
        }),
        None => {
            !host.is_empty()
                && host
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
        }
    }
}

fn is_windows_drive_path(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(
        (characters.next(), characters.next(), characters.next()),
        (Some(drive), Some(':'), Some('/' | '\\')) if drive.is_ascii_alphabetic()
    )
}

/// A character libgit2 and git accept in a URL scheme.
fn is_scheme_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.')
}

/// Whether git would hand the address to a remote helper program: a run of
/// scheme characters (the first of them a letter or a digit), then `::`. The
/// address after the `::` is the helper's own business (`ext::` runs it as a
/// command line), so nothing in it is a location.
fn is_transport_helper(url: &str) -> bool {
    let scheme_length = url
        .bytes()
        .enumerate()
        .take_while(|&(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && matches!(byte, b'+' | b'-' | b'.'))
        })
        .count();
    url[scheme_length..].starts_with("::")
}

/// Whether `text` holds a character that can make a terminal show something
/// other than what the text is, or nothing at all: any character of the Unicode
/// general categories `Cc` (controls, ESC among them), `Cf` (format characters:
/// the bidirectional controls and isolates, the Arabic letter mark, the soft
/// hyphen, zero-width characters, joiners, the byte order mark, the interlinear
/// annotation anchors, the tag characters, and more), `Zl` and `Zp` (the line
/// and paragraph separators). The categories come from the `regex` crate's
/// Unicode tables, not from a list of ranges that could fall behind them.
fn has_unsafe_display_char(text: &str) -> bool {
    static UNSAFE: OnceLock<Regex> = OnceLock::new();
    UNSAFE
        .get_or_init(|| re(r"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}]"))
        .is_match(text)
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
    let capture = capture_acknowledgement_in(repo, &state, current_user().as_deref());
    Ok(PrivacyStatus {
        capture_acknowledged: capture.is_acknowledged(),
        capture_acknowledgement: capture,
        sharing_consented: consent.is_some(),
        auto_push: consent.is_some_and(|x| x.auto_push),
        remote_fingerprint: fp.to_owned(),
    })
}

/// The first line of the [`capture_binding`] layout.
const CAPTURE_BINDING_DOMAIN: &[u8] = b"cvc.capture-acknowledgement/v1\n";

/// What a capture acknowledgement is bound to: the checkout, as the canonical
/// path of its common Git directory, and the user. It is lowercase hex of the
/// SHA-256 of
///
/// ```text
/// cvc.capture-acknowledgement/v1\n
/// path:<path>\n
/// user:<user>
/// ```
///
/// with no line feed after `<user>`. `<path>` is `std::fs::canonicalize` of
/// the common Git directory: its bytes on Unix, its UTF-16 code units
/// (little-endian) on Windows. `<user>` is [`current_user`]'s, and must be
/// non-empty and free of control characters, so the layout reads back one way
/// only, whatever the path holds: `user:<user>` follows the last line feed,
/// and the path is everything between `path:` and that line feed. Linked
/// worktrees share the common directory, and so one binding.
///
/// Nothing that changes across boots or mounts (host name, machine id, device
/// or inode numbers) is part of it, so the same checkout opened by the same
/// user keeps its binding across reboots and remounts. A copy, a move or a
/// restore of the repository at another path, or the repository opened by
/// another user, yields another. A copy at the same path, opened by a user
/// with the same identity, yields the same one. `None` when the directory
/// cannot be canonicalized or the user is unusable: no acknowledgement
/// matches that.
fn capture_binding(common_git_dir: &Path, user: &str) -> Option<String> {
    if user.is_empty() || user.chars().any(char::is_control) {
        return None;
    }
    let path = std::fs::canonicalize(common_git_dir).ok()?;
    let mut hash = Sha256::new();
    hash.update(CAPTURE_BINDING_DOMAIN);
    hash.update(b"path:");
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        hash.update(path.as_os_str().as_bytes());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        for unit in path.as_os_str().encode_wide() {
            hash.update(unit.to_le_bytes());
        }
    }
    #[cfg(not(any(unix, windows)))]
    hash.update(path.to_str()?.as_bytes());
    hash.update(b"\nuser:");
    hash.update(user.as_bytes());
    Some(hex::encode(hash.finalize()))
}

/// The user a capture acknowledgement is recorded for and checked against:
/// the real user id of this process, in decimal.
#[cfg(unix)]
fn current_user() -> Option<String> {
    // SAFETY: getuid(2) takes no arguments and cannot fail.
    Some(unsafe { libc::getuid() }.to_string())
}
/// The user a capture acknowledgement is recorded for and checked against:
/// the `USERNAME` of this process. Unset or not Unicode is no user, which no
/// acknowledgement matches.
#[cfg(windows)]
fn current_user() -> Option<String> {
    std::env::var("USERNAME").ok()
}
/// No user can be named here, so no capture acknowledgement matches.
#[cfg(not(any(unix, windows)))]
fn current_user() -> Option<String> {
    None
}

/// The binding `user` would have in `repo` (see [`capture_binding`]).
fn capture_binding_for(repo: &git2::Repository, user: &str) -> Option<String> {
    let common_git_dir = crate::repository::common_git_dir(repo).ok()?;
    capture_binding(&common_git_dir, user)
}

/// Judges the capture acknowledgement recorded in `state` for `user` in
/// `repo`. This is the one place that does: [`capture_acknowledged`],
/// [`capture_acknowledgement`] and [`privacy_status_for_fingerprint`] all
/// come here. Only the current notice version, recorded together with exactly
/// the binding this checkout and `user` have now, is
/// [`CaptureAcknowledgement::Acknowledged`]; a missing, different or
/// unreadable binding, and an unknown user, are not.
fn capture_acknowledgement_in(
    repo: &git2::Repository,
    state: &PrivacyFile,
    user: Option<&str>,
) -> CaptureAcknowledgement {
    if state.capture_acknowledged_version != Some(PRIVACY_NOTICE_VERSION) {
        return CaptureAcknowledgement::NotAcknowledged;
    }
    let current = user.and_then(|user| capture_binding_for(repo, user));
    match (state.capture_acknowledged_binding.as_deref(), current) {
        (Some(recorded), Some(current)) if recorded == current => {
            CaptureAcknowledgement::Acknowledged
        }
        _ => CaptureAcknowledgement::OtherCheckoutOrUser,
    }
}

/// Where the capture acknowledgement stands for this checkout and the user
/// running this process (see [`CaptureAcknowledgement`]).
pub fn capture_acknowledgement(repo: &git2::Repository) -> Result<CaptureAcknowledgement> {
    capture_acknowledgement_as(repo, current_user().as_deref())
}
fn capture_acknowledgement_as(
    repo: &git2::Repository,
    user: Option<&str>,
) -> Result<CaptureAcknowledgement> {
    let state = read_privacy(repo)?;
    Ok(capture_acknowledgement_in(repo, &state, user))
}
/// Passive collection is opt-in. Explicit local actions may still persist private
/// data, but background IDE observation must fail closed until this is true.
/// True only for an acknowledgement given in this checkout by this user; one
/// that came with a copy of the repository is not (see
/// [`capture_acknowledgement`] for which it is).
pub fn capture_acknowledged(repo: &git2::Repository) -> Result<bool> {
    Ok(capture_acknowledgement(repo)?.is_acknowledged())
}
/// Records the capture acknowledgement for this checkout and the user running
/// this process, replacing whatever acknowledgement was on record. Production
/// code calls this only once the typed challenge of
/// `cvc privacy acknowledge-capture` has been answered.
pub fn acknowledge_capture(repo: &git2::Repository) -> Result<()> {
    acknowledge_capture_as(repo, current_user().as_deref())
}
fn acknowledge_capture_as(repo: &git2::Repository, user: Option<&str>) -> Result<()> {
    let user = user.ok_or_else(|| {
        PrivacyError::Policy(
            "the current user cannot be identified, so capture cannot be acknowledged for them"
                .into(),
        )
    })?;
    let binding = capture_binding_for(repo, user).ok_or_else(|| {
        PrivacyError::Policy(
            "the capture acknowledgement cannot be bound to this checkout and user".into(),
        )
    })?;
    let _lock = privacy_lock(repo)?;
    let mut s = read_privacy(repo)?;
    s.capture_acknowledged_version = Some(PRIVACY_NOTICE_VERSION);
    s.capture_acknowledged_binding = Some(binding);
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

/// Whether `value` is an identifier the capture policy accepts: 1 to 256 bytes,
/// every one an ASCII letter, a digit, or one of `.`, `_`, `-` and `:`.
///
/// Conversation ids and source request ids are relational keys, so they are
/// never rewritten: a randomized redaction marker cannot stand in for one
/// without breaking what points at it. They are refused when they are not safe
/// instead. `prepare` does that at the one capture boundary every source goes
/// through (the MCP server, `cvc run`, the editor, a Claude Code transcript, and
/// a pull, before it namespaces an imported id with `remote:<fingerprint>:`), so
/// a stored id is always one of these. It also means an id cannot carry anything
/// that acts on a terminal: no control, format or separator character, and no
/// space.
///
/// Public so that a front that lets a client choose an id (the MCP server, for
/// `conversation_id`) refuses with this very function, before anything is
/// recorded, and not with a rule of its own that could disagree with the
/// capture's.
pub fn is_safe_identifier(value: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn repository() -> (TempDir, git2::Repository) {
        let temp = TempDir::new().unwrap();
        let repo = git2::Repository::init(temp.path()).unwrap();
        (temp, repo)
    }

    #[test]
    fn the_binding_is_the_sha256_of_the_documented_layout() {
        let (temp, repo) = repository();
        let common = crate::repository::common_git_dir(&repo).unwrap();
        let canonical = std::fs::canonicalize(&common).unwrap();
        let mut layout = b"cvc.capture-acknowledgement/v1\npath:".to_vec();
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            layout.extend_from_slice(canonical.as_os_str().as_bytes());
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            for unit in canonical.as_os_str().encode_wide() {
                layout.extend_from_slice(&unit.to_le_bytes());
            }
        }
        layout.extend_from_slice(b"\nuser:1000");
        let binding = capture_binding(&common, "1000").unwrap();
        assert_eq!(binding, hex::encode(Sha256::digest(&layout)));
        assert_eq!(binding.len(), 64);
        assert!(binding
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')));

        // The same directory spelled another way is the same checkout.
        let spelled = temp.path().join(".git").join("..").join(".git");
        assert_eq!(capture_binding(&spelled, "1000"), Some(binding.clone()));
        #[cfg(unix)]
        {
            let link = TempDir::new().unwrap();
            std::os::unix::fs::symlink(temp.path(), link.path().join("via")).unwrap();
            let through_link = link.path().join("via").join(".git");
            assert_eq!(
                capture_binding(&through_link, "1000"),
                Some(binding.clone())
            );
        }
        // Another user, or another directory, is another binding.
        assert_ne!(capture_binding(&common, "1001"), Some(binding.clone()));
        let elsewhere = TempDir::new().unwrap();
        assert_ne!(
            capture_binding(elsewhere.path(), "1000"),
            Some(binding.clone())
        );
        // A user that would make the layout ambiguous, or that is no user at
        // all, binds nothing, and neither does a directory that is not there.
        for user in ["", "10\n00", "1000\n", "1000\r", "\0", "\u{85}"] {
            assert_eq!(capture_binding(&common, user), None, "{user:?}");
        }
        assert_eq!(capture_binding(&temp.path().join("missing"), "1000"), None);
    }

    /// The identity is a parameter here, so another user of the same checkout
    /// can be tried without running as one.
    #[test]
    fn an_acknowledgement_counts_only_for_the_user_who_gave_it() {
        use CaptureAcknowledgement::{Acknowledged, NotAcknowledged, OtherCheckoutOrUser};
        let (_temp, repo) = repository();
        assert_eq!(
            capture_acknowledgement_as(&repo, Some("1000")).unwrap(),
            NotAcknowledged
        );
        acknowledge_capture_as(&repo, Some("1000")).unwrap();
        assert_eq!(
            capture_acknowledgement_as(&repo, Some("1000")).unwrap(),
            Acknowledged
        );
        // Another user at the same path: an image restored for another
        // account, a shared directory.
        assert_eq!(
            capture_acknowledgement_as(&repo, Some("1001")).unwrap(),
            OtherCheckoutOrUser
        );
        // A user who cannot be identified is no user it was given for.
        assert_eq!(
            capture_acknowledgement_as(&repo, None).unwrap(),
            OtherCheckoutOrUser
        );

        // Nor can such a user give one: the state on record stays as it was.
        let path = privacy_path(&repo).unwrap();
        let before = std::fs::read(&path).unwrap();
        for user in [None, Some(""), Some("10\n01")] {
            assert!(acknowledge_capture_as(&repo, user).is_err(), "{user:?}");
        }
        assert_eq!(std::fs::read(&path).unwrap(), before);

        // The other user acknowledging replaces the binding on record.
        acknowledge_capture_as(&repo, Some("1001")).unwrap();
        assert_eq!(
            capture_acknowledgement_as(&repo, Some("1001")).unwrap(),
            Acknowledged
        );
        assert_eq!(
            capture_acknowledgement_as(&repo, Some("1000")).unwrap(),
            OtherCheckoutOrUser
        );
    }

    /// The production wrappers name the process's own user, and the status
    /// report judges exactly as the gate does.
    #[test]
    fn what_this_process_acknowledges_is_for_its_own_user_only() {
        let (_temp, repo) = repository();
        let me = current_user().expect("the test process has a user");
        let someone_else = format!("{me}0");
        acknowledge_capture(&repo).unwrap();
        assert!(capture_acknowledged(&repo).unwrap());
        assert_eq!(
            capture_acknowledgement_as(&repo, Some(&me)).unwrap(),
            CaptureAcknowledgement::Acknowledged
        );
        assert_eq!(
            capture_acknowledgement_as(&repo, Some(&someone_else)).unwrap(),
            CaptureAcknowledgement::OtherCheckoutOrUser
        );
        let status = privacy_status_for_fingerprint(&repo, &"0".repeat(64)).unwrap();
        assert!(status.capture_acknowledged);
        assert_eq!(
            status.capture_acknowledgement,
            CaptureAcknowledgement::Acknowledged
        );

        acknowledge_capture_as(&repo, Some(&someone_else)).unwrap();
        assert!(!capture_acknowledged(&repo).unwrap());
        let status = privacy_status_for_fingerprint(&repo, &"0".repeat(64)).unwrap();
        assert!(!status.capture_acknowledged);
        assert_eq!(
            status.capture_acknowledgement,
            CaptureAcknowledgement::OtherCheckoutOrUser
        );
    }

    #[cfg(unix)]
    #[test]
    fn on_unix_the_user_is_the_real_uid() {
        // SAFETY: getuid(2) takes no arguments and cannot fail.
        let uid = unsafe { libc::getuid() };
        assert_eq!(current_user(), Some(uid.to_string()));
    }
}
