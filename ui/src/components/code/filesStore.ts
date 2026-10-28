// The Files tab's tree, per folder: the directories read so far, which are open, which file is in
// the viewer. In a store rather than the component so a look at another tab — or another session
// in the same folder — comes back to the tree as it was left, without reading it all again.
//
// Read lazily, one level at a time (`tree(root, dir, 1)`): a repository's top level is a few dozen
// entries, its whole tree can be a hundred thousand.

import { create } from 'zustand'
import { api } from '../../lib/api'
import type { WorkspaceEntry } from '../../lib/types'

export type DirState = {
  status: 'loading' | 'ok' | 'error'
  entries: WorkspaceEntry[]
  truncated: boolean
  error: string | null
}

export type FileTree = {
  dirs: Record<string, DirState>
  expanded: string[]
  /** The file in the viewer; null while the tree is showing. */
  selected: string | null
  /** The file viewed last, marked in the tree when it comes back. */
  lastViewed: string | null
  query: string
  /** Where the tree was scrolled to, so coming back from the viewer lands there. */
  scrollTop: number
}

export const EMPTY_TREE: FileTree = { dirs: {}, expanded: [], selected: null, lastViewed: null, query: '', scrollTop: 0 }

export const useFiles = create<{ byRoot: Record<string, FileTree> }>(() => ({ byRoot: {} }))

/** Entries read per directory — the runtime's own ceiling for one listing. */
const DIR_LIMIT = 5000

function update(root: string, fn: (tree: FileTree) => Partial<FileTree>) {
  useFiles.setState((s) => {
    const current = s.byRoot[root] ?? EMPTY_TREE
    return { byRoot: { ...s.byRoot, [root]: { ...current, ...fn(current) } } }
  })
}

function setDir(root: string, path: string, dir: DirState) {
  update(root, (t) => ({ dirs: { ...t.dirs, [path]: dir } }))
}

/** Read one directory's entries (`.` is the folder itself). Kept while it is shown again; `force`
 *  reads it anew. */
export async function loadDir(root: string, path = '.', force = false): Promise<void> {
  const current = useFiles.getState().byRoot[root]?.dirs[path]
  if (!force && current && current.status !== 'error') return
  // A directory already on screen stays there while it is read again, rather than blinking empty.
  setDir(root, path, current?.status === 'ok' ? current : { status: 'loading', entries: [], truncated: false, error: null })
  try {
    const tree = await api.workspace.tree(root, path, 1, DIR_LIMIT)
    setDir(root, path, { status: 'ok', entries: tree.entries, truncated: tree.truncated, error: null })
  } catch (error) {
    setDir(root, path, { status: 'error', entries: [], truncated: false, error: (error as Error).message })
  }
}

export function toggleDir(root: string, path: string, open?: boolean): void {
  const tree = useFiles.getState().byRoot[root] ?? EMPTY_TREE
  const isOpen = tree.expanded.includes(path)
  const next = open ?? !isOpen
  if (next === isOpen) return
  // Folders inside keep their own open state, as an editor's explorer does: opening this one again
  // shows them as they were.
  update(root, (t) => ({ expanded: next ? [...t.expanded, path] : t.expanded.filter((p) => p !== path) }))
  if (next) void loadDir(root, path)
}

export function selectFile(root: string, path: string | null): void {
  update(root, (t) => ({ selected: path, lastViewed: path ?? t.lastViewed }))
}

export function setFilesQuery(root: string, query: string): void {
  update(root, () => ({ query }))
}

export function rememberScroll(root: string, scrollTop: number): void {
  const tree = useFiles.getState().byRoot[root]
  if (tree && tree.scrollTop !== scrollTop) update(root, () => ({ scrollTop }))
}

/** Read the folder again: the top level and every directory that is open. */
export async function refreshTree(root: string): Promise<void> {
  const tree = useFiles.getState().byRoot[root] ?? EMPTY_TREE
  await Promise.all(['.', ...tree.expanded].map((path) => loadDir(root, path, true)))
}

export function useFileTree(root: string | null): FileTree {
  return useFiles((s) => (root ? (s.byRoot[root] ?? EMPTY_TREE) : EMPTY_TREE))
}
