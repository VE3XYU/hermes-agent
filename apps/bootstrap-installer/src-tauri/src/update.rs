//! Update orchestration — thin event shell.
//!
//! Driven when the installer is launched as `Hermes-Setup.exe --update` (see
//! `AppMode` in lib.rs). The desktop app hands off to us — it exits, then we
//! exec `hermes-updater apply --report json` and stream its events onto the
//! existing `BootstrapEvent` channel.
//!
//! Per phase 4 task 4.2 (05-phase4-desktop.md:78-99):
//!   - The updater owns marker creation, old checkout lock probing,
//!     force-kill behavior, and all download/verify/stage/preflight/flip/
//!     restart/notify logic.
//!   - This module is a thin shell: spawn the updater, relay its
//!     `--report json` events as progress UI stages, report completion.
//!   - No `--relaunch-app` is passed — the updater handles relaunch from
//!     the new slot's `desktop/` directory.
//!   - No second desktop launch — the updater does that.
//!   - No synthetic rebuild/install stages or macOS bundle swap.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::events::{BootstrapEvent, LogStream, StageInfo, StageState};

/// Guards against concurrent update runs. The frontend kicks `startUpdate()`
/// from a mount effect, which can fire more than once (React strict-mode
/// double-invokes effects in dev; a window reload or stray re-init can do it
/// in prod). Exactly one task may hold this flag at a time.
static UPDATE_RUNNING: AtomicBool = AtomicBool::new(false);

/// Frontend → Rust: kick off the update flow. Mirrors `start_bootstrap`'s
/// fire-and-forget shape; progress arrives on the `bootstrap` event channel.
#[tauri::command]
pub async fn start_update(app: AppHandle) -> Result<(), String> {
    // Re-entrancy guard (see UPDATE_RUNNING). compare_exchange lets exactly one
    // caller flip false→true; any concurrent caller no-ops instead of spawning
    // a second racing update.
    if UPDATE_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        // Already running: re-emit the manifest so a duplicate startUpdate()
        // call (which resets the frontend store) can recover its stage list.
        emit(
            &app,
            BootstrapEvent::Manifest {
                stages: update_stages(),
                protocol_version: None,
            },
        );
        return Ok(());
    }
    tokio::spawn(async move {
        if let Err(err) = run_update(app.clone()).await {
            // run_update already emits a Failed event on the paths that matter;
            // this catches anything that escaped. Emit defensively.
            emit(
                &app,
                BootstrapEvent::Failed {
                    stage: None,
                    error: format!("{err:#}"),
                },
            );
        }
        UPDATE_RUNNING.store(false, Ordering::SeqCst);
    });
    Ok(())
}

/// Thin shell: resolve the updater, spawn `hermes-updater apply --report json`,
/// stream its output, and map exit code → success/failure.
async fn run_update(app: AppHandle) -> Result<()> {
    let hermes_home = crate::paths::hermes_home();
    let install_root = hermes_home.join("hermes-agent");

    // The hermes-updater binary. In a managed install it's at
    // $HERMES_HOME/bin/hermes-updater (or .exe on Windows).
    let updater = resolve_hermes_updater(&hermes_home).ok_or_else(|| {
        let msg = format!(
            "Could not find hermes-updater under {}. Is Hermes installed? \
             Re-run the installer to repair the install.",
            hermes_home.display()
        );
        emit(
            &app,
            BootstrapEvent::Failed {
                stage: None,
                error: msg.clone(),
            },
        );
        anyhow!(msg)
    })?;

    // Emit the manifest so the progress UI renders our stage.
    emit(
        &app,
        BootstrapEvent::Manifest {
            stages: update_stages(),
            protocol_version: None,
        },
    );

    // ---- stage: hermes-updater apply ----------------------------------
    // The updater does: download → verify → stage → preflight → flip →
    // self-restage → restart services → relaunch desktop. We stream its
    // --report json output onto the BootstrapEvent channel so the progress
    // UI shows the live log underneath.
    //
    // We do NOT pass --relaunch-app — the updater resolves the new slot's
    // desktop/ entry and relaunches from there. We do NOT launch the desktop
    // ourselves.
    emit_stage(&app, "update", StageState::Running, None, None);
    let started = std::time::Instant::now();

    let child_env = update_child_env(&install_root);
    // No --relaunch-app: the updater handles relaunch from the new slot.
    let updater_args: Vec<String> = vec![
        "apply".into(),
        "--report".into(),
        "json".into(),
    ];

    let mut update = run_streamed(
        &app,
        &updater,
        &updater_args,
        &install_root,
        &child_env,
        Some("update"),
    )
    .await?;

    let update_ms = started.elapsed().as_millis() as u64;

    match update.exit_code {
        Some(0) => {
            emit_stage(&app, "update", StageState::Succeeded, Some(update_ms), None);
        }
        other => {
            let msg = format!(
                "hermes-updater apply failed (exit {:?}). See {} for details.",
                other,
                crate::paths::hermes_home()
                    .join("logs")
                    .join("update.log")
                    .display()
            );
            emit_stage(
                &app,
                "update",
                StageState::Failed,
                Some(update_ms),
                Some(msg.clone()),
            );
            emit(
                &app,
                BootstrapEvent::Failed {
                    stage: Some("update".into()),
                    error: msg.clone(),
                },
            );
            return Err(anyhow!(msg));
        }
    }

    // The updater has already flipped the slot, restaged itself, restarted
    // services, and relaunched the desktop from the new slot. We just signal
    // completion to the progress UI.
    emit(
        &app,
        BootstrapEvent::Complete {
            install_root: install_root.to_string_lossy().into_owned(),
            marker: None,
        },
    );

    Ok(())
}

/// Spawn `hermes-updater <args>` from `cwd`, stream stdout/stderr as Log events
/// on the bootstrap channel, and return the exit code.
async fn run_streamed(
    app: &AppHandle,
    program: &Path,
    args: &[String],
    cwd: &Path,
    envs: &[(String, OsString)],
    stage: Option<&str>,
) -> Result<CmdResult> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in envs {
        cmd.env(key, value);
    }

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW = 0x08000000 — no flashing console behind the GUI.
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("spawning {} {:?}: {e}", program.display(), args))?;

    let stdout = child.stdout.take().expect("stdout piped");
    let stderr = child.stderr.take().expect("stderr piped");
    let mut out = BufReader::new(stdout).lines();
    let mut err = BufReader::new(stderr).lines();

    let stage_owned = stage.map(|s| s.to_string());
    loop {
        tokio::select! {
            line = out.next_line() => match line {
                Ok(Some(l)) => emit_log(app, stage_owned.as_deref(), LogStream::Stdout, &l),
                Ok(None) => break,
                Err(e) => { tracing::warn!("stdout read error: {e}"); break; }
            },
            line = err.next_line() => match line {
                Ok(Some(l)) => emit_log(app, stage_owned.as_deref(), LogStream::Stderr, &l),
                Ok(None) => {}
                Err(e) => { tracing::warn!("stderr read error: {e}"); }
            },
        }
    }
    while let Ok(Some(l)) = out.next_line().await {
        emit_log(app, stage_owned.as_deref(), LogStream::Stdout, &l);
    }
    while let Ok(Some(l)) = err.next_line().await {
        emit_log(app, stage_owned.as_deref(), LogStream::Stderr, &l);
    }

    let status = child.wait().await.map_err(|e| anyhow!("waiting for child: {e}"))?;
    Ok(CmdResult {
        exit_code: status.code(),
    })
}

struct CmdResult {
    exit_code: Option<i32>,
}

/// Resolve the hermes-updater binary.
///
/// In a managed install, it's at `$HERMES_HOME/bin/hermes-updater`.
/// In a checkout/dev install, the launcher may be at
/// `.hermes-launcher/hermes` — but that's the launcher, not the updater.
/// For now we look in `$HERMES_HOME/bin/` (the managed-slot path) and
/// fall back to PATH.
fn resolve_hermes_updater(hermes_home: &Path) -> Option<PathBuf> {
    let exe = if cfg!(target_os = "windows") { "hermes-updater.exe" } else { "hermes-updater" };
    let managed = hermes_home.join("bin").join(exe);
    if managed.exists() {
        return Some(managed);
    }
    // PATH fallback.
    if let Ok(path) = std::env::var("PATH") {
        let sep = if cfg!(target_os = "windows") { ';' } else { ':' };
        for dir in path.split(sep) {
            let cand = Path::new(dir).join(exe);
            if cand.exists() {
                return Some(cand);
            }
        }
    }
    None
}

fn update_child_env(install_root: &Path) -> Vec<(String, OsString)> {
    let hermes_home = crate::paths::hermes_home();
    let mut envs = vec![(
        "HERMES_HOME".to_string(),
        hermes_home.as_os_str().to_os_string(),
    )];
    if let Some(path) = path_with_prepended_entries(&[
        hermes_home.join("node").join("bin"),
        venv_bin_dir(install_root),
    ]) {
        envs.push(("PATH".to_string(), path));
    }
    envs
}

fn venv_bin_dir(install_root: &Path) -> PathBuf {
    if cfg!(target_os = "windows") {
        install_root.join("venv").join("Scripts")
    } else {
        install_root.join("venv").join("bin")
    }
}

fn path_with_prepended_entries(entries: &[PathBuf]) -> Option<OsString> {
    let mut parts: Vec<PathBuf> = entries.to_vec();
    if let Some(existing) = std::env::var_os("PATH") {
        parts.extend(std::env::split_paths(&existing));
    }
    std::env::join_paths(parts).ok()
}

// ---------------------------------------------------------------------------
// Event helpers — keep emit shape identical to bootstrap.rs so the UI is reused
// ---------------------------------------------------------------------------

fn stage_info(name: &str, title: &str) -> StageInfo {
    StageInfo {
        name: name.to_string(),
        title: title.to_string(),
        category: "update".to_string(),
        needs_user_input: false,
    }
}

/// The update manifest. A single stage mirrors the updater's apply operation;
/// the updater streams its own `--report json` progress which we relay as
/// log lines underneath.
fn update_stages() -> Vec<StageInfo> {
    vec![
        stage_info("update", "Applying the update"),
    ]
}

fn emit(app: &AppHandle, event: BootstrapEvent) {
    if let Err(e) = app.emit(BootstrapEvent::CHANNEL, &event) {
        tracing::warn!(?e, "failed to emit update event");
    }
}

fn emit_stage(
    app: &AppHandle,
    name: &str,
    state: StageState,
    duration_ms: Option<u64>,
    error: Option<String>,
) {
    tracing::info!(stage = %name, ?state, ?duration_ms, ?error, "update stage");
    emit(
        app,
        BootstrapEvent::Stage {
            name: name.to_string(),
            state,
            duration_ms,
            result: None,
            error,
        },
    );
}

fn emit_log(app: &AppHandle, stage: Option<&str>, stream: LogStream, line: &str) {
    match stage {
        Some(s) => tracing::info!(target: "bootstrap.log", stage = %s, "{line}"),
        None => tracing::info!(target: "bootstrap.log", "{line}"),
    }
    emit(
        app,
        BootstrapEvent::Log {
            stage: stage.map(|s| s.to_string()),
            line: line.to_string(),
            stream,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_stages_has_single_update_stage() {
        let stages = update_stages();
        assert_eq!(stages.len(), 1, "thin shell has exactly one stage");
        assert_eq!(stages[0].name, "update");
        assert_eq!(stages[0].category, "update");
    }
}
