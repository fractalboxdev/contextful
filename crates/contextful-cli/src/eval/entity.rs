//! The native entity benchmark adapts the admitted knowledge-card, recall and ranked reads.

use super::*;
use contextful_context::read::explain::ExplainRequest;
use contextful_context::read::recall::RecallRequest;
use contextful_core::memory::resolve;
use contextful_core::memory::synthesize::EvidenceRef;
use contextful_eval::entity::{self, Answer, Claim, Manifest};
use serde_json::json;
use sha2::{Digest, Sha256};

#[derive(clap::Args)]
pub struct Args {
    /// Versioned, adjudicated native fixture manifest.
    #[arg(long)]
    manifest: PathBuf,
    #[arg(long)]
    report: Option<PathBuf>,
    /// A version/config-bound envelope holding ordinary baseline metrics.
    #[arg(long)]
    baseline: Option<PathBuf>,
    #[command(flatten)]
    admit: AdmitArgs,
}

fn corpus_digest(corpus: &Corpus) -> Result<String> {
    let mut paths = vec![corpus.dir.join(CORPUS_MANIFEST)];
    paths.extend(corpus.tables()?.into_iter().map(|(_, p)| p));
    paths.sort();
    let mut hash = Sha256::new();
    for path in paths {
        let relative = path
            .strip_prefix(&corpus.dir)?
            .components()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        hash.update(relative.as_bytes());
        hash.update([0]);
        hash.update(std::fs::read(path)?);
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn objects(response: &contextful_core::read::respond::Response) -> Vec<Value> {
    response
        .rows
        .iter()
        .map(|row| {
            Value::Object(
                response
                    .columns
                    .iter()
                    .cloned()
                    .zip(row.iter().cloned())
                    .collect(),
            )
        })
        .collect()
}

fn claim(row: &Value) -> Result<Claim> {
    let refs: Vec<EvidenceRef> = serde_json::from_str(
        row["evidence"]
            .as_str()
            .context("a memory claim has evidence references")?,
    )?;
    Ok(Claim {
        id: field(row, "claim_id")?,
        subject: field(row, "subject")?,
        predicate: field(row, "predicate")?,
        object: field(row, "object")?,
        citations: refs.iter().map(EvidenceRef::label).collect(),
    })
}

fn field(row: &Value, key: &str) -> Result<String> {
    row[key]
        .as_str()
        .map(str::to_string)
        .with_context(|| format!("the benchmark row has no `{key}`"))
}

fn sequence(value: &Value) -> Result<i64> {
    value
        .as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| value.as_i64())
        .context("the evidence row has an integer sequence")
}

fn recalled(
    face: &Face,
    session: &Session,
    manifest: &Manifest,
    subject: &str,
    anchor: Instant,
) -> Result<Vec<Claim>> {
    objects(&face.recall(
        session,
        &RecallRequest::new(&manifest.claims, subject, anchor),
    )?)
    .iter()
    .map(claim)
    .collect()
}

struct Eligible {
    references: BTreeSet<String>,
    ids: Vec<String>,
    by_id: BTreeMap<String, String>,
}

/// Both arms receive the evidence admitted by the same recall gate and reader session.
fn eligible(
    face: &Face,
    session: &Session,
    manifest: &Manifest,
    anchor: Instant,
) -> Result<Eligible> {
    let mut references = BTreeSet::new();
    for record in face.entities(session, &manifest.entities)? {
        for claim in recalled(face, session, manifest, &record.entity.entity_id, anchor)? {
            references.extend(claim.citations);
        }
    }
    let evidence = table_rows(face, session, &manifest.evidence)?;
    let mut by_id = BTreeMap::new();
    for row in evidence {
        let reference = EvidenceRef::row(
            &manifest.evidence,
            row["_run_id"].as_str().context("evidence run id")?,
            sequence(&row["_row_seq"])?,
        )
        .label();
        if references.contains(&reference) {
            by_id.insert(
                row["evidence_id"]
                    .as_str()
                    .context("evidence primary key")?
                    .to_string(),
                reference,
            );
        }
    }
    let ids = by_id.keys().cloned().collect();
    Ok(Eligible {
        references,
        ids,
        by_id,
    })
}

fn structured(
    face: &Face,
    session: &Session,
    manifest: &Manifest,
    case: &entity::Case,
    anchor: Instant,
) -> Result<Answer> {
    let request = ExplainRequest::new(&manifest.entities, &case.mention, anchor);
    let card = face.explain(session, &request)?;
    let id = card["entity"]["entity_id"].as_str().map(str::to_string);
    if id.is_none() {
        return Ok(Answer {
            entity: None,
            claims: vec![],
            reason: Some(
                if card["candidates"].as_array().is_some_and(|c| !c.is_empty()) {
                    "ambiguous_entity"
                } else {
                    "absent_entity"
                }
                .into(),
            ),
        });
    }
    let mut subject = id.clone().unwrap();
    let mut claims = Vec::new();
    for relation in &case.relations {
        let next = recalled(face, session, manifest, &subject, anchor)?;
        let selected = entity::follow(Some(subject), std::slice::from_ref(relation), &next);
        if selected.reason.is_some() {
            return Ok(Answer {
                entity: id,
                claims: vec![],
                reason: selected.reason,
            });
        }
        subject = selected.claims[0].object.clone();
        claims.extend(selected.claims);
    }
    Ok(Answer {
        entity: id,
        claims,
        reason: None,
    })
}

fn ordinary(
    face: &Face,
    session: &Session,
    manifest: &Manifest,
    case: &entity::Case,
    anchor: Instant,
    eligible: &Eligible,
) -> Result<Answer> {
    if eligible.ids.is_empty() {
        return Ok(Answer {
            entity: None,
            claims: vec![],
            reason: Some("missing_evidence".into()),
        });
    }
    let request = RetrieveRequest {
        filter: Some(json!({ "evidence_id": eligible.ids })),
        limit: Some(manifest.k as u64),
        ..RetrieveRequest::new(&manifest.evidence, &case.question, anchor)
    };
    let response = face.retrieve(session, &request, Bounds::default())?;
    let column = response
        .columns
        .iter()
        .position(|c| c == "_row")
        .context("ranked evidence rows")?;
    let mut claims: Vec<Claim> = Vec::new();
    for row in &response.rows {
        let r = &row[column];
        let evidence_id = field(r, "evidence_id")?;
        let citation = eligible
            .by_id
            .get(&evidence_id)
            .context("ranked row is outside the eligible evidence set")?
            .clone();
        let decoded = Claim {
            id: field(r, "claim_id")?,
            subject: field(r, "subject")?,
            predicate: field(r, "predicate")?,
            object: field(r, "object")?,
            citations: vec![citation],
        };
        // Every ranked row decoding one tuple cites that tuple together, in rank order.
        match claims.iter_mut().find(|c| c.same_tuple(&decoded)) {
            Some(claim) => claim.citations.extend(decoded.citations),
            None => claims.push(decoded),
        }
    }
    let subjects: BTreeSet<&str> = claims.iter().map(|c| c.subject.as_str()).collect();
    let identities: Vec<resolve::Entity> = face
        .entities(session, &manifest.entities)?
        .into_iter()
        .map(|r| r.entity)
        .filter(|e| subjects.contains(e.entity_id.as_str()))
        .collect();
    let id = match resolve::resolve(&case.mention, &identities) {
        resolve::Resolution::One(id) => Some(id),
        resolve::Resolution::Ambiguous(_) => {
            return Ok(Answer {
                entity: None,
                claims: vec![],
                reason: Some("ambiguous_entity".into()),
            })
        }
        resolve::Resolution::Unresolved => None,
    };
    Ok(entity::follow(id, &case.relations, &claims))
}

pub fn run(args: Args) -> Result<()> {
    let text = std::fs::read_to_string(&args.manifest)?;
    let manifest = Manifest::parse(&text).map_err(anyhow::Error::msg)?;
    let mut corpus = Corpus::open(
        &args
            .manifest
            .parent()
            .unwrap_or(Path::new("."))
            .join(&manifest.corpus),
    )?;
    corpus.decls = TableDecl::parse_declaration_set(&corpus.manifest, &[])?;
    if !corpus.labeled()? {
        return Err(EvalError::EvalCorpusUnlabeled {
            corpus: corpus.dir.display().to_string(),
        }
        .into());
    }
    let digest = corpus_digest(&corpus)?;
    if digest != manifest.corpus_sha256 {
        bail!("entity fixture corpus digest differs from its manifest");
    }
    let benchmark = json!({ "version": manifest.version, "fixture_sha256": entity::digest(text.as_bytes()), "corpus_sha256": digest, "adjudication": manifest.adjudication, "license": manifest.license });
    let baseline = args
        .baseline
        .as_ref()
        .map(|path| -> Result<Baselines> {
            let envelope: Value = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            if envelope["benchmark"] != benchmark {
                bail!("entity baseline fixture version/config differs from the run");
            }
            Ok(Baselines::parse(&serde_json::to_string(
                &envelope["metrics"],
            )?)?)
        })
        .transpose()?;
    let stamp = RunStamp {
        k: manifest.k,
        tier: Tier::Deterministic,
        model: None,
        samples: 1,
        reader: None,
        judge: None,
    };
    if let Some(b) = &baseline {
        b.check_run(&stamp)?;
    }
    let (authority, revocation) = args.admit.admit(None, "the entity evidence benchmark")?;
    let scratch = tempfile::tempdir()?;
    let store = Store::open(scratch.path(), SCRATCH_PROJECT)?;
    land_corpus(
        &corpus,
        &store,
        &StubEmbedder::new(0),
        Instant::from_unix_nanos(0)?,
    )?;
    let face = Face::open(
        store,
        &corpus.manifest,
        Pepper::resolve(|k| std::env::var(k).ok()),
    )?;
    let session = session(&face, &authority, &revocation, None)?;
    let mut cache = BTreeMap::new();
    let (mut structured_scores, mut ordinary_scores, mut cases) =
        (Vec::new(), Vec::new(), Vec::new());
    for case in &manifest.cases {
        let anchor = Instant::parse(&case.anchor)?;
        if !cache.contains_key(&case.anchor) {
            cache.insert(
                case.anchor.clone(),
                eligible(&face, &session, &manifest, anchor)?,
            );
        }
        let eligible = &cache[&case.anchor];
        let structured = structured(&face, &session, &manifest, case, anchor)?;
        let ordinary = ordinary(&face, &session, &manifest, case, anchor, eligible)?;
        let (s, o) = (
            entity::grade(&case.expected, &structured, &eligible.references),
            entity::grade(&case.expected, &ordinary, &eligible.references),
        );
        cases.push(json!({ "id": case.id, "tags": case.tags, "arms": {
            "structured": { "answer": structured, "scores": s, "eligible_evidence": eligible.ids },
            "ordinary": { "answer": ordinary, "scores": o, "eligible_evidence": eligible.ids }
        }}));
        structured_scores.push(s);
        ordinary_scores.push(o);
    }
    let report = json!({ "run": stamp, "seed": 0, "benchmark": benchmark, "n_cases": cases.len(),
        "entity_evidence": { "structured": entity::summarize(&structured_scores), "ordinary": entity::summarize(&ordinary_scores) }, "cases": cases });
    let rendered = serde_json::to_string_pretty(&report)? + "\n";
    match args.report {
        Some(path) => std::fs::write(path, rendered)?,
        None => print!("{rendered}"),
    }
    if let Some(baseline) = baseline {
        for comparison in baseline::comparisons(&report, &baseline)? {
            if comparison.outcome == Outcome::Regressed {
                bail!(
                    "entity baseline {} regressed: {}",
                    comparison.path,
                    comparison.current
                );
            }
        }
    }
    Ok(())
}
