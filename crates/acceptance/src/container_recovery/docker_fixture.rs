//! Isolated PostgreSQL and loopback-forwarded signed S3/provider services.

use super::{docker, validate_image, Container, RecoveryFixture, Result};
use crate::http::{Response, Server};
use crate::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub struct SyntheticFixture {
    // Proxies stop before the shared network namespace disappears.
    _proxies: Vec<Container>,
    postgres: Container,
    _bucket: S3Server,
    _provider: Server,
    first: Arc<AtomicUsize>,
    second: Arc<(Mutex<(bool, bool)>, Condvar)>,
}

impl SyntheticFixture {
    /// Both service images must already exist locally under immutable manifest digests.
    /// The fixture owns all containers it starts and never contacts a production service.
    pub fn start(postgres_image: &str, proxy_image: &str, timeout: Duration) -> Result<Self> {
        validate_image(postgres_image)?;
        validate_image(proxy_image)?;
        if timeout.is_zero() {
            return Err("ConformanceFixtureInvalid".into());
        }
        for image in [postgres_image, proxy_image] {
            docker(&["image", "inspect", image, "--format", "{{.Id}}"], timeout)
                .map_err(|_| "ConformanceFixtureImageUnavailable")?;
        }
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let name = format!(
            "contextful-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let postgres = Container(format!("{name}-postgres"));
        docker(
            &[
                "run",
                "--detach",
                "--rm",
                "--name",
                &postgres.0,
                "--env",
                "POSTGRES_HOST_AUTH_METHOD=trust",
                "--env",
                "POSTGRES_USER=conformance",
                postgres_image,
            ],
            timeout,
        )?;
        let deadline = Instant::now() + timeout;
        while docker(
            &[
                "exec",
                &postgres.0,
                "pg_isready",
                "--username",
                "conformance",
                "--host",
                "127.0.0.1",
            ],
            Duration::from_secs(3),
        )
        .is_err()
        {
            if Instant::now() >= deadline {
                return Err("ConformancePostgresFixtureUnavailable".into());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let first = Arc::new(AtomicUsize::new(0));
        let second = Arc::new((Mutex::new((false, false)), Condvar::new()));
        let (calls, page) = (first.clone(), second.clone());
        let provider = Server::start(move |request| {
            if request.path() == "/health" {
                return Response {
                    status: 200,
                    headers: vec![],
                    body: b"ready".to_vec(),
                };
            }
            if request.query("cursor").is_none() {
                calls.fetch_add(1, Ordering::SeqCst);
                Response::json(200, &serde_json::json!({"rows":[{"id":"a","payload":"x".repeat(80 * 1024)}],"next":"p2"}).to_string())
            } else {
                let (state, ready) = &*page;
                let mut state = state.lock().unwrap();
                state.0 = true;
                ready.notify_all();
                while !state.1 {
                    state = ready.wait(state).unwrap();
                }
                Response::json(200, r#"{"rows":[{"id":"b","payload":"done"}],"next":null}"#)
            }
        });
        let bucket = S3Server::start("conformance");
        let s3_port = bucket
            .endpoint
            .rsplit(':')
            .next()
            .ok_or("ConformanceFixtureInvalid")?;
        let mut proxies = Vec::new();
        for (suffix, local, host) in [
            ("provider", 18080, provider.port.to_string()),
            ("bucket", 18081, s3_port.to_string()),
        ] {
            let proxy = Container(format!("{name}-{suffix}"));
            docker(
                &[
                    "run",
                    "--detach",
                    "--rm",
                    "--name",
                    &proxy.0,
                    "--network",
                    &format!("container:{}", postgres.0),
                    proxy_image,
                    &format!("TCP-LISTEN:{local},bind=127.0.0.1,reuseaddr,fork"),
                    &format!("TCP:host.docker.internal:{host}"),
                ],
                timeout,
            )?;
            proxies.push(proxy);
        }
        Ok(Self {
            _proxies: proxies,
            postgres,
            _bucket: bucket,
            _provider: provider,
            first,
            second,
        })
    }
}

impl RecoveryFixture for SyntheticFixture {
    fn network_container(&self) -> &str {
        &self.postgres.0
    }
    fn declaration(&self) -> String {
        "site_id = \"conformance\"\nauthoring_posture = \"per_request\"\n[[pipeline]]\nid = \"feed\"\ntables = [{ name = \"items\", primary_key = [\"id\"] }]\n[pipeline.source]\nname = \"http\"\nconfig = { endpoint = \"http://127.0.0.1:18080/items\", records = \"/rows\", next_cursor_path = \"/next\", cursor_param = \"cursor\" }\n".into()
    }
    fn corpus_config(&self) -> String {
        "[node]\nid = \"conformance\"\n[sync]\nendpoint = \"http://127.0.0.1:18081\"\nbucket = \"conformance\"\nprefix = \"image-recovery\"\ncoordination = \"cas\"\npull_before_run = true\npush_after_run = true\naccess_key_id = \"env://CONTEXTFUL_SYNC_ACCESS_KEY_ID\"\nsecret_access_key = \"env://CONTEXTFUL_SYNC_SECRET_ACCESS_KEY\"\n".into()
    }
    fn environment(&self) -> BTreeMap<String, String> {
        [
            (
                "CONTEXTFUL_CATALOG_URL",
                "postgres://conformance@127.0.0.1:5432/postgres?sslmode=disable",
            ),
            ("CONTEXTFUL_RECOVERY_ID", "synthetic-image-v1"),
            ("CONTEXTFUL_SYNC_ACCESS_KEY_ID", ACCESS_KEY),
            ("CONTEXTFUL_SYNC_SECRET_ACCESS_KEY", SECRET_KEY),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    fn wait_for_second_page(&self, timeout: Duration) -> Result<()> {
        let (state, ready) = &*self.second;
        let (state, _) = ready
            .wait_timeout_while(state.lock().unwrap(), timeout, |state| !state.0)
            .map_err(|_| "ConformanceFixtureStateUnavailable")?;
        if state.0 {
            Ok(())
        } else {
            Err("ConformanceCrashPointNotReached".into())
        }
    }
    fn release_second_page(&self) -> Result<()> {
        let (state, ready) = &*self.second;
        state
            .lock()
            .map_err(|_| "ConformanceFixtureStateUnavailable")?
            .1 = true;
        ready.notify_all();
        Ok(())
    }
    fn first_page_calls(&self) -> usize {
        self.first.load(Ordering::SeqCst)
    }
    fn verification(&self) -> (String, serde_json::Value) {
        (
            "SELECT id, length(payload) AS bytes FROM feed_items ORDER BY id".into(),
            serde_json::json!([["a", 81920], ["b", 4]]),
        )
    }
}

impl Drop for SyntheticFixture {
    fn drop(&mut self) {
        let _ = self.release_second_page();
    }
}
