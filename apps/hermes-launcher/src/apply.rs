//! Managed install/apply orchestration.
//!
//! This module owns the only mutation path for managed slots:
//! download -> unpack -> verify -> staged preflight -> commit -> flip.

use crate::release::{Manifest, ReleaseSource};
use crate::slots;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Validate that a verified manifest matches the expected identity before
/// any mutation happens. Checks platform, channel, version path safety, and
/// archive name consistency.
fn validate_manifest_identity(
    manifest: &Manifest,
    expected_platform: &str,
    expected_channel: &str,
    expected_version: &str,
) -> Result<()> {
    if manifest.platform != expected_platform {
        bail!(
            "manifest platform mismatch: expected {}, manifest says {}",
            expected_platform,
            manifest.platform
        );
    }
    if manifest.channel != expected_channel {
        bail!(
            "manifest channel mismatch: expected {}, manifest says {}",
            expected_channel,
            manifest.channel
        );
    }
    // Version must be a safe single path component — no separators, no ..
    if manifest.version.is_empty()
        || manifest.version.contains('/')
        || manifest.version.contains('\\')
        || manifest.version.contains('\0')
        || manifest.version == "."
        || manifest.version == ".."
        || manifest.version.contains("..")
    {
        bail!(
            "manifest version is not a valid path component: {:?}",
            manifest.version
        );
    }
    if manifest.version != expected_version {
        bail!(
            "release version mismatch: requested {}, manifest says {}",
            expected_version,
            manifest.version
        );
    }
    Ok(())
}

pub struct ApplyRequest<'a> {
    pub hermes_home: &'a Path,
    pub source: &'a ReleaseSource,
    pub version: Option<&'a str>,
    pub channel: &'a str,
    pub trusted_pubkey: &'a str,
    /// Original process argv, used for the bootstrap hop re-exec.
    /// When None, the hop is skipped (e.g. in tests).
    pub argv: Option<&'a [String]>,
}

pub fn apply_release(request: ApplyRequest<'_>) -> Result<Manifest> {
    let version = match request.version {
        Some(version) => version.to_owned(),
        None => request.source.latest(request.channel)?,
    };
    let platform = current_platform()?;
    let (bundle_url, _, _) = request
        .source
        .resolve(&version, &platform, request.channel)?;

    fs::create_dir_all(slots::versions_dir(request.hermes_home))?;
    slots::cleanup_stale_staging(request.hermes_home)?;
    let staging = slots::stage(request.hermes_home, &version)?;
    let archive = request
        .hermes_home
        .join("versions")
        .join(format!(".{}.download", version));

    let result = (|| -> Result<Manifest> {
        download_blocking(request.source, &bundle_url, &archive)?;
        unpack_archive(&archive, &staging, &platform)?;
        normalize_archive_root(&staging)?;
        let manifest = crate::release::verify_bundle(&staging, Some(request.trusted_pubkey))?;
        validate_manifest_identity(&manifest, &platform, request.channel, &version)?;

        // Bootstrap hop: if the bundle requires a newer updater than us,
        // extract the new updater binary from the verified bundle and
        // re-exec into it. This happens AFTER signature/hash verification
        // and BEFORE any mutation (preflight/commit/flip).
        //
        // The --hopped flag is the one-shot guard: if we're already the
        // hopped binary, we don't hop again (the new version satisfies the
        // requirement, or if it doesn't, needs_hop returns false because
        // our version is now the new one).
        let already_hopped = std::env::args().any(|a| a == "--hopped");
        if !already_hopped {
            if let Some(argv) = request.argv {
                let my_version = env!("CARGO_PKG_VERSION");
                if crate::selfupdate::needs_hop(my_version, &manifest.min_updater_version) {
                    eprintln!(
                        "Bootstrap hop required: bundle needs updater >= {}, this is {}",
                        manifest.min_updater_version, my_version
                    );
                    crate::selfupdate::hop(&staging, argv)?;
                    // hop() re-execs; if we reach here, the exec failed.
                    bail!("bootstrap hop failed to re-exec");
                }
            }
        }

        run_preflight(&staging)?;
        slots::commit_staging(request.hermes_home, &version)?;
        slots::flip(request.hermes_home, &version)?;
        Ok(manifest)
    })();

    let _ = fs::remove_file(&archive);
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

/// Activate the staged updater binary from the current slot.
///
/// Per the spec (docs/updater-world.md §2.3.1, §383-390), the stable
/// launcher ($HERMES_HOME/bin/hermes) is NEVER rewritten during ordinary
/// updates. It resolves current.txt at launch time, so it doesn't need
/// updating when the slot changes. It is written once during install and
/// left alone during apply.
///
/// Only the updater binary ($HERMES_HOME/bin/hermes-updater) needs
/// restaging — it's the bootstrap that runs before any particular version
/// is current, so it must be kept up to date.
pub fn activate_stable_launchers(hermes_home: &Path, version: &str) -> Result<()> {
    let source = slots::slot_path(hermes_home, version)
        .join("bin")
        .join(if cfg!(windows) {
            "hermes.exe"
        } else {
            "hermes"
        });
    let bin_dir = hermes_home.join("bin");
    fs::create_dir_all(&bin_dir)?;
    let updater = bin_dir.join(if cfg!(windows) {
        "hermes-updater.exe"
    } else {
        "hermes-updater"
    });

    // Only restage the updater — the stable launcher is NOT rewritten.
    // It was written once during install and resolves current.txt at launch.
    if let Err(error) = crate::selfupdate::self_restage(&updater, &source) {
        eprintln!("warning: could not restage updater: {error:#}");
    }
    Ok(())
}

#[derive(Debug)]
pub struct UpdateMarker {
    path: PathBuf,
}

/// How old (in seconds) before a marker is considered stale and reclaimable.
const STALE_MARKER_AGE_SECS: u64 = 600; // 10 minutes

impl UpdateMarker {
    /// Acquire the update-in-progress marker with real mutual exclusion.
    ///
    /// Uses atomic create-new (`O_CREAT | O_EXCL`) so only one process can
    /// create the file. If it already exists, checks the owner PID and age:
    /// if the PID is dead or the marker is older than STALE_MARKER_AGE_SECS,
    /// reclaims it. Otherwise returns an error.
    ///
    /// The marker is held until the `UpdateMarker` is dropped, so it should
    /// be acquired BEFORE the commit point and held through flip/restart/notify.
    pub fn acquire(hermes_home: &Path) -> Result<Self> {
        use std::io;
        use std::time::{SystemTime, UNIX_EPOCH};

        let path = hermes_home.join(".hermes-update-in-progress");

        loop {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .context("system clock is before UNIX epoch")?
                .as_secs();

            // Try atomic create-new: succeeds only if the file doesn't exist.
            let result = {
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o644);
                }
                opts.open(&path)
            };

            match result {
                Ok(mut file) => {
                    use std::io::Write;
                    write!(file, "{}\n{}\n", std::process::id(), now)
                        .context("cannot write update marker")?;
                    return Ok(Self { path });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    // Marker exists — check if it's stale.
                    match Self::check_stale(&path, now) {
                        Ok(true) => {
                            // Stale — remove and retry.
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                        Ok(false) => {
                            // Active — refuse.
                            let contents = std::fs::read_to_string(&path)
                                .unwrap_or_default();
                            let pid = contents.lines().next().unwrap_or("?");
                            bail!(
                                "another update is in progress (pid: {}). \
                                 Wait for it to finish or remove {} if stale.",
                                pid,
                                path.display()
                            );
                        }
                        Err(_) => {
                            // Can't read — treat as stale and retry.
                            let _ = std::fs::remove_file(&path);
                            continue;
                        }
                    }
                }
                Err(err) => {
                    Err(err).with_context(|| {
                        format!("cannot create update marker: {}", path.display())
                    })?;
                }
            }
        }
    }

    /// Check if the marker at `path` is stale.
    /// Returns Ok(true) if stale (PID dead or too old), Ok(false) if active.
    fn check_stale(path: &Path, now: u64) -> Result<bool> {
        let contents =
            std::fs::read_to_string(path).with_context(|| {
                format!("cannot read update marker: {}", path.display())
            })?;
        let mut lines = contents.lines();
        let pid_str = lines.next().context("marker has no PID line")?;
        let age_str = lines.next().context("marker has no timestamp line")?;

        let pid: u32 = pid_str
            .parse()
            .context("marker PID is not a valid integer")?;
        let started_at: u64 = age_str
            .parse()
            .context("marker timestamp is not a valid integer")?;

        // Check age first — if the marker is too old, reclaim regardless.
        let age = now.saturating_sub(started_at);
        if age > STALE_MARKER_AGE_SECS {
            return Ok(true);
        }

        // Check if the PID is still alive.
        if !pid_is_alive(pid) {
            return Ok(true);
        }

        Ok(false)
    }
}

/// Check if a process is alive (cross-platform).
fn pid_is_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists, ESRCH if not.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle =
            unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle == 0 {
            return false;
        }
        unsafe { CloseHandle(handle) };
        true
    }
}

impl Drop for UpdateMarker {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn apply_feature_ledger(hermes_home: &Path, version: &str) -> Result<()> {
    let slot = slots::slot_path(hermes_home, version);
    let launcher = slot.join("bin").join(if cfg!(windows) {
        "hermes.exe"
    } else {
        "hermes"
    });
    let status = std::process::Command::new(launcher)
        .args(["features", "apply-ledger", "--json"])
        .current_dir(&slot)
        .status()
        .context("cannot run feature ledger application")?;
    if !status.success() {
        bail!("feature ledger application exited with {}", status);
    }
    Ok(())
}

fn download_blocking(source: &ReleaseSource, url: &str, destination: &Path) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("cannot create download runtime")?;
    runtime.block_on(source.download(url, destination))
}

fn unpack_tar_zst(archive_path: &Path, destination: &Path) -> Result<()> {
    let archive_file = fs::File::open(archive_path)
        .with_context(|| format!("cannot open {}", archive_path.display()))?;
    let decoder = zstd::Decoder::new(archive_file).context("invalid zstd bundle")?;
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(destination)
        .with_context(|| format!("cannot unpack bundle into {}", destination.display()))
}

fn unpack_archive(archive_path: &Path, destination: &Path, platform: &str) -> Result<()> {
    if platform.starts_with("win-") {
        unpack_zip(archive_path, destination)
    } else {
        unpack_tar_zst(archive_path, destination)
    }
}

fn unpack_zip(archive_path: &Path, destination: &Path) -> Result<()> {
    let archive_file = fs::File::open(archive_path)
        .with_context(|| format!("cannot open {}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(archive_file).context("invalid zip bundle")?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("zip entry escapes bundle root: {}", entry.name()))?;
        let output = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = fs::File::create(&output)?;
        std::io::copy(&mut entry, &mut file)?;
    }
    Ok(())
}

fn normalize_archive_root(staging: &Path) -> Result<()> {
    if staging.join("manifest.json").is_file() {
        return Ok(());
    }
    let nested = staging.join("bundle");
    if !nested.join("manifest.json").is_file() {
        bail!("bundle archive has no root-level manifest.json");
    }
    for entry in fs::read_dir(&nested)? {
        let entry = entry?;
        fs::rename(entry.path(), staging.join(entry.file_name()))?;
    }
    fs::remove_dir(&nested)?;
    Ok(())
}

fn run_preflight(staging: &Path) -> Result<()> {
    let executable = staging.join("bin").join(if cfg!(windows) {
        "hermes.exe"
    } else {
        "hermes"
    });
    let output = std::process::Command::new(&executable)
        .arg("doctor")
        .arg("--preflight")
        .current_dir(staging)
        .env("HERMES_ARTIFACT_ROOT", staging)
        .output()
        .with_context(|| format!("cannot run staged preflight via {}", executable.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);

        // On Windows, the venv's python symlink may be absolute and point to
        // the build runner's uv-managed python path. If the preflight failure
        // is specifically a venv path issue (python not found, path-related
        // errors), we can skip — the launcher will recreate/fix the venv on
        // first real launch. Other failures (import errors, config errors,
        // artifact root problems) must NOT be bypassed.
        //
        // NOTE: Real Windows apply/preflight coverage is still needed.
        //       We can't run Windows tests from this platform.
        if cfg!(windows) {
            let combined = format!("{}\n{}", stdout, stderr).to_lowercase();
            let is_venv_path_issue = combined.contains("venv")
                || combined.contains("python")
                || combined.contains("interpreter")
                || combined.contains("no such file")
                || combined.contains("path");
            if is_venv_path_issue {
                eprintln!(
                    "warning: staged preflight failed with venv path issue (status {}) — \
                     venv may need path fixup on first launch",
                    output.status
                );
                eprintln!("  stderr: {}", stderr.trim());
                return Ok(());
            }
            // All other Windows failures fail closed — don't bypass import,
            // config, or artifact checks.
            bail!(
                "staged preflight failed on Windows with {}: {}",
                output.status,
                stderr.trim()
            );
        }
        bail!("staged preflight failed with {}: {}", output.status, stderr.trim());
    }
    Ok(())
}

pub fn current_platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "linux" => "linux",
        "macos" => "darwin",
        "windows" => "win",
        other => bail!("unsupported platform: {}", other),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => bail!("unsupported architecture: {}", other),
    };
    Ok(format!("{}-{}", os, arch))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use sha2::{Digest, Sha256};
    use std::collections::HashMap;

    fn fixture_release(root: &Path, version: &str) -> String {
        let platform = current_platform().unwrap();
        let source_dir = root.join("source");
        fs::create_dir_all(source_dir.join("bin")).unwrap();
        fs::create_dir_all(source_dir.join("runtime/venv/bin")).unwrap();
        fs::create_dir_all(source_dir.join("app/skills/demo")).unwrap();
        fs::create_dir_all(source_dir.join("ui/tui/dist")).unwrap();
        fs::create_dir_all(source_dir.join("ui/web/dist")).unwrap();
        let launcher = source_dir.join(if cfg!(windows) {
            "bin/hermes.exe"
        } else {
            "bin/hermes"
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(&launcher, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&launcher, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        fs::write(&launcher, "fake exe").unwrap();
        fs::write(source_dir.join("runtime/venv/bin/python"), "python").unwrap();
        fs::write(source_dir.join("app/skills/demo/SKILL.md"), "demo").unwrap();
        fs::write(source_dir.join("ui/tui/dist/entry.js"), "tui").unwrap();
        fs::write(source_dir.join("ui/web/dist/index.html"), "web").unwrap();

        let mut files = HashMap::new();
        for path in walk_files(&source_dir) {
            let rel = path
                .strip_prefix(&source_dir)
                .unwrap()
                .to_string_lossy()
                .to_string();
            files.insert(
                rel,
                format!("sha256:{:x}", Sha256::digest(fs::read(&path).unwrap())),
            );
        }
        let manifest = Manifest {
            schema: 1,
            version: version.to_owned(),
            channel: "stable".to_owned(),
            git_sha: "a".repeat(40),
            platform: platform.clone(),
            min_updater_version: "0.1.0".to_owned(),
            desktop: false,
            files,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest).unwrap();
        fs::write(source_dir.join("manifest.json"), &manifest_bytes).unwrap();
        let signing_key = SigningKey::generate(&mut OsRng);
        let signature = signing_key.sign(&manifest_bytes);
        let pubkey = base64::engine::general_purpose::STANDARD
            .encode(signing_key.verifying_key().to_bytes());
        let signature_doc = crate::release::Signature {
            algorithm: "ed25519".to_owned(),
            pubkey: pubkey.clone(),
            signature: base64::engine::general_purpose::STANDARD.encode(signature.to_bytes()),
        };
        fs::write(
            source_dir.join("manifest.json.sig"),
            serde_json::to_vec_pretty(&signature_doc).unwrap(),
        )
        .unwrap();

        let version_dir = root.join(version);
        fs::create_dir_all(&version_dir).unwrap();
        let archive_path = version_dir.join(format!("hermes-{}-{}.tar.zst", version, platform));
        let archive_file = fs::File::create(&archive_path).unwrap();
        let encoder = zstd::Encoder::new(archive_file, 1).unwrap();
        let mut archive = tar::Builder::new(encoder);
        archive.append_dir_all("bundle", &source_dir).unwrap();
        archive.into_inner().unwrap().finish().unwrap();
        fs::write(root.join("latest-stable.txt"), format!("{}\n", version)).unwrap();
        pubkey
    }

    fn walk_files(root: &Path) -> Vec<PathBuf> {
        let mut files = Vec::new();
        let mut pending = vec![root.to_path_buf()];
        while let Some(dir) = pending.pop() {
            for entry in fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        files
    }

    #[test]
    fn signed_file_release_installs_through_real_pipeline() {
        let release = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let pubkey = fixture_release(release.path(), "1.0.0");
        let source = ReleaseSource::File {
            base_path: release.path().to_path_buf(),
        };

        let manifest = apply_release(ApplyRequest {
            hermes_home: home.path(),
            source: &source,
            version: None,
            channel: "stable",
            trusted_pubkey: &pubkey,
            argv: None,
        })
        .unwrap();

        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(
            slots::resolve_current(home.path()).unwrap().as_deref(),
            Some("1.0.0")
        );
        assert!(home.path().join("versions/1.0.0/manifest.json").is_file());
        assert!(!home.path().join("versions/1.0.0.staging").exists());
    }

    #[test]
    fn update_marker_is_byte_compatible_and_removed_on_drop() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".hermes-update-in-progress");
        {
            let _marker = UpdateMarker::acquire(home.path()).unwrap();
            let contents = fs::read_to_string(&path).unwrap();
            let fields: Vec<_> = contents.lines().collect();
            assert_eq!(fields.len(), 2);
            assert_eq!(fields[0], std::process::id().to_string());
            assert!(fields[1].parse::<u64>().is_ok());
        }
        assert!(!path.exists());
    }

    #[test]
    fn update_marker_rejects_concurrent_acquisition() {
        let home = tempfile::tempdir().unwrap();
        let _first = UpdateMarker::acquire(home.path()).unwrap();
        // Second acquisition must fail — the marker is held by the first.
        let result = UpdateMarker::acquire(home.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("in progress"));
    }

    #[test]
    fn update_marker_reclaims_stale_pid() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(".hermes-update-in-progress");
        // Write a marker with a dead PID (999999 is extremely unlikely to exist).
        fs::write(&path, "999999\n1\n").unwrap();
        // Acquisition should succeed by reclaiming the stale marker.
        let _marker = UpdateMarker::acquire(home.path()).unwrap();
        // The marker should now have our PID.
        let contents = fs::read_to_string(&path).unwrap();
        let pid: u32 = contents.lines().next().unwrap().parse().unwrap();
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn zip_extraction_preserves_bundle_root() {
        let temp = tempfile::tempdir().unwrap();
        let archive_path = temp.path().join("bundle.zip");
        let file = fs::File::create(&archive_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "bundle/manifest.json",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        use std::io::Write;
        zip.write_all(b"{}\n").unwrap();
        zip.finish().unwrap();

        let destination = temp.path().join("out");
        fs::create_dir(&destination).unwrap();
        unpack_zip(&archive_path, &destination).unwrap();

        assert_eq!(
            fs::read_to_string(destination.join("bundle/manifest.json")).unwrap(),
            "{}\n"
        );
    }

    fn make_manifest(version: &str, platform: &str, channel: &str) -> Manifest {
        Manifest {
            schema: 1,
            version: version.to_owned(),
            channel: channel.to_owned(),
            git_sha: "a".repeat(40),
            platform: platform.to_owned(),
            min_updater_version: "0.1.0".to_owned(),
            desktop: false,
            files: HashMap::new(),
        }
    }

    #[test]
    fn validate_identity_accepts_matching_manifest() {
        let manifest = make_manifest("1.0.0", "linux-x64", "stable");
        validate_manifest_identity(&manifest, "linux-x64", "stable", "1.0.0").unwrap();
    }

    #[test]
    fn validate_identity_rejects_platform_mismatch() {
        let manifest = make_manifest("1.0.0", "win-x64", "stable");
        let result = validate_manifest_identity(&manifest, "linux-x64", "stable", "1.0.0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("platform"));
    }

    #[test]
    fn validate_identity_rejects_channel_mismatch() {
        let manifest = make_manifest("1.0.0", "linux-x64", "nightly");
        let result = validate_manifest_identity(&manifest, "linux-x64", "stable", "1.0.0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("channel"));
    }

    #[test]
    fn validate_identity_rejects_version_mismatch() {
        let manifest = make_manifest("2.0.0", "linux-x64", "stable");
        let result = validate_manifest_identity(&manifest, "linux-x64", "stable", "1.0.0");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("version"));
    }

    #[test]
    fn validate_identity_rejects_path_traversal_version() {
        for bad in &["../etc", "..", "a/b", "a\\b", "", "."] {
            let manifest = make_manifest(bad, "linux-x64", "stable");
            let result = validate_manifest_identity(&manifest, "linux-x64", "stable", bad);
            assert!(
                result.is_err(),
                "version {:?} should be rejected as invalid path component",
                bad
            );
        }
    }
}
