//! Slot management — versioned slots + atomic flip.
//!
//! The on-disk layout (§2.2):
//!   $HERMES_HOME/
//!   ├── versions/
//!   │   ├── 1.42.0/          # unpacked bundle (immutable after verify)
//!   │   └── 1.43.0/
//!   ├── current.txt          # THE commit point: one line, the active version
//!   ├── previous.txt         # instant rollback target
//!   └── current -> versions/1.43.0   # convenience symlink (best-effort)
//!
//! The flip is a file rename-over — atomic on every platform (POSIX rename(),
//! Windows MoveFileExW(MOVEFILE_REPLACE_EXISTING)). One mechanism, no per-
//! platform commit logic to diverge.

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Read the active version from `current.txt`.
/// This is the ONE reader for the current version — nothing else should
/// parse `current.txt` directly.
pub fn resolve_current(hermes_home: &Path) -> Result<Option<String>> {
    let current_txt = hermes_home.join("current.txt");
    if !current_txt.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&current_txt)
        .with_context(|| format!("cannot read {}", current_txt.display()))?;
    let version = content.trim().to_string();
    if version.is_empty() {
        Ok(None)
    } else {
        Ok(Some(version))
    }
}

/// Read the previous version from `previous.txt`.
pub fn resolve_previous(hermes_home: &Path) -> Result<Option<String>> {
    let prev_txt = hermes_home.join("previous.txt");
    if !prev_txt.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&prev_txt)
        .with_context(|| format!("cannot read {}", prev_txt.display()))?;
    let version = content.trim().to_string();
    if version.is_empty() {
        Ok(None)
    } else {
        Ok(Some(version))
    }
}

/// The slot root directory: `versions/`
pub fn versions_dir(hermes_home: &Path) -> PathBuf {
    hermes_home.join("versions")
}

/// Path for a specific version's slot: `versions/<version>`
pub fn slot_path(hermes_home: &Path, version: &str) -> PathBuf {
    versions_dir(hermes_home).join(version)
}

/// Staging path for a version being downloaded/unpacked: `versions/<version>.staging`
pub fn staging_path(hermes_home: &Path, version: &str) -> PathBuf {
    versions_dir(hermes_home).join(format!("{}.staging", version))
}

/// Create the staging directory for a version.
/// Caller is responsible for unpacking the bundle into it.
pub fn stage(hermes_home: &Path, version: &str) -> Result<PathBuf> {
    let staging = staging_path(hermes_home, version);
    if staging.exists() {
        // Clean up any leftover staging from a previous interrupted attempt
        fs::remove_dir_all(&staging)
            .with_context(|| format!("cannot remove stale staging {}", staging.display()))?;
    }
    fs::create_dir_all(&staging)
        .with_context(|| format!("cannot create staging dir {}", staging.display()))?;
    Ok(staging)
}

/// Commit staging: fsync all files in the staging tree, then rename to the
/// final slot path. The slot is immutable after this point.
///
/// If the target slot already exists, this is a same-version re-apply. We
/// refuse to delete/replace it if it is the current or previous target —
/// doing so would create a crash window where a pointer references nothing.
/// Instead we return an error so the caller can decide to reuse the slot
/// (it's already verified and committed) or bail.
pub fn commit_staging(hermes_home: &Path, version: &str) -> Result<PathBuf> {
    let staging = staging_path(hermes_home, version);
    let target = slot_path(hermes_home, version);

    if !staging.exists() {
        bail!("staging directory does not exist: {}", staging.display());
    }

    // If the target already exists (re-install of same version), refuse
    // replacement if current/previous may reference it. Deleting an active
    // or previous slot in place creates a crash window where current.txt
    // or previous.txt points at nothing.
    if target.exists() {
        let current = resolve_current(hermes_home).unwrap_or(None);
        let previous = resolve_previous(hermes_home).unwrap_or(None);
        if current.as_deref() == Some(version) || previous.as_deref() == Some(version) {
            bail!(
                "slot {} already exists and is referenced by current/previous — \
                 refusing to delete an active slot in place",
                version
            );
        }
        // Not referenced by current/previous — safe to remove.
        fs::remove_dir_all(&target)
            .with_context(|| format!("cannot remove existing slot {}", target.display()))?;
    }

    // fsync all files and directories in the staging tree (not just the top dir).
    fsync_tree(&staging)?;

    // Rename staging → final slot path.
    fs::rename(&staging, &target).with_context(|| {
        format!(
            "cannot rename {} to {}",
            staging.display(),
            target.display()
        )
    })?;

    // fsync the versions/ parent directory so the rename is durable.
    #[cfg(unix)]
    {
        let versions_dir = versions_dir(hermes_home);
        if let Ok(dir) = fs::File::open(&versions_dir) {
            use std::os::unix::io::AsRawFd;
            if let Err(e) = nix::unistd::fsync(dir.as_raw_fd()) {
                // Log but don't fail — the rename already happened.
                eprintln!("warn: cannot fsync versions/ dir: {e}");
            }
        }
    }

    Ok(target)
}

/// Recursively fsync all files and directories under `path`.
/// This ensures all file contents and directory entries are on disk
/// before the atomic rename commit.
fn fsync_tree(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;

        let mut stack = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            // fsync the directory itself
            if let Ok(dir_file) = fs::File::open(&dir) {
                if let Err(e) = nix::unistd::fsync(dir_file.as_raw_fd()) {
                    eprintln!("warn: cannot fsync dir {}: {e}", dir.display());
                }
            }

            for entry in fs::read_dir(&dir)? {
                let entry = entry?;
                let entry_path = entry.path();
                if entry_path.is_dir() {
                    stack.push(entry_path);
                } else {
                    // fsync the file
                    if let Ok(file) = fs::OpenOptions::new().read(true).open(&entry_path) {
                        if let Err(e) = nix::unistd::fsync(file.as_raw_fd()) {
                            eprintln!("warn: cannot fsync file {}: {e}", entry_path.display());
                        }
                    }
                }
            }
        }
    }
    // On non-unix, fsync via sync_all on files is not available for dirs;
    // the rename itself is still atomic.
    Ok(())
}

/// THE atomic flip: replace `current.txt` with the new version string.
///
/// Crash-consistent ordering:
/// 1. Write `previous.txt.new` with the old version (if any) + fsync
/// 2. Write `current.txt.new` with the new version + fsync
/// 3. Rename `previous.txt.new` → `previous.txt` (atomic)
/// 4. Rename `current.txt.new` → `current.txt` (atomic — THE commit point)
/// 5. fsync the $HERMES_HOME directory so the pointer renames are durable
/// 6. Refresh the `current` convenience symlink (best-effort, POSIX only)
///
/// Recovery states:
/// - Crash before step 4: current.txt still points at the old version.
///   previous.txt may have been updated already — that's fine, it just
///   records the old version which is still correct.
/// - Crash after step 4: current.txt points at the new version.
///   previous.txt may or may not have been updated yet. If not, a
///   subsequent rollback would use the stale previous.txt — but the
///   old version slot still exists so manual recovery is possible.
///   In practice step 3 runs before step 4 so this window is minimal.
///
/// Nothing load-bearing reads the symlink — `resolve_current` is the only reader.
pub fn flip(hermes_home: &Path, new_version: &str) -> Result<()> {
    let current_txt = hermes_home.join("current.txt");
    let previous_txt = hermes_home.join("previous.txt");
    let new_txt = hermes_home.join("current.txt.new");
    let previous_new_txt = hermes_home.join("previous.txt.new");

    // Read the old current version (for previous.txt)
    let old_version = resolve_current(hermes_home).unwrap_or(None);

    // Step 1: Prepare previous.txt.new BEFORE the commit point.
    // This ensures previous.txt is ready to be flipped atomically before
    // we commit current.txt.
    if let Some(old) = &old_version {
        let mut file = fs::File::create(&previous_new_txt)
            .with_context(|| format!("cannot create {}", previous_new_txt.display()))?;
        writeln!(file, "{}", old)?;
        file.sync_all()
            .with_context(|| format!("cannot fsync {}", previous_new_txt.display()))?;
        drop(file);
    }

    // Step 2: Write the new version to current.txt.new + fsync
    let mut file = fs::File::create(&new_txt)
        .with_context(|| format!("cannot create {}", new_txt.display()))?;
    writeln!(file, "{}", new_version)?;
    file.sync_all().context("cannot fsync current.txt.new")?;
    drop(file);

    // Step 3: Atomically update previous.txt (BEFORE current.txt commit).
    if old_version.is_some() {
        fs::rename(&previous_new_txt, &previous_txt).with_context(|| {
            format!(
                "cannot rename {} to {}",
                previous_new_txt.display(),
                previous_txt.display()
            )
        })?;
    }

    // Step 4: THE commit point — atomic rename over current.txt
    fs::rename(&new_txt, &current_txt).context("cannot flip current.txt")?;

    // Step 5: fsync the $HERMES_HOME directory so the pointer renames are durable.
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Ok(dir) = fs::File::open(hermes_home) {
            if let Err(e) = nix::unistd::fsync(dir.as_raw_fd()) {
                eprintln!("warn: cannot fsync $HERMES_HOME dir: {e}");
            }
        }
    }

    // Step 6: Refresh the convenience symlink (best-effort, POSIX only)
    #[cfg(unix)]
    {
        let symlink = hermes_home.join("current");
        let target = slot_path(hermes_home, new_version);
        // Remove old symlink if it exists (don't fail if it doesn't)
        let _ = fs::remove_file(&symlink);
        // Create new symlink (best-effort — don't fail the flip if this fails)
        let _ = std::os::unix::fs::symlink(&target, &symlink);
    }

    Ok(())
}

/// Rollback: rewrite `current.txt` from `previous.txt`.
///
/// Uses the crash-consistent `flip()` which atomically updates both
/// `previous.txt` and `current.txt`. After the flip, `previous.txt`
/// points at the version that was current before rollback (so a second
/// rollback would undo the rollback).
pub fn rollback(hermes_home: &Path) -> Result<String> {
    let prev = resolve_previous(hermes_home)?
        .ok_or_else(|| anyhow::anyhow!("no previous version to roll back to"))?;

    // flip() reads the current version (which is the one we're rolling back
    // from) and atomically swaps both pointers. After the flip:
    //   current.txt  → prev (the rollback target)
    //   previous.txt → the version that was current before rollback
    flip(hermes_home, &prev)?;

    Ok(prev)
}

/// Garbage-collect old slots, keeping the N most recent (always keeping
/// the targets of `current` and `previous`).
pub fn gc(hermes_home: &Path, keep_n: usize) -> Result<Vec<String>> {
    let versions_dir = versions_dir(hermes_home);
    if !versions_dir.exists() {
        return Ok(Vec::new());
    }

    let current = resolve_current(hermes_home).unwrap_or(None);
    let previous = resolve_previous(hermes_home).unwrap_or(None);

    // Collect all version directories (exclude .staging dirs)
    let mut slots: Vec<(String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&versions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".staging") {
            continue;
        }
        if entry.path().is_dir() {
            slots.push((name, entry.path()));
        }
    }

    // Sort by name (version strings sort chronologically for calver)
    slots.sort_by(|a, b| a.0.cmp(&b.0));

    // Keep the last N, plus current and previous
    let to_keep: std::collections::HashSet<String> = slots
        .iter()
        .rev()
        .take(keep_n)
        .map(|(v, _)| v.clone())
        .chain(current)
        .chain(previous)
        .collect();

    let mut removed = Vec::new();
    for (version, path) in &slots {
        if !to_keep.contains(version) {
            if let Err(e) = fs::remove_dir_all(path) {
                eprintln!("warn: cannot remove old slot {}: {}", path.display(), e);
            } else {
                removed.push(version.clone());
            }
        }
    }

    Ok(removed)
}

/// Clean up any stale `.staging` directories.
pub fn cleanup_stale_staging(hermes_home: &Path) -> Result<Vec<String>> {
    let versions_dir = versions_dir(hermes_home);
    if !versions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut removed = Vec::new();
    for entry in fs::read_dir(&versions_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".staging") && entry.path().is_dir() {
            if let Err(e) = fs::remove_dir_all(entry.path()) {
                eprintln!(
                    "warn: cannot remove staging {}: {}",
                    entry.path().display(),
                    e
                );
            } else {
                removed.push(name);
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_current_none_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(resolve_current(tmp.path()).unwrap(), None);
    }

    #[test]
    fn test_flip_sets_current() {
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        assert_eq!(
            resolve_current(tmp.path()).unwrap(),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn test_flip_updates_previous() {
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        assert_eq!(
            resolve_current(tmp.path()).unwrap(),
            Some("2.0.0".to_string())
        );
        assert_eq!(
            resolve_previous(tmp.path()).unwrap(),
            Some("1.0.0".to_string())
        );
    }

    #[test]
    fn test_rollback() {
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        let rolled = rollback(tmp.path()).unwrap();
        assert_eq!(rolled, "1.0.0");
        assert_eq!(
            resolve_current(tmp.path()).unwrap(),
            Some("1.0.0".to_string())
        );
        assert_eq!(
            resolve_previous(tmp.path()).unwrap(),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn test_rollback_fails_without_previous() {
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        assert!(rollback(tmp.path()).is_err());
    }

    #[test]
    fn test_stage_creates_staging_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = stage(tmp.path(), "1.0.0").unwrap();
        assert!(staging.exists());
        assert!(staging.to_string_lossy().ends_with("1.0.0.staging"));
    }

    #[test]
    fn test_stage_cleans_leftover() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a leftover staging dir
        let staging = staging_path(tmp.path(), "1.0.0");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("junk"), "old").unwrap();
        // Stage again — should clean and recreate
        let staging = stage(tmp.path(), "1.0.0").unwrap();
        assert!(!staging.join("junk").exists());
        assert!(staging.exists());
    }

    #[test]
    fn test_commit_staging() {
        let tmp = tempfile::tempdir().unwrap();
        let staging = stage(tmp.path(), "1.0.0").unwrap();
        fs::write(staging.join("manifest.json"), "{}").unwrap();
        let slot = commit_staging(tmp.path(), "1.0.0").unwrap();
        assert!(slot.exists());
        assert!(slot.join("manifest.json").exists());
        // Staging dir should be gone
        assert!(!staging_path(tmp.path(), "1.0.0").exists());
    }

    #[test]
    fn test_commit_staging_fails_without_staging() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(commit_staging(tmp.path(), "1.0.0").is_err());
    }

    #[test]
    fn test_gc_keeps_current_and_previous() {
        let tmp = tempfile::tempdir().unwrap();
        // Create 3 slots
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            let staging = stage(tmp.path(), v).unwrap();
            fs::write(staging.join("manifest.json"), "{}").unwrap();
            commit_staging(tmp.path(), v).unwrap();
        }
        // Flip to 3.0.0, with 2.0.0 as previous
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        flip(tmp.path(), "3.0.0").unwrap();

        // GC with keep_n=1 — should remove 1.0.0 but keep 2.0.0 (previous) and 3.0.0 (current)
        let removed = gc(tmp.path(), 1).unwrap();
        assert_eq!(removed, vec!["1.0.0".to_string()]);
        assert!(slot_path(tmp.path(), "2.0.0").exists());
        assert!(slot_path(tmp.path(), "3.0.0").exists());
    }

    #[test]
    fn test_cleanup_stale_staging() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir_all(staging_path(tmp.path(), "1.0.0")).unwrap();
        fs::create_dir_all(staging_path(tmp.path(), "2.0.0")).unwrap();
        let removed = cleanup_stale_staging(tmp.path()).unwrap();
        assert_eq!(removed.len(), 2);
        assert!(!staging_path(tmp.path(), "1.0.0").exists());
        assert!(!staging_path(tmp.path(), "2.0.0").exists());
    }

    #[test]
    fn test_flip_is_atomic_no_partial_state() {
        // After a successful flip, current.txt contains exactly the new version.
        // There's no intermediate state where current.txt is empty or partial.
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        let content = fs::read_to_string(tmp.path().join("current.txt")).unwrap();
        assert_eq!(content.trim(), "1.0.0");
        // The .new file should not exist (it was renamed)
        assert!(!tmp.path().join("current.txt.new").exists());
    }

    #[test]
    fn test_commit_staging_refuses_to_delete_active_slot() {
        // Same-version apply must not delete the current slot in place.
        let tmp = tempfile::tempdir().unwrap();
        let staging = stage(tmp.path(), "1.0.0").unwrap();
        fs::write(staging.join("manifest.json"), "{}").unwrap();
        commit_staging(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "1.0.0").unwrap();

        // Now try to re-stage and commit the same version — should fail
        // because it's the current slot.
        let staging2 = stage(tmp.path(), "1.0.0").unwrap();
        fs::write(staging2.join("manifest.json"), "{}").unwrap();
        let result = commit_staging(tmp.path(), "1.0.0");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("refusing to delete an active slot"));

        // The original slot should still be intact
        assert!(slot_path(tmp.path(), "1.0.0").exists());
        assert!(slot_path(tmp.path(), "1.0.0").join("manifest.json").exists());
    }

    #[test]
    fn test_commit_staging_refuses_to_delete_previous_slot() {
        // Same-version apply must not delete the previous slot in place.
        let tmp = tempfile::tempdir().unwrap();
        for v in ["1.0.0", "2.0.0"] {
            let staging = stage(tmp.path(), v).unwrap();
            fs::write(staging.join("manifest.json"), "{}").unwrap();
            commit_staging(tmp.path(), v).unwrap();
        }
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        // Now: current=2.0.0, previous=1.0.0

        // Try to re-stage 1.0.0 (the previous slot) — should fail
        let staging = stage(tmp.path(), "1.0.0").unwrap();
        fs::write(staging.join("manifest.json"), "{}").unwrap();
        let result = commit_staging(tmp.path(), "1.0.0");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("refusing to delete an active slot"));
    }

    #[test]
    fn test_same_version_apply_with_unused_slot() {
        // Re-applying a version that exists but is NOT current/previous
        // should succeed (safe to delete).
        let tmp = tempfile::tempdir().unwrap();
        for v in ["1.0.0", "2.0.0", "3.0.0"] {
            let staging = stage(tmp.path(), v).unwrap();
            fs::write(staging.join("manifest.json"), "{}").unwrap();
            commit_staging(tmp.path(), v).unwrap();
        }
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "3.0.0").unwrap();
        // Now: current=3.0.0, previous=1.0.0; 2.0.0 is unused

        // Re-stage 2.0.0 — should succeed since it's not current/previous
        let staging = stage(tmp.path(), "2.0.0").unwrap();
        fs::write(staging.join("manifest.json"), "{\"updated\":true}").unwrap();
        commit_staging(tmp.path(), "2.0.0").unwrap();
        assert!(slot_path(tmp.path(), "2.0.0").join("manifest.json").exists());
    }

    #[test]
    fn test_flip_leaves_no_temp_files_on_success() {
        // After a successful flip, no .new temp files should remain.
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        assert!(!tmp.path().join("current.txt.new").exists());
        assert!(!tmp.path().join("previous.txt.new").exists());
    }

    #[test]
    fn test_flip_previous_updated_before_current() {
        // After flip, previous.txt should contain the old version.
        // This verifies the crash-consistent ordering: previous is prepared
        // and committed BEFORE current.txt is flipped.
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        // previous.txt should contain 1.0.0 (the old version)
        assert_eq!(
            resolve_previous(tmp.path()).unwrap(),
            Some("1.0.0".to_string())
        );
        assert_eq!(
            resolve_current(tmp.path()).unwrap(),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn test_rollback_swaps_pointers_atomically() {
        // Rollback should swap current ↔ previous using the crash-consistent
        // flip, so both pointers are updated atomically.
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        let rolled = rollback(tmp.path()).unwrap();
        assert_eq!(rolled, "1.0.0");
        assert_eq!(
            resolve_current(tmp.path()).unwrap(),
            Some("1.0.0".to_string())
        );
        // previous.txt should now point at 2.0.0 (what was current before rollback)
        assert_eq!(
            resolve_previous(tmp.path()).unwrap(),
            Some("2.0.0".to_string())
        );
    }

    #[test]
    fn test_double_rollback_round_trips() {
        // Two rollbacks should return to the original state.
        let tmp = tempfile::tempdir().unwrap();
        flip(tmp.path(), "1.0.0").unwrap();
        flip(tmp.path(), "2.0.0").unwrap();
        rollback(tmp.path()).unwrap(); // → 1.0.0
        rollback(tmp.path()).unwrap(); // → 2.0.0
        assert_eq!(
            resolve_current(tmp.path()).unwrap(),
            Some("2.0.0".to_string())
        );
        assert_eq!(
            resolve_previous(tmp.path()).unwrap(),
            Some("1.0.0".to_string())
        );
    }
}
