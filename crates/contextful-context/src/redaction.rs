//! Canonical declared writer policy, applied before persistence and replay comparison.
use crate::land::Batch;
use crate::{ContextError, Result};
use contextful_core::redaction::{CompiledRule, Matcher, Operation, RULES_PER_PIPELINE};
use contextful_core::store::declare::TableDecl;
use contextful_core::store::reconcile::ColumnType;
use contextful_policy::enforce::mask::{class, ColumnPolicy, Pepper};
use contextful_policy::enforce::policy::TablePolicy;
use serde_json::json;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn invalid(e: impl std::fmt::Display) -> ContextError {
    ContextError::Invalid(e.to_string())
}

#[derive(Debug, Clone)]
enum Substitute {
    Drop,
    Marker(String),
    Policy(Box<ColumnPolicy>),
}

impl Substitute {
    fn selector(&self) -> Option<&str> {
        match self {
            Self::Policy(policy) => policy.selector_column(),
            _ => None,
        }
    }

    fn apply(
        &self,
        pepper: &Pepper,
        value: &str,
        selected: Option<&str>,
        ty: &ColumnType,
    ) -> std::result::Result<Option<String>, contextful_core::enforce::EnforceError> {
        Ok(match self {
            Self::Drop => Some(String::new()),
            Self::Marker(marker) => Some(marker.clone()),
            Self::Policy(policy) => {
                if !policy.admits(ty) {
                    return Err(contextful_core::enforce::EnforceError::RedactionInvalid("writer substitution is outside the selected value's type".into()));
                }
                let mask = policy.mask_for(selected).expect("compiled writer substitution carries a mask");
                let result = mask.apply(pepper, Some(value), ty);
                if result.is_none() && mask.strategy_name() != "drop" {
                    return Err(contextful_core::enforce::EnforceError::RedactionInvalid("writer substitution cannot decode its typed source value".into()));
                }
                result
            }
        })
    }
}

fn selected(value: &serde_json::Value) -> Result<Option<String>> {
    match value {
        serde_json::Value::Null => Ok(None),
        serde_json::Value::String(value) => Ok(Some(value.clone())),
        _ => Err(invalid("a writer class selector contains text or null")),
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecordedTable {
    pub rows: Vec<contextful_core::run::ports::Row>,
    pub types: BTreeMap<String, String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordingPayload {
    version: u8,
    table: String,
    authority: String,
    normalize: Option<(String, u32)>,
    tables: BTreeMap<String, RecordedTable>,
    signature: String,
}

/// Canonical rewritten values admitted for journal replay; callers cannot construct one.
pub struct PreparedRecording(RecordingPayload);

impl PreparedRecording {
    pub fn encode(&self) -> Result<Vec<u8>> { serde_json::to_vec(&self.0).map_err(invalid) }
    pub(crate) fn tables(&self) -> &BTreeMap<String, RecordedTable> { &self.0.tables }
    pub(crate) fn table(&self) -> &str { &self.0.table }
    pub(crate) fn normalize(&self) -> Result<Option<contextful_core::pipeline::normalize::Normalize>> {
        self.0.normalize.as_ref().map(|(mode, depth)| {
            use contextful_core::pipeline::normalize::{Mode, Normalize};
            let mode = match mode.as_str() { "native" => Mode::Native, "relational" => Mode::Relational, _ => return Err(invalid("recorded normalization mode is unknown")) };
            Ok(Normalize { mode, depth:*depth })
        }).transpose()
    }
}

fn normalization(normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Option<(String, u32)> {
    normalize.map(|n| (match n.mode { contextful_core::pipeline::normalize::Mode::Native => "native", contextful_core::pipeline::normalize::Mode::Relational => "relational" }.into(), n.depth))
}

#[derive(Debug, Clone)]
struct WriterRule {
    rule: CompiledRule,
    substitute: Substitute,
}

impl WriterRule {
    fn whole(&self) -> bool {
        matches!(self.rule.rule.matcher, Matcher::Whole(_)) && self.rule.rule.json_path.is_none()
    }

    fn substitution_type(&self, source: &ColumnType) -> std::result::Result<ColumnType, contextful_core::enforce::EnforceError> {
        if self.whole() {
            return Ok(source.clone());
        }
        if self.rule.rule.json_path.is_some() && (source.is_binary() || source.is_vector() || source.is_nested()) {
            return Err(contextful_core::enforce::EnforceError::RedactionInvalid(
                "JSON-path removal requires JSON value lineage, not an encoded typed value".into(),
            ));
        }
        if self.rule.rule.json_path.is_none() && *source != ColumnType::Utf8 && *source != ColumnType::Null {
            return Err(contextful_core::enforce::EnforceError::RedactionInvalid("pattern removal selects text values alone".into()));
        }
        Ok(ColumnType::Utf8)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Writer {
    tables: BTreeMap<String, Vec<WriterRule>>,
    declared: BTreeSet<String>,
    source_types: BTreeMap<String, BTreeMap<String, ColumnType>>,
    protected_relational: bool,
    pepper: Pepper,
    manifest_identity: String,
}

impl Writer {
    pub fn validate_recording_clock(&self, table: &str, columns: &BTreeSet<String>, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        if self.recording_identity(table, normalize)?.is_none() { return Ok(()); }
        let relational = normalize.is_some_and(|n| n.mode == contextful_core::pipeline::normalize::Mode::Relational);
        if relational || self.tables.get(table).is_some_and(|rules| rules.iter().any(|rule| columns.contains(&rule.rule.rule.column))) {
            return Err(invalid("protected cursor continuation requires safe typed progress lineage"));
        }
        Ok(())
    }
    pub fn validate_source_plan(&self, plan: &contextful_core::run::plan::Plan, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        plan.validate()?;
        let mut landing = plan.clone();
        landing.spec.journal = false;
        self.validate_plan(&landing, normalize)?;
        if plan.spec.journal && self.recording_identity(&plan.spec.table, normalize)?.is_some() {
            if let Some(field) = &plan.spec.cursor.field {
                self.validate_recording_clock(&plan.spec.table, &BTreeSet::from([field.clone()]), normalize)?;
            }
        }
        Ok(())
    }

    pub fn validate_recorded_control(&self, table: &str, pull: &contextful_core::run::ports::Pull) -> Result<()> {
        let Some(cursor) = &pull.cursor else { return Ok(()); };
        if cursor.is_null() || (!pull.more && *cursor == json!({"next":null})) { return Ok(()); }
        let Some(text) = cursor.as_str() else { return Err(invalid("protected source continuation requires safe typed progress lineage")); };
        for rule in self.tables.values().flatten() {
            if rule.rule.rule.json_path.is_some() || rule.whole() {
                return Err(invalid("protected source continuation requires safe typed progress lineage"));
            }
            let mut value = serde_json::Value::String(text.into());
            rule.rule.rewrite(&mut value, &|_, _| Ok(Some("[removed]".into()))).map_err(invalid)?;
            if value != *cursor { return Err(invalid(format!("protected source continuation matches removal authority for `{table}`"))); }
        }
        Ok(())
    }
    pub fn recording_identity(&self, table: &str, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<Option<String>> {
        let relational = normalize.is_some_and(|n| n.mode == contextful_core::pipeline::normalize::Mode::Relational);
        let protected = self.tables.get(table).is_some_and(|rules| !rules.is_empty()) || (relational && self.tables.values().any(|rules| !rules.is_empty()));
        if !protected { return Ok(None); }
        if !self.declared.contains(table) { return Err(invalid("prepared recording requires a declared destination")); }
        let identity = serde_json::to_vec(&("prepared-pull-v1", &self.manifest_identity, table, normalization(normalize), self.pepper.digest("prepared-authority-v1"))).map_err(invalid)?;
        Ok(Some(contextful_core::run::journal::sha256_hex(&identity)))
    }

    pub fn prepare_recording(&self, table: &str, batch: &Batch, normalize: Option<contextful_core::pipeline::normalize::Normalize>, load_id: &str) -> Result<PreparedRecording> {
        let authority = self.recording_identity(table, normalize)?.ok_or_else(|| invalid("prepared recording requires canonical removal authority"))?;
        let tables = if let Some(n) = normalize.filter(|n| n.mode == contextful_core::pipeline::normalize::Mode::Relational) {
            self.group(contextful_core::pipeline::normalize::NormalizedGroup::new(batch.rows.clone(), table, load_id, n.depth))?.into_iter().map(|(name, rows)| {
                let types = if name == table { batch.types.iter().map(|(name, ty)| (name.clone(), ty.name())).collect() } else { [("list_index".into(), "Int64".into())].into() };
                (name, RecordedTable { rows, types })
            }).collect()
        } else {
            let rewritten = self.batch(table, batch)?;
            [(table.into(), RecordedTable { rows:rewritten.rows, types:rewritten.types.iter().map(|(name, ty)| (name.clone(), ty.name())).collect() })].into()
        };
        let mut payload = RecordingPayload { version:1, table:table.into(), authority, normalize:normalization(normalize), tables, signature:String::new() };
        payload.signature = self.pepper.digest_bytes(&serde_json::to_vec(&payload).map_err(invalid)?);
        Ok(PreparedRecording(payload))
    }

    pub fn admit_recording(&self, table: &str, bytes: &[u8], normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<PreparedRecording> {
        let mut payload: RecordingPayload = serde_json::from_slice(bytes).map_err(invalid)?;
        let identity = self.recording_identity(table, normalize)?.ok_or_else(|| invalid("prepared recording has no canonical authority"))?;
        if payload.version != 1 || payload.table != table || payload.authority != identity || payload.normalize != normalization(normalize) || !payload.tables.contains_key(table) {
            return Err(invalid("prepared recording authority or destination changed"));
        }
        let signature = std::mem::take(&mut payload.signature);
        if signature != self.pepper.digest_bytes(&serde_json::to_vec(&payload).map_err(invalid)?) { return Err(invalid("prepared recording values changed")); }
        for recorded in payload.tables.values() {
            for ty in recorded.types.values() {
                if ColumnType::parse(ty).is_none() { return Err(invalid("prepared recording carries an unknown type")); }
            }
        }
        payload.signature = signature;
        Ok(PreparedRecording(payload))
    }
    pub fn validate_recording(&self, table: &str, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        let relational = normalize.is_some_and(|n| n.mode == contextful_core::pipeline::normalize::Mode::Relational);
        let protected = self.tables.get(table).is_some_and(|rules| !rules.is_empty()) || (relational && self.tables.values().any(|rules| !rules.is_empty()));
        if protected {
            return Err(contextful_core::run::RunError::JournalRedactionConflict(format!(
                "canonical writer rules bind destination `{table}` before source pulls or recorded body calls; verbatim recording requires typed removal lineage"
            )).into());
        }
        Ok(())
    }

    pub fn validate_plan(&self, plan: &contextful_core::run::plan::Plan, normalize: Option<contextful_core::pipeline::normalize::Normalize>) -> Result<()> {
        plan.validate()?;
        let rules = self.tables.get(&plan.spec.table).map(Vec::as_slice).unwrap_or_default();
        if plan.spec.journal {
            self.validate_recording(&plan.spec.table, normalize)?;
        }
        if !plan.spec.redaction.is_empty() && plan.spec.redaction != rules.iter().map(|rule| rule.rule.rule.clone()).collect::<Vec<_>>() {
            return Err(invalid("a source plan's explicit removal list must equal its complete canonical writer rules"));
        }
        Ok(())
    }

    pub fn group(&self, mut group: contextful_core::pipeline::normalize::NormalizedGroup) -> Result<BTreeMap<String, Vec<contextful_core::run::ports::Row>>> {
        let root = group.root().to_string();
        if self.protected_relational && !self.declared.contains(group.root()) {
            return Err(invalid("normalized group requires a canonically declared root"));
        }
        for table in group.destinations().iter().filter(|table| *table != &root) {
            if let Some(rules) = self.tables.get(table) {
                for rule in rules {
                    if rule.substitute.selector().is_some() {
                        return Err(invalid("a child row selector requires typed sibling-row lineage"));
                    }
                    if !group.cells().iter().any(|cell| &cell.table == table && cell.column == rule.rule.rule.column) {
                        return Err(invalid(format!("removal column `{}` is absent from normalized child `{table}`", rule.rule.rule.column)));
                    }
                }
            }
        }
        let rules = self.tables.get(group.root()).map(Vec::as_slice).unwrap_or_default();
        let mut selectors = Vec::new();
        for rule in rules {
            if !group.has_column(&rule.rule.rule.column) {
                return Err(invalid(format!("removal column `{}` is absent from normalized root `{}`", rule.rule.rule.column, group.root())));
            }
            selectors.push(match rule.substitute.selector() {
                Some(column) => Some(
                    group
                        .column_values(column)
                        .ok_or_else(|| invalid(format!("writer class selector `{column}` is absent")))?
                        .iter()
                        .map(selected)
                        .collect::<Result<Vec<_>>>()?,
                ),
                None => None,
            });
        }
        group
            .rewrite_cells(&mut |cell, value| {
                let original = value.clone();
                let mut root_type = if cell.table != root {
                    self.source_types.get(&cell.table).and_then(|types| types.get(&cell.column)).cloned().unwrap_or(ColumnType::Utf8)
                } else if cell.path.is_empty() {
                    self.source_types.get(&root).and_then(|types| types.get(&cell.source_column)).cloned().unwrap_or(ColumnType::Utf8)
                } else {
                    ColumnType::Utf8
                };
                for (index, rule) in rules.iter().enumerate().filter(|(_, rule)| rule.rule.rule.column == cell.source_column) {
                    let selected = selectors[index].as_ref().and_then(|s| s[cell.source_row].as_deref());
                    let ty = rule.substitution_type(&root_type)?;
                    rule.rule.rewrite_projected(value, &cell.path, &|_, value| rule.substitute.apply(&self.pepper, value, selected, &ty))?;
                    if rule.whole() && rule.rule.rule.operation != Operation::Drop {
                        root_type = ColumnType::Utf8;
                    }
                }
                if cell.table != root {
                    if let Some(child_rules) = self.tables.get(&cell.table) {
                        let mut child_type = if *value != original {
                            ColumnType::Utf8
                        } else {
                            self.source_types.get(&cell.table).and_then(|types| types.get(&cell.column)).cloned().unwrap_or(ColumnType::Utf8)
                        };
                        for rule in child_rules.iter().filter(|rule| rule.rule.rule.column == cell.column) {
                            let ty = rule.substitution_type(&child_type)?;
                            rule.rule.rewrite(value, &|_, value| rule.substitute.apply(&self.pepper, value, None, &ty))?;
                            if rule.whole() && rule.rule.rule.operation != Operation::Drop {
                                child_type = ColumnType::Utf8;
                            }
                        }
                    }
                }
                Ok(())
            })
            .map_err(invalid)?;
        Ok(group.into_tables())
    }
    pub fn open(project_dir: &Path) -> Result<Self> {
        Self::open_declared(project_dir, None)
    }

    pub fn open_declared(project_dir: &Path, extra: Option<&Path>) -> Result<Self> {
        let declaration = project_dir.join(crate::project::DECLARATION_FILE);
        let mut files = crate::project::manifests(&declaration)?;
        if let Some(extra) = extra {
            files.extend(crate::project::manifests(extra)?);
        }
        let mut seen = BTreeSet::new();
        let mut unique = Vec::new();
        for file in files {
            let path = std::fs::canonicalize(&file.path).map_err(invalid)?;
            if seen.insert(path) {
                unique.push(file);
            }
        }
        let files = unique;
        let manifest_identity = contextful_core::run::journal::sha256_hex(&serde_json::to_vec(&files.iter().map(|file| &file.text).collect::<Vec<_>>()).map_err(invalid)?);
        contextful_core::pipeline::declare::collect(&files).map_err(invalid)?;
        let mut decls = Vec::new();
        let mut protected_relational = false;
        for file in &files {
            let specs = contextful_core::pipeline::declare::read_manifest(file).map_err(invalid)?;
            if specs.is_empty() && file.path.ends_with(".toml") {
                decls.extend(TableDecl::parse_pipeline(&file.text).map_err(invalid)?);
            }
            for declared in specs {
                let spec = declared.spec;
                spec.validate().map_err(invalid)?;
                for table in &spec.tables {
                    let decl = spec.destination_decl(table);
                    if contextful_core::pipeline::normalize::Normalize::parse(spec.normalize.as_ref()).map_err(invalid)?.mode
                        == contextful_core::pipeline::normalize::Mode::Relational
                    {
                        protected_relational = true;
                    }
                    decls.push(decl);
                }
            }
        }
        let mut authority = BTreeMap::<String, TableDecl>::new();
        for decl in &decls {
            let combined = authority.entry(decl.name.clone()).or_insert_with(|| TableDecl::named(&decl.name));
            for (key, value) in decl.columns.iter().flatten() {
                let columns = combined.columns.get_or_insert_with(BTreeMap::new);
                if columns.get(key).is_some_and(|existing| ColumnType::parse(existing) != ColumnType::parse(value)) {
                    return Err(invalid(format!("conflicting declared type for `{}` column `{key}`", decl.name)));
                }
                columns.entry(key.clone()).or_insert_with(|| value.clone());
            }
            for (name, current, incoming) in [("class", &mut combined.class, &decl.class), ("policy", &mut combined.policy, &decl.policy)] {
                if current.is_some() && incoming.is_some() && current != incoming {
                    return Err(invalid(format!("conflicting writer {name} for `{}`", decl.name)));
                }
                if current.is_none() { *current = incoming.clone(); }
            }
            for rule in decl.redaction.iter().flatten() {
                let rules = combined.redaction.get_or_insert_with(Vec::new);
                if rules.contains(rule) {
                    return Err(invalid(format!("duplicate removal rule for `{}` column `{}`", decl.name, rule.column)));
                }
                if rules.len() >= RULES_PER_PIPELINE {
                    return Err(invalid("combined removal rule count exceeds its bound"));
                }
                rules.push(rule.clone());
            }
        }
        for mut decl in decls {
            let combined = &authority[&decl.name];
            decl.redaction = combined.redaction.clone();
            decl.columns = combined.columns.clone();
            decl.validate_index_declaration().map_err(invalid)?;
        }
        let mut tables = BTreeMap::new();
        let mut declared = BTreeSet::new();
        let mut source_types = BTreeMap::new();
        for decl in authority.into_values() {
            declared.insert(decl.name.clone());
            source_types.insert(decl.name.clone(), decl.column_types());
            let Some(rules) = &decl.redaction else { continue };
            if rules.len() > RULES_PER_PIPELINE {
                return Err(invalid("removal rule count exceeds its bound"));
            }
            let policy = TablePolicy::from_decl(&decl).map_err(invalid)?;
            let mut compiled = Vec::new();
            for rule in rules {
                if rule.table != decl.name {
                    return Err(invalid("a table removal rule names another table"));
                }
                let substitute = match rule.operation {
                    Operation::Drop => Substitute::Drop,
                    Operation::Replace => {
                        let name = rule.argument.as_ref().and_then(|v| v.as_str()).ok_or_else(|| invalid("replace names a class"))?;
                        class(name).map_err(invalid)?;
                        Substitute::Marker(format!("[REDACTED:{name}]"))
                    }
                    operation => {
                        let declared = policy.columns.get(&rule.column);
                        let strategy = match operation {
                            Operation::Hash => "hash".to_string(),
                            Operation::Tokenize => "tokenize".to_string(),
                            Operation::Truncate => {
                                format!("truncate:{}", rule.argument.as_ref().and_then(|v| v.as_u64()).ok_or_else(|| invalid("truncate names its width"))?)
                            }
                            _ => unreachable!(),
                        };
                        let mut raw = json!({"strategy": strategy});
                        let original = decl.policy.as_ref().and_then(|p| p.get("columns")).and_then(|c| c.get(&rule.column));
                        if declared.is_some_and(|p| p.selector_column().is_some()) {
                            let original = original.ok_or_else(|| invalid("row-selected policy has no declaration"))?;
                            raw["class"] = original["class"].clone();
                            raw["fallback"] = json!("drop");
                            let strategies = original["strategies"].as_object().ok_or_else(|| invalid("row-selected policy has no strategy map"))?;
                            raw["strategies"] = serde_json::Value::Object(strategies.keys().map(|key| (key.clone(), json!(strategy))).collect());
                            raw.as_object_mut().expect("a compiled policy is an object").remove("strategy");
                        } else if let Some(class) = declared.and_then(|p| p.class) {
                            raw["class"] = json!(class.name);
                        } else if let Some(class) = &decl.class {
                            raw["class"] = class.clone();
                        }
                        // A declared inseparable combine follows the column's digest.
                        if let Some(combine) =
                            decl.policy.as_ref().and_then(|p| p.get("columns")).and_then(|c| c.get(&rule.column)).and_then(|p| p.get("combine"))
                        {
                            raw["combine"] = combine.clone();
                        }
                        let parsed = ColumnPolicy::parse(&rule.column, serde_json::from_value(raw).map_err(invalid)?).map_err(invalid)?;
                        Substitute::Policy(Box::new(parsed))
                    }
                };
                compiled.push(WriterRule { rule: CompiledRule::compile(rule.clone()).map_err(invalid)?, substitute });
            }
            tables.insert(decl.name.clone(), compiled);
        }
        protected_relational &= tables.values().any(|rules| !rules.is_empty());
        Ok(Self { tables, declared, source_types, protected_relational, pepper: Pepper::resolve(|key| std::env::var(key).ok()), manifest_identity })
    }

    pub fn batch(&self, table: &str, batch: &Batch) -> Result<Batch> {
        if self.protected_relational && !self.declared.contains(table) {
            return Err(invalid(format!("undeclared destination `{table}` requires canonical normalized-group lineage")));
        }
        let Some(rules) = self.tables.get(table) else { return Ok(batch.clone()) };
        let mut source_types: BTreeMap<_, _> = crate::land::batch_schema(batch)?.columns.into_iter().map(|c| (c.name, c.ty)).collect();
        if let Some(declared) = self.source_types.get(table) {
            source_types.extend(declared.clone());
        }
        let mut batch = batch.clone();
        for row in &mut batch.rows {
            let mut types = source_types.clone();
            let selectors = rules
                .iter()
                .map(|rule| {
                    rule.substitute
                        .selector()
                        .map(|column| row.get(column).ok_or_else(|| invalid(format!("writer class selector `{column}` is absent"))).and_then(selected))
                        .transpose()
                        .map(Option::flatten)
                })
                .collect::<Result<Vec<_>>>()?;
            for (index, rule) in rules.iter().enumerate() {
                let column = &rule.rule.rule.column;
                let whole = rule.whole();
                let ty = types.get(column).ok_or_else(|| invalid(format!("removal column `{column}` is absent from `{table}`")))?;
                let value = row.get_mut(column).ok_or_else(|| invalid(format!("removal column `{}` is absent from `{table}`", rule.rule.rule.column)))?;
                let selected_type = rule.substitution_type(ty).map_err(invalid)?;
                rule.rule
                    .rewrite(value, &|_, value| rule.substitute.apply(&self.pepper, value, selectors[index].as_deref(), &selected_type))
                    .map_err(invalid)?;
                if whole && rule.rule.rule.operation != Operation::Drop {
                    types.insert(column.clone(), ColumnType::Utf8);
                    batch.types.insert(column.clone(), ColumnType::Utf8);
                }
            }
        }
        Ok(batch)
    }
}
