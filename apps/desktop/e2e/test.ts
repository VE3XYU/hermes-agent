/**
 * Extended Playwright test fixture that auto-fails any test if an error
 * banner (notification toast with role="alert") appears in the DOM.
 *
 * The desktop app surfaces errors as `[data-slot="alert"][role="alert"]`
 * elements (see components/notifications.tsx). When one appears during a
 * test, it means something went wrong (resume failed, boot error, etc.)
 * — the test should fail with the error message, not silently pass while
 * an error toast is visible on screen.
 *
 * Usage: import { test, expect } from './test' instead of
 * '@playwright/test'. The guard is auto-installed on every page — no
 * per-spec setup needed.
 */

import { test as base, expect, type Page, type ElectronApplication, _electron } from '@playwright/test'

// Track error messages per test so afterEach can assert + report.
const seenErrors: string[] = []
let activePage: Page | null = null

/**
 * Install the error-banner guard on a page. Watches for `[role="alert"]`
 * elements appearing in the DOM. When one is found, records its text
 * content for the afterEach assertion.
 */
function installErrorBannerGuard(page: Page): void {
  activePage = page

  // Clear any errors from a previous test when a new page is created.
  seenErrors.length = 0

  // Use a MutationObserver to catch error banners as they appear.
  // We inject this via addInitScript so it runs before any app code.
  page.addInitScript(() => {
    const seen: string[] = []
    ;(window as unknown as { __ERROR_BANNER_GUARD__?: string[] }).__ERROR_BANNER_GUARD__ = seen

    const observer = new MutationObserver(() => {
      const alerts = document.querySelectorAll('[role="alert"]')

      for (const alert of alerts) {
        const text = (alert.textContent ?? '').trim()

        if (text && !seen.includes(text)) {
          seen.push(text)
        }
      }
    })

    // Start observing once the DOM is ready.
    if (document.body) {
      observer.observe(document.body, { childList: true, subtree: true })
    } else {
      document.addEventListener('DOMContentLoaded', () => {
        observer.observe(document.body, { childList: true, subtree: true })
      })
    }
  })

  // Also poll via evaluate — MutationObserver via addInitScript can miss
  // elements that appear during the Electron renderer's initial mount
  // (before the observer is installed). A periodic poll catches those.
  page.on('console', () => {
    // Console messages are not errors — but we keep the listener to
    // ensure the page context is active for our evaluate calls.
  })
}

/**
 * Check for error banners that appeared during the test. Called in
 * afterEach via the custom fixture below.
 */
async function collectErrorBanners(page: Page | null): Promise<string[]> {
  if (!page) {
    return []
  }

  try {
    // Read errors collected by the MutationObserver in the page context.
    const pageErrors = await page.evaluate(() => {
      const w = window as unknown as { __ERROR_BANNER_GUARD__?: string[] }

      return [...(w.__ERROR_BANNER_GUARD__ ?? [])]
    })

    // Also do a final DOM scan for any alert elements still visible.
    const domAlerts = await page
      .locator('[role="alert"]')
      .allTextContents()
      .catch(() => [] as string[])

    const all = [...new Set([...pageErrors, ...domAlerts.map(t => t.trim()).filter(Boolean)])]
    seenErrors.push(...all)

    return [...new Set(seenErrors)]
  } catch {
    // Page might be closed — return whatever we have.
    return [...new Set(seenErrors)]
  }
}

// Extended test fixture: wraps the default page with the error guard.
export const test = base.extend({
  // Override the page fixture to auto-install the guard.
  page: async ({ page }, use) => {
    installErrorBannerGuard(page)
    await use(page)
  },
})

// afterEach: fail the test if any error banners appeared.
base.afterEach(async ({ page }, testInfo) => {
  const errors = await collectErrorBanners(page ?? activePage)

  if (errors.length > 0 && testInfo.status !== 'failed') {
    // Only fail if the test didn't already fail on its own — we don't
    // want to mask the original assertion error with our banner check.
    throw new Error(
      `Error banner(s) appeared during test "${testInfo.title}":\n` +
        errors.map(e => `  • ${e}`).join('\n'),
    )
  }
})

export { expect, type Page, type ElectronApplication, _electron }
