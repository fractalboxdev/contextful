//! A bucket on an S3-compatible endpoint: AWS S3, R2, or any backend speaking the S3
//! object API (`store.endpoint`). Objects are addressed path-style, every request carries
//! a Signature Version 4 query signature, and conditional puts ride `If-None-Match: *` and
//! `If-Match: <etag>`, so the backend itself arbitrates every compare-and-set.

use contextful_core::connector::reference::Hydrated;
use contextful_core::store::object::{CasScope, Condition, ObjectError, ObjectStore, Put};
use rusty_s3::actions::{ListObjectsV2, S3Action};
use rusty_s3::{Bucket, Credentials, UrlStyle};
use std::time::Duration;

/// Life of one signed request URL. A request is signed immediately before it is sent, so
/// the window covers the transfer alone.
const SIGNATURE_TTL: Duration = Duration::from_secs(300);

/// Wall-clock budget of one request, transfer included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

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
    credentials: Credentials,
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

impl S3Bucket {
    /// Open `bucket` on the endpoint `url`, signing for `region` (`store.endpoint.addressing`).
    pub fn open(url: &str, region: &str, bucket: &str, credentials: S3Credentials) -> Result<S3Bucket, ObjectError> {
        if !bucket_name(bucket) {
            return Err(ObjectError::Unsupported(format!("`{bucket}` is not an S3 bucket name")));
        }
        let base = format!("{}/", url.trim_end_matches('/'));
        let endpoint = base.parse().map_err(|e| ObjectError::Unsupported(format!("endpoint `{url}`: {e}")))?;
        let bucket = Bucket::new(endpoint, UrlStyle::Path, bucket.to_string(), region.to_string())
            .map_err(|e| ObjectError::Unsupported(format!("endpoint `{url}`: {e}")))?;
        let credentials = match &credentials.session_token {
            Some(token) => Credentials::new_with_token(credentials.access_key_id.reveal(), credentials.secret_access_key.reveal(), token.reveal()),
            None => Credentials::new(credentials.access_key_id.reveal(), credentials.secret_access_key.reveal()),
        };
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(REQUEST_TIMEOUT))
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
            _ => ObjectError::Transport(message),
        }
    }

    fn transport(what: &str, e: ureq::Error) -> ObjectError {
        ObjectError::Transport(format!("{what}: {e}"))
    }
}

/// The response's `ETag`, as the backend spells it.
fn etag(response: &ureq::http::Response<ureq::Body>) -> Option<String> {
    response.headers().get("etag").and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn read_text(response: &mut ureq::http::Response<ureq::Body>) -> String {
    response.body_mut().read_to_string().unwrap_or_default()
}

impl ObjectStore for S3Bucket {
    fn get(&self, key: &str) -> Result<Option<(Vec<u8>, String)>, ObjectError> {
        let what = format!("GET `{key}`");
        let url = self.bucket.get_object(Some(&self.credentials), key).sign(SIGNATURE_TTL);
        let mut response = self.agent.get(url.as_str()).call().map_err(|e| S3Bucket::transport(&what, e))?;
        match response.status().as_u16() {
            200 => {
                let tag = etag(&response).ok_or_else(|| ObjectError::Transport(format!("{what}: the answer carries no ETag")))?;
                let bytes = response.body_mut().with_config().limit(u64::MAX).read_to_vec().map_err(|e| S3Bucket::transport(&what, e))?;
                Ok(Some((bytes, tag)))
            }
            404 => Ok(None),
            status => Err(S3Bucket::failure(&what, status, &read_text(&mut response))),
        }
    }

    fn put(&self, key: &str, bytes: &[u8], condition: Condition) -> Result<Put, ObjectError> {
        let what = format!("PUT `{key}`");
        let mut action = self.bucket.put_object(Some(&self.credentials), key);
        let header = match &condition {
            Condition::None => None,
            Condition::IfNoneMatch => Some(("if-none-match", "*".to_string())),
            Condition::IfMatch(tag) => Some(("if-match", tag.clone())),
        };
        if let Some((name, value)) = &header {
            action.headers_mut().insert(*name, value.clone());
        }
        let url = action.sign(SIGNATURE_TTL);
        let mut request = self.agent.put(url.as_str());
        if let Some((name, value)) = &header {
            request = request.header(*name, value);
        }
        let mut response = request.send(bytes).map_err(|e| S3Bucket::transport(&what, e))?;
        match (response.status().as_u16(), &condition) {
            (200, _) => etag(&response).map(Put::Applied).ok_or_else(|| ObjectError::Transport(format!("{what}: the answer carries no ETag"))),
            // A lost condition, a concurrent conditional write, or `If-Match` on a missing object.
            (412 | 409, _) | (404, Condition::IfMatch(_)) => Ok(Put::ConditionFailed),
            (status, _) => Err(S3Bucket::failure(&what, status, &read_text(&mut response))),
        }
    }

    fn delete(&self, key: &str) -> Result<(), ObjectError> {
        let what = format!("DELETE `{key}`");
        let url = self.bucket.delete_object(Some(&self.credentials), key).sign(SIGNATURE_TTL);
        let mut response = self.agent.delete(url.as_str()).call().map_err(|e| S3Bucket::transport(&what, e))?;
        match response.status().as_u16() {
            200 | 204 | 404 => Ok(()),
            status => Err(S3Bucket::failure(&what, status, &read_text(&mut response))),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<String>, ObjectError> {
        let what = format!("LIST `{prefix}`");
        let mut keys = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut action: ListObjectsV2<'_> = self.bucket.list_objects_v2(Some(&self.credentials));
            action.with_prefix(prefix.to_string());
            action.with_max_keys(LIST_PAGE);
            if let Some(t) = &token {
                action.with_continuation_token(t.clone());
            }
            let url = action.sign(SIGNATURE_TTL);
            let mut response = self.agent.get(url.as_str()).call().map_err(|e| S3Bucket::transport(&what, e))?;
            let status = response.status().as_u16();
            let text = response.body_mut().with_config().limit(u64::MAX).read_to_string().map_err(|e| S3Bucket::transport(&what, e))?;
            if status != 200 {
                return Err(S3Bucket::failure(&what, status, &text));
            }
            let page = ListObjectsV2::parse_response(&text).map_err(|e| ObjectError::Transport(format!("{what}: {e}")))?;
            keys.extend(page.contents.into_iter().map(|c| c.key));
            match page.next_continuation_token {
                Some(next) if !next.is_empty() => token = Some(next),
                _ => break,
            }
        }
        keys.sort();
        keys.dedup();
        Ok(keys)
    }

    fn cas_scope(&self) -> CasScope {
        CasScope::Backend
    }
}
