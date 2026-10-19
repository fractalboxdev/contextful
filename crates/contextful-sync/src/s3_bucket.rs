//! A bucket on an S3-compatible endpoint: AWS S3, R2, or any backend speaking the S3
//! object API (`store.endpoint`). Objects are addressed path-style, every request carries
//! a Signature Version 4 query signature, and conditional puts ride `If-None-Match: *` and
//! `If-Match: <etag>`, so the backend itself arbitrates every compare-and-set. The `s3`
//! object source reads through the same adapter, anonymously where it binds no key pair.

use crate::s3_retry::{request_budget, resend_until, Resend, ANSWER_TIMEOUT, REQUEST_TIMEOUT};
use contextful_core::connector::reference::Hydrated;
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use rusty_s3::actions::{ListObjectsV2, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use std::time::{Duration, Instant};

/// Life of one signed request URL. A request is signed immediately before it is sent, so
/// the window covers the transfer alone.
const SIGNATURE_TTL: Duration = Duration::from_secs(300);

/// The connection establishment budget for each request attempt.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Keys one list page asks for; a backend may answer fewer.
const LIST_PAGE: usize = 1000;

/// The material an S3 bucket signs with, each value in the redacting wrapper.
pub struct S3Credentials {
    pub access_key_id: Hydrated,
    pub secret_access_key: Hydrated,
    pub session_token: Option<Hydrated>,
}

/// The bucket `<url>/<bucket>/` on an S3-compatible endpoint.
pub struct S3Bucket {
    bucket: Bucket,
    /// Absent for an anonymous reader, whose requests go unsigned.
    credentials: Option<Credentials>,
    agent: ureq::Agent,
}

impl std::fmt::Debug for S3Bucket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S3Bucket").field("url", &self.bucket.base_url().as_str()).field("region", &self.bucket.region()).finish_non_exhaustive()
    }
}

/// Whether `name` is an S3 bucket name: 3 to 63 chars of `[a-z0-9.-]`, opening and closing
/// on a letter or digit.
fn bucket_name(name: &str) -> bool {
    let edge = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    (3..=63).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && edge(name.as_bytes().first())
        && edge(name.as_bytes().last())
}

/// The `<Code>` of an S3 error document, or the empty string.
fn error_code(body: &str) -> &str {
    body.split_once("<Code>").and_then(|(_, rest)| rest.split_once("</Code>")).map_or("", |(code, _)| code)
}

/// Whether a `404` answers an absent key: its error code is `NoSuchKey`, or the answer carries
/// no error document. A `404` naming any other code, `NoSuchBucket` among them, is a failure
/// (`store.endpoint.missing-bucket`).
fn absent_key(body: &str) -> bool {
    matches!(error_code(body), "NoSuchKey" | "")
}

impl S3Bucket {
    /// Open `bucket` on the endpoint `url`, signing for `region` (`store.endpoint.addressing`).
    pub fn open(url: &str, region: &str, bucket: &str, credentials: S3Credentials) -> Result<S3Bucket, ObjectError> {
        let credentials = match &credentials.session_token {
            Some(token) => Credentials::new_with_token(credentials.access_key_id.reveal(), credentials.secret_access_key.reveal(), token.reveal()),
            None => Credentials::new(credentials.access_key_id.reveal(), credentials.secret_access_key.reveal()),
        };
        S3Bucket::with(url, region, bucket, Some(credentials))
    }

    /// Open `bucket` for unsigned requests, as a public bucket answers them.
    pub fn anonymous(url: &str, region: &str, bucket: &str) -> Result<S3Bucket, ObjectError> {
        S3Bucket::with(url, region, bucket, None)
    }

    fn with(url: &str, region: &str, bucket: &str, credentials: Option<Credentials>) -> Result<S3Bucket, ObjectError> {
        if !bucket_name(bucket) {
            return Err(ObjectError::Unsupported(format!("`{bucket}` is not an S3 bucket name")));
        }
        let base = format!("{}/", url.trim_end_matches('/'));
        let endpoint = base.parse().map_err(|e| ObjectError::Unsupported(format!("endpoint `{url}`: {e}")))?;
        let bucket = Bucket::new(endpoint, UrlStyle::Path, bucket.to_string(), region.to_string())
            .map_err(|e| ObjectError::Unsupported(format!("endpoint `{url}`: {e}")))?;
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(ANSWER_TIMEOUT))
            // One idle connection per transfer in flight, so a transfer reuses its TLS session (`store.pull.transfer-width`).
            .max_idle_connections(crate::transfer::TRANSFER_WIDTH)
            .max_idle_connections_per_host(crate::transfer::TRANSFER_WIDTH)
            .build()
            .new_agent();
        Ok(S3Bucket { bucket, credentials, agent })
    }

    /// The error a non-success answer carries (`store.endpoint.conditional-answers`).
    fn failure(what: &str, status: u16, body: &str) -> ObjectError {
        let code = error_code(body);
        let message = format!("{what}: HTTP {status} {code}");
        match status {
            403 => ObjectError::Forbidden(message),
            501 => ObjectError::Unsupported(message),
            429 | 500.. => ObjectError::Unavailable(message),
            _ => ObjectError::Transport(message),
        }
    }

    /// Send a request, each attempt waiting the answer window `send` is handed, and resend it
    /// as `mode` allows: after a transport failure that left no whole answer, within 3 attempts,
    /// and after a throttled answer, within 5. The last answer returns whatever its status.
    fn resend<T>(what: &str, mode: Resend, send: impl Fn(Duration) -> Result<(u16, T), ureq::Error>) -> Result<(u16, T), ObjectError> {
        resend_until(what, mode, None, send)
    }

    /// A presigned `GET` of `key`, for a caller sending through its own client.
    pub fn presign_get(&self, key: &str) -> String {
        self.bucket.get_object(self.credentials.as_ref(), key).sign(SIGNATURE_TTL).into()
    }

    /// A presigned `HEAD` of `key`, for a caller sending through its own client.
    pub fn presign_head(&self, key: &str) -> String {
        self.bucket.head_object(self.credentials.as_ref(), key).sign(SIGNATURE_TTL).into()
    }

    /// A presigned list-type-2 page under `prefix`, continuing from `token`.
    pub fn presign_list(&self, prefix: &str, token: Option<&str>) -> String {
        let mut action: ListObjectsV2<'_> = self.bucket.list_objects_v2(self.credentials.as_ref());
        action.with_prefix(prefix.to_string());
        action.with_max_keys(LIST_PAGE);
        if let Some(t) = token {
            action.with_continuation_token(t.to_string());
        }
        action.sign(SIGNATURE_TTL).into()
    }

    /// One list page's keys, each with its ETag, and the token continuing the listing; `None`
    /// where the backend reports the listing complete.
    pub fn parse_list(body: &str) -> Result<(Vec<(String, String)>, Option<String>), ObjectError> {
        let page = ListObjectsV2::parse_response(body).map_err(|e| ObjectError::Transport(format!("the listing does not parse: {e}")))?;
        let keys = page.contents.into_iter().map(|c| (c.key, c.etag)).collect();
        Ok((keys, page.next_continuation_token.filter(|t| !t.is_empty())))
    }

    /// The ETag of the object under `key`, read by a head request that transfers no bytes. A
    /// head answer carries no error document, so a `404` is settled by a get, whose error
    /// document tells a missing key from a missing bucket (`store.endpoint.missing-bucket`).
    pub fn head(&self, key: &str) -> Result<Option<String>, ObjectError> {
        let what = format!("HEAD `{key}`");
        let url = self.presign_head(key);
        let (status, mut response) = S3Bucket::resend(&what, Resend::Repeatable, |window| {
            let response = self.agent.head(&url).config().timeout_recv_response(Some(window)).build().call()?;
            Ok((response.status().as_u16(), response))
        })?;
        match status {
            200 => etag(&response).map(Some).ok_or_else(|| ObjectError::Transport(format!("{what}: the answer carries no ETag"))),
            404 => Ok(self.get(key)?.map(|(_, tag)| tag)),
            status => Err(S3Bucket::failure(&what, status, &read_text(&mut response))),
        }
    }

    /// Every key under `prefix` with the ETag the listing reports, sorted by key, following each
    /// continuation token (`store.endpoint.list-pages`).
    pub fn list_tagged(&self, prefix: &str) -> Result<Vec<(String, String)>, ObjectError> {
        let what = format!("LIST `{prefix}`");
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let url = self.presign_list(prefix, token.as_deref());
            let (status, text) = S3Bucket::resend(&what, Resend::Repeatable, |window| {
                let mut response = self.agent.get(&url).config().timeout_recv_response(Some(window)).build().call()?;
                Ok((response.status().as_u16(), response.body_mut().with_config().limit(u64::MAX).read_to_string()?))
            })?;
            if status != 200 {
                return Err(S3Bucket::failure(&what, status, &text));
            }
            let (page, next) = S3Bucket::parse_list(&text).map_err(|e| ObjectError::Transport(format!("{what}: {e}")))?;
            keys.extend(page);
            match next {
                Some(next) => token = Some(next),
                None => break,
            }
        }
        keys.sort();
        keys.dedup_by(|a, b| a.0 == b.0);
        Ok(keys)
    }
}

/// The response's `ETag`, as the backend spells it.
fn etag(response: &ureq::http::Response<ureq::Body>) -> Option<String> {
    response.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn read_text(response: &mut ureq::http::Response<ureq::Body>) -> String {
    response.body_mut().read_to_string().unwrap_or_default()
}

impl S3Bucket {
    fn get_until(&self, key: &str, deadline: Option<Instant>) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        let what = format!("GET `{key}`");
        let url = self.presign_get(key);
        let (status, (tag, bytes)) = resend_until(&what, Resend::Repeatable, deadline, |window| -> Result<_, ureq::Error> {
            let timeout = request_budget(&what, deadline).map_err(|_| ureq::Error::Timeout(ureq::Timeout::Global))?;
            let mut response = self.agent.get(&url).config().timeout_global(Some(timeout)).timeout_recv_response(Some(window)).build().call()?;
            let tag = etag(&response);
            Ok((response.status().as_u16(), (tag, response.body_mut().with_config().limit(u64::MAX).read_to_vec()?)))
        })?;
        match status {
            200 => {
                let tag = tag.ok_or_else(|| ObjectError::Transport(format!("{what}: the answer carries no ETag")))?;
                Ok(Some((bytes, tag)))
            }
            status => {
                let body = String::from_utf8_lossy(&bytes);
                if status == 404 && absent_key(&body) {
                    return Ok(None);
                }
                Err(S3Bucket::failure(&what, status, &body))
            }
        }
    }
}

impl ObjectStore for S3Bucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.get_until(key, None)
    }

    fn get_within(&self, key: &str, deadline: Instant) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        self.get_until(key, Some(deadline))
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        let what = format!("PUT `{key}`");
        let mut action = self.bucket.put_object(self.credentials.as_ref(), key);
        let header = match &condition {
            Condition::None => None,
            Condition::IfNoneMatch => Some(("if-none-match", "*".to_string())),
            Condition::IfMatch(tag) => Some(("if-match", tag.clone())),
        };
        if let Some((name, value)) = &header {
            action.headers_mut().insert(*name, value.clone());
        }
        let url = action.sign(SIGNATURE_TTL);
        let send = |window| {
            let mut request = self.agent.put(url.as_str()).config().timeout_recv_response(Some(window)).build();
            if let Some((name, value)) = &header {
                request = request.header(*name, value);
            }
            let response = request.send(bytes)?;
            Ok((response.status().as_u16(), response))
        };
        // An unconditional put of the same bytes leaves the state its first landing left; a
        // conditional put whose answer is lost may have applied, so it resends only a throttled answer.
        let mode = match condition {
            Condition::None => Resend::Repeatable,
            _ => Resend::Conditional,
        };
        let (status, mut response) = S3Bucket::resend(&what, mode, send)?;
        if status == 200 {
            return etag(&response).map(Put::Applied).ok_or_else(|| ObjectError::Transport(format!("{what}: the answer carries no ETag")));
        }
        let body = read_text(&mut response);
        match (status, &condition) {
            // A lost condition, a concurrent conditional write, or `If-Match` on a missing object.
            (412 | 409, _) => Ok(Put::ConditionFailed),
            (404, Condition::IfMatch(_)) if absent_key(&body) => Ok(Put::ConditionFailed),
            (status, _) => Err(S3Bucket::failure(&what, status, &body)),
        }
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        let what = format!("DELETE `{key}`");
        let url = self.bucket.delete_object(self.credentials.as_ref(), key).sign(SIGNATURE_TTL);
        let (status, mut response) = S3Bucket::resend(&what, Resend::Repeatable, |window| {
            let response = self.agent.delete(url.as_str()).config().timeout_recv_response(Some(window)).build().call()?;
            Ok((response.status().as_u16(), response))
        })?;
        match status {
            200 | 204 => Ok(()),
            status => {
                let body = read_text(&mut response);
                if status == 404 && absent_key(&body) {
                    return Ok(());
                }
                Err(S3Bucket::failure(&what, status, &body))
            }
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        Ok(self.list_tagged(prefix)?.into_iter().map(|(k, _)| k).collect())
    }

    fn cas_scope(&self) -> CasScope {
        CasScope::Backend
    }
}
