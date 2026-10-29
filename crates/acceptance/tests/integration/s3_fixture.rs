//! The loopback S3 bucket's multipart upload: parts assemble in order on completion, an abort
//! leaves nothing, a completion naming a part the upload lacks refuses, and a scripted part fails.

use contextful_acceptance::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use rusty_s3::actions::CreateMultipartUpload;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};
use std::time::Duration;

const SIGNED: Duration = Duration::from_secs(600);
const MIB: usize = 1024 * 1024;

struct Client {
    bucket: Bucket,
    credentials: Credentials,
}

impl Client {
    fn new(s3: &S3Server) -> Client {
        let bucket = Bucket::new(url::Url::parse(&s3.endpoint).unwrap(), UrlStyle::Path, s3.bucket.clone(), "auto").unwrap();
        Client { bucket, credentials: Credentials::new(ACCESS_KEY, SECRET_KEY) }
    }

    /// Send `method` to `url` with `body`: the status, the `ETag` header and the body text.
    fn send(&self, method: &str, url: url::Url, body: &[u8]) -> (u16, Option<String>, String) {
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let response = match method {
            "PUT" => agent.put(url.as_str()).send(body),
            "POST" => agent.post(url.as_str()).send(body),
            "DELETE" => agent.delete(url.as_str()).call(),
            other => panic!("no method {other}"),
        }
        .unwrap();
        let status = response.status().as_u16();
        let etag = response.headers().get("etag").map(|v| v.to_str().unwrap().to_string());
        (status, etag, response.into_body().read_to_string().unwrap())
    }

    fn create(&self, key: &str) -> String {
        let (status, _, body) = self.send("POST", self.bucket.create_multipart_upload(Some(&self.credentials), key).sign(SIGNED), &[]);
        assert_eq!(status, 200, "{body}");
        CreateMultipartUpload::parse_response(&body).unwrap().upload_id().to_string()
    }

    fn part(&self, key: &str, id: &str, number: u16, bytes: &[u8]) -> String {
        let (status, etag, body) = self.send("PUT", self.bucket.upload_part(Some(&self.credentials), key, number, id).sign(SIGNED), bytes);
        assert_eq!(status, 200, "{body}");
        etag.unwrap()
    }

    fn complete(&self, key: &str, id: &str, etags: &[String]) -> (u16, String) {
        let action = self.bucket.complete_multipart_upload(Some(&self.credentials), key, id, etags.iter().map(String::as_str));
        let url = action.sign(SIGNED);
        let (status, _, body) = self.send("POST", url, action.body().as_bytes());
        (status, body)
    }

    fn abort(&self, key: &str, id: &str) -> u16 {
        self.send("DELETE", self.bucket.abort_multipart_upload(Some(&self.credentials), key, id).sign(SIGNED), &[]).0
    }
}

/// A completed upload holds its parts in order under its key, and the server records each part's size; an aborted
/// upload leaves no object and no open upload, and a completion naming an ETag the upload never answered refuses.
#[test]
fn the_loopback_bucket_assembles_completed_parts_and_drops_aborted_ones() {
    let s3 = S3Server::start("exports");
    let client = Client::new(&s3);
    let (first, second) = (vec![b'a'; 5 * MIB], b"tail".to_vec());

    let id = client.create("t/data/f.parquet");
    assert_eq!(s3.uploads(), ["t/data/f.parquet"]);
    let etags = vec![client.part("t/data/f.parquet", &id, 1, &first), client.part("t/data/f.parquet", &id, 2, &second)];
    let (status, body) = client.complete("t/data/f.parquet", &id, &etags);
    assert_eq!(status, 200, "{body}");
    assert_eq!(s3.object("t/data/f.parquet"), Some([first.clone(), second].concat()));
    assert_eq!(s3.parts(), [5 * MIB as u64, 4]);
    assert!(s3.uploads().is_empty(), "a completed upload closes");

    let dropped = client.create("t/data/g.parquet");
    client.part("t/data/g.parquet", &dropped, 1, &first);
    assert_eq!(client.abort("t/data/g.parquet", &dropped), 204);
    assert!(s3.uploads().is_empty());
    assert_eq!(s3.object("t/data/g.parquet"), None);
    assert_eq!(client.abort("t/data/g.parquet", &dropped), 404, "an aborted upload is gone");

    let wrong = client.create("t/data/h.parquet");
    client.part("t/data/h.parquet", &wrong, 1, b"x");
    let (status, body) = client.complete("t/data/h.parquet", &wrong, &["\"0000\"".to_string()]);
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("InvalidPart"), "{body}");
    assert_eq!(s3.uploads(), ["t/data/h.parquet"]);

    s3.fail_parts(1, &["InternalError"]);
    client.part("t/data/h.parquet", &wrong, 2, b"y");
    let url = client.bucket.upload_part(Some(&client.credentials), "t/data/h.parquet", 3, &wrong).sign(SIGNED);
    assert_eq!(client.send("PUT", url, b"z").0, 500, "the scripted part fails after the admitted one");
}
