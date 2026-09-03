use crate::providers::api::ForgejoError;
use crate::providers::config::RedmineProvider;
use crate::providers::redmine::model::issue::{
    AttachmentUploadOutput, RedmineIssueUploadFields, RedmineIssueUploadUpdate, RedmineUploadEntry,
};
use std::path::Path;

/// Conservative attachment size cap: 25 MiB. Matches the documented
/// Redmine limit and keeps the raw `POST /uploads.json` bounded so a
/// single CLI invocation cannot exhaust memory or Redmine storage.
const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_FILENAME_BYTES: usize = 255;

impl RedmineProvider {
    /// Upload a local file as a Redmine issue attachment via the two-step
    /// protocol: raw `POST /uploads.json?filename=...` followed by
    /// `PUT /issues/<id>.json` with `{"issue":{"uploads":[...]}}`.
    ///
    /// Validates the local file before any network access and never exposes
    /// file contents or the transient upload token in output or errors.
    pub fn upload_attachment(
        &self,
        issue: u64,
        path: &str,
        description: Option<&str>,
    ) -> Result<AttachmentUploadOutput, ForgejoError> {
        const OPERATION: &str = "issue upload-attachment";
        if issue == 0 {
            return Err(ForgejoError::request(
                OPERATION,
                "issue number must be greater than zero".to_owned(),
            ));
        }
        let trimmed_path = path.trim();
        if trimmed_path.is_empty() {
            return Err(ForgejoError::request(
                OPERATION,
                "attachment --path requires a non-empty value".to_owned(),
            ));
        }
        let file_path = Path::new(trimmed_path);
        let metadata = std::fs::metadata(file_path).map_err(|_| {
            ForgejoError::request(
                OPERATION,
                "attachment file not found or not accessible".to_owned(),
            )
        })?;
        if !metadata.is_file() {
            return Err(ForgejoError::request(
                OPERATION,
                "attachment path is not a regular file".to_owned(),
            ));
        }
        let size = metadata.len();
        if size == 0 {
            return Err(ForgejoError::request(
                OPERATION,
                "attachment file is empty".to_owned(),
            ));
        }
        if size > MAX_ATTACHMENT_BYTES {
            return Err(ForgejoError::request(
                OPERATION,
                format!(
                    "attachment file too large: {size} bytes exceeds 25 MiB cap ({} bytes)",
                    MAX_ATTACHMENT_BYTES
                ),
            ));
        }
        let filename_os = file_path.file_name().ok_or_else(|| {
            ForgejoError::request(OPERATION, "attachment filename is invalid".to_owned())
        })?;
        let filename = filename_os.to_str().ok_or_else(|| {
            ForgejoError::request(
                OPERATION,
                "attachment filename is not valid UTF-8".to_owned(),
            )
        })?;
        validate_filename(filename, OPERATION)?;
        // Read after metadata checks so an oversized file never reaches memory.
        let bytes = std::fs::read(file_path).map_err(|_| {
            ForgejoError::request(OPERATION, "failed to read attachment file".to_owned())
        })?;
        if bytes.is_empty() {
            return Err(ForgejoError::request(
                OPERATION,
                "attachment file is empty".to_owned(),
            ));
        }
        if bytes.len() as u64 > MAX_ATTACHMENT_BYTES {
            return Err(ForgejoError::request(
                OPERATION,
                format!(
                    "attachment file too large: {} bytes exceeds 25 MiB cap ({} bytes)",
                    bytes.len(),
                    MAX_ATTACHMENT_BYTES
                ),
            ));
        }
        // Step 1: raw upload.
        let token = self.http.post_upload(filename, &bytes, OPERATION)?;
        // Step 2: attach token to issue.
        let notes = description
            .map(|value| value.trim())
            .filter(|value| !value.is_empty());
        let payload = RedmineIssueUploadUpdate {
            issue: RedmineIssueUploadFields {
                notes,
                uploads: vec![RedmineUploadEntry {
                    token: &token,
                    filename,
                }],
            },
        };
        let path = format!("issues/{issue}.json");
        // Success is 2xx or 204 (empty body). Body, if present, is an issue
        // response; we do not need to decode it for attachment success.
        let _: Option<serde_json::Value> = self.http.put(&path, &payload, OPERATION)?;
        Ok(AttachmentUploadOutput {
            issue,
            filename: filename.to_owned(),
            bytes: bytes.len(),
            success: true,
        })
    }
}

fn validate_filename(filename: &str, operation: &str) -> Result<(), ForgejoError> {
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        return Err(ForgejoError::request(
            operation,
            "attachment filename is invalid".to_owned(),
        ));
    }
    if trimmed == "." || trimmed == ".." {
        return Err(ForgejoError::request(
            operation,
            "attachment filename is invalid".to_owned(),
        ));
    }
    if trimmed.len() > MAX_FILENAME_BYTES {
        return Err(ForgejoError::request(
            operation,
            "attachment filename too long".to_owned(),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(ForgejoError::request(
            operation,
            "attachment filename contains invalid characters".to_owned(),
        ));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(ForgejoError::request(
            operation,
            "attachment filename contains invalid characters".to_owned(),
        ));
    }
    // Reject NUL and other non-printable implicitly via control check.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_filename;

    #[test]
    fn filename_validation_rejects_invalid() {
        for bad in ["", " ", ".", "..", "a/b.txt", "a\\b.txt", "bad\x00name"] {
            assert!(
                validate_filename(bad, "issue upload-attachment").is_err(),
                "should reject {bad:?}"
            );
        }
        let long = "a".repeat(super::MAX_FILENAME_BYTES + 1);
        assert!(validate_filename(&long, "issue upload-attachment").is_err());
        assert!(validate_filename("valid.png", "issue upload-attachment").is_ok());
        assert!(validate_filename("file-with-dash_123.txt", "issue upload-attachment").is_ok());
    }
}
