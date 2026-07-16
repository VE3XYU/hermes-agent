mod adopt;
mod apply;
mod cli;
mod cwd_guard;
mod launch;
mod release;
mod selfupdate;
mod services;
mod slots;
mod tree;

use anyhow::Context;
use cli::Command;
use std::path::PathBuf;

fn hermes_home() -> anyhow::Result<PathBuf> {
    if let Some(path) = std::env::var_os("HERMES_HOME") {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs::home_dir()
        .context("cannot find home directory")?
        .join(".hermes"))
}

fn main() -> anyhow::Result<()> {
    // Sweep old .exe binaries from previous Windows restages at startup.
    // On POSIX this is a no-op (no .old.exe files). On Windows, this cleans
    // up leftover hermes-updater.old.exe files from previous restages that
    // couldn't be removed because the running process had them locked.
    if let Ok(home) = hermes_home() {
        let bin_dir = home.join("bin");
        let _ = crate::selfupdate::sweep_old_binaries(&bin_dir);
    }

    let args = apply_cwd_guard()?;
    let cli = cli::parse_from(args);

    match cli.command {
        Some(Command::Launch { args }) => launch(args),
        Some(Command::Install {
            source,
            channel,
            source_mode,
            branch,
        }) => install(source, channel, source_mode, branch),
        Some(Command::Apply {
            source,
            target_version,
            notify_file,
            relaunch_app,
            report,
        }) => apply(source, target_version, notify_file, relaunch_app, report),
        Some(Command::Rollback) => rollback(),
        Some(Command::Status { check, json }) => status(check, json),
        Some(Command::Adopt {
            from_checkout,
            source,
            undo,
        }) => adopt(from_checkout, source, undo),
        Some(Command::SelfRestage) => self_restage(),
        None => {
            // Should not happen — parse() fills in a default.
            unreachable!("cli::parse() should always set a command")
        }
    }
}

fn apply_cwd_guard() -> anyhow::Result<Vec<String>> {
    let mut argv: Vec<String> = std::env::args().collect();
    if cli::invoked_as_updater() {
        return Ok(argv);
    }
    let has_dev = argv.iter().any(|arg| arg == "--dev");
    let has_global = argv.iter().any(|arg| arg == "--global");
    if has_dev && has_global {
        eprintln!("hermes: --dev and --global are contradictory — pick one.");
        std::process::exit(2);
    }
    if has_global {
        argv.retain(|arg| arg != "--global");
        return Ok(argv);
    }
    let cwd = std::env::current_dir().context("cannot resolve current directory")?;
    let executable = std::env::current_exe().context("cannot resolve launcher executable")?;
    let launcher_tree = tree::resolve_tree_root(&executable)?;
    match cwd_guard::cwd_guard(&launcher_tree.root, &cwd, &argv) {
        cwd_guard::GuardDecision::Refuse(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
        cwd_guard::GuardDecision::Run => {
            argv.retain(|arg| arg != "--dev" && arg != "--global");
            Ok(argv)
        }
        cwd_guard::GuardDecision::ReExec(path) => reexec_checkout(path, &argv[1..]),
    }
}

fn reexec_checkout(path: PathBuf, args: &[String]) -> anyhow::Result<Vec<String>> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = std::process::Command::new(&path).args(args).exec();
        Err(error).with_context(|| format!("cannot exec {}", path.display()))
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new(&path)
            .args(args)
            .status()
            .with_context(|| format!("cannot launch {}", path.display()))?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn launch(args: Vec<String>) -> anyhow::Result<()> {
    launch::launch(args)
}

/// The trusted Ed25519 public key for release manifest signatures.
///
/// Trust root: the key embedded in-repo at `keys/hermes-release.pub`
/// (via `include_str!`). This makes the trust root auditable — it's in
/// the git history, not a build-time env var.
///
/// For CI and testing, `HERMES_RELEASE_PUBLIC_KEY` overrides the embedded
/// key. This lets release CI sign with its own keypair and test fixtures
/// use throwaway keys without modifying the repo file.
fn trusted_release_pubkey() -> anyhow::Result<&'static str> {
    // Env var override (CI/testing).
    if let Some(key) = option_env!("HERMES_RELEASE_PUBLIC_KEY") {
        let key = key.trim();
        if !key.is_empty() {
            return Ok(key);
        }
    }

    // Fall back to the in-repo embedded key (via include_str!).
    // The file may contain comment lines starting with '#'; the key is the
    // first non-comment, non-empty line.
    const EMBEDDED_KEY: &str = include_str!("../keys/hermes-release.pub");
    for line in EMBEDDED_KEY.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() && !trimmed.starts_with('#') {
            return Ok(trimmed);
        }
    }

    Err(anyhow::anyhow!(
        "no trusted release public key: neither HERMES_RELEASE_PUBLIC_KEY \
         nor keys/hermes-release.pub provides a key"
    ))
}

/// The canonical release source this updater was built for.
///
/// Embedded at compile time via `HERMES_RELEASE_SOURCE`. If absent at
/// build time, falls back to the official GitHub releases URL. This is
/// the "known good" origin — overriding it via `--source` produces a
/// scary warning because the user is trusting a different origin for
/// release artifacts.
const EMBEDDED_RELEASE_SOURCE: &str = match option_env!("HERMES_RELEASE_SOURCE") {
    Some(s) => s,
    None => "https://github.com/NousResearch/hermes-agent/releases/download",
};

fn release_source(source: Option<String>) -> anyhow::Result<release::ReleaseSource> {
    let url = source.as_deref().unwrap_or(EMBEDDED_RELEASE_SOURCE);

    // Warn loudly if the caller is trusting a different origin than the one
    // this updater was built for. The signature still protects integrity,
    // but a different origin means a different release pipeline — the user
    // should know they've diverged from the default trust path.
    if let Some(custom) = &source {
        if custom != EMBEDDED_RELEASE_SOURCE {
            eprintln!("⚠  WARNING: using a non-default release source: {custom}");
            eprintln!("   This updater was built for: {EMBEDDED_RELEASE_SOURCE}");
            eprintln!("   Release artifacts will be fetched from a different origin.");
            eprintln!("   The manifest signature is still verified, but the release");
            eprintln!("   pipeline (key, CI, provenance) may differ.");
            eprintln!();
        }
    }

    release::ReleaseSource::parse(url)
}

fn install(
    source: Option<String>,
    channel: String,
    source_mode: bool,
    branch: String,
) -> anyhow::Result<()> {
    let home = hermes_home()?;

    if source_mode {
        return install_source(&home, &branch);
    }

    let source = release_source(source)?;
    let argv: Vec<String> = std::env::args().collect();
    let _marker = apply::UpdateMarker::acquire(&home)?;
    let manifest = apply::apply_release(apply::ApplyRequest {
        hermes_home: &home,
        source: &source,
        version: None,
        channel: &channel,
        trusted_pubkey: trusted_release_pubkey()?,
        argv: Some(&argv),
    })?;
    apply::activate_stable_launchers(&home, &manifest.version)?;
    println!("Installed Hermes {}", manifest.version);
    Ok(())
}

/// Clone the source repo and provision it via `hermes dev sync`.
fn install_source(home: &std::path::Path, branch: &str) -> anyhow::Result<()> {
    let checkout = home.join("hermes-agent");

    if checkout.join(".git").exists() {
        anyhow::bail!(
            "checkout already exists at {} — use `hermes update` to update it",
            checkout.display()
        );
    }

    std::fs::create_dir_all(home)?;
    let repo_url = "https://github.com/NousResearch/hermes-agent.git";

    println!("→ Cloning {repo_url} (branch: {branch})...");
    let status = std::process::Command::new("git")
        .args(["clone", "--depth", "1", "--branch", branch, repo_url])
        .arg(&checkout)
        .status()
        .context("failed to run git clone")?;
    if !status.success() {
        anyhow::bail!("git clone failed with status {status}");
    }

    // Stamp the install method so `hermes update` knows this is a source checkout.
    std::fs::write(checkout.join(".install_method"), "source\n")
        .context("failed to write .install_method")?;

    // Wire the command link: hermes → checkout's bin/hermes (if it exists)
    // or fall back to the venv python entry point after dev sync.
    let bin_dir = home.join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let launcher = checkout.join("bin").join("hermes");
    let link = bin_dir.join("hermes");
    let _ = std::fs::remove_file(&link);
    #[cfg(unix)]
    {
        if launcher.exists() {
            std::os::unix::fs::symlink(&launcher, &link)?;
        }
    }
    #[cfg(windows)]
    {
        if launcher.with_extension("exe").exists() {
            std::os::windows::fs::symlink_file(
                launcher.with_extension("exe"),
                link.with_extension("exe"),
            )?;
        }
    }

    // Also wire hermes-updater to the same binary (busybox-style).
    let updater_link = bin_dir.join("hermes-updater");
    let _ = std::fs::remove_file(&updater_link);
    #[cfg(unix)]
    {
        if launcher.exists() {
            std::os::unix::fs::symlink(&launcher, &updater_link)?;
        }
    }
    #[cfg(windows)]
    {
        if launcher.with_extension("exe").exists() {
            std::os::windows::fs::symlink_file(
                launcher.with_extension("exe"),
                updater_link.with_extension("exe"),
            )?;
        }
    }

    println!("→ Running hermes dev sync (venv, deps, builds)...");
    let venv_python = checkout.join("venv").join("bin").join("python");
    let python = if venv_python.exists() {
        venv_python
    } else {
        // dev sync will create the venv; use the system python for the call
        PathBuf::from("python3")
    };

    let status = std::process::Command::new(&python)
        .args(["-m", "hermes_cli.main", "dev", "sync"])
        .current_dir(&checkout)
        .env("HERMES_HOME", home)
        .status()
        .context("failed to run hermes dev sync")?;
    if !status.success() {
        anyhow::bail!("hermes dev sync failed with status {status}");
    }

    // After dev sync, the venv exists. Re-wire the command link to the
    // venv's hermes entry point if we didn't have a bin/hermes before.
    if !launcher.exists() {
        let venv_hermes = checkout.join("venv").join("bin").join("hermes");
        if venv_hermes.exists() {
            let _ = std::fs::remove_file(&link);
            #[cfg(unix)]
            std::os::unix::fs::symlink(&venv_hermes, &link)?;
            #[cfg(windows)]
            {
                let exe = venv_hermes.with_extension("exe");
                if exe.exists() {
                    std::os::windows::fs::symlink_file(exe, link.with_extension("exe"))?;
                }
            }
        }
    }

    println!("✓ Source install complete at {}", checkout.display());
    println!("  Run `hermes` to start, or `hermes setup` to configure.");
    Ok(())
}

fn apply(
    source: Option<String>,
    version: Option<String>,
    notify_file: Option<String>,
    relaunch_app: Option<String>,
    _report: String,
) -> anyhow::Result<()> {
    let home = hermes_home()?;
    let source = release_source(source)?;
    // Acquire the update marker BEFORE apply_release so the entire
    // download→verify→stage→preflight→commit→flip→restart→notify
    // critical section is mutually exclusive.
    let _marker = apply::UpdateMarker::acquire(&home)?;
    let argv: Vec<String> = std::env::args().collect();

    let manifest = match apply::apply_release(apply::ApplyRequest {
        hermes_home: &home,
        source: &source,
        version: version.as_deref(),
        channel: "stable",
        trusted_pubkey: trusted_release_pubkey()?,
        argv: Some(&argv),
    }) {
        Ok(manifest) => manifest,
        Err(error) => {
            // Item 9: Report terminal failures to detached gateway/desktop
            // callers BEFORE returning the error. Write the exit code and
            // error message to the notify files so the watcher can pick
            // them up.
            let error_message = format!("Update failed: {error:#}");
            eprintln!("{error_message}");
            if let Err(write_err) = services::write_notify_files(
                &home,
                1,
                &error_message,
                notify_file.as_deref(),
            ) {
                eprintln!("warning: cannot write failure notify files: {write_err}");
            }
            return Err(error);
        }
    };

    apply::activate_stable_launchers(&home, &manifest.version)?;
    if let Err(error) = apply::apply_feature_ledger(&home, &manifest.version) {
        eprintln!("warning: feature ledger application failed: {error:#}");
    }
    if let Err(error) = services::restart_gateway(&home, &manifest.version) {
        eprintln!("warning: gateway restart failed: {error:#}");
    }

    // Item 8: Wire slot GC into production. Run after a successful apply
    // (after flip). Keep current/previous and the configured keep count
    // (default 2). Tolerate locked old Windows slots for later runs.
    let removed = slots::gc(&home, 2);
    if let Ok(removed) = &removed {
        if !removed.is_empty() {
            println!("  GC removed old slots: {}", removed.join(", "));
        }
    } else if let Err(e) = &removed {
        // GC failure is non-fatal — don't block the update.
        eprintln!("warning: slot GC failed: {e}");
    }

    services::write_notify_files(
        &home,
        0,
        &format!("Updated Hermes to {}", manifest.version),
        notify_file.as_deref(),
    )?;

    // Item 14: Relaunch desktop from the newly active slot, not the old
    // absolute executable. If the new slot has a desktop/ directory, spawn
    // from there. Otherwise fall back to the provided path.
    if let Some(executable) = relaunch_app {
        let slot_desktop = slots::slot_path(&home, &manifest.version).join("desktop");
        let launch_path = if slot_desktop.is_dir() {
            // Find the desktop entry in the new slot's desktop/ directory.
            // On POSIX this is typically an executable or shell script;
            // on Windows it's an .exe.
            let entries: Vec<_> = std::fs::read_dir(&slot_desktop)
                .into_iter()
                .flatten()
                .flatten()
                .collect();
            let desktop_entry = entries.into_iter().find_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let path = e.path();
                if cfg!(windows) {
                    if name.ends_with(".exe") {
                        Some(path)
                    } else {
                        None
                    }
                } else {
                    // On POSIX, look for an executable file
                    if path.is_file() {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            if let Ok(meta) = path.metadata() {
                                if meta.permissions().mode() & 0o111 != 0 {
                                    return Some(path);
                                }
                            }
                        }
                        None
                    } else {
                        None
                    }
                }
            });
            desktop_entry.unwrap_or_else(|| PathBuf::from(executable))
        } else {
            PathBuf::from(executable)
        };
        if let Err(e) = std::process::Command::new(&launch_path).spawn() {
            eprintln!("warning: cannot relaunch desktop from {}: {e}", launch_path.display());
        }
    }
    println!("Updated Hermes to {}", manifest.version);
    Ok(())
}

fn rollback() -> anyhow::Result<()> {
    let home = hermes_home()?;
    // Acquire the update marker so rollback is mutually exclusive with applies.
    let _marker = apply::UpdateMarker::acquire(&home)?;

    let version = match slots::rollback(&home) {
        Ok(version) => version,
        Err(error) => {
            // Item 9: Report terminal failures to detached callers.
            let error_message = format!("Rollback failed: {error:#}");
            eprintln!("{error_message}");
            if let Err(write_err) =
                services::write_notify_files(&home, 1, &error_message, None)
            {
                eprintln!("warning: cannot write failure notify files: {write_err}");
            }
            return Err(error);
        }
    };

    // Item 7: Give rollback the same post-flip lifecycle as apply.
    // After rollback flip: activate stable launchers, restart gateway,
    // write notify files, and relaunch desktop if applicable.
    apply::activate_stable_launchers(&home, &version)?;
    if let Err(error) = apply::apply_feature_ledger(&home, &version) {
        eprintln!("warning: feature ledger application failed: {error:#}");
    }
    if let Err(error) = services::restart_gateway(&home, &version) {
        eprintln!("warning: gateway restart failed: {error:#}");
    }
    services::write_notify_files(
        &home,
        0,
        &format!("Rolled back Hermes to {}", version),
        None,
    )?;

    println!("Rolled back to {}", version);
    Ok(())
}

#[derive(serde::Serialize)]
struct StatusChangelog {
    version: String,
    summary: String,
    author: String,
    at: i64,
    sha: String,
}

#[derive(serde::Serialize)]
struct StatusReport {
    current_version: Option<String>,
    previous_version: Option<String>,
    channel: String,
    staged_leftovers: Vec<String>,
    latest_version: Option<String>,
    update_available: bool,
    behind: usize,
    changelog: Vec<StatusChangelog>,
    current_sha: Option<String>,
    target_sha: Option<String>,
    error: Option<String>,
}

fn status_report(hermes_home: &std::path::Path, check: bool) -> StatusReport {
    let current = slots::resolve_current(hermes_home).unwrap_or(None);
    let previous = slots::resolve_previous(hermes_home).unwrap_or(None);
    let versions = hermes_home.join("versions");
    let mut staged_leftovers: Vec<String> = std::fs::read_dir(&versions)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            name.strip_suffix(".staging").map(str::to_owned)
        })
        .collect();
    staged_leftovers.sort();
    let current_manifest = current
        .as_deref()
        .map(|version| versions.join(version).join("manifest.json"))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<release::Manifest>(&bytes).ok());
    let channel = current_manifest
        .as_ref()
        .map(|manifest| manifest.channel.clone())
        .unwrap_or_else(|| "stable".to_owned());
    let mut report = StatusReport {
        current_version: current.clone(),
        previous_version: previous,
        channel: channel.clone(),
        staged_leftovers,
        latest_version: None,
        update_available: false,
        behind: 0,
        changelog: Vec::new(),
        current_sha: current_manifest.map(|manifest| manifest.git_sha),
        target_sha: None,
        error: None,
    };
    if check {
        match release_source(None).and_then(|source| source.history(&channel)) {
            Ok(history) => {
                report.latest_version = history.first().map(|item| item.version.clone());
                report.target_sha = history.first().and_then(|item| item.target_sha.clone());
                let newer: Vec<_> = history
                    .iter()
                    .take_while(|item| Some(&item.version) != current.as_ref())
                    .collect();
                report.update_available =
                    !newer.is_empty() && report.latest_version.as_ref() != current.as_ref();
                report.behind = if report.update_available {
                    newer.len()
                } else {
                    0
                };
                report.changelog = newer
                    .into_iter()
                    .map(|item| StatusChangelog {
                        version: item.version.clone(),
                        summary: item.summary.clone(),
                        author: String::new(),
                        at: item
                            .published_at
                            .as_deref()
                            .and_then(|value| {
                                time::OffsetDateTime::parse(
                                    value,
                                    &time::format_description::well_known::Rfc3339,
                                )
                                .ok()
                            })
                            .map(|value| value.unix_timestamp())
                            .unwrap_or(0),
                        sha: item.target_sha.clone().unwrap_or_default(),
                    })
                    .collect();
            }
            Err(error) => report.error = Some(error.to_string()),
        }
    }
    report
}

fn status(check: bool, json: bool) -> anyhow::Result<()> {
    let report = status_report(&hermes_home()?, check);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("hermes-updater 0.1.0");
    println!(
        "  current:  {}",
        report.current_version.as_deref().unwrap_or("(none)")
    );
    println!(
        "  previous: {}",
        report.previous_version.as_deref().unwrap_or("(none)")
    );
    println!("  channel:  {}", report.channel);
    println!(
        "  staged:   {}",
        if report.staged_leftovers.is_empty() {
            "(none)".to_owned()
        } else {
            report.staged_leftovers.join(", ")
        }
    );
    if check {
        if let Some(error) = report.error {
            println!("  check:    failed: {}", error);
        } else {
            println!(
                "  latest:   {} ({} release(s) behind)",
                report.latest_version.as_deref().unwrap_or("(none)"),
                report.behind
            );
        }
    }
    Ok(())
}

fn adopt(from_checkout: Option<String>, source: Option<String>, undo: bool) -> anyhow::Result<()> {
    let hermes_home = hermes_home()?;

    let checkout = match from_checkout {
        Some(path) => std::path::PathBuf::from(path),
        None => {
            // Default to the current checkout (PROJECT_ROOT)
            std::path::PathBuf::from(".")
        }
    };

    let trusted_pubkey = if undo { "" } else { trusted_release_pubkey()? };
    adopt::adopt(
        &hermes_home,
        &checkout,
        source.as_deref(),
        undo,
        trusted_pubkey,
    )
}

fn self_restage() -> anyhow::Result<()> {
    let home = hermes_home()?;
    let version =
        slots::resolve_current(&home)?.ok_or_else(|| anyhow::anyhow!("no current managed slot"))?;
    apply::activate_stable_launchers(&home, &version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    struct EnvRestore {
        key: &'static str,
        value: Option<OsString>,
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            match &self.value {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn hermes_home_honors_environment_override() {
        let _restore = EnvRestore {
            key: "HERMES_HOME",
            value: std::env::var_os("HERMES_HOME"),
        };
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("HERMES_HOME", temp.path());

        assert_eq!(hermes_home().unwrap(), temp.path());
    }

    #[test]
    fn status_report_includes_slot_metadata_and_staging() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join("versions/1.0.0")).unwrap();
        std::fs::create_dir_all(temp.path().join("versions/2.0.0.staging")).unwrap();
        std::fs::write(temp.path().join("current.txt"), "1.0.0\n").unwrap();
        std::fs::write(temp.path().join("previous.txt"), "0.9.0\n").unwrap();
        std::fs::write(
            temp.path().join("versions/1.0.0/manifest.json"),
            serde_json::json!({
                "schema": 1,
                "version": "1.0.0",
                "channel": "nightly",
                "git_sha": "abc",
                "platform": "linux-x64",
                "min_updater_version": "0.1.0",
                "desktop": false,
                "files": {}
            })
            .to_string(),
        )
        .unwrap();

        let report = status_report(temp.path(), false);
        assert_eq!(report.current_version.as_deref(), Some("1.0.0"));
        assert_eq!(report.previous_version.as_deref(), Some("0.9.0"));
        assert_eq!(report.channel, "nightly");
        assert_eq!(report.current_sha.as_deref(), Some("abc"));
        assert_eq!(report.staged_leftovers, ["2.0.0"]);
    }

    #[test]
    fn test_status_works() {
        // status is the one verb that isn't a stub — it prints a version line.
        assert!(status(false, false).is_ok());
    }
}
