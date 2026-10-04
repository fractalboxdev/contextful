//! Exclusive create: one winner, an untouched loser, no staging file left behind, on every
//! backend the primitive chooses between.

use contextful_fs::{create_exclusive, create_exclusive_locked, create_new, tmp_sibling};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

type Create = fn(&Path, &Path) -> std::io::Result<bool>;

/// Stage `bytes` beside `path` and publish them through `create`.
fn stage_and(create: Create, path: &Path, bytes: &[u8]) -> bool {
    let tmp = tmp_sibling(path);
    std::fs::write(&tmp, bytes).unwrap();
    let landed = create(&tmp, path).unwrap();
    assert!(!tmp.exists(), "the staging file is gone after the create");
    landed
}

/// The directory's entries, less the `._` AppleDouble companions macOS writes on volumes
/// without extended attributes.
fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with("._"))
        .collect();
    names.sort();
    names
}

fn first_wins_and_the_second_is_refused(create: Create, dir: &Path) {
    let path = dir.join("_manifest.json");
    assert!(stage_and(create, &path, b"first"));
    assert!(!stage_and(create, &path, b"second"));
    assert_eq!(std::fs::read(&path).unwrap(), b"first");
    assert_eq!(entries(dir), ["_manifest.json"]);
}

const RACERS: usize = 16;

/// Sixteen threads create one path at once; exactly one lands and its bytes stand. Returns
/// the winner count.
fn one_of_sixteen_racers_wins(create: Create, dir: &Path) -> usize {
    let path: Arc<PathBuf> = Arc::new(dir.join("entry-0001.json"));
    let start = Arc::new(Barrier::new(RACERS));
    let handles: Vec<_> = (0..RACERS)
        .map(|i| {
            let (path, start) = (path.clone(), start.clone());
            std::thread::spawn(move || {
                let tmp = tmp_sibling(&path);
                std::fs::write(&tmp, format!("racer-{i}")).unwrap();
                start.wait();
                create(&tmp, &path).unwrap().then_some(i)
            })
        })
        .collect();
    let winners: Vec<usize> = handles.into_iter().filter_map(|h| h.join().unwrap()).collect();
    assert_eq!(winners.len(), 1, "winners: {winners:?}");
    assert_eq!(std::fs::read_to_string(&*path).unwrap(), format!("racer-{}", winners[0]));
    assert_eq!(entries(dir), ["entry-0001.json"]);
    winners.len()
}

#[test]
fn the_first_create_lands_and_a_second_leaves_it_untouched() {
    let dir = tempfile::tempdir().unwrap();
    first_wins_and_the_second_is_refused(create_exclusive, dir.path());
}

#[test]
fn the_locked_fallback_lands_the_first_and_refuses_the_second() {
    let dir = tempfile::tempdir().unwrap();
    first_wins_and_the_second_is_refused(create_exclusive_locked, dir.path());
}

#[test]
fn racing_creates_land_exactly_one() {
    let dir = tempfile::tempdir().unwrap();
    let winners = one_of_sixteen_racers_wins(create_exclusive, dir.path());
    contextful_eval::record::emit("exclusive-create-no-hardlink", winners as f64, RACERS as u64, 0);
}

#[test]
fn racing_creates_through_the_locked_fallback_land_exactly_one() {
    let dir = tempfile::tempdir().unwrap();
    one_of_sixteen_racers_wins(create_exclusive_locked, dir.path());
}

#[test]
fn create_new_stages_and_publishes_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("node-id");
    assert!(create_new(&path, b"node-1\n").unwrap());
    assert!(!create_new(&path, b"node-2\n").unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), b"node-1\n");
    assert_eq!(entries(dir.path()), ["node-id"]);
}

#[test]
fn a_create_into_a_missing_directory_fails_and_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("absent/_manifest.json");
    assert!(create_new(&path, b"x").is_err());
    assert!(entries(dir.path()).is_empty());
}

/// exFAT holds no hard links and refuses `RENAME_EXCL` onto an absent target.
#[cfg(target_os = "macos")]
#[test]
fn an_exfat_volume_creates_through_the_fallback() {
    let volume = contextful_fs::test_volume::ExfatVolume::mount();
    let probe = volume.path().join("probe");
    std::fs::write(&probe, b"x").unwrap();
    assert!(std::fs::hard_link(&probe, volume.path().join("probe-link")).is_err(), "exFAT refuses hard links");
    std::fs::remove_file(&probe).unwrap();
    let dir = volume.path().join("single");
    std::fs::create_dir(&dir).unwrap();
    first_wins_and_the_second_is_refused(create_exclusive, &dir);
    let dir = volume.path().join("race");
    std::fs::create_dir(&dir).unwrap();
    one_of_sixteen_racers_wins(create_exclusive, &dir);
}

#[cfg(target_os = "macos")]
#[test]
fn exfat_mount_uses_system_disk_with_custom_tmpdir() {
    const CHILD: &str = "CONTEXTFUL_EXFAT_TEMP_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let volume = contextful_fs::test_volume::ExfatVolume::mount();
        assert!(volume.path().starts_with("/private/tmp"), "mount: {}", volume.path().display());
        return;
    }

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "create::exfat_mount_uses_system_disk_with_custom_tmpdir", "--nocapture"])
        .env(CHILD, "1")
        .env("TMPDIR", "/var/tmp")
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stdout));
}
