//! `disclosure.release`: budget reservation before computing, exact noise, bounded sums,
//! noisy partition selection and the envelope.

use contextful_core::disclosure::noise::{discrete_laplace, statistic_noise, Entropy, Spend};
use contextful_core::disclosure::release::{release, Breakdown, MemoryLedger, ReleaseError, ReleaseRequest, UnitBudget};
use contextful_core::disclosure::suppress::DISCLOSURE_SENTINEL;
use contextful_core::disclosure::DisclosureError;
use contextful_core::pipeline::model::DisclosureDecl;
use serde_json::{json, Map, Value};

/// A seeded xorshift source, so every draw replays.
struct Seeded(u64);

impl Entropy for Seeded {
    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const EPSILON: u64 = 1_000_000;

fn policy() -> DisclosureDecl {
    toml::from_str(
        r#"
grouping_allowlist = ["industry"]
contributor_key = "tenant_id"
min_group_size = 3
max_contributor_share = 0.4
emit_sentinel = true
forbidden_columns = ["tenant_id"]
[metric_bounds]
revenue = { lower = 0, upper = 10000, quantum = 1 }
"#,
    )
    .unwrap()
}

fn rows(values: &[(&str, &str, f64)]) -> Vec<Map<String, Value>> {
    values
        .iter()
        .map(|(t, i, v)| json!({"industry": i, "tenant_id": t, "revenue": v}).as_object().unwrap().clone())
        .collect()
}

/// The benchmark breakdown: `energy` balanced across three tenants, `retail` dominated by one.
fn benchmark() -> Breakdown {
    Breakdown {
        columns: vec!["industry".into(), "tenant_id".into(), "revenue".into()],
        grain: vec!["industry".into(), "tenant_id".into()],
        rows: rows(&[
            ("t1", "energy", 100.0),
            ("t2", "energy", 110.0),
            ("t3", "energy", 95.0),
            ("t4", "retail", 9_000.0),
            ("t5", "retail", 40.0),
            ("t6", "retail", 35.0),
        ]),
    }
}

fn ledger(units: &[&str], cap: u64) -> MemoryLedger {
    let l = MemoryLedger::default();
    for u in units {
        l.grant(u, cap);
    }
    l
}

const TENANTS: [&str; 6] = ["t1", "t2", "t3", "t4", "t5", "t6"];

fn request(epsilon_micros: u64) -> ReleaseRequest {
    ReleaseRequest { release_id: "r-1".into(), table: "revenue_by_industry".into(), spend: Spend::from_micros(epsilon_micros).unwrap() }
}

fn refused(r: Result<impl std::fmt::Debug, ReleaseError>) -> DisclosureError {
    match r {
        Err(ReleaseError::Refused(e)) => e,
        other => panic!("{other:?}"),
    }
}

/// Before computing, a release reserves each contributing unit's declared per-run spend against its lifetime cap in one catalog transaction. A failed reservation raises `DisclosureUnitBudgetExhausted`, and the release publishes nothing.
// spec: disclosure.release.budget-reservation@3333631a
#[test]
fn a_release_reserves_every_units_spend_or_none_and_publishes_nothing_on_refusal() {
    let l = ledger(&TENANTS, EPSILON);
    let mut rng = Seeded(7);
    release(&policy(), &benchmark(), &request(600_000), &l, &mut rng).unwrap();
    for t in TENANTS {
        assert_eq!(l.budget(t), Some(UnitBudget { cap: EPSILON, spent: 600_000 }), "{t}");
    }

    // A second release overruns every cap: it refuses, and no unit is charged.
    let e = refused(release(&policy(), &benchmark(), &request(600_000), &l, &mut rng));
    assert_eq!(e.identifier(), "DisclosureUnitBudgetExhausted");
    assert!(!e.to_string().contains("t1"), "the refusal names no unit: {e}");
    for t in TENANTS {
        assert_eq!(l.budget(t).unwrap().spent, 600_000, "{t}");
    }

    // One unit short refuses the whole release; the units with room stay uncharged.
    let l = ledger(&TENANTS[..5], EPSILON);
    let e = refused(release(&policy(), &benchmark(), &request(100_000), &l, &mut rng));
    assert_eq!(e.identifier(), "DisclosureUnitBudgetExhausted");
    for t in &TENANTS[..5] {
        assert_eq!(l.budget(t).unwrap().spent, 0, "{t}");
    }
    assert_eq!(l.budget("t6"), None);
}

fn frequencies(draw: impl Fn(&mut Seeded) -> i64) -> (f64, f64, f64) {
    let mut rng = Seeded(0x9e37_79b9_7f4a_7c15);
    let n = 40_000;
    let (mut zero, mut one, mut sum) = (0u32, 0u32, 0i64);
    for _ in 0..n {
        let x = draw(&mut rng);
        sum += x;
        zero += (x == 0) as u32;
        one += (x == 1) as u32;
    }
    (zero as f64 / n as f64, one as f64 / n as f64, sum as f64 / n as f64)
}

/// A noised release draws exact two-sided geometric noise scaled to each statistic's sensitivity for distinct-contributor counts and fixed-point sums; the per-run epsilon is divided among released statistics.
// spec: disclosure.release.noise-mechanism@86a1baef
#[test]
fn noise_is_two_sided_geometric_at_sensitivity_over_the_divided_epsilon() {
    // Two-sided geometric with a = ε / Δ: P(0) = (1 - e^-a) / (1 + e^-a), P(1) / P(0) = e^-a.
    let p0 = |a: f64| (1.0 - (-a).exp()) / (1.0 + (-a).exp());
    let one = Spend::epsilon(1).unwrap();

    let (zero, plus_one, mean) = frequencies(|r| statistic_noise(r, 1, one, 1));
    assert!((zero - p0(1.0)).abs() < 0.01, "P(0) = {zero}");
    assert!((plus_one / zero - (-1.0f64).exp()).abs() < 0.03, "P(1)/P(0) = {}", plus_one / zero);
    assert!(mean.abs() < 0.05, "mean {mean}");

    // Dividing ε across two statistics, or doubling the sensitivity, halves a.
    let (split, _, _) = frequencies(|r| statistic_noise(r, 1, one, 2));
    let (wide, _, _) = frequencies(|r| statistic_noise(r, 2, one, 1));
    assert!((split - p0(0.5)).abs() < 0.01, "P(0) at ε/2 = {split}");
    assert!((wide - p0(0.5)).abs() < 0.01, "P(0) at Δ = 2 = {wide}");

    // The sampler is integer-only and symmetric: the scale 1/3 draw has P(0) = (1 - e^-3) / (1 + e^-3).
    let (tight, _, _) = frequencies(|r| discrete_laplace(r, 1, 3));
    assert!((tight - p0(3.0)).abs() < 0.01, "P(0) at scale 1/3 = {tight}");
}

/// A noised sum folds each contributor to one value, clips that total to declared finite bounds and rounds it to a positive quantum; absent bounds or quantum raise `DisclosureMetricBoundsAbsent`.
// spec: disclosure.release.metric-bounds@427737c1
#[test]
fn a_sum_folds_clips_and_quantizes_each_contributor_and_refuses_without_bounds() {
    let mut p = policy();
    p.max_contributor_share = None;
    p.metric_bounds.get_mut("revenue").unwrap().quantum = 10.0;
    let b = Breakdown {
        rows: rows(&[
            // t1's two rows fold to 12000 and clip to the upper bound before rounding.
            ("t1", "energy", 7_000.0),
            ("t1", "energy", 5_000.0),
            ("t2", "energy", 104.0),
            ("t3", "energy", -50.0),
        ]),
        ..benchmark()
    };
    // At a spend of 10^7 epsilon the noise is zero with probability past 1 - 10^-80.
    let l = ledger(&["t1", "t2", "t3"], u64::MAX);
    let out = release(&p, &b, &request(10_000_000 * EPSILON), &l, &mut Seeded(3)).unwrap();
    assert_eq!(out.table.rows.len(), 1);
    // 10000 + round(104 / 10) * 10 + 0 (clipped from -50).
    assert_eq!(out.table.rows[0]["revenue"], json!(10_100));

    let mut unbounded = policy();
    unbounded.metric_bounds.clear();
    let e = refused(release(&unbounded, &benchmark(), &request(EPSILON), &ledger(&TENANTS, EPSILON), &mut Seeded(1)));
    assert_eq!(e.identifier(), "DisclosureMetricBoundsAbsent");

    for (lower, upper, quantum) in [(0.0, 10.0, 0.0), (0.0, 10.0, -1.0), (0.0, f64::INFINITY, 1.0), (10.0, 0.0, 1.0)] {
        let mut bad = policy();
        let m = bad.metric_bounds.get_mut("revenue").unwrap();
        (m.lower, m.upper, m.quantum) = (lower, upper, quantum);
        let l = ledger(&TENANTS, EPSILON);
        let e = refused(release(&bad, &benchmark(), &request(EPSILON), &l, &mut Seeded(1)));
        assert_eq!(e.identifier(), "DisclosureMetricBoundsAbsent", "{lower} {upper} {quantum}");
        assert_eq!(l.budget("t1").unwrap().spent, 0, "a refused shape reserves nothing");
    }
}

/// A release reports its published table, policy hash, reserved per-run spend, suppressed-group presence and row count without carrying a contributor key.
// spec: disclosure.release.release-envelope@91c9f5ff
#[test]
fn the_envelope_reports_table_hash_spend_suppression_and_rows_and_no_contributor() {
    let out = release(&policy(), &benchmark(), &request(10_000_000 * EPSILON), &ledger(&TENANTS, u64::MAX), &mut Seeded(11)).unwrap();
    let e = &out.envelope;
    assert_eq!(e.table, "revenue_by_industry");
    assert!(e.policy_hash.starts_with("sha256:") && e.policy_hash.len() == 7 + 64, "{}", e.policy_hash);
    assert_eq!(e.reserved_spend, 10_000_000 * EPSILON);
    assert!(e.suppressed_present);
    assert_eq!(e.row_count, out.table.rows.len());
    let text = serde_json::to_string(e).unwrap();
    for leak in ["tenant_id", "t1", "t4"] {
        assert!(!text.contains(leak), "{leak} in {text}");
    }

    // The hash follows the policy, not the order of its set-valued fields.
    let mut reordered = policy();
    reordered.forbidden_columns = vec!["tenant_id".into()];
    reordered.grouping_allowlist.reverse();
    let again = release(&reordered, &benchmark(), &request(EPSILON), &ledger(&TENANTS, EPSILON), &mut Seeded(11)).unwrap();
    assert_eq!(again.envelope.policy_hash, e.policy_hash);
    let mut stricter = policy();
    stricter.min_group_size = Some(4);
    let other = release(&stricter, &benchmark(), &request(EPSILON), &ledger(&TENANTS, EPSILON), &mut Seeded(11)).unwrap();
    assert_ne!(other.envelope.policy_hash, e.policy_hash);
}

#[test]
fn the_balanced_group_publishes_and_the_dominated_one_collapses_into_one_sentinel() {
    let out = release(&policy(), &benchmark(), &request(10_000_000 * EPSILON), &ledger(&TENANTS, u64::MAX), &mut Seeded(5)).unwrap();
    assert_eq!(out.table.columns, vec!["industry".to_string(), "revenue".to_string()]);
    assert_eq!(out.table.rows.len(), 2);
    assert_eq!(out.table.rows[0]["industry"], json!("energy"));
    assert_eq!(out.table.rows[0]["revenue"], json!(305));
    assert_eq!(out.table.rows[1]["industry"], json!(DISCLOSURE_SENTINEL));
    assert_eq!(out.table.rows[1]["revenue"], Value::Null);
    let text = serde_json::to_string(&out.table).unwrap();
    assert!(!text.contains("retail") && !text.contains("tenant_id") && !text.contains("\"t"), "{text}");

    // Without the sentinel the withheld group leaves only the envelope's flag.
    let mut quiet = policy();
    quiet.emit_sentinel = false;
    let out = release(&quiet, &benchmark(), &request(10_000_000 * EPSILON), &ledger(&TENANTS, u64::MAX), &mut Seeded(5)).unwrap();
    assert_eq!(out.table.rows.len(), 1);
    assert!(out.envelope.suppressed_present);
}

#[test]
fn partition_selection_reads_a_noised_count_at_low_spend() {
    // At ε = 0.1 split two ways the count noise has scale 20, so a three-contributor
    // group under a floor of 3 publishes in some seeds and is withheld in others.
    let mut p = policy();
    p.max_contributor_share = None;
    let mut published = 0;
    for seed in 1..=200u64 {
        let out = release(&p, &benchmark(), &request(100_000), &ledger(&TENANTS, u64::MAX), &mut Seeded(seed)).unwrap();
        published += out.table.rows.iter().filter(|r| r["industry"] == json!("energy")).count();
    }
    assert!(published > 20 && published < 180, "energy published in {published} of 200 seeds");
}

/// A non-numeric, non-grain column rides the grouping, and one not constant within its group raises `DisclosureDuplicateGrain`.
// spec: disclosure.release.duplicate-grain@f3acc108
#[test]
fn a_carried_column_varying_within_its_group_raises_duplicate_grain() {
    let mut b = benchmark();
    b.columns.push("sector".into());
    for r in &mut b.rows {
        let sector = if r["industry"] == json!("energy") { "primary" } else { "tertiary" };
        r.insert("sector".into(), json!(sector));
    }
    let out = release(&policy(), &b, &request(10_000_000 * EPSILON), &ledger(&TENANTS, u64::MAX), &mut Seeded(2)).unwrap();
    assert_eq!(out.table.rows[0]["sector"], json!("primary"));

    b.rows[1].insert("sector".into(), json!("secondary"));
    let l = ledger(&TENANTS, EPSILON);
    let e = refused(release(&policy(), &b, &request(EPSILON), &l, &mut Seeded(2)));
    assert_eq!(e.identifier(), "DisclosureDuplicateGrain");
    assert_eq!(l.budget("t1").unwrap().spent, 0);
}

/// A group key not a subset of the permitted grouping columns raises `DisclosureGroupKeyNotPermitted`.
// spec: disclosure.release.group-key@899ddd75
#[test]
fn a_group_key_outside_the_allowlist_raises_group_key_not_permitted() {
    let mut b = benchmark();
    b.columns.insert(1, "region".into());
    b.grain = vec!["industry".into(), "region".into(), "tenant_id".into()];
    for r in &mut b.rows {
        r.insert("region".into(), json!("emea"));
    }
    let l = ledger(&TENANTS, EPSILON);
    let e = refused(release(&policy(), &b, &request(EPSILON), &l, &mut Seeded(1)));
    assert_eq!(e.identifier(), "DisclosureGroupKeyNotPermitted");
    assert!(e.to_string().contains("region"), "{e}");
    assert_eq!(l.budget("t1").unwrap().spent, 0);

    let mut wider = policy();
    wider.grouping_allowlist.push("region".into());
    assert!(release(&wider, &b, &request(EPSILON), &l, &mut Seeded(1)).is_ok());
}

/// A governed statement omitting the contributor key raises `DisclosureContributorKeyAbsent`.
// spec: disclosure.release.contributor-key@2fd7c65a
#[test]
fn a_statement_without_the_contributor_key_raises_contributor_key_absent() {
    let mut b = benchmark();
    b.columns.retain(|c| c != "tenant_id");
    b.grain = vec!["industry".into()];
    for r in &mut b.rows {
        r.remove("tenant_id");
    }
    let l = ledger(&TENANTS, EPSILON);
    let e = refused(release(&policy(), &b, &request(EPSILON), &l, &mut Seeded(1)));
    assert_eq!(e.identifier(), "DisclosureContributorKeyAbsent");
    assert!(TENANTS.iter().all(|t| l.budget(t).unwrap().spent == 0));
}

/// A contract declaring a forbidden column, or a materialization producing one, raises `DisclosureForbiddenColumnPublished`. The check runs after the contributor key leaves the published columns.
// spec: disclosure.release.forbidden-column@4dccb076
#[test]
fn a_published_forbidden_column_raises_and_the_contributor_key_does_not() {
    // `tenant_id` is forbidden and the contributor key: it leaves before the check.
    assert!(release(&policy(), &benchmark(), &request(EPSILON), &ledger(&TENANTS, EPSILON), &mut Seeded(1)).is_ok());

    let mut b = benchmark();
    b.columns.push("account_email".into());
    for r in &mut b.rows {
        r.insert("account_email".into(), json!("ops@example.com"));
    }
    let mut p = policy();
    p.forbidden_columns.push("account_email".into());
    let l = ledger(&TENANTS, EPSILON);
    let e = refused(release(&p, &b, &request(EPSILON), &l, &mut Seeded(1)));
    assert_eq!(e.identifier(), "DisclosureForbiddenColumnPublished");
    assert!(e.to_string().contains("account_email"), "{e}");
    assert_eq!(l.budget("t1").unwrap().spent, 0);
}
