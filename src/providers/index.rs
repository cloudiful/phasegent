//! Provider-neutral issue index model with stable keys, deterministic
//! hashes, and bounded UTF-8-safe chunks.

#![allow(dead_code)]

use std::fmt;

use async_trait::async_trait;

pub const ISSUE_INDEX_MAX_SOURCE_LEN: usize = 64;
pub const ISSUE_INDEX_MAX_PROJECT_LEN: usize = 200;
pub const ISSUE_INDEX_MAX_EXTERNAL_ID_LEN: usize = 128;
pub const ISSUE_INDEX_MAX_TITLE_CHARS: usize = 1024;
pub const ISSUE_INDEX_MAX_TITLE_BYTES: usize = 8192;
pub const ISSUE_INDEX_MAX_STATE_CHARS: usize = 64;
pub const ISSUE_INDEX_MAX_URL_CHARS: usize = 2048;
pub const ISSUE_INDEX_MAX_CHUNK_BYTES: usize = 4000;
pub const ISSUE_INDEX_MAX_CHUNKS: usize = 64;
pub const ISSUE_INDEX_MAX_DOCUMENT_BYTES: usize =
    ISSUE_INDEX_MAX_CHUNK_BYTES * ISSUE_INDEX_MAX_CHUNKS;
pub const ISSUE_INDEX_MAX_LIST_LIMIT: usize = 100;
pub const ISSUE_INDEX_DEFAULT_LIST_LIMIT: usize = 50;
pub const ISSUE_INDEX_SYNC_MAX_PAGES: usize = 100;
pub const ISSUE_INDEX_SEARCH_DEFAULT_LIMIT: usize = 20;
pub const ISSUE_INDEX_SEARCH_DEFAULT_OFFSET: usize = 0;
pub const ISSUE_INDEX_SEARCH_MAX_LIMIT: usize = 100;

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct IssueIndexKey {
    pub source: String,
    pub project: String,
    pub external_id: String,
}

impl IssueIndexKey {
    pub fn new(
        source: impl Into<String>,
        project: impl Into<String>,
        external_id: impl Into<String>,
    ) -> Result<Self, String> {
        let source = source.into();
        let project = project.into();
        let external_id = external_id.into();
        validate_identifier(&source, "source", ISSUE_INDEX_MAX_SOURCE_LEN)?;
        validate_identifier(&project, "project", ISSUE_INDEX_MAX_PROJECT_LEN)?;
        validate_identifier(&external_id, "external_id", ISSUE_INDEX_MAX_EXTERNAL_ID_LEN)?;
        Ok(Self {
            source: source.trim().to_owned(),
            project: project.trim().to_owned(),
            external_id: external_id.trim().to_owned(),
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        validate_identifier(&self.source, "source", ISSUE_INDEX_MAX_SOURCE_LEN)?;
        validate_identifier(&self.project, "project", ISSUE_INDEX_MAX_PROJECT_LEN)?;
        validate_identifier(
            &self.external_id,
            "external_id",
            ISSUE_INDEX_MAX_EXTERNAL_ID_LEN,
        )?;
        Ok(())
    }
}

impl fmt::Display for IssueIndexKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}", self.source, self.project, self.external_id)
    }
}

#[rustfmt::skip]
fn validate_identifier(v: &str, f: &str, m: usize) -> Result<(), String> { let t=v.trim(); if t.is_empty(){return Err(format!("{f} must be non-empty"));} if t.chars().count()>m{return Err(format!("{f} must be at most {m} characters"));} if t.chars().any(|c| c.is_control()){return Err(format!("{f} must not contain control characters"));} Ok(()) }
#[rustfmt::skip]
fn validate_title(v: &str) -> Result<(), String> { let t=v.trim(); if t.is_empty(){return Err("title must be non-empty".into());} if t.chars().count()>ISSUE_INDEX_MAX_TITLE_CHARS{return Err(format!("title must be at most {} characters",ISSUE_INDEX_MAX_TITLE_CHARS));} if t.len()>ISSUE_INDEX_MAX_TITLE_BYTES{return Err(format!("title must be at most {} bytes",ISSUE_INDEX_MAX_TITLE_BYTES));} if t.chars().any(|c| c.is_control()&&c!=' '&&c!='\t'){return Err("title must not contain control characters".into());} Ok(()) }
#[rustfmt::skip]
fn validate_state(v: &str) -> Result<(), String> { let t=v.trim(); if t.is_empty(){return Err("state must be non-empty".into());} if t.chars().count()>ISSUE_INDEX_MAX_STATE_CHARS{return Err(format!("state must be at most {} characters",ISSUE_INDEX_MAX_STATE_CHARS));} if t.chars().any(|c| c.is_control()){return Err("state must not contain control characters".into());} Ok(()) }
#[rustfmt::skip]
fn validate_url(v: &str) -> Result<(), String> { let t=v.trim(); if t.is_empty(){return Err("url must be non-empty when present".into());} if t.chars().count()>ISSUE_INDEX_MAX_URL_CHARS{return Err(format!("url must be at most {} characters",ISSUE_INDEX_MAX_URL_CHARS));} if t.chars().any(|c| c.is_control()){return Err("url must not contain control characters".into());} Ok(()) }

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueIndexChunk {
    pub ordinal: usize,
    pub text: String,
    pub byte_start: usize,
    pub byte_end: usize,
    pub hash: String,
}
impl IssueIndexChunk {
    pub fn validate(&self) -> Result<(), String> {
        if self.text.len() > ISSUE_INDEX_MAX_CHUNK_BYTES {
            return Err(format!(
                "chunk {} exceeds max {} bytes",
                self.ordinal, ISSUE_INDEX_MAX_CHUNK_BYTES
            ));
        }
        if self.byte_end < self.byte_start {
            return Err(format!("chunk {} has inverted byte offsets", self.ordinal));
        }
        if self.text.len() != self.byte_end - self.byte_start {
            return Err(format!(
                "chunk {} length {} does not match range {}..{}",
                self.ordinal,
                self.text.len(),
                self.byte_start,
                self.byte_end
            ));
        }
        if self.hash.trim().is_empty() {
            return Err(format!("chunk {} hash must be non-empty", self.ordinal));
        }
        if self.hash.chars().any(|c| c.is_control()) {
            return Err(format!(
                "chunk {} hash must not contain control characters",
                self.ordinal
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueIndexDocument {
    pub key: IssueIndexKey,
    pub issue_number: u64,
    pub title: String,
    pub body: String,
    pub state: String,
    pub url: Option<String>,
    pub provider_updated_at: Option<i64>,
    pub indexed_at: i64,
    pub content_hash: String,
    pub deleted: bool,
    pub deleted_at: Option<i64>,
    pub chunks: Vec<IssueIndexChunk>,
}
impl IssueIndexDocument {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: IssueIndexKey,
        issue_number: u64,
        title: String,
        body: String,
        state: String,
        url: Option<String>,
        provider_updated_at: Option<i64>,
        indexed_at: i64,
    ) -> Result<Self, String> {
        key.validate()?;
        if issue_number == 0 {
            return Err("issue_number must be greater than zero".to_owned());
        }
        let title_trimmed = title.trim().to_owned();
        let state_trimmed = state.trim().to_owned();
        let url_trimmed = url.map(|v| v.trim().to_owned());
        validate_title(&title_trimmed)?;
        validate_state(&state_trimmed)?;
        if let Some(ref u) = url_trimmed {
            validate_url(u)?;
        }
        if indexed_at <= 0 {
            return Err("indexed_at must be greater than zero".to_owned());
        }
        if let Some(ts) = provider_updated_at {
            if ts <= 0 {
                return Err("provider_updated_at must be greater than zero when present".to_owned());
            }
        }
        if body.len() > ISSUE_INDEX_MAX_DOCUMENT_BYTES {
            return Err(format!(
                "body must be at most {} bytes ({} chunks of {} bytes)",
                ISSUE_INDEX_MAX_DOCUMENT_BYTES, ISSUE_INDEX_MAX_CHUNKS, ISSUE_INDEX_MAX_CHUNK_BYTES
            ));
        }
        let content_hash = content_hash(&title_trimmed, &body, &state_trimmed);
        let chunks = build_chunks(&body, ISSUE_INDEX_MAX_CHUNK_BYTES, ISSUE_INDEX_MAX_CHUNKS)?;
        for c in &chunks {
            c.validate()?;
        }
        Ok(Self {
            key,
            issue_number,
            title: title_trimmed,
            body,
            state: state_trimmed,
            url: url_trimmed,
            provider_updated_at,
            indexed_at,
            content_hash,
            deleted: false,
            deleted_at: None,
            chunks,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        self.key.validate()?;
        if self.issue_number == 0 && !self.deleted {
            return Err("issue_number must be greater than zero".to_owned());
        }
        validate_title(&self.title)?;
        validate_state(&self.state)?;
        if let Some(ref u) = self.url {
            validate_url(u)?;
        }
        if self.indexed_at <= 0 {
            return Err("indexed_at must be greater than zero".to_owned());
        }
        if let Some(ts) = self.provider_updated_at {
            if ts <= 0 {
                return Err("provider_updated_at must be greater than zero when present".to_owned());
            }
        }
        if self.deleted {
            match self.deleted_at {
                Some(ts) if ts > 0 => {}
                Some(_) => return Err("deleted_at must be greater than zero".to_owned()),
                None => return Err("deleted document must have deleted_at".to_owned()),
            }
            if !self.chunks.is_empty() {
                return Err("deleted document must have no chunks".to_owned());
            }
        } else if self.deleted_at.is_some() {
            return Err("non-deleted document must not have deleted_at".to_owned());
        }
        if self.content_hash.trim().is_empty() {
            return Err("content_hash must be non-empty".to_owned());
        }
        if self.content_hash.chars().any(|c| c.is_control()) {
            return Err("content_hash must not contain control characters".to_owned());
        }
        if self.chunks.len() > ISSUE_INDEX_MAX_CHUNKS {
            return Err(format!(
                "document has {} chunks but max is {}",
                self.chunks.len(),
                ISSUE_INDEX_MAX_CHUNKS
            ));
        }
        let mut exp = 0;
        for chunk in &self.chunks {
            if chunk.ordinal != exp {
                return Err(format!(
                    "chunk ordinal {} does not match expected {}",
                    chunk.ordinal, exp
                ));
            }
            chunk.validate()?;
            exp += 1;
        }
        let expected = content_hash(&self.title, &self.body, &self.state);
        if self.content_hash != expected {
            return Err(format!(
                "content_hash mismatch: expected {expected}, got {}",
                self.content_hash
            ));
        }
        if !self.deleted {
            let total: usize = self.chunks.iter().map(|c| c.text.len()).sum();
            if total != self.body.len() {
                return Err(format!(
                    "chunk bytes {} do not match body len {}",
                    total,
                    self.body.len()
                ));
            }
        }
        Ok(())
    }
}

pub fn content_hash(title: &str, body: &str, state: &str) -> String {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;
    let mut h = OFFSET;
    for &b in title.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h ^= 0xFF;
    h = h.wrapping_mul(PRIME);
    for &b in body.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    h ^= 0xFF;
    h = h.wrapping_mul(PRIME);
    for &b in state.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}
pub fn hash_text(text: &str) -> String {
    const OFFSET: u64 = 14695981039346656037;
    const PRIME: u64 = 1099511628211;
    let mut h = OFFSET;
    for &b in text.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(PRIME);
    }
    format!("{h:016x}")
}
pub fn build_chunks(
    text: &str,
    max_bytes: usize,
    max_chunks: usize,
) -> Result<Vec<IssueIndexChunk>, String> {
    if max_bytes == 0 || max_chunks == 0 {
        return Err("max_bytes and max_chunks must be greater than zero".to_owned());
    }
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if text.len() > max_bytes * max_chunks {
        return Err(format!(
            "document too large: {} bytes exceeds {} * {} = {} bytes",
            text.len(),
            max_chunks,
            max_bytes,
            max_bytes * max_chunks
        ));
    }
    let mut chunks = Vec::new();
    let mut offset = 0;
    let mut ordinal = 0;
    while offset < text.len() {
        if ordinal >= max_chunks {
            return Err(format!(
                "document too large: needs more than {max_chunks} chunks"
            ));
        }
        let mut end = (offset + max_bytes).min(text.len());
        while end > offset && !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == offset {
            return Err(format!(
                "chunk boundary could not be found within {max_bytes} bytes"
            ));
        }
        let slice = &text[offset..end];
        chunks.push(IssueIndexChunk {
            ordinal,
            text: slice.to_owned(),
            byte_start: offset,
            byte_end: end,
            hash: hash_text(slice),
        });
        offset = end;
        ordinal += 1;
    }
    Ok(chunks)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IssueIndexListOptions {
    pub limit: usize,
    pub offset: usize,
}
impl IssueIndexListOptions {
    pub fn new(limit: usize, offset: usize) -> Result<Self, String> {
        if limit == 0 || limit > ISSUE_INDEX_MAX_LIST_LIMIT {
            return Err(format!(
                "list limit must be between 1 and {}",
                ISSUE_INDEX_MAX_LIST_LIMIT
            ));
        }
        Ok(Self { limit, offset })
    }
}
#[async_trait(?Send)]
pub trait IssueIndexStore {
    async fn upsert(&self, doc: &IssueIndexDocument) -> Result<(), String>;
    async fn get(&self, key: &IssueIndexKey) -> Result<Option<IssueIndexDocument>, String>;
    async fn list(
        &self,
        options: &IssueIndexListOptions,
    ) -> Result<Vec<IssueIndexDocument>, String>;
    async fn tombstone(&self, key: &IssueIndexKey, indexed_at: i64) -> Result<(), String>;
    async fn lexical_search(
        &self,
        query: &str,
        limit: usize,
        offset: usize,
        include_body: bool,
    ) -> Result<crate::providers::index_store::IssueIndexSearchResult, String>;
    async fn list_active_keys_for_scope(
        &self,
        source: &str,
        project: &str,
    ) -> Result<Vec<IssueIndexKey>, String>;
}
