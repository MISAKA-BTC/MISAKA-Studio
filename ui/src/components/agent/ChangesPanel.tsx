// The files this thread's agent changed: each with its diff from before the agent's first edit to
// now, and Revert / Keep.
//
// Revert reads the file again before it writes anything (`lib/agent/changes.ts`): a file edited
// since the agent left it — by the person, a formatter, a command — is not overwritten without a
// second, informed click. A successful revert leaves a note in the thread so the agent, if asked
// to carry on, reads the file again instead of editing a version that no longer exists. Reverts
// wait while the agent is running, so that note and the files never move under a run.

import { useMemo, useState } from 'react'
import { api } from '../../lib/api'
import { diskState, revertChange, revertNote, type DiskState } from '../../lib/agent/changes'
import { useAgent } from '../../lib/agent/run'
import { diffStats } from '../../lib/diff'
import type { Conversation, FileChange } from '../../lib/types'
import { uid, useStudio } from '../../store/studio'
import { EmptyState, Icon, Spinner } from '../common'
import { DiffStat, DiffView } from './DiffView'

type RowState = { busy?: boolean; conflict?: DiskState; error?: string }

function splitPath(path: string): { dir: string; name: string } {
  const at = path.lastIndexOf('/')
  return at === -1 ? { dir: '', name: path } : { dir: path.slice(0, at + 1), name: path.slice(at + 1) }
}

function dropChanges(conversationId: string, paths: string[]) {
  useStudio.getState().updateConversation(conversationId, (c) => ({ changes: (c.changes ?? []).filter((change) => !paths.includes(change.path)) }))
}

/** The note a revert leaves for the agent — a hidden tool-results turn, like the agent's own. */
function noteReverted(conversation: Conversation, paths: string[]) {
  if (!paths.length || conversation.messages.length === 0) return
  useStudio.getState().updateConversation(conversation.id, (c) => ({
    messages: [...c.messages, { id: uid(), role: 'user' as const, content: revertNote(paths), toolResults: true, createdAt: Date.now() }],
    updatedAt: Date.now(),
  }))
}

function ChangeRow({
  change,
  state,
  disk,
  locked,
  onRevert,
  onKeep,
  onCancelConflict,
}: {
  change: FileChange
  state: RowState
  disk: DiskState | undefined
  locked: boolean
  onRevert: (force: boolean) => void
  onKeep: () => void
  onCancelConflict: () => void
}) {
  const [open, setOpen] = useState(false)
  const stats = useMemo(() => (change.untracked ? null : diffStats(change.original, change.current)), [change])
  const { dir, name } = splitPath(change.path)
  const created = !change.untracked && change.original === null
  const deleted = !change.untracked && change.current === null
  const conflict = state.conflict

  return (
    <div className="border-b border-ink-100 last:border-b-0 dark:border-ink-800/80">
      <div className="group flex items-center gap-1.5 px-2 py-1.5">
        <button
          type="button"
          className="flex min-w-0 flex-1 items-center gap-1.5 rounded-md px-1 py-0.5 text-left hover:bg-ink-100 dark:hover:bg-ink-800/70"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          disabled={change.untracked}
          title={change.path}
        >
          <Icon name="chevron" className={`size-3 shrink-0 text-ink-400 transition-transform ${open ? 'rotate-90' : ''} ${change.untracked ? 'invisible' : ''}`} />
          <Icon name={created ? 'filePlus' : 'file'} className={`size-3.5 shrink-0 ${created ? 'text-emerald-600 dark:text-emerald-400' : 'text-ink-400'}`} />
          <span className="min-w-0 truncate text-[0.8rem]">
            <span className="text-ink-400 dark:text-ink-500">{dir}</span>
            <span className="font-medium text-ink-800 dark:text-ink-100">{name}</span>
          </span>
          {created && <span className="badge shrink-0 bg-emerald-50 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300">new</span>}
          {deleted && <span className="badge shrink-0 bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300">deleted</span>}
          {change.untracked && <span className="badge shrink-0 bg-ink-100 text-ink-500 dark:bg-ink-800 dark:text-ink-400">too large to track</span>}
          {disk?.state === 'edited' && (
            <span className="badge shrink-0 bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300" title="Changed outside the agent since its last edit">
              edited since
            </span>
          )}
          {disk?.state === 'missing' && <span className="badge shrink-0 bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">gone from disk</span>}
          {stats && <DiffStat added={stats.added} removed={stats.removed} className="ml-auto pl-1" />}
        </button>
        <div className={`flex shrink-0 items-center ${open || state.busy ? '' : 'opacity-60 group-hover:opacity-100'}`}>
          {state.busy ? (
            <Spinner className="mx-1.5 size-3.5 text-ink-400" />
          ) : (
            <>
              <button
                type="button"
                className="icon-btn size-6"
                title={change.untracked ? 'Too large to keep a copy of: cannot be reverted here' : locked ? 'Stop the agent to revert' : created ? 'Revert: remove the file the agent created' : 'Revert to before the agent changed it'}
                aria-label={`Revert ${change.path}`}
                disabled={change.untracked || locked}
                onClick={() => onRevert(false)}
              >
                <Icon name="undo" className="size-3.5" />
              </button>
              <button type="button" className="icon-btn size-6" title="Keep: take it off this list" aria-label={`Keep ${change.path}`} onClick={onKeep}>
                <Icon name="check" className="size-3.5" />
              </button>
            </>
          )}
        </div>
      </div>

      {conflict && (
        <div className="mx-2 mb-2 rounded-lg border border-amber-200 bg-amber-50 p-2.5 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
          <p className="flex items-start gap-1.5">
            <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
            <span>
              {conflict.state === 'missing'
                ? `${change.path} is no longer on disk. Reverting writes the original back.`
                : conflict.state === 'unknown'
                  ? `${change.path} cannot be compared with what the agent left (it is too large or binary now). Reverting overwrites it.`
                  : `${change.path} changed after the agent's last edit — by you, a formatter or a command. Reverting discards those changes too.`}
            </span>
          </p>
          <div className="mt-2 flex justify-end gap-1.5">
            <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={onCancelConflict}>
              Cancel
            </button>
            <button type="button" className="btn-danger bg-white px-2 py-0.5 text-xs dark:bg-transparent" onClick={() => onRevert(true)}>
              Revert anyway
            </button>
          </div>
        </div>
      )}
      {state.error && <p className="mx-3 mb-2 text-xs text-red-600 dark:text-red-400">{state.error}</p>}

      {open && !change.untracked && (
        <div className="mx-2 mb-2 overflow-hidden rounded-lg border border-ink-200 dark:border-ink-800">
          <DiffView before={change.original} after={change.current} maxHeight="28rem" />
        </div>
      )}
    </div>
  )
}

/** The files this conversation's agent changed, with diffs, Revert and Keep. */
export function ChangesPanel({ conversationId }: { conversationId: string }) {
  const conversation = useStudio((s) => s.conversations.find((c) => c.id === conversationId) ?? null)
  const locked = useAgent((s) => s.run?.conversationId === conversationId)
  const [rows, setRows] = useState<Record<string, RowState>>({})
  const [disk, setDisk] = useState<Record<string, DiskState>>({})
  const [refreshing, setRefreshing] = useState(false)
  const [bulk, setBulk] = useState(false)
  const changes = conversation?.changes ?? []
  const root = conversation?.workspace ?? null

  const totals = useMemo(() => {
    let added = 0
    let removed = 0
    for (const change of changes) {
      if (change.untracked) continue
      const stats = diffStats(change.original, change.current)
      added += stats.added
      removed += stats.removed
    }
    return { added, removed }
  }, [changes])

  if (!conversation) return null
  if (changes.length === 0) {
    return (
      <EmptyState icon="diff" title="No changes yet">
        <p>Files the agent edits in this thread appear here, so you can review each diff and revert it.</p>
        <p className="mt-2 text-xs">Changes made by commands — formatters, package installs — are not tracked.</p>
      </EmptyState>
    )
  }

  const setRow = (path: string, state: RowState) => setRows((all) => ({ ...all, [path]: state }))

  const revertOne = async (change: FileChange, force: boolean): Promise<boolean> => {
    if (!root) return false
    setRow(change.path, { busy: true })
    const result = await revertChange(api.workspace, root, change, force)
    if (result.ok) {
      setRow(change.path, {})
      return true
    }
    setRow(change.path, 'conflict' in result ? { conflict: result.conflict } : { error: result.error })
    return false
  }

  const revert = async (change: FileChange, force: boolean) => {
    if (await revertOne(change, force)) {
      dropChanges(conversationId, [change.path])
      noteReverted(conversation, [change.path])
    }
  }

  // Each file on its own: the ones that changed since the agent left them stay listed, each asking
  // for its own confirmation, and the rest go back.
  const revertAll = async () => {
    setBulk(true)
    const done: string[] = []
    for (const change of changes) {
      if (change.untracked) continue
      if (await revertOne(change, false)) done.push(change.path)
    }
    dropChanges(conversationId, done)
    noteReverted(conversation, done)
    setBulk(false)
  }

  const refresh = async () => {
    if (!root) return
    setRefreshing(true)
    const next: Record<string, DiskState> = {}
    await Promise.all(
      changes
        .filter((c) => !c.untracked)
        .map(async (change) => {
          try {
            next[change.path] = await diskState(api.workspace, root, change)
          } catch {
            next[change.path] = { state: 'unknown' }
          }
        }),
    )
    setDisk(next)
    setRefreshing(false)
  }

  const revertible = changes.some((c) => !c.untracked)

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center gap-2 border-b border-ink-200 px-3 py-2 dark:border-ink-800">
        <div className="min-w-0 flex-1">
          <div className="text-[0.8rem] font-medium text-ink-800 dark:text-ink-100">
            {changes.length} file{changes.length === 1 ? '' : 's'} changed
          </div>
          <DiffStat added={totals.added} removed={totals.removed} />
        </div>
        <button type="button" className="icon-btn" title="Check the files again for changes made outside the agent" aria-label="Refresh" onClick={() => void refresh()} disabled={refreshing || !root}>
          {refreshing ? <Spinner className="size-3.5" /> : <Icon name="refresh" className="size-3.5" />}
        </button>
        <button type="button" className="btn-ghost px-2 py-1 text-xs" onClick={() => dropChanges(conversationId, changes.map((c) => c.path))} title="Keep every change: clear this list">
          Keep all
        </button>
        <button
          type="button"
          className="btn-outline px-2 py-1 text-xs"
          onClick={() => void revertAll()}
          disabled={!revertible || locked || bulk || !root}
          title={locked ? 'Stop the agent to revert' : 'Put every file back as it was before the agent changed it'}
        >
          {bulk ? <Spinner className="size-3" /> : <Icon name="undo" className="size-3" />}
          Revert all
        </button>
      </div>
      {locked && (
        <p className="border-b border-ink-100 px-3 py-1.5 text-[0.7rem] text-ink-500 dark:border-ink-800 dark:text-ink-400">
          The agent is working — reverting is paused until it stops.
        </p>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">
        {changes.map((change) => (
          <ChangeRow
            key={change.path}
            change={change}
            state={rows[change.path] ?? {}}
            disk={disk[change.path]}
            locked={locked || !root}
            onRevert={(force) => void revert(change, force)}
            onKeep={() => dropChanges(conversationId, [change.path])}
            onCancelConflict={() => setRow(change.path, {})}
          />
        ))}
        <p className="px-3 py-2 text-[0.7rem] text-ink-400 dark:text-ink-500">Changes made by commands are not tracked here.</p>
      </div>
    </div>
  )
}
