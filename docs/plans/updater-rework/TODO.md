# Updater rework TODO

Review follow-up for `ethie/updater-rework`, written 2026-07-16 against the implementation specs in this directory and `docs/updater-world.md`.

This is an implementation checklist, not a record that the phases are complete. Items are grouped by urgency. A checked item should have a behavioral test or E2E observable proving the contract, not only helper-level unit coverage.

## Release blockers

### Bundle and release contract

- [x] Make the Python manifest writer and Rust verifier agree about symlinks.
  - `scripts/release/write-manifest.py:63-75,130-146` skips symlinks.
  - `apps/hermes-launcher/src/release.rs:457-467,503-515` follows file symlinks and rejects them as unsigned extras.
  - Reproduced: the Python verifier accepted a signed fixture while the Rust updater rejected `runtime/venv/bin/python` as an extra file.
  - Covered: Rust `walkdir_inner` now uses `entry.file_type()` (lstat, non-following) to skip symlinks. Tests on both sides: `test_verify_bundle_with_symlinks` (Rust) + `test_bundle_with_file_symlink_verifies` (Python).
  - Spec: `01-phase0-bundles.md:271-280`.

- [x] Validate signed manifest identity before activation.
  - `validate_manifest_identity()` in `apply.rs` asserts platform, channel, version path safety, and version match after signature verification, before preflight.
  - Tests: 5 new covering happy path, platform/channel/version mismatch, path-traversal rejection.
  - Spec: `01-phase0-bundles.md:248-280`.

- [x] Make signing fail closed in the canonical manifest writer.
  - `sign_manifest()` now raises `RuntimeError` when PyNaCl is absent instead of returning False. `main()` treats signing as mandatory.
  - Stale minisign/`.minisig` prose in module docstring reconciled with the implemented Ed25519 JSON `.sig` protocol.
  - Algorithm validated on both sides: Python `verify_signature()` + Rust `verify_bundle()` reject non-ed25519 algorithms.
  - Tests: `test_verify_rejects_wrong_algorithm` (Python + Rust).
  - Spec: `01-phase0-bundles.md:248-280`.

- [x] Establish one explicit bootstrap trust root.
  - Rust updater's public key now embedded in-repo via `include_str!("keys/hermes-release.pub")` as primary trust root; `HERMES_RELEASE_PUBLIC_KEY` remains as CI/testing override.
  - Canonical release source embedded at compile time via `HERMES_RELEASE_SOURCE` (defaults to official GitHub releases URL); `--source` override prints a scary warning when it differs from the embedded default.
  - `http://` release sources now rejected by `ReleaseSource::parse()` — only `https://` and `file://` accepted.
  - Install scripts/adopt.py still trust exe+checksum from same origin (documented design limitation — the updater's signature verification is the real trust boundary once a trusted updater is installed).
  - Spec: `02-phase1-updater.md:115-125`; `03-phase2-compat-and-adoption.md:172-180`.

- [x] Record the desktop correctly in published manifests.
  - `write_manifest()` now auto-detects `desktop/` directory presence when `desktop=None` (the default), so CI that builds desktop via `build-bundle.sh` produces correct manifests without needing `--desktop`.
  - Tests: `test_desktop_auto_detected_when_dir_present` + `test_desktop_auto_detected_when_dir_absent`.

- [x] Make Node acquisition reproducible and integrity-checked.
  - `build-bundle.sh` now downloads `SHASUMS256.txt` from the Node.js release directory and verifies the tarball's sha256 before unpacking. Fails closed on checksum mismatch or missing SHASUMS file.
  - Resolved full Node version (e.g. `v22.12.1`) recorded in `runtime/node/.node-version` for reproducibility auditing.
  - Spec: `01-phase0-bundles.md:227-234`.

### Updater atomicity and lifecycle

- [x] Implement real updater mutual exclusion and acquire it before the commit point.
  - `UpdateMarker::acquire()` now uses atomic create-new (`O_CREAT|O_EXCL`) instead of `fs::write`; concurrent updaters get an error instead of overwriting each other.
  - Stale marker reclamation: checks owner PID (`kill(pid,0)` on Unix, `OpenProcess` on Windows) and age (>10min); reclaims if dead/stale, refuses if active.
  - Marker acquisition moved BEFORE `apply_release()` in `main.rs` so the entire download→verify→stage→preflight→commit→flip→restart→notify critical section is mutually exclusive.
  - Tests: `update_marker_rejects_concurrent_acquisition` + `update_marker_reclaims_stale_pid`.
  - Spec: `02-phase1-updater.md:171-175`; `docs/updater-world.md:524-528`.

- [x] Wire `min_updater_version` and the bootstrap hop into production apply.
  - `apply_release()` now reads `manifest.min_updater_version` after signature verification and before preflight/stage; calls `selfupdate::hop()` if `needs_hop(env!("CARGO_PKG_VERSION"), min_updater_version)` is true.
  - One-shot `--hopped` guard preserved — hopped binary doesn't re-hop.
  - `ApplyRequest` gained `argv: Option<&[String]>` for the re-exec; all three callers (apply, install, adopt) updated.
  - Also fixed install path which had marker-after-apply bug (same as apply path).
  - Spec: `02-phase1-updater.md:204-233`; `docs/updater-world.md:450-502`.

- [x] Never delete or replace an active/previous immutable slot in place.
  - `apps/hermes-launcher/src/slots.rs:94-98` removes an existing version directory before rename.
  - Same-version apply can delete the current slot and create a crash window where `current.txt` points at nothing.
  - Reuse an already valid slot or refuse replacement while current/previous/running processes may reference it.
  - Add same-version apply and long-running-process tests.

- [x] Make `current.txt` / `previous.txt` transition crash-consistent.
  - `slots.rs:145-152` commits `current.txt` before writing `previous.txt` non-atomically.
  - `rollback()` repeats a non-atomic previous write at `:176-184`.
  - Prepare and sync rollback state before the current commit and define recoverable states for failures at every boundary.
  - Add fault-injection tests; the current “atomic” test only checks final successful content at `slots.rs:397-405`.

- [x] Complete the fsync protocol and stop discarding fsync failures.
  - `slots.rs:100-107` syncs only the top staging directory and ignores the result.
  - Nested files/directories, the `versions/` parent after slot rename, and `$HERMES_HOME` after pointer replacement are not synced.
  - Spec: `02-phase1-updater.md:134-145`; `docs/updater-world.md:358-368`.

- [x] Fail closed on Windows staged preflight.
  - `apps/hermes-launcher/src/apply.rs:228-237` converts every Windows preflight failure into success.
  - The claimed first-launch venv repair does not exist in `launch.rs`.
  - Fix relocatability before activation and distinguish specific diagnosable failures if needed; do not bypass all import/config/artifact checks.
  - Add real Windows apply/preflight coverage.

- [x] Make Windows self-restage failure-safe and run old-binary sweeping.
  - `selfupdate.rs:125-133` ignores old-exe rename failure and copies directly into the canonical path.
  - Prepare the new binary first, require a safe rename/move sequence, and preserve a canonical working updater on every failure.
  - `sweep_old_binaries()` is never called.
  - Add Windows failure-injection and next-run sweep tests.

- [x] Decide whether `$HERMES_HOME/bin/hermes` is stable or self-restaged, then implement the design consistently.
  - `activate_stable_launchers()` rewrites the launcher on every apply: `apps/hermes-launcher/src/apply.rs:63-87`.
  - `docs/updater-world.md:383-390` says the stable launcher is never rewritten during ordinary updates and only the staged updater needs ceremony.
  - Ensure launcher compatibility with future slots without introducing a second lock-sensitive self-update path.

- [x] Give rollback the same post-flip lifecycle as apply.
  - `apps/hermes-launcher/src/main.rs:303-307` only flips pointers and prints success.
  - Reconcile launcher/updater state, feature handling if needed, service drain/restart, desktop relaunch behavior, and notification output.

- [x] Wire slot GC into production.
  - `slots::gc()` exists only as dead unit-tested code: `apps/hermes-launcher/src/slots.rs:191-238`.
  - Keep current/previous and the configured keep count; tolerate locked old Windows slots for later runs.

- [x] Report terminal failures to detached gateway/desktop callers.
  - Managed apply writes notification state only on success: `apps/hermes-launcher/src/main.rs:273-300`.
  - `--notify-file` writes only an exit code and skips output at `services.rs:46-49`.
  - Preserve the expected `.update_exit_code` + `.update_output.txt` contract and report failures before returning.

### Launcher behavior

- [x] Forward `hermes --version` to the active Hermes tree instead of Clap's launcher version.
  - Root Clap handling intercepts it at `apps/hermes-launcher/src/cli.rs:8-10`.
  - Direct probe printed `hermes 0.1.0` instead of the active Hermes version.
  - Spec: `02-phase1-updater.md:104-106,316-317`.

- [x] Build child environments with platform-native path handling and the correct interpreter.
  - `tree.rs:131-137,155-157` hardcodes `:` separators, breaking Windows PATH.
  - `tree.rs:140,159` sets `UV_PYTHON` to the venv directory rather than its interpreter.
  - Checkout resolution omits the legacy `venv` fallback.
  - Use `split_paths`/`join_paths` and test Windows path shapes.

- [x] Run the launcher health probe under the sanitized child environment.
  - `launch.rs:52-76` imports before `build_child_env()` is applied at `:79-91`.
  - Inherited `PYTHONPATH`/`PYTHONHOME` can false-pass, false-fail, and poison the cached health stamp.

- [x] Implement the strict cwd guard in the bare-checkout stub.
  - `bin/hermes` and `bin/hermes.cmd` do not inspect cwd or enforce `--dev`/`--global`.
  - Direct probe from the checkout with plain `./bin/hermes --version` exited 0.
  - Test both the native-launcher path and the no-native-launcher fallback path.
  - Spec: `04-phase3-ejected-dev.md:166-207`.

### Desktop and Docker

- [x] Relaunch desktop from the newly active slot, not the old absolute executable.
  - Electron sends `process.execPath`: `apps/desktop/electron/main.ts:2606-2613`.
  - Tauri sends `std::env::current_exe()`: `apps/bootstrap-installer/src-tauri/src/update.rs:209-218`.
  - The updater blindly spawns that exact path at `apps/hermes-launcher/src/main.rs:296-298`.
  - Resolve the new slot's platform desktop entry after the flip and prove the relaunched process reports v2.

- [x] Classify desktop updates by the active tree before global managed-home state.
  - `apps/desktop/electron/update-status.ts:252-275` returns `slot` whenever `$HERMES_HOME/versions` + `current.txt` exist, even if `activeHermesRoot` is an ejected checkout.
  - Preserve managed-slot and checkout coexistence; checkout-built desktop must route to worktree update.

- [x] Reduce Tauri update mode to the specified thin updater event shell.
  - `apps/bootstrap-installer/src-tauri/src/update.rs` still owns marker creation, old checkout lock probing, force-kill behavior, synthetic rebuild/install stages, macOS bundle swap, retry-era helpers, and a second desktop launch.
  - It passes `--relaunch-app` and then launches again itself.
  - Remove stale stages and old orchestration after the updater owns those responsibilities.
  - Spec: `05-phase4-desktop.md:78-99`.

- [x] Wire Docker CI to the required `hermes_bundle` BuildKit context.
  - `Dockerfile:3-8` requires `FROM hermes_bundle AS bundle`.
  - `.github/workflows/docker.yml:52-63,75-89` supplies only `context: .` and neither builds/downloads a bundle nor sets `build-contexts`.
  - Build the same release artifact once, pass it to Docker, then run the existing image tests.
  - Spec: `06-phase5-ledger-and-sunset.md:65-85`.

## High-priority behavior gaps

### CLI and source-mode wiring

- [x] Register the public `hermes dev` command in the real CLI parser.
  - `hermes_cli/main.py` imports `build_dev_parser` but does not call it in the registration sequence.
  - Direct runtime proof from the review: `python -m hermes_cli.main dev status` exited 2 with `invalid choice: 'dev'`.
  - Add parser/dispatch tests; current tests call private handlers directly.
  - Spec: `04-phase3-ejected-dev.md:46-106`.

- [x] Restore or deliberately revise the documented `--in-place` / unavailable-worktree fallback contract.
  - The update parser exposes no `--in-place`; `_cmd_update_impl()` hardcodes `in_place=False`.
  - Worktree creation/unavailability fails closed rather than using the retained legacy path.
  - The phase spec and sunset checklist say the fallback still exists; tests were changed to assert the opposite.
  - Decide the intended design, update implementation/spec/checklist together, and add public CLI tests.
  - Spec: `04-phase3-ejected-dev.md:133-164,285-287`.

- [x] Run `dev sync` after a clean checkout fast-forward.
  - `hermes_cli/dev_update.py:436-443` returns immediately after `git pull --ff-only`.
  - Dependency, launcher, ledger, and frontend changes are left unsynced.
  - Verify a clean update containing Python lockfile and UI changes reaches a launchable fresh state.

- [x] Make launcher installation during `dev sync` best-effort, integrity-checked, and point at a published asset.
  - `hermes_cli/dev_sync.py:456-480` requests `hermes-<platform>`, while release CI publishes `hermes-updater-<platform>`.
  - It has no checksum/signature verification and turns all failures into fatal `DevSyncError`, contradicting the stub-fallback design.
  - Add success, offline/missing asset, bad checksum, and fallback tests.

- [x] Extract/reuse the established Python dependency fallback instead of duplicating a weaker one.
  - `dev_sync.py:426-451` implements its own `uv sync` → `uv pip install -e .[all]` ladder.
  - It omits the established per-extra fallback and verification behavior in `hermes_cli/main.py`.
  - Spec: `04-phase3-ejected-dev.md:82-87`.

- [x] Do not mutate or filter the original checkout to claim byte-identical worktree switching.
  - `dev_update.py:294-315` may edit/create `.gitignore` before status capture.
  - `_git_porcelain_status()` hides selected infrastructure changes at `:115-139`.
  - Compare raw status/index/worktree state and leave the tree exactly unchanged; this repo already ignores `.worktrees/`.

- [x] Make `detect_tree_kind()` reject unknown trees.
  - `hermes_cli/dev_sync.py:39-51` returns `checkout` for every path without `manifest.json`, despite documenting `.git` + `pyproject.toml` requirements.

- [x] Consolidate duplicate worktree GC implementations and protect the actual active PATH target.
  - `hermes_cli/subcommands/dev.py` duplicates `hermes_cli/dev_update.py` GC behavior and checks the wrong activation mechanism.

### Adoption and eject

- [x] Honor `updates.adopt` consistently in both launch-time and `hermes update` paths.
  - `_cmd_update_impl()` auto-adopts any pristine checkout regardless of `auto|prompt|never`: `hermes_cli/main.py:7819-7840`.
  - Default config is currently `auto`, while phase 2 specifies `prompt`: `hermes_cli/config.py:3148-3153` vs `03-phase2-compat-and-adoption.md:135-139`.
  - Add `never`, `prompt`, and `auto` dispatch tests.

- [x] Make launch-time auto-adoption a real handoff rather than detached parallel mutation.
  - `hermes_cli/adoption_offer.py:151-159` uses `Popen(["hermes", "adopt", "--yes"], start_new_session=True)` and lets normal startup continue.
  - It also omits required cwd intent inside a checkout.
  - Replace the process or exit after starting a verified updater; never continue booting alongside adoption.

- [x] Implement Windows adoption activation and undo.
  - `apps/hermes-launcher/src/adopt.rs:53-80,120-129` has Unix-only symlink mutation and no Windows copy/hardlink equivalent.

- [x] Capture and preserve old lazy-feature intent before adoption flips.
  - Rust adoption flips and invokes the new slot's ledger, which cannot discover optional features present only in the old venv.
  - Write/merge `features.pending.json` from the old checkout before activation.
  - Extend the historical adoption E2E by activating a real feature before migration.
  - Spec: `03-phase2-compat-and-adoption.md:210-212`.

- [x] Validate checkout invariants before committing external adoption changes, or rollback on late failure.
  - `adopt.rs:36-80` flips and repoints PATH before checking checkout HEAD/status at `:89-96`.
  - A late failure currently reports adoption failure after managed activation already happened.

- [x] Fail eject before PATH activation when clone/checkout or `dev sync` fails.
  - `hermes_cli/subcommands/eject.py:289-317` warns on failed provisioning and still activates the checkout.
  - Existing non-empty destination fetch/checkout return codes are ignored at `:95-109`.
  - Update tests that currently enshrine activation after sync failure.

### Feature ledger and artifact roots

- [x] Record feature intent when `ensure()` finds dependencies already satisfied.
  - `tools/lazy_deps.py:766-768` returns before `record_feature()` at `:880-882`.
  - Using a feature already present via `[all]` or a bundle can therefore fail to persist intent.
  - Revisit the test currently asserting non-recording for an already-satisfied feature.

- [x] Merge and consume `features.pending.json` even when `features.json` already exists.
  - Pending merge only occurs during absent-ledger seeding: `tools/lazy_deps.py:1093-1125,1153-1163`.
  - Reproduced during review: existing ledger remained unchanged and the pending file remained present.

- [x] Complete artifact-root migration for bundled providers/plugins and other repo-relative assets.
  - `providers/__init__.py` still derives model-provider plugins relative to site-packages rather than slot `app/plugins/model-providers`.
  - Extend the inventory beyond skills/web/TUI and make preflight exercise each load-bearing asset consumer.

## Required test and CI work

- [x] Run the phase-0 bare-container bundle boot gate in CI and make it fail closed.
  - No workflow invokes `scripts/e2e/test-bundle-boot.sh`.
  - Its local fallback still prints `E2E_PASS`, permits failed `doctor --preflight`, and warns instead of failing for a missing manifest.
  - The mandatory gate must require Docker/Podman isolation or a dedicated CI container job.

- [x] Strengthen the managed slot lifecycle E2E.
  - Replace the `sleep` + `cat VERSION` simulated old process with a real Hermes process/API identity check.
  - Add interruption during download/staging, concurrent apply exclusion, bootstrap hop, restage failure containment, same-version apply, rollback restart, and crash/fault points around `previous.txt`/`current.txt`.
  - Add a real macOS lifecycle job and expand Windows beyond install/status/self-restage to apply/rollback/tamper/preflight.

- [x] Replace the phase-3 E2E's fake provisioner with real `hermes dev sync` coverage.
  - `scripts/e2e/test-ejected-worktrees.sh:63-75` writes a fake launcher instead of independent venvs/builds.
  - Cover stub and native launcher paths; original checkout ↔ new worktree ↔ managed slot switching; raw unchanged git state; and GC preserving the active target.

- [x] Implement the packaged desktop E2E in its intended separate workstream.
  - Known expected gap for this review.
  - The checked-in `scripts/e2e/test-desktop-update.sh` currently calls nonexistent `apps/desktop/e2e/desktop-update.mjs` and uses a fake venv interpreter.
  - Until the real harness lands, do not label this script a working “real packaged Electron updater gate” or wire it as a passing requirement.

- [x] Add public parser/dispatch tests for `hermes dev`, `hermes update --in-place` if retained, adoption policy, launcher `--version`, updater reporting, rollback lifecycle, and Docker refusal at the native updater layer.

- [x] Make Rust lint clean.
  - `cargo test --locked`: 65 passed.
  - `cargo clippy --locked --all-targets -- -D warnings`: failed with dead production functions and unused imports.

- [x] Remove the blank-line-at-EOF `git diff --check` warning in `apps/desktop/electron/update-status.test.ts` when code edits resume.

## Documentation and sunset consistency

- [x] Reconcile the default install flip with its gate and documentation.
  - Both installers already default to bundle mode, but `default-flip.md` still says the change is gated/not active.
  - Record maintainer sign-off and required green-window evidence, or revert the default until the gate is met.
  - Update POSIX help text, English installation docs, and the zh-Hans mirror to describe managed default vs `--source`.

- [x] Fill the Windows verification checklist with real results.
  - `windows-verification.md` has no checked PASS cells.
  - Do not count a workflow as passing stale-updater cleanup while `hermes-updater.old.exe` remains.

- [x] Update the sunset checklist to match actual deletions and surviving mechanisms.
  - `gateway/code_skew.py` and substantial legacy updater code/tests were already deleted, while checklist entries remain unchecked.
  - Other entries describe fallbacks that no longer exist even though their deletion is still marked pending.
  - Give each deletion its own precondition and behavioral verification as required by phase 5.

- [x] Remove stale comments and dead retry/update-era helpers after behavior is settled.
  - Tauri comments/stages describe operations no longer performed.
  - Rust dead-code warnings expose advertised but unwired functionality.

## Verification baseline from the review

The following checks passed on 2026-07-16, but do not close the TODOs above:

- `cargo test --locked`: 65 passed.
- Targeted Python updater/adoption/dev/ledger/artifact tests: 181 passed in the parent review; the delegated Python review separately ran 95 targeted tests successfully.
- Desktop routing vitest: 29 passed.
- Release/Docker/classifier delegated checks: 52 passed.
- `scripts/e2e/test-slot-lifecycle.sh`: passed its current install/apply/rollback/tamper/feature-ledger fixture.
- Shell syntax checks for release/E2E scripts: passed.
- Worktree was clean after the read-only review.

These green results establish a useful baseline. They do not prove the missing production wiring, crash behavior, platform behavior, or false-pass gates listed above.
