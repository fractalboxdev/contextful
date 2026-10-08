//! Fail-closed checks for the sole thread of a suspended command process.

use std::io;

pub(super) fn reaped_members(
    active: u32,
    child_reaped: bool,
    members: impl IntoIterator<Item = bool>,
) -> bool {
    active == 0 && child_reaped && members.into_iter().all(|signalled| signalled)
}

pub(super) fn sole_thread(
    process: u32,
    threads: impl IntoIterator<Item = (u32, u32)>,
) -> io::Result<u32> {
    let mut owned = threads
        .into_iter()
        .filter_map(|(thread, owner)| (owner == process).then_some(thread));
    match (owned.next(), owned.next()) {
        (Some(thread), None) => Ok(thread),
        _ => Err(io::Error::other(
            "the suspended command requires exactly one owned thread",
        )),
    }
}

pub(super) fn thread_owner(process: u32, owner: u32) -> io::Result<()> {
    if owner == process && owner != 0 {
        Ok(())
    } else {
        Err(io::Error::other(
            "the retained launch thread belongs to another process",
        ))
    }
}

pub(super) fn resume_count(count: u32) -> io::Result<()> {
    if count == 1 {
        Ok(())
    } else {
        Err(io::Error::other(
            "the launch thread requires exactly one suspended count",
        ))
    }
}
