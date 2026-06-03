import type { BackendExit } from '@/global'

/**
 * Decide whether a backend-process exit should fail the desktop boot.
 *
 * The local Hermes helper process is only the desktop's backend when we're
 * attached in local mode. When the desktop is operating against a remote
 * gateway — or when the exit was a deliberate teardown (e.g. switching
 * gateways, which SIGTERMs the leftover local helper) — the exiting process
 * is not our backend, so its exit must not latch a boot failure.
 *
 * See issue #37869: remote-ready boots were getting stuck behind a stale
 * local-exit failure overlay.
 */
export function isFatalBackendExit(payload: BackendExit | null | undefined, connectionMode: string | null | undefined): boolean {
  if (payload?.deliberate) {
    return false
  }

  if (connectionMode === 'remote' || payload?.mode === 'remote') {
    return false
  }

  return true
}
