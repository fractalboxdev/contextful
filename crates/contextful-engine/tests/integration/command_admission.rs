#[path = "../../src/command/admission.rs"]
mod admission;

use admission::{reaped_members, resume_count, sole_thread, thread_owner};

#[test]
fn suspended_launch_selects_only_its_single_owned_thread() {
    assert_eq!(sole_thread(42, [(7, 11), (9, 42), (12, 13)]).unwrap(), 9);
}

#[test]
fn suspended_launch_refuses_missing_or_ambiguous_owned_threads() {
    assert!(sole_thread(42, [(7, 11)]).is_err());
    assert!(sole_thread(42, [(9, 42), (10, 42)]).is_err());
}

#[test]
fn retained_thread_handle_requires_the_childs_ownership() {
    thread_owner(42, 42).unwrap();
    assert!(thread_owner(42, 43).is_err());
    assert!(thread_owner(42, 0).is_err());
}

#[test]
fn launch_resume_requires_exactly_one_suspend_count() {
    resume_count(1).unwrap();
    for count in [0, 2, u32::MAX] {
        assert!(resume_count(count).is_err());
    }
}

#[test]
fn zero_job_accounting_waits_for_retained_member_handles() {
    assert!(!reaped_members(0, true, [true, false]));
    assert!(!reaped_members(1, true, [true, true]));
    assert!(!reaped_members(0, false, [true, true]));
    assert!(reaped_members(0, true, [true, true]));
}
