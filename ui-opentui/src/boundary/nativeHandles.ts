/**
 * Native handle-table exhaustion safety for @opentui/core 0.4.0 — sibling of
 * the ffiSafe.ts coordinate shim (same class of fix: harden OUR side of the
 * Node-FFI seam, TODO(upstream) to delete).
 *
 * Root cause (bench crash: every otui mem3000 cell died at ≈3000 lumpy fixture
 * messages, exit 7, ~880MB RSS — far below the 2GB cgroup cap): the native
 * core indexes EVERY object — TextBuffer, TextBufferView, SyntaxStyle,
 * OptimizedBuffer, … — through ONE global handle registry with 16-bit slot
 * indices (core `src/zig/handles.zig`: `INDEX_BITS = 16` → `MAX_SLOTS = 65535`,
 * slot 0 reserved). Measured on this install: exactly 65,534 live handles, the
 * 65,535th `createSyntaxStyle()` fails; `destroy()` does recycle slots, so
 * exhaustion means LIVE objects.
 *
 * Every `TextBufferRenderable` burns THREE slots at construction
 * (`TextBufferRenderable.ts:77-80`: `TextBuffer.create()` +
 * `TextBufferView.create()` + `SyntaxStyle.create()`). The mount-everything
 * transcript hits the wall at ≈1,400 store rows (≈21.8k text renderables ×3 ≈
 * 65.5k handles): the next mount throws `Failed to create SyntaxStyle`
 * (zig.ts:4554) out of a Solid mount effect → uncaught → the renderer's OWN
 * `uncaughtException` handler (renderer.ts `handleError`) calls
 * `console.show()`, which allocates the console-overlay `OptimizedBuffer` —
 * needing ANOTHER slot — so the handler itself throws `Failed to create
 * optimized buffer: WxH` and Node dies with exit 7 (fatal error in the
 * uncaughtException handler), MASKING the real error. (The exception-handler
 * guard lives in renderer.ts `guardRendererErrorHandlers`.)
 *
 * Why we can't just SHARE one SyntaxStyle across renderables (the obvious
 * 3→2 fix): the per-buffer style is load-bearing. The native styled-text path
 * (text-buffer.zig `setStyledText`) registers each chunk's color by NAME —
 * "chunk0", "chunk1", … — into the buffer's OWN syntax style, and
 * registration is name-keyed-overwrite (syntax-style.zig `putStyle`: existing
 * name → overwrite that id's definition). A shared style would have every
 * styled `<text>` overwrite every other one's chunk colors (live highlights
 * reference style IDS, re-resolved at render). So pooling is unsound at our
 * layer; the table pressure itself is bounded by the store row cap
 * (logic/store.ts, clamped to a handle-safe ceiling) until #27 lands
 * renderable-weight-aware capping/virtualization.
 *
 * What THIS shim does: makes style allocation failure DEGRADE instead of
 * throwing out of mount/render. `SyntaxStyle.create()` on a full table
 * returns a DETACHED style (handle 0 = the native INVALID_HANDLE):
 *  - JS-side styling still works — markdown/code chunk colors come from
 *    `getStyle`/`mergeStyles`, which read the instance's JS `styleDefs` map
 *    (see core lib/tree-sitter-styled-text.ts), never the native handle;
 *  - every native call on handle 0 is already a safe no-op in zig (acquire
 *    fails → early return), and `textBuffer.setSyntaxStyle(detached)` passes
 *    ptr 0 which the native side treats as "no style" — buffer-level styled
 *    -text highlights are skipped, i.e. that text renders unstyled;
 *  - `destroy()` on a detached style is a native no-op (beginDestroy(0)).
 *
 * TODO(upstream): file an OpenTUI issue — (a) a global 64k handle table with a
 * 3-slot cost per text renderable is too small for transcript-style TUIs;
 * (b) allocation failure throws out of the render loop with no degrade path;
 * (c) `handleError` allocates (console overlay) and so crashes on the very
 * condition it is reporting, masking the root cause with exit 7.
 */
import { SyntaxStyle, resolveRenderLib, type SyntaxStyleHandle } from '@opentui/core'

import { getLog } from './log.ts'

/** The native side's INVALID_HANDLE — every FFI entry point no-ops on it. */
const DETACHED: SyntaxStyleHandle = 0 as never

let installed = false
let warnedExhausted = false

/** Build a SyntaxStyle backed by NO native handle: JS-side styleDefs/merge
 *  caches fully functional, all native calls safe no-ops (handle 0). */
function detachedSyntaxStyle(): SyntaxStyle {
  return new SyntaxStyle(resolveRenderLib(), DETACHED)
}

/**
 * Patch `SyntaxStyle.create` (the static the core's own TextBufferRenderable
 * constructor calls — @opentui/core is external, one shared class object) so
 * native handle-table exhaustion degrades to a detached, unstyled-but-inert
 * style instead of throwing out of a Solid mount effect. Idempotent.
 *
 * @param factory test seam — inject a failing allocator to exercise the
 *                degrade path (defaults to the real `SyntaxStyle.create`).
 */
export function installSyntaxStyleDegrade(factory?: () => SyntaxStyle): void {
  if (installed) return
  installed = true

  const origCreate = factory ?? SyntaxStyle.create.bind(SyntaxStyle)

  SyntaxStyle.create = function create(): SyntaxStyle {
    try {
      return origCreate()
    } catch (cause) {
      if (!warnedExhausted) {
        warnedExhausted = true
        try {
          getLog().error(
            'native',
            'SyntaxStyle allocation failed — native handle table exhausted; degrading to unstyled',
            {
              cause: String(cause)
            }
          )
        } catch {
          // logging is best-effort inside a degrade path
        }
      }
      return detachedSyntaxStyle()
    }
  }
}
