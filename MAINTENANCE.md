# Gateway Maintenance Log — 2026-07-22

## Session: Matrix stability deep-dive (CLU + Matthew)

### Issues diagnosed and fixed

#### 1. Media upload crash: "A coroutine object is required"
- **File:** `plugins/platforms/matrix/adapter.py` — `_run_on_client_loop()`
- **Root cause:** No-timeout path passed a raw `Task` to `run_coroutine_threadsafe()`, which requires a coroutine. `send()` never hit this because it always passed `timeout=45` → `wait_for()` wraps in a coroutine. `_upload_and_send()` omitted the timeout.
- **Fix:** Always wrap bridged Task in `asyncio.wait_for()` (120s default). Added explicit timeouts to both `_upload_and_send` call sites.
- **Commit:** `07556fcf0`

#### 2. "Token is not active" infinite retry loop (~12,800 errors)
- **File:** `plugins/platforms/matrix/adapter.py` — `_sync_loop()`
- **Root cause:** `MUnknownToken.__str__()` returns an empty string (mautrix 0.21.0 quirk). The sync loop's string-matching check for "401"/"403"/"unauthorized"/"forbidden" never matched. Fell into 5-second retry forever.
- **Fix:** Added `isinstance(exc, (MUnknownToken, MatrixInvalidToken))` check before string matching.
- **Commit:** same as above (combined commit)

#### 3. Duplicate gateway instances (token rotation race)
- **Root cause:** User-level systemd service was still running alongside the system service. Both logged in with password → each generated a new access token → invalidated the other's token.
- **Fix:** Stopped + masked user service, removed unit file:
  ```bash
  systemctl --user disable --now hermes-gateway
  rm ~/.config/systemd/user/hermes-gateway.service
  systemctl --user daemon-reload
  ```
- **Verification:** `ps aux | grep "gateway run"` shows exactly one process.

#### 4. Cron `last_status: "ok"` despite delivery failure
- **File:** `cron/scheduler.py` — `run_one_job()`
- **Root cause:** `success` flag only reflected agent completion, not delivery outcome. Delivery errors were tracked separately in `delivery_error` but didn't affect status.
- **Fix:** Added guard that downgrades `success` to `False` when `delivery_error` is set.
- **Commit:** `1e2ea41a5`
- **Tests:** 730 cron tests pass, 241 matrix tests pass.

### Infrastructure notes
- Gateway restart requires `sudo systemctl restart hermes-gateway` (hermes binary not in root PATH)
- Only one gateway instance should run at a time
- `.env` uses `MATRIX_PASSWORD` (not access token) — password login generates fresh tokens on restart
- Device ID: `hermes-brain-vm-02` (set via `MATRIX_DEVICE_ID` in `.env`)
- npm lockfiles bumped for `web/` and `ui-tui/` workspaces — zero vulnerabilities
