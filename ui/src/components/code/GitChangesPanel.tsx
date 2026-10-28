// The Changes tab of a Code session: the folder's working tree as git sees it, and below it the
// edits this session's agent made.
//
// Two views on purpose. Git's is the whole truth about the folder — what the agent wrote, what a
// command generated, what the person changed by hand — measured against the last commit. The
// agent's is narrower but can do something git's cannot here: put a file back exactly as it was
// before the agent touched it, after checking nobody has edited it since. Someone reviewing a
// session wants the first; someone undoing one wants the second.

import { useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../../lib/api'
import { parsePatch } from '../../lib/diff'
import { bytes } from '../../lib/format'
import type { Conversation, GitFile } from '../../lib/types'
import { ChangesPanel } from '../agent/ChangesPanel'
import { DiffStat, DiffView } from '../agent/DiffView'
import { Icon, Spinner } from '../common'
import { useCodeUi } from './codeStore'
import { GIT_STATUS, isMissingEndpoint, refreshGit, useGitStatus } from './gitStore'
import { NeedsRuntime, NoFolder, PaneHeading, splitPath } from './shared'

/** Rows drawn before the rest are summed up — a generated folder can add thousands at once. */
const ROW_LIMIT = 300
/** How much of an untracked file is read to show it as added. */
const UNTRACKED_READ_BYTES = 256 * 1024

/** Which rows are open, per folder — so the tab keeps them open when it is looked at again. */
const openRows = new Map<string, Set<string>>()

type DiffState =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'patch'; patch: string; truncated: boolean }
  /** An untracked file, shown whole as added. */
  | { kind: 'content'; content: string; truncated: boolean; size: number }
  | { kind: 'binary' }

/** An entry git lists as a whole folder (`vendor/lib/`): a repository of its own inside this one. */
function isFolderEntry(file: GitFile): boolean {
  return file.path.endsWith('/')
}

function FileDiff({ root, file }: { root: string; file: GitFile }) {
  const [state, setState] = useState<DiffState>(file.binary ? { kind: 'binary' } : { kind: 'loading' })
  // The diff is read again when git's numbers for the file change — not on every status refresh.
  const signature = `${file.status}|${file.index}${file.worktree}|${file.added}|${file.removed}`

  useEffect(() => {
    if (file.binary) {
      setState({ kind: 'binary' })
      return
    }
    if (isFolderEntry(file)) return
    let live = true
    setState((s) => (s.kind === 'patch' || s.kind === 'content' ? s : { kind: 'loading' }))
    void (async () => {
      try {
        const diff = await api.workspace.gitDiff(root, file.path)
        if (!diff.patch.trim() && file.status === 'untracked') {
          // `git diff` says nothing about a file it does not track; the file itself is the change.
          const read = await api.workspace.read(root, file.path, { max_bytes: UNTRACKED_READ_BYTES })
          if (!live) return
          setState(read.binary ? { kind: 'binary' } : { kind: 'content', content: read.content, truncated: read.truncated, size: read.size })
          return
        }
        if (live) setState({ kind: 'patch', patch: diff.patch, truncated: diff.truncated })
      } catch (error) {
        if (live) setState({ kind: 'error', message: isMissingEndpoint(error) ? 'Diffs need the updated runtime.' : (error as Error).message })
      }
    })()
    return () => {
      live = false
    }
  }, [root, file.path, file.binary, file.status, signature])

  // Parsed once per patch, not per redraw: a patch can run to a megabyte.
  const hunks = useMemo(() => (state.kind === 'patch' ? parsePatch(state.patch).hunks.length : 0), [state])
  const note = (text: string) => <p className="px-3 py-2 text-[0.7rem] text-ink-500 dark:text-ink-400">{text}</p>
  let body
  if (isFolderEntry(file)) {
    body = note('A repository of its own inside this one: git lists it as a folder, and its changes are its own. Open it as a session to see them.')
  } else if (state.kind === 'loading') {
    body = (
      <div className="flex items-center gap-1.5 px-3 py-2 text-[0.7rem] text-ink-400">
        <Spinner className="size-3" />
        Reading the diff…
      </div>
    )
  } else if (state.kind === 'error') body = <p className="px-3 py-2 text-[0.7rem] text-red-600 dark:text-red-400">{state.message}</p>
  else if (state.kind === 'binary') body = note('A binary file — there is no text diff to show.')
  else if (state.kind === 'content') {
    body = (
      <>
        <DiffView before={null} after={state.content} maxHeight="28rem" />
        {state.truncated && note(`Only the first ${bytes(UNTRACKED_READ_BYTES, 0)} of ${bytes(state.size)} is shown.`)}
      </>
    )
  } else {
    if (hunks === 0) {
      body = /^Binary files /m.test(state.patch)
        ? note('A binary file — there is no text diff to show.')
        : file.status === 'renamed' || file.status === 'copied'
          ? note(`${file.status === 'renamed' ? 'Renamed' : 'Copied'} from ${file.orig_path ?? 'another path'}; the content is the same.`)
          : note('No line changed — only the file’s mode or type.')
    } else {
      body = (
        <>
          <DiffView patch={state.patch} maxHeight="28rem" />
          {state.truncated && note('The diff is longer than the runtime sends; this is its start.')}
        </>
      )
    }
  }
  return <div className="mx-2 mb-2 overflow-hidden rounded-lg border border-ink-200 dark:border-ink-800">{body}</div>
}

/** The size of a file's change: its line counts, or why there are none. */
function ChangeSize({ file }: { file: GitFile }) {
  const quiet = 'shrink-0 text-[0.65rem] text-ink-400 dark:text-ink-500'
  if (file.binary) return <span className="badge shrink-0 bg-ink-100 text-ink-500 dark:bg-ink-800 dark:text-ink-400">binary</span>
  if (isFolderEntry(file)) return <span className={quiet}>repository</span>
  // Git leaves very large untracked files uncounted — a count of nothing is not "no change".
  if (file.added === null && file.removed === null) return <span className={quiet} title="Too large to count line by line">{file.status === 'untracked' ? 'new' : '—'}</span>
  return <DiffStat added={file.added ?? 0} removed={file.removed ?? 0} />
}

function FileRow({ root, file, open, onToggle }: { root: string; file: GitFile; open: boolean; onToggle: () => void }) {
  const meta = GIT_STATUS[file.status]
  const folder = isFolderEntry(file)
  const { dir, name } = splitPath(folder ? file.path.slice(0, -1) : file.path)
  const inIndex = file.status !== 'untracked' && file.index !== '.' && file.index !== '?'
  const staged = !inIndex ? '' : file.worktree === '.' ? ' · staged' : ' · partly staged'
  return (
    <div className="border-b border-ink-100 last:border-b-0 dark:border-ink-800/80">
      <button
        type="button"
        className="flex w-full items-center gap-2 px-2 py-1.5 text-left hover:bg-ink-50 focus-visible:bg-ink-50 focus-visible:outline-none dark:hover:bg-ink-800/60 dark:focus-visible:bg-ink-800/60"
        aria-expanded={open}
        onClick={onToggle}
        title={`${meta.label}${staged}: ${file.orig_path ? `${file.orig_path} → ` : ''}${file.path}`}
      >
        <Icon name="chevron" className={`size-3 shrink-0 text-ink-400 transition-transform ${open ? 'rotate-90' : ''}`} />
        <span className={`mono w-3 shrink-0 text-center text-[0.7rem] font-semibold ${meta.tone}`}>
          {meta.letter}
          <span className="sr-only"> {meta.label}</span>
        </span>
        <span className="min-w-0 flex-1 truncate text-[0.8rem]">
          {file.orig_path && <span className="text-ink-400 dark:text-ink-500">{file.orig_path} → </span>}
          <span className="text-ink-400 dark:text-ink-500">{dir}</span>
          <span className="font-medium text-ink-800 dark:text-ink-100">{name}</span>
          {folder && <span className="text-ink-400 dark:text-ink-500">/</span>}
        </span>
        <ChangeSize file={file} />
      </button>
      {open && <FileDiff root={root} file={file} />}
    </div>
  )
}

function WorkingTree({ root }: { root: string }) {
  const git = useGitStatus(root)
  const status = git?.status ?? null
  const [open, setOpen] = useState<Set<string>>(() => openRows.get(root) ?? new Set())
  const [showAll, setShowAll] = useState(false)

  useEffect(() => {
    setOpen(openRows.get(root) ?? new Set())
    setShowAll(false)
  }, [root])

  const toggle = (path: string) =>
    setOpen((current) => {
      const next = new Set(current)
      if (next.has(path)) next.delete(path)
      else next.add(path)
      openRows.set(root, next)
      return next
    })

  const refresh = (
    <button
      type="button"
      className="icon-btn size-6"
      title="Read git status again"
      aria-label="Refresh git status"
      onClick={() => void refreshGit(root)}
      disabled={git?.loading}
    >
      {git?.loading ? <Spinner className="size-3.5" /> : <Icon name="refresh" className="size-3.5" />}
    </button>
  )

  let body
  if (git?.unsupported) {
    body = <NeedsRuntime title="Git changes need the updated runtime">The agent&apos;s own edits are listed below either way.</NeedsRuntime>
  } else if (!status && git?.error) {
    body = <p className="text-[0.7rem] text-red-600 dark:text-red-400">git status could not be read: {git.error}</p>
  } else if (!status) {
    body = (
      <p className="flex items-center gap-1.5 text-[0.7rem] text-ink-400">
        <Spinner className="size-3" />
        Reading git status…
      </p>
    )
  } else if (!status.is_git) {
    body = (
      <p className="text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
        This folder is not a git repository, so there is no working tree to compare. The agent&apos;s own edits are still tracked below.
      </p>
    )
  } else if (status.files.length === 0) {
    body = (
      <p className="flex items-center gap-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
        <Icon name="circleCheck" className="size-3.5 text-emerald-600 dark:text-emerald-400" />
        Nothing has changed since the last commit{status.head ? ` (${status.head})` : ''}.
      </p>
    )
  } else {
    const rows = showAll ? status.files : status.files.slice(0, ROW_LIMIT)
    body = (
      <>
        <div className="mb-1.5 flex items-center gap-2 text-[0.7rem] text-ink-500 dark:text-ink-400">
          <span>
            {status.files.length} file{status.files.length === 1 ? '' : 's'} changed
          </span>
          <DiffStat added={status.added} removed={status.removed} />
        </div>
        <div className="overflow-hidden rounded-xl border border-ink-200 dark:border-ink-800">
          {rows.map((file) => (
            <FileRow key={file.path} root={root} file={file} open={open.has(file.path)} onToggle={() => toggle(file.path)} />
          ))}
          {rows.length < status.files.length && (
            <button type="button" className="w-full px-3 py-2 text-left text-[0.7rem] text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-200" onClick={() => setShowAll(true)}>
              Show {(status.files.length - rows.length).toLocaleString('en-US')} more files
            </button>
          )}
        </div>
      </>
    )
  }

  return (
    <section>
      <PaneHeading
        aside={
          <>
            {status?.is_git && status.branch && (
              <span className="flex min-w-0 items-center gap-1 text-[0.7rem] text-ink-500 dark:text-ink-400" title={status.upstream ? `Tracking ${status.upstream}` : undefined}>
                <Icon name="gitBranch" className="size-3 shrink-0" />
                <span className="max-w-40 truncate">{status.branch}</span>
                {status.ahead > 0 && <span className="mono">↑{status.ahead}</span>}
                {status.behind > 0 && <span className="mono">↓{status.behind}</span>}
              </span>
            )}
            {refresh}
          </>
        }
      >
        Working tree
      </PaneHeading>
      <div className="mt-2">{body}</div>
      {status?.is_git && git?.error && <p className="mt-1.5 text-[0.7rem] text-amber-700 dark:text-amber-400">Showing the last reading: {git.error}</p>}
    </section>
  )
}

/** The last "show the agent's edits" request this tab has answered. */
let answeredTick = 0

function AgentEdits({ conversation }: { conversation: Conversation }) {
  const ref = useRef<HTMLElement>(null)
  const tick = useCodeUi((s) => s.agentEditsTick)
  const count = conversation.changes?.length ?? 0

  useEffect(() => {
    if (tick === answeredTick) return
    answeredTick = tick
    ref.current?.scrollIntoView({ block: 'start', behavior: 'smooth' })
  }, [tick])

  return (
    <section ref={ref} className="scroll-mt-3">
      <PaneHeading aside={count > 0 ? <span className="text-[0.7rem] tabular-nums text-ink-400">{count}</span> : null}>Agent edits in this session</PaneHeading>
      {count === 0 ? (
        <p className="mt-2 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
          No edits from the agent to review. When it edits a file, the file is listed here with its diff, Revert and Keep.
        </p>
      ) : (
        <div className="mt-2 overflow-hidden rounded-xl border border-ink-200 dark:border-ink-800">
          <ChangesPanel conversationId={conversation.id} />
        </div>
      )}
    </section>
  )
}

export function GitChangesPanel({ conversation }: { conversation: Conversation | null }) {
  const root = conversation?.workspace ?? null
  if (!conversation || !root) return <NoFolder conversation={conversation} what="what changed in it" />
  return (
    <div className="space-y-6 p-3">
      <WorkingTree root={root} />
      <AgentEdits conversation={conversation} />
    </div>
  )
}
