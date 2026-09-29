//! A loopback S3 server holding one bucket in memory. `s3s` parses the S3 wire protocol
//! and verifies every request's Signature Version 4, header or query form, against the one
//! key pair the server admits; each conditional put is judged and applied under one lock,
//! so compare-and-set holds across concurrent clients as on a real backend.

use s3s::auth::SimpleAuth;
use s3s::dto::{
    DeleteObjectInput, DeleteObjectOutput, ETag, ETagCondition, GetObjectInput, GetObjectOutput, ListObjectsV2Input, ListObjectsV2Output, Object,
    PutObjectInput, PutObjectOutput, StreamingBlob,
};
use s3s::service::S3ServiceBuilder;
use s3s::{s3_error, S3Error, S3ErrorCode, S3Request, S3Response, S3Result, S3};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

/// The key pair the server admits.
pub const ACCESS_KEY: &str = "AKIACONTEXTFULTEST01";
pub const SECRET_KEY: &str = "contextful-test-secret-access-key-0000001";

#[derive(Default)]
struct State {
    objects: BTreeMap<String, (Vec<u8>, String)>,
    version: u64,
    /// Errors the next puts answer with, one each, ahead of any condition.
    scripted: VecDeque<S3ErrorCode>,
    puts: u64,
}

struct Backend {
    bucket: String,
    /// Keys one list page returns at most, whatever the client asks.
    page: usize,
    state: Arc<Mutex<State>>,
}

impl Backend {
    fn bucket(&self, name: &str) -> S3Result<()> {
        if name == self.bucket {
            Ok(())
        } else {
            Err(s3_error!(NoSuchBucket))
        }
    }
}

#[async_trait::async_trait]
impl S3 for Backend {
    async fn put_object(&self, req: S3Request<PutObjectInput>) -> S3Result<S3Response<PutObjectOutput>> {
        use futures_util::StreamExt;
        let input = req.input;
        self.bucket(&input.bucket)?;
        let mut bytes = Vec::new();
        if let Some(mut body) = input.body {
            while let Some(chunk) = body.next().await {
                bytes.extend_from_slice(&chunk.map_err(|e| S3Error::with_message(S3ErrorCode::IncompleteBody, e.to_string()))?);
            }
        }
        let mut state = self.state.lock().unwrap();
        state.puts += 1;
        if let Some(code) = state.scripted.pop_front() {
            return Err(S3Error::new(code));
        }
        let current = state.objects.get(&input.key).map(|(_, e)| e.clone());
        if input.if_none_match.as_ref().is_some_and(ETagCondition::is_any) && current.is_some() {
            return Err(s3_error!(PreconditionFailed, "an object holds the key"));
        }
        if let Some(condition) = &input.if_match {
            let Some(held) = &current else { return Err(s3_error!(NoSuchKey)) };
            if let ETagCondition::ETag(tag) = condition {
                if tag.value() != held {
                    return Err(s3_error!(PreconditionFailed, "the ETag differs"));
                }
            }
        }
        state.version += 1;
        let etag = format!("{:032x}", state.version);
        state.objects.insert(input.key, (bytes, etag.clone()));
        Ok(S3Response::new(PutObjectOutput { e_tag: Some(ETag::Strong(etag)), ..Default::default() }))
    }

    async fn get_object(&self, req: S3Request<GetObjectInput>) -> S3Result<S3Response<GetObjectOutput>> {
        self.bucket(&req.input.bucket)?;
        let Some((bytes, etag)) = self.state.lock().unwrap().objects.get(&req.input.key).cloned() else { return Err(s3_error!(NoSuchKey)) };
        Ok(S3Response::new(GetObjectOutput {
            content_length: Some(bytes.len() as i64),
            e_tag: Some(ETag::Strong(etag)),
            body: Some(StreamingBlob::from(bytes::Bytes::from(bytes))),
            ..Default::default()
        }))
    }

    async fn delete_object(&self, req: S3Request<DeleteObjectInput>) -> S3Result<S3Response<DeleteObjectOutput>> {
        self.bucket(&req.input.bucket)?;
        self.state.lock().unwrap().objects.remove(&req.input.key);
        Ok(S3Response::new(DeleteObjectOutput::default()))
    }

    async fn list_objects_v2(&self, req: S3Request<ListObjectsV2Input>) -> S3Result<S3Response<ListObjectsV2Output>> {
        let input = req.input;
        self.bucket(&input.bucket)?;
        let prefix = input.prefix.clone().unwrap_or_default();
        let after = input.continuation_token.clone().unwrap_or_default();
        let limit = input.max_keys.map_or(1000, |m| m.max(1) as usize).min(self.page);
        let state = self.state.lock().unwrap();
        let matching: Vec<&String> = state.objects.keys().filter(|k| k.starts_with(&prefix) && k.as_str() > after.as_str()).collect();
        let truncated = matching.len() > limit;
        let page: Vec<Object> = matching
            .iter()
            .take(limit)
            .map(|k| Object {
                key: Some((*k).clone()),
                size: Some(state.objects[*k].0.len() as i64),
                e_tag: Some(ETag::Strong(state.objects[*k].1.clone())),
                last_modified: Some(s3s::dto::Timestamp::from(std::time::SystemTime::now())),
                ..Default::default()
            })
            .collect();
        Ok(S3Response::new(ListObjectsV2Output {
            name: Some(self.bucket.clone()),
            prefix: input.prefix,
            key_count: Some(page.len() as i32),
            max_keys: input.max_keys,
            is_truncated: Some(truncated),
            next_continuation_token: truncated.then(|| page.last().and_then(|o| o.key.clone()).unwrap_or_default()),
            continuation_token: input.continuation_token,
            contents: Some(page),
            ..Default::default()
        }))
    }
}

/// A running server. Dropping it stops the listener.
pub struct S3Server {
    /// `http://127.0.0.1:<port>`.
    pub endpoint: String,
    pub bucket: String,
    state: Arc<Mutex<State>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl S3Server {
    /// Serve `bucket` on a loopback port.
    pub fn start(bucket: &str) -> S3Server {
        S3Server::start_paged(bucket, 1000)
    }

    /// Serve `bucket`, answering at most `page` keys per list page.
    pub fn start_paged(bucket: &str, page: usize) -> S3Server {
        let state = Arc::new(Mutex::new(State::default()));
        let backend = Backend { bucket: bucket.to_string(), page, state: state.clone() };
        let mut builder = S3ServiceBuilder::new(backend);
        builder.set_auth(SimpleAuth::from_single(ACCESS_KEY, SECRET_KEY));
        let service = builder.build();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::from_std(listener).unwrap();
                loop {
                    tokio::select! {
                        accepted = listener.accept() => {
                            let Ok((stream, _)) = accepted else { continue };
                            let service = service.clone();
                            tokio::spawn(async move {
                                let io = hyper_util::rt::TokioIo::new(stream);
                                let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new()).serve_connection(io, service).await;
                            });
                        }
                        _ = &mut stopped => break,
                    }
                }
            });
            rt.shutdown_background();
        });
        S3Server { endpoint, bucket: bucket.to_string(), state, stop: Some(stop), thread: Some(thread) }
    }

    /// Answer each of the next puts with one of `codes`, in order, before judging its condition.
    pub fn fail_puts(&self, codes: &[&str]) {
        let mut state = self.state.lock().unwrap();
        for c in codes {
            state.scripted.push_back(S3ErrorCode::from_bytes(c.as_bytes()).unwrap_or(S3ErrorCode::InternalError));
        }
    }

    /// Every key the bucket holds, sorted.
    pub fn keys(&self) -> Vec<String> {
        self.state.lock().unwrap().objects.keys().cloned().collect()
    }

    /// The bytes under `key`.
    pub fn object(&self, key: &str) -> Option<Vec<u8>> {
        self.state.lock().unwrap().objects.get(key).map(|(b, _)| b.clone())
    }

    /// Put requests the server has answered, signature failures excluded.
    pub fn puts(&self) -> u64 {
        self.state.lock().unwrap().puts
    }
}

impl Drop for S3Server {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
