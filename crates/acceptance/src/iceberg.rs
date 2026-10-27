//! A loopback Apache Iceberg REST catalog over the loopback S3 bucket. The reference
//! library builds every table's metadata and judges every commit's requirements, so the
//! catalog refuses what a conforming catalog refuses. A test scripts the next commits: a
//! foreign snapshot landing first, as catalog compaction does, or an answer the client
//! never sees. [`Catalog::scan`] reads the current snapshot through the same library, a
//! third-party reader holding no engine code.

use crate::http::{Request, Response, Server};
use crate::s3::{S3Server, ACCESS_KEY, SECRET_KEY};
use arrow_array::RecordBatch;
use futures::TryStreamExt;
use iceberg::io::FileIO;
use iceberg::spec::{Operation, Schema, Snapshot, SnapshotReference, SnapshotRetention, Summary, TableMetadata, TableMetadataBuilder, MAIN_BRANCH};
use iceberg::table::Table;
use iceberg::{TableCreation, TableIdent, TableRequirement, TableUpdate};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};

/// What the catalog does with one coming commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Script {
    /// Commit a foreign `replace` snapshot over the same files first, as compaction does,
    /// then judge the request against it.
    Compact,
    /// Apply the commit, then answer `status` with no body, as a dropped connection leaves it.
    AppliedThenAnswer(u16),
    /// Refuse the commit with `status`, applying nothing.
    Refuse(u16),
}

#[derive(Default)]
struct State {
    namespaces: BTreeSet<String>,
    tables: BTreeMap<(String, String), TableMetadata>,
    scripts: VecDeque<Script>,
    /// Commit requests received, judged or not.
    commits: usize,
    /// The `s3.endpoint` a table configuration names, when a test overrides it.
    storage: Option<String>,
}

/// A running catalog. Every request must carry `Authorization: Bearer <token>`.
pub struct Catalog {
    /// `http://127.0.0.1:<port>`, the catalog URI ahead of `/v1`.
    pub endpoint: String,
    pub bucket: String,
    s3_endpoint: String,
    state: Arc<Mutex<State>>,
    _server: Server,
}

fn error(status: u16, kind: &str, message: &str) -> Response {
    Response::json(status, &json!({ "error": { "message": message, "type": kind, "code": status } }).to_string())
}

impl Catalog {
    /// Serve a catalog whose tables live in `s3`'s bucket.
    pub fn start(s3: &S3Server, token: &str) -> Catalog {
        let state: Arc<Mutex<State>> = Arc::default();
        let (bucket, s3_endpoint, bearer) = (s3.bucket.clone(), s3.endpoint.clone(), format!("Bearer {token}"));
        let (shared, b, e) = (state.clone(), bucket.clone(), s3_endpoint.clone());
        let server = Server::start(move |request| {
            if request.header("authorization") != Some(bearer.as_str()) {
                return error(401, "NotAuthorizedException", "the bearer token is absent or wrong");
            }
            let mut state = shared.lock().unwrap();
            route(&mut state, &b, &e, request)
        });
        Catalog { endpoint: server.url(""), bucket, s3_endpoint, state, _server: server }
    }

    /// Answer the next commits as `scripts` say, in order.
    pub fn script(&self, scripts: &[Script]) {
        self.state.lock().unwrap().scripts.extend(scripts.iter().copied());
    }

    /// Name `endpoint` as every table configuration's `s3.endpoint`.
    pub fn name_storage(&self, endpoint: &str) {
        self.state.lock().unwrap().storage = Some(endpoint.to_string());
    }

    /// Commit requests received.
    pub fn commits(&self) -> usize {
        self.state.lock().unwrap().commits
    }

    /// The current metadata of `namespace.table`.
    pub fn metadata(&self, namespace: &str, table: &str) -> Option<TableMetadata> {
        self.state.lock().unwrap().tables.get(&(namespace.to_string(), table.to_string())).cloned()
    }

    fn table(&self, s3: &S3Server, namespace: &str, name: &str) -> (Table, tokio::runtime::Runtime) {
        let metadata = self.metadata(namespace, name).expect("the table exists");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let io = FileIO::new_with_memory();
        rt.block_on(async {
            for key in s3.keys() {
                let bytes = s3.object(&key).unwrap();
                io.new_output(format!("s3://{}/{key}", self.bucket)).unwrap().write(bytes.into()).await.unwrap();
            }
        });
        let table = Table::builder()
            .metadata(metadata)
            .identifier(TableIdent::from_strs([namespace, name]).unwrap())
            .file_io(io)
            .runtime(iceberg::Runtime::new(&rt))
            .metadata_location(format!("s3://{}/{namespace}/{name}/metadata/current.metadata.json", self.bucket))
            .build()
            .unwrap();
        (table, rt)
    }

    /// Every row of `namespace.table`'s current snapshot, read through the reference library.
    pub fn scan(&self, s3: &S3Server, namespace: &str, table: &str) -> Vec<RecordBatch> {
        let (table, rt) = self.table(s3, namespace, table);
        rt.block_on(async { table.scan().build().unwrap().to_arrow().await.unwrap().try_collect().await.unwrap() })
    }

    /// The data files `namespace.table`'s current snapshot references, as object keys.
    pub fn live_files(&self, s3: &S3Server, namespace: &str, table: &str) -> Vec<String> {
        let (table, rt) = self.table(s3, namespace, table);
        let prefix = format!("s3://{}/", self.bucket);
        let mut files: Vec<String> = rt.block_on(async {
            let tasks: Vec<_> = table.scan().build().unwrap().plan_files().await.unwrap().try_collect().await.unwrap();
            tasks.into_iter().map(|t| t.data_file_path.trim_start_matches(&prefix).to_string()).collect()
        });
        files.sort();
        files
    }

    /// The S3 endpoint the catalog vends by default.
    pub fn s3_endpoint(&self) -> &str {
        &self.s3_endpoint
    }
}

fn load_result(state: &State, bucket: &str, s3: &str, namespace: &str, name: &str, metadata: &TableMetadata) -> Response {
    let config = json!({
        "s3.endpoint": state.storage.clone().unwrap_or_else(|| s3.to_string()),
        "s3.access-key-id": ACCESS_KEY,
        "s3.secret-access-key": SECRET_KEY,
        "s3.region": "us-east-1",
        "s3.path-style-access": "true",
    });
    let location = format!("s3://{bucket}/{namespace}/{name}/metadata/{}.metadata.json", metadata.last_updated_ms());
    Response::json(200, &json!({ "metadata-location": location, "metadata": metadata, "config": config }).to_string())
}

fn route(state: &mut State, bucket: &str, s3: &str, request: &Request) -> Response {
    let path: Vec<&str> = request.path().trim_start_matches('/').split('/').collect();
    match (request.method.as_str(), path.as_slice()) {
        ("GET", ["v1", "config"]) => {
            let warehouse = request.query("warehouse").unwrap_or_default();
            Response::json(200, &json!({ "defaults": {}, "overrides": { "prefix": warehouse } }).to_string())
        }
        ("GET", ["v1", _, "namespaces", ns]) => match state.namespaces.contains(*ns) {
            true => Response::json(200, &json!({ "namespace": [ns], "properties": {} }).to_string()),
            false => error(404, "NoSuchNamespaceException", &format!("namespace `{ns}` does not exist")),
        },
        ("POST", ["v1", _, "namespaces"]) => {
            let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let Some(ns) = body["namespace"][0].as_str() else { return error(400, "BadRequestException", "no namespace") };
            if !state.namespaces.insert(ns.to_string()) {
                return error(409, "AlreadyExistsException", &format!("namespace `{ns}` exists"));
            }
            Response::json(200, &json!({ "namespace": [ns], "properties": {} }).to_string())
        }
        ("POST", ["v1", warehouse, "namespaces", ns, "tables"]) => {
            if !state.namespaces.contains(*ns) {
                return error(404, "NoSuchNamespaceException", &format!("namespace `{ns}` does not exist"));
            }
            let body: Value = serde_json::from_slice(&request.body).unwrap_or_default();
            let Some(name) = body["name"].as_str().map(str::to_string) else { return error(400, "BadRequestException", "no name") };
            if state.tables.contains_key(&(ns.to_string(), name.clone())) {
                return error(409, "AlreadyExistsException", &format!("table `{ns}.{name}` exists"));
            }
            let schema: Schema = match serde_json::from_value(body["schema"].clone()) {
                Ok(s) => s,
                Err(e) => return error(400, "BadRequestException", &format!("schema: {e}")),
            };
            let mut properties: HashMap<String, String> = serde_json::from_value(body["properties"].clone()).unwrap_or_default();
            // The reserved property selects the format version and is no table property.
            let version = match properties.remove("format-version").as_deref() {
                Some("3") => iceberg::spec::FormatVersion::V3,
                _ => iceberg::spec::FormatVersion::V2,
            };
            let creation = TableCreation::builder()
                .name(name.clone())
                .location(format!("s3://{bucket}/{warehouse}/{ns}/{name}"))
                .schema(schema)
                .properties(properties)
                .format_version(version)
                .build();
            let metadata = match TableMetadataBuilder::from_table_creation(creation).and_then(|b| b.build()) {
                Ok(built) => built.metadata,
                Err(e) => return error(400, "BadRequestException", &e.to_string()),
            };
            state.tables.insert((ns.to_string(), name.clone()), metadata.clone());
            load_result(state, bucket, s3, ns, &name, &metadata)
        }
        ("GET", ["v1", _, "namespaces", ns, "tables", name]) => match state.tables.get(&(ns.to_string(), name.to_string())) {
            Some(metadata) => load_result(state, bucket, s3, ns, name, metadata),
            None => error(404, "NoSuchTableException", &format!("table `{ns}.{name}` does not exist")),
        },
        ("POST", ["v1", _, "namespaces", ns, "tables", name]) => commit(state, ns, name, request),
        _ => error(404, "NoSuchEndpoint", request.path()),
    }
}

fn apply(current: &TableMetadata, updates: Vec<TableUpdate>) -> iceberg::Result<TableMetadata> {
    let mut builder = TableMetadataBuilder::new_from_metadata(current.clone(), None);
    for update in updates {
        builder = update.apply(builder)?;
    }
    Ok(builder.build()?.metadata)
}

/// A `replace` snapshot over the parent's own manifest list, committed to `main`.
fn compact(current: &TableMetadata) -> TableMetadata {
    let parent = current.current_snapshot().expect("compaction needs a snapshot");
    let snapshot_id = parent.snapshot_id() + 1_000_003;
    let snapshot = Snapshot::builder()
        .with_snapshot_id(snapshot_id)
        .with_parent_snapshot_id(Some(parent.snapshot_id()))
        .with_sequence_number(current.next_sequence_number())
        .with_timestamp_ms(current.last_updated_ms() + 1)
        .with_manifest_list(parent.manifest_list().to_string())
        .with_schema_id(current.current_schema_id())
        .with_summary(Summary { operation: Operation::Replace, additional_properties: HashMap::new() })
        .with_row_range(current.next_row_id(), 0)
        .build();
    let reference = SnapshotReference::new(snapshot_id, SnapshotRetention::branch(None, None, None));
    apply(current, vec![TableUpdate::AddSnapshot { snapshot }, TableUpdate::SetSnapshotRef { ref_name: MAIN_BRANCH.to_string(), reference }]).expect("the compaction commit applies")
}

fn commit(state: &mut State, ns: &str, name: &str, request: &Request) -> Response {
    state.commits += 1;
    let key = (ns.to_string(), name.to_string());
    let Some(mut current) = state.tables.get(&key).cloned() else {
        return error(404, "NoSuchTableException", &format!("table `{ns}.{name}` does not exist"));
    };
    let script = state.scripts.pop_front();
    if let Some(Script::Refuse(status)) = script {
        return error(status, "ServiceFailureException", "scripted refusal");
    }
    if script == Some(Script::Compact) {
        current = compact(&current);
        state.tables.insert(key.clone(), current.clone());
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(e) => return error(400, "BadRequestException", &e.to_string()),
    };
    let requirements: Vec<TableRequirement> = match serde_json::from_value(body["requirements"].clone()) {
        Ok(r) => r,
        Err(e) => return error(400, "BadRequestException", &format!("requirements: {e}")),
    };
    let updates: Vec<TableUpdate> = match serde_json::from_value(body["updates"].clone()) {
        Ok(u) => u,
        Err(e) => return error(400, "BadRequestException", &format!("updates: {e}")),
    };
    for requirement in &requirements {
        if let Err(e) = requirement.check(Some(&current)) {
            return error(409, "CommitFailedException", &e.to_string());
        }
    }
    let next = match apply(&current, updates) {
        Ok(m) => m,
        Err(e) => return error(400, "BadRequestException", &e.to_string()),
    };
    state.tables.insert(key, next.clone());
    if let Some(Script::AppliedThenAnswer(status)) = script {
        return Response { status, headers: Vec::new(), body: Vec::new() };
    }
    let location = format!("s3://metadata/{ns}/{name}/{}.metadata.json", next.last_updated_ms());
    Response::json(200, &json!({ "metadata-location": location, "metadata": next }).to_string())
}
