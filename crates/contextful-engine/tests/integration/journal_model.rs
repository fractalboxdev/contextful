//! `run.journal` as a state machine: three callers interleave [`Journal::step`] one port
//! call at a time over two keys, with lapsing leases, crashes, notes, sweeps and
//! retirement, against every adapter the conformance suite covers.

use contextful_core::run::ports::{BlobStore, JournalStore};
use contextful_engine::conformance::interleave::{Interleaving, ModelRow, Outcome, Phase, Resolution, Step, CALLERS, KEYS};
use contextful_engine::stores::{FileBlobStore, FileJournalStore, MemoryBlobStore, MemoryJournalStore};
use proptest::prelude::*;
use proptest_state_machine::{prop_state_machine, ReferenceStateMachine, StateMachineTest};
use std::marker::PhantomData;
use std::sync::Arc;
use tempfile::TempDir;

/// The reference only steers generation: the journal's properties are checked on the adapter.
struct Steps;

impl ReferenceStateMachine for Steps {
    type State = ();
    type Transition = Step;

    fn init_state() -> BoxedStrategy<()> {
        Just(()).boxed()
    }

    fn transitions(_: &()) -> BoxedStrategy<Step> {
        steps().boxed()
    }

    fn apply(state: (), _: &Step) {
        state
    }
}

fn steps() -> impl Strategy<Value = Step> {
    let caller = 0..CALLERS;
    let outcome = prop_oneof![Just(Outcome::Inline), Just(Outcome::Blob), Just(Outcome::Fail), Just(Outcome::Unjournaled)];
    prop_oneof![
        3 => (caller.clone(), 0..KEYS).prop_map(|(caller, key)| Step::Begin { caller, key }),
        8 => caller.clone().prop_map(|caller| Step::Advance { caller }),
        3 => (caller.clone(), outcome).prop_map(|(caller, outcome)| Step::Effect { caller, outcome }),
        1 => caller.clone().prop_map(|caller| Step::Note { caller }),
        1 => caller.clone().prop_map(|caller| Step::Lapse { caller }),
        1 => caller.clone().prop_map(|caller| Step::Crash { caller }),
        1 => caller.prop_map(|caller| Step::Finish { caller }),
        1 => any::<bool>().prop_map(|aged| Step::Sweep { aged }),
        1 => (0..KEYS).prop_map(|key| Step::Retire { key }),
    ]
}

/// An adapter pair the conformance suite covers.
trait Adapter {
    type J: JournalStore + Clone + 'static;
    type B: BlobStore + Clone + 'static;
    fn fresh() -> (Self::J, Self::B, Option<TempDir>);
}

struct File;
impl Adapter for File {
    type J = FileJournalStore;
    type B = FileBlobStore;
    fn fresh() -> (FileJournalStore, FileBlobStore, Option<TempDir>) {
        let dir = tempfile::tempdir().unwrap();
        (FileJournalStore::open(dir.path()), FileBlobStore::open(dir.path()), Some(dir))
    }
}

struct Memory;
impl Adapter for Memory {
    type J = MemoryJournalStore;
    type B = MemoryBlobStore;
    fn fresh() -> (MemoryJournalStore, MemoryBlobStore, Option<TempDir>) {
        (MemoryJournalStore::new(), MemoryBlobStore::new(), None)
    }
}

struct SharedDynamic;
impl Adapter for SharedDynamic {
    type J = Arc<dyn JournalStore>;
    type B = Arc<dyn BlobStore>;
    fn fresh() -> (Arc<dyn JournalStore>, Arc<dyn BlobStore>, Option<TempDir>) {
        (Arc::new(MemoryJournalStore::new()), Arc::new(MemoryBlobStore::new()), None)
    }
}

struct Case<A: Adapter> {
    case: Interleaving<A::J, A::B>,
    rows: Vec<Option<ModelRow>>,
    _dir: Option<TempDir>,
}

struct Model<A>(PhantomData<A>);

impl<A: Adapter> StateMachineTest for Model<A> {
    type SystemUnderTest = Case<A>;
    type Reference = Steps;

    fn init_test(_: &()) -> Case<A> {
        let (rows, blobs, dir) = A::fresh();
        Case { case: Interleaving::new(rows, blobs, true), rows: vec![None; KEYS], _dir: dir }
    }

    fn apply(mut state: Case<A>, _: &(), step: Step) -> Case<A> {
        state.case.apply(step);
        let rows: Vec<Option<ModelRow>> = (0..KEYS).map(|k| state.case.row(k)).collect();
        // recorded_stable: a recorded row keeps its value until its execution retires.
        for k in 0..KEYS {
            if let Some(ModelRow::Recorded(v)) = state.rows[k] {
                if step != (Step::Retire { key: k }) {
                    assert_eq!(rows[k], Some(ModelRow::Recorded(v)), "after `{}`: key {k}'s recorded value moved: {}", step.line(), state.case.show());
                }
            }
        }
        state.rows = rows;
        state
    }

    fn check_invariants(state: &Case<A>, _: &()) {
        let case = &state.case;
        for k in 0..KEYS {
            // At most one recorded value per key: the adapter lists one row under the key's execution.
            assert!(case.listed(k) <= 1, "key {k} lists {} rows: {}", case.listed(k), case.show());
            // no_dangling_blob: a recorded blob value's blob is stored.
            if let Some(ModelRow::Recorded(v)) = state.rows[k] {
                assert!(!v.blob || case.blob(v.origin), "key {k} records a blob the store lost: {}", case.show());
            }
        }
        for c in 0..CALLERS {
            match case.phase(c) {
                // resolutions_agree: a value read from the journal is the key's recorded value.
                Phase::Done(k, Resolution::Replayed(v) | Resolution::Recorded(v)) | Phase::Loading(k, v) => {
                    assert_eq!(state.rows[k], Some(ModelRow::Recorded(v)), "caller {c} resolved a value key {k} does not record: {}", case.show());
                }
                Phase::Done(_, Resolution::Missing) => panic!("caller {c} read `BlobMissing` under the grace hypothesis: {}", case.show()),
                // effect_only_when_claimed: an effect runs under the caller's claim, after its lease
                // lapsed, or after a lapsed holder recorded.
                Phase::Claimed(k) => assert!(
                    matches!(state.rows[k], Some(ModelRow::Pending { holder, .. }) if holder == c) || case.lapsed(c) || matches!(state.rows[k], Some(ModelRow::Recorded(_))),
                    "caller {c} runs its effect outside a claim: {}",
                    case.show()
                ),
                _ => {}
            }
        }
    }
}

prop_state_machine! {
    #![proptest_config(ProptestConfig { cases: 128, failure_persistence: None, ..ProptestConfig::default() })]

    /// Callers interleaving one port call at a time keep a recorded value stable, resolve every journaled read to it,
    /// list one row per key, run an effect only under a claim or a lapsed lease, and never strand a recorded blob.
    #[test]
    fn interleaved_callers_over_the_file_journal_keep_one_recorded_value(sequential 1..40 => Model<File>);

    #[test]
    fn interleaved_callers_over_the_memory_journal_keep_one_recorded_value(sequential 1..40 => Model<Memory>);

    #[test]
    fn interleaved_callers_over_a_shared_dynamic_journal_keep_one_recorded_value(sequential 1..40 => Model<SharedDynamic>);
}
