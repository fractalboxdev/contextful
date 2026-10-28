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
    /// Apply nothing and answer `status` with no body, as a timeout before the catalog acts leaves it.
    NotAppliedThenAnswer(u16),
    /// Answer `status` with no body and hold the commit, judging and applying it only when the
    /// next commit request arrives, as a catalog still processing a timed-out request does.
    Deferred(u16),
    /// Drop the table and create it afresh under a new uuid first, as another party may.
    Recreate,
    /// Add a column to the current schema first, moving no snapshot.
    EvolveSchema,
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
    /// Vend credentials in the table `config` rather than in `storage-credentials`.
    vend_in_config: bool,
    /// A held commit and the table it targets ([`Script::Deferred`]).
    deferred: Option<((String, String), Value)>,
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

    /// Vend storage credentials in the table `config`, as some catalogs do, rather than in
    /// the REST specification's `storage-credentials`.
    pub fn vend_in_config(&self) {
        self.state.lock().unwrap().vend_in_config = true;
    }

    /// Drop `namespace.table`, as another party may, purging nothing.
    pub fn drop_table(&self, namespace: &str, table: &str) {
        self.state.lock().unwrap().tables.remove(&(namespace.to_string(), table.to_string()));
    }

    /// Make `namespace.table` partitioned by the identity of `column`, as another engine may.
    pub fn partition_by(&self, namespace: &str, table: &str, column: &str) {
        let mut state = self.state.lock().unwrap();
        let key = (namespace.to_string(), table.to_string());
        let current = state.tables[&key].clone();
        let source = current.current_schema().field_by_name(column).expect("the column exists").id;
        let spec = iceberg::spec::UnboundPartitionSpec::builder().add_partition_field(source, column, iceberg::spec::Transform::Identity).unwrap().build();
        let next = TableMetadataBuilder::new_from_metadata(current, None).add_default_partition_spec(spec).unwrap().build().unwrap().metadata;
        state.tables.insert(key, next);
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

/// A load result. Credentials are vended only to a request asking for them with
/// `X-Iceberg-Access-Delegation: vended-credentials`, as the REST specification has it.
fn load_result(state: &State, bucket: &str, s3: &str, namespace: &str, name: &str, metadata: &TableMetadata, request: &Request) -> Response {
    let mut config = json!({ "s3.endpoint": state.storage.clone().unwrap_or_else(|| s3.to_string()), "s3.path-style-access": "true" });
    let credentials = json!({ "s3.access-key-id": ACCESS_KEY, "s3.secret-access-key": SECRET_KEY, "s3.region": "us-east-1" });
    let delegated = request.header("x-iceberg-access-delegation").is_some_and(|h| h.split(',').any(|m| m.trim() == "vended-credentials"));
    let mut storage_credentials = json!([]);
    if delegated && state.vend_in_config {
        config.as_object_mut().unwrap().extend(credentials.as_object().unwrap().clone());
    } else if delegated {
        storage_credentials = json!([{ "prefix": format!("s3://{bucket}/"), "config": credentials }]);
    }
    let location = format!("s3://{bucket}/{namespace}/{name}/metadata/{}.metadata.json", metadata.last_updated_ms());
    Response::json(200, &json!({ "metadata-location": location, "metadata": metadata, "config": config, "storage-credentials": storage_credentials }).to_string())
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
            load_result(state, bucket, s3, ns, &name, &metadata, request)
        }
        ("GET", ["v1", _, "namespaces", ns, "tables", name]) => match state.tables.get(&(ns.to_string(), name.to_string())) {
            Some(metadata) => load_result(state, bucket, s3, ns, name, metadata, request),
            None => error(404, "NoSuchTableException", &format!("table `{ns}.{name}` does not exist")),
        },
        ("POST", ["v1", _, "namespaces", ns, "tables", name]) => commit(state, ns, name, request),
        ("DELETE", ["v1", _, "namespaces", ns, "tables", name]) => match state.tables.remove(&(ns.to_string(), name.to_string())) {
            Some(_) => Response { status: 204, headers: Vec::new(), body: Vec::new() },
            None => error(404, "NoSuchTableException", &format!("table `{ns}.{name}` does not exist")),
        },
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

/// Judge `body`'s requirements against `current` and apply its updates.
fn judge(current: &TableMetadata, body: &Value) -> Result<TableMetadata, Response> {
    let requirements: Vec<TableRequirement> = serde_json::from_value(body["requirements"].clone()).map_err(|e| error(400, "BadRequestException", &format!("requirements: {e}")))?;
    let updates: Vec<TableUpdate> = serde_json::from_value(body["updates"].clone()).map_err(|e| error(400, "BadRequestException", &format!("updates: {e}")))?;
    for requirement in &requirements {
        requirement.check(Some(current)).map_err(|e| error(409, "CommitFailedException", &e.to_string()))?;
    }
    apply(current, updates).map_err(|e| error(400, "BadRequestException", &e.to_string()))
}

fn commit(state: &mut State, ns: &str, name: &str, request: &Request) -> Response {
    state.commits += 1;
    let key = (ns.to_string(), name.to_string());
    // A held commit is judged now, ahead of the request that arrived after it.
    if let Some((held, body)) = state.deferred.take() {
        if let Some(Ok(next)) = state.tables.get(&held).map(|m| judge(m, &body)) {
            state.tables.insert(held, next);
        }
    }
    let Some(mut current) = state.tables.get(&key).cloned() else {
        return error(404, "NoSuchTableException", &format!("table `{ns}.{name}` does not exist"));
    };
    let script = state.scripts.pop_front();
    if let Some(Script::Deferred(status)) = script {
        state.deferred = Some((key, serde_json::from_slice(&request.body).unwrap_or_default()));
        return Response { status, headers: Vec::new(), body: Vec::new() };
    }
    if script == Some(Script::Recreate) {
        let creation = TableCreation::builder().name(name.to_string()).location(current.location().to_string()).schema(current.current_schema().as_ref().clone()).format_version(current.format_version()).build();
        current = TableMetadataBuilder::from_table_creation(creation).and_then(|b| b.build()).expect("the table recreates").metadata;
        state.tables.insert(key.clone(), current.clone());
    }
    if script == Some(Script::EvolveSchema) {
        let schema = current.current_schema();
        let mut fields: Vec<_> = schema.as_struct().fields().to_vec();
        fields.push(Arc::new(iceberg::spec::NestedField::optional(current.last_column_id() + 1, "added_elsewhere", iceberg::spec::Type::Primitive(iceberg::spec::PrimitiveType::String))));
        let evolved = Schema::builder().with_fields(fields).build().unwrap();
        current = TableMetadataBuilder::new_from_metadata(current, None).add_current_schema(evolved).unwrap().build().unwrap().metadata;
        state.tables.insert(key.clone(), current.clone());
    }
    if let Some(Script::Refuse(status)) = script {
        return error(status, "ServiceFailureException", "scripted refusal");
    }
    if let Some(Script::NotAppliedThenAnswer(status)) = script {
        return Response { status, headers: Vec::new(), body: Vec::new() };
    }
    if script == Some(Script::Compact) {
        current = compact(&current);
        state.tables.insert(key.clone(), current.clone());
    }
    let body: Value = match serde_json::from_slice(&request.body) {
        Ok(v) => v,
        Err(e) => return error(400, "BadRequestException", &e.to_string()),
    };
    let next = match judge(&current, &body) {
        Ok(m) => m,
        Err(refusal) => return refusal,
    };
    state.tables.insert(key, next.clone());
    if let Some(Script::AppliedThenAnswer(status)) = script {
        return Response { status, headers: Vec::new(), body: Vec::new() };
    }
    let location = format!("s3://metadata/{ns}/{name}/{}.metadata.json", next.last_updated_ms());
    Response::json(200, &json!({ "metadata-location": location, "metadata": next }).to_string())
}
