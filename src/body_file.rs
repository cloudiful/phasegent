//! One-shot `--body-file` input for the provider write commands
//! (`issue create`, `issue update-body`, `comment create`).
//!
//! Contract (issue 298): the file is read and validated here — regular
//! file, bounded size, valid UTF-8 — before any provider resolution or
//! network access. After a successful provider write the file is
//! deleted unless `--keep-body-file` was supplied; every read,
//! validation, or provider failure keeps the file. The pre-delete
//! re-check compares file identity and content, so a path that was
//! replaced or modified after the read is never destroyed.

use crate::command::{CommentCommand, IssueCommand, has_flag, optional_option};
use std::fs;
use std::path::PathBuf;
use std::time::SystemTime;

/// Conservative one-shot body cap: 2 MiB. Far above any plan or audit
/// note and bounded so one invocation cannot read an unbounded file.
pub(crate) const MAX_BODY_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Upper bound for cleanup warning text, matching the bounded-warning
/// convention of the local lifecycle helpers.
const MAX_CLEANUP_WARNING_CHARS: usize = 300;

/// Identity signals captured right after the read so the pre-delete
/// re-check can prove the on-disk file is still the one that was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
}

impl FileIdentity {
    fn of(metadata: &fs::Metadata) -> Self {
        Self {
            len: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            dev: std::os::unix::fs::MetadataExt::dev(metadata),
            #[cfg(unix)]
            ino: std::os::unix::fs::MetadataExt::ino(metadata),
        }
    }
}

/// A validated body file awaiting provider success.
#[derive(Debug, Clone)]
pub(crate) struct BodyFile {
    path: PathBuf,
    identity: FileIdentity,
    body: String,
    keep: bool,
}

impl BodyFile {
    /// Read and validate `raw_path` (regular file, size cap, UTF-8) and
    /// capture the post-read identity. Any failure keeps the file and is
    /// rendered as a structured argument error by the caller.
    pub(crate) fn load(raw_path: &str, keep: bool) -> Result<Self, String> {
        let trimmed = raw_path.trim();
        if trimmed.is_empty() {
            return Err("--body-file requires a non-empty path".to_owned());
        }
        let path = PathBuf::from(trimmed);
        let metadata = fs::metadata(&path).map_err(|_| {
            format!(
                "--body-file path not found or not accessible: {}",
                path.display()
            )
        })?;
        if !metadata.is_file() {
            return Err(format!(
                "--body-file path is not a regular file: {}",
                path.display()
            ));
        }
        let size = metadata.len();
        if size > MAX_BODY_FILE_BYTES {
            return Err(format!(
                "--body-file too large: {size} bytes exceeds the {} byte cap",
                MAX_BODY_FILE_BYTES
            ));
        }
        let bytes = fs::read(&path).map_err(|error| {
            format!(
                "--body-file could not be read: {error}; path: {}",
                path.display()
            )
        })?;
        if bytes.len() as u64 > MAX_BODY_FILE_BYTES {
            return Err(format!(
                "--body-file too large: {} bytes exceeds the {} byte cap",
                bytes.len(),
                MAX_BODY_FILE_BYTES
            ));
        }
        let body = String::from_utf8(bytes).map_err(|error| {
            format!(
                "--body-file is not valid UTF-8 (valid up to byte {}); path: {}",
                error.utf8_error().valid_up_to(),
                path.display()
            )
        })?;
        let identity = fs::metadata(&path)
            .map(|metadata| FileIdentity::of(&metadata))
            .map_err(|error| {
                format!(
                    "--body-file could not be re-stat'ed after read: {error}; path: {}",
                    path.display()
                )
            })?;
        Ok(Self {
            path,
            identity,
            body,
            keep,
        })
    }

    /// Validated UTF-8 content read at load time; this is exactly the
    /// text handed to the provider.
    pub(crate) fn body(&self) -> &str {
        &self.body
    }

    /// Delete after a successful provider write unless `--keep-body-file`
    /// was set. Returns a bounded warning when the file was intentionally
    /// kept: the identity or content re-check failed, or the unlink
    /// itself failed. A file that is already gone is a no-op.
    pub(crate) fn cleanup_after_success(&self) -> Option<String> {
        if self.keep {
            return None;
        }
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(_) => return None,
        };
        if !metadata.is_file() || FileIdentity::of(&metadata) != self.identity {
            return Some(bound_warning(format!(
                "--body-file changed after read; kept for diagnosis: {}",
                self.path.display()
            )));
        }
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
            Err(error) => {
                return Some(bound_warning(format!(
                    "--body-file could not be re-verified after success (kept): {error}; path: {}",
                    self.path.display()
                )));
            }
        };
        if bytes != self.body.as_bytes() {
            return Some(bound_warning(format!(
                "--body-file changed after read; kept for diagnosis: {}",
                self.path.display()
            )));
        }
        match fs::remove_file(&self.path) {
            Ok(()) => None,
            Err(error) => Some(bound_warning(format!(
                "--body-file could not be deleted after success (kept): {error}; path: {}",
                self.path.display()
            ))),
        }
    }
}

/// A resolved body plus the cleanup handle that owns a `--body-file`.
pub(crate) type ResolvedBody = (String, Option<BodyFile>);

/// Per-command body resolution: `None` when the command takes no body;
/// otherwise the resolved body or a `(operation, message)` argument
/// error.
pub(crate) type BodyResolution = Result<Option<ResolvedBody>, (&'static str, String)>;

/// Resolve the legacy `--body` value against the one-shot `--body-file`
/// value. Returns the final body text plus an optional cleanup handle.
/// Purely local: no provider config, network, or credential access.
pub(crate) fn resolve(
    body: Option<&str>,
    body_file: Option<&str>,
    keep_body_file: bool,
) -> Result<Option<ResolvedBody>, String> {
    match body_file {
        Some(path) => {
            let file = BodyFile::load(path, keep_body_file)?;
            Ok(Some((file.body().to_owned(), Some(file))))
        }
        None => Ok(body.map(|body| (body.to_owned(), None))),
    }
}

/// Shared `--body` / `--body-file` pairing rules for the three write
/// commands: the flags are mutually exclusive, `--keep-body-file`
/// requires `--body-file`, and `required` selects whether at least one
/// of the two body flags must be present.
pub(crate) fn parse_body_flags(
    args: &[String],
    operation: &str,
    required: bool,
) -> Result<(String, Option<String>, bool), String> {
    let body = optional_option(args, "--body");
    let body_file = optional_option(args, "--body-file");
    if body.is_some() && body_file.is_some() {
        return Err("--body and --body-file are mutually exclusive".to_owned());
    }
    let keep_body_file = has_flag(args, "--keep-body-file");
    if keep_body_file && body_file.is_none() {
        return Err("--keep-body-file requires --body-file".to_owned());
    }
    if required && body.is_none() && body_file.is_none() {
        return Err(format!("{operation} requires --body or --body-file"));
    }
    Ok((body.unwrap_or_default(), body_file, keep_body_file))
}

/// Extract the body inputs of an issue write command. `None` for every
/// command that takes no body.
pub(crate) fn resolve_for_issue(command: &IssueCommand) -> Option<BodyResolution> {
    let (operation, body, body_file, keep) = match command {
        IssueCommand::Create {
            body,
            body_file,
            keep_body_file,
            ..
        } => (
            "issue create",
            Some(body.as_str()),
            body_file.as_deref(),
            *keep_body_file,
        ),
        IssueCommand::UpdateBody {
            body,
            body_file,
            keep_body_file,
            ..
        } => (
            "issue update-body",
            Some(body.as_str()),
            body_file.as_deref(),
            *keep_body_file,
        ),
        _ => return None,
    };
    Some(resolve(body, body_file, keep).map_err(|error| (operation, error)))
}

/// Extract the body inputs of a comment write command.
pub(crate) fn resolve_for_comment(command: &CommentCommand) -> Option<BodyResolution> {
    let (body, body_file, keep) = match command {
        CommentCommand::Create {
            body,
            body_file,
            keep_body_file,
            ..
        } => (Some(body.as_str()), body_file.as_deref(), *keep_body_file),
        _ => return None,
    };
    Some(resolve(body, body_file, keep).map_err(|error| ("comment create", error)))
}

fn bound_warning(text: String) -> String {
    text.chars().take(MAX_CLEANUP_WARNING_CHARS).collect()
}
