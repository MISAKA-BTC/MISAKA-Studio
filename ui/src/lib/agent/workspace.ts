// The folders agent threads work in, as the runtime describes them: canonical path, name, branch.
//
// Asked of the runtime (`api.workspace.open`) rather than worked out from the path, because the
// answer can change under the window — a folder renamed, a branch switched in a terminal — and the
// folder chip, the system prompt and the picker should all say the same thing. Cached for a short
// while so a composer re-rendering does not ask on every keystroke; a run always asks afresh.

import { useEffect } from 'react'
import { create } from 'zustand'
import { api } from '../api'
import type { WorkspaceInfo } from '../types'

export type WorkspaceEntry =
  | { status: 'loading' }
  | { status: 'ok'; info: WorkspaceInfo; at: number }
  | { status: 'error'; error: string; at: number }

export const useWorkspaces = create<{ byRoot: Record<string, WorkspaceEntry> }>(() => ({ byRoot: {} }))

const FRESH_MS = 30_000
const inflight = new Map<string, Promise<WorkspaceInfo>>()

function put(root: string, entry: WorkspaceEntry) {
  useWorkspaces.setState((s) => ({ byRoot: { ...s.byRoot, [root]: entry } }))
}

/** The runtime's account of `root`; `force` asks again even when a recent answer is cached. */
export function openWorkspace(root: string, force = false): Promise<WorkspaceInfo> {
  const cached = useWorkspaces.getState().byRoot[root]
  if (!force && cached?.status === 'ok' && Date.now() - cached.at < FRESH_MS) return Promise.resolve(cached.info)
  const pending = inflight.get(root)
  if (pending) return pending
  // A known folder keeps showing while it is asked about again, rather than flickering to "loading".
  if (cached?.status !== 'ok') put(root, { status: 'loading' })
  const request = api.workspace
    .open(root)
    .then((info) => {
      const entry: WorkspaceEntry = { status: 'ok', info, at: Date.now() }
      put(root, entry)
      if (info.root !== root) put(info.root, entry)
      return info
    })
    .catch((error: unknown) => {
      put(root, { status: 'error', error: (error as Error).message, at: Date.now() })
      throw error
    })
    .finally(() => inflight.delete(root))
  inflight.set(root, request)
  return request
}

/** Seed the cache with an answer already in hand — the picker's, when a folder is chosen. */
export function rememberWorkspace(info: WorkspaceInfo) {
  put(info.root, { status: 'ok', info, at: Date.now() })
}

/** The folder's details for a component, fetched when missing or stale. */
export function useWorkspaceInfo(root: string | null | undefined): WorkspaceEntry | null {
  const entry = useWorkspaces((s) => (root ? (s.byRoot[root] ?? null) : null))
  useEffect(() => {
    if (!root) return
    const current = useWorkspaces.getState().byRoot[root]
    if (current?.status === 'loading') return
    if (current && Date.now() - current.at < FRESH_MS) return
    openWorkspace(root).catch(() => {
      /* the entry carries the error, and the chip shows it */
    })
  }, [root])
  return entry
}

/** The last part of a path: what a folder is called. */
export function folderName(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path
}

/** A path with the home directory written as `~`, for showing (the runtime accepts `~` too). */
export function tildePath(path: string, home: string | null | undefined): string {
  if (home && (path === home || path.startsWith(`${home}/`) || path.startsWith(`${home}\\`))) return `~${path.slice(home.length)}`
  return path
}
