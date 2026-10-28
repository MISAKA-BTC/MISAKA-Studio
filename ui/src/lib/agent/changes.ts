// Reverting and reviewing what the agent changed — the Changes panel's actions, kept out of the
// component so the rule that matters can be tested on its own:
//
// **A revert never overwrites work it has not seen.** Before writing the original back (or
// removing a file the agent created), the file is read again; if it is no longer what the agent
// left — the person edited it, a formatter ran, a command rewrote it — the revert stops and says
// so, and only goes ahead when asked to again, knowingly. Without that check, "Revert" on a file
// someone has been editing since would silently discard their edits.

import type { FileChange } from '../types.ts'
import { readWhole, type WorkspaceApi } from './tools.ts'

export type ChangesApi = Pick<WorkspaceApi, 'read' | 'write'> & { remove: (root: string, path: string) => Promise<{ removed: string }> }

/** What a file on disk says about a change: as the agent left it, edited since, or gone. */
export type DiskState =
  | { state: 'as_left' }
  | { state: 'edited'; content: string }
  | { state: 'missing' }
  /** Too large or binary to compare. */
  | { state: 'unknown' }

export type RevertResult =
  | { ok: true }
  /** The file changed since the agent's last edit; nothing was written. */
  | { ok: false; conflict: DiskState }
  | { ok: false; error: string }

/** How the file on disk compares with what the agent left there. */
export async function diskState(api: Pick<WorkspaceApi, 'read'>, root: string, change: FileChange): Promise<DiskState> {
  const file = await readWhole(api as WorkspaceApi, root, change.path)
  if (!file.exists) return change.current === null ? { state: 'as_left' } : { state: 'missing' }
  if (!file.complete) return { state: 'unknown' }
  return file.content === change.current ? { state: 'as_left' } : { state: 'edited', content: file.content }
}

/**
 * Put a file back as it was before the agent's first change: the original written back, or a file
 * the agent created removed. Refused when the file changed since the agent left it, unless
 * `force` — the person has seen that and chosen to discard it.
 */
export async function revertChange(api: ChangesApi, root: string, change: FileChange, force = false): Promise<RevertResult> {
  if (change.untracked) return { ok: false, error: `${change.path} was too large to keep a copy of, so it cannot be reverted here.` }
  try {
    const disk = await diskState(api, root, change)
    if (!force && disk.state !== 'as_left') return { ok: false, conflict: disk }
    if (change.original === null) {
      if (disk.state !== 'missing' && !(disk.state === 'as_left' && change.current === null)) await api.remove(root, change.path)
    } else {
      await api.write(root, change.path, change.original)
    }
    return { ok: true }
  } catch (error) {
    return { ok: false, error: (error as Error).message }
  }
}

/** The sentence a revert leaves in the thread, so the agent does not go on editing a file that is
 *  no longer what it last saw. */
export function revertNote(paths: string[]): string {
  const list = paths.map((p) => `\`${p}\``).join(', ')
  return paths.length === 1
    ? `Note: the user reverted your changes to ${list}. It is back to how it was before you changed it; read it again before editing it.`
    : `Note: the user reverted your changes to ${list}. They are back to how they were before you changed them; read them again before editing them.`
}
