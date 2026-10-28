// `git status` of the folders Code sessions work in — one reading per folder, shared by the header's
// count, the Session panel and the Changes tab, so the three never disagree.
//
// Read when a pane first needs it and again when something may have changed it: an agent step, the
// end of a run, a terminal command, the window coming back into focus. Those come in bursts (a run
// ends as its last step lands), so a refresh is debounced per folder, and one that is asked for
// while another is in flight runs once more after it rather than twice alongside it.
//
// A runtime from before the git endpoints answers 404/405. That is not an error to show in red: the
// panes say the feature needs the updated runtime, and the next focus asks again — the runtime may
// have been updated in the meantime.

import { useEffect } from 'react'
import { create } from 'zustand'
import { ApiError, api } from '../../lib/api'
import type { GitStatus } from '../../lib/types'

export type GitEntry = {
  status: GitStatus | null
  /** The runtime has no git endpoints yet. */
  unsupported: boolean
  error: string | null
  loading: boolean
  /** When the last answer (or failure) came. */
  at: number
}

export const useGit = create<{ byRoot: Record<string, GitEntry> }>(() => ({ byRoot: {} }))

/** How long an answer is fresh enough that a pane opening does not ask again. */
const FRESH_MS = 15_000

/** Whether `error` is a runtime that does not have the endpoint at all. A POST to a path the
 *  runtime does not route reaches its UI fallback, which only answers GET — hence 405 too. */
export function isMissingEndpoint(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 404 || error.status === 405 || error.status === 501)
}

const inflight = new Map<string, Promise<void>>()
const again = new Set<string>()
const timers = new Map<string, ReturnType<typeof setTimeout>>()

function patch(root: string, update: Partial<GitEntry>) {
  useGit.setState((s) => {
    const previous = s.byRoot[root] ?? { status: null, unsupported: false, error: null, loading: false, at: 0 }
    return { byRoot: { ...s.byRoot, [root]: { ...previous, ...update } } }
  })
}

/** Ask the runtime now. */
export function refreshGit(root: string): Promise<void> {
  const running = inflight.get(root)
  if (running) {
    again.add(root)
    return running
  }
  patch(root, { loading: true })
  const request = api.workspace
    .gitStatus(root)
    .then(
      (status) => patch(root, { status, unsupported: false, error: null, loading: false, at: Date.now() }),
      (error: unknown) => {
        if (isMissingEndpoint(error)) patch(root, { status: null, unsupported: true, error: null, loading: false, at: Date.now() })
        // A failed read keeps the last good one on screen, with the failure beside it.
        else patch(root, { error: (error as Error).message, loading: false, at: Date.now() })
      },
    )
    .finally(() => {
      inflight.delete(root)
      if (again.delete(root)) void refreshGit(root)
    })
  inflight.set(root, request)
  return request
}

/** Ask the runtime soon — once, however many times this is called in the meantime. */
export function scheduleGitRefresh(root: string, delayMs = 400): void {
  const pending = timers.get(root)
  if (pending) clearTimeout(pending)
  timers.set(
    root,
    setTimeout(() => {
      timers.delete(root)
      void refreshGit(root)
    }, delayMs),
  )
}

/** The folder's git status for a component, read when missing or stale. */
export function useGitStatus(root: string | null | undefined): GitEntry | null {
  const entry = useGit((s) => (root ? (s.byRoot[root] ?? null) : null))
  useEffect(() => {
    if (!root) return
    const current = useGit.getState().byRoot[root]
    if (current && (current.loading || Date.now() - current.at < FRESH_MS)) return
    void refreshGit(root)
  }, [root])
  return entry
}

/** What a status letter means, and how it is drawn. */
export const GIT_STATUS: Record<GitStatus['files'][number]['status'], { letter: string; label: string; tone: string }> = {
  modified: { letter: 'M', label: 'Modified', tone: 'text-amber-600 dark:text-amber-400' },
  added: { letter: 'A', label: 'Added', tone: 'text-emerald-600 dark:text-emerald-400' },
  deleted: { letter: 'D', label: 'Deleted', tone: 'text-red-600 dark:text-red-400' },
  renamed: { letter: 'R', label: 'Renamed', tone: 'text-arc-600 dark:text-arc-400' },
  copied: { letter: 'C', label: 'Copied', tone: 'text-arc-600 dark:text-arc-400' },
  untracked: { letter: '?', label: 'Untracked', tone: 'text-emerald-600/80 dark:text-emerald-400/80' },
  conflicted: { letter: 'U', label: 'Conflicted', tone: 'text-red-600 dark:text-red-400' },
  typechange: { letter: 'T', label: 'Type changed', tone: 'text-amber-600 dark:text-amber-400' },
}
