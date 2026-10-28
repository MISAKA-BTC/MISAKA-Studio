// The body of an agent step: what the model said, and each tool it called as a card.
//
// A card says what the call did in one line — "Read src/main.rs · 1–120", "Edited App.tsx +12 −3",
// "Ran npm test · exit 1" — and opens to the detail: the lines read, the matches, the diff, the
// command's output. Reads stay quiet (a row, no border) because an agent reads a lot and the
// person rarely needs to see it; edits and commands are cards, because those are what changed
// something.
//
// A call still streaming is drawn from the parsed text ("Editing src/main.rs…"), never as the XML
// the model is writing. A call waiting for approval carries the approval bar: the diff or the
// command above it, Approve / Deny / Always allow below — and Enter approves while the bar has
// focus, which it takes when nobody is typing elsewhere.

import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { parseAgentText, type ToolSegment } from '../../lib/agent/protocol'
import { resolveApproval, useAgent } from '../../lib/agent/run'
import { canonicalToolName } from '../../lib/agent/tools'
import { folderName } from '../../lib/agent/workspace'
import { duration } from '../../lib/format'
import { isTypingTarget, shortcutKeys } from '../../lib/keys'
import type { ChatMessage, ToolCall, ToolDisplay, WorkspaceEntry, WorkspaceMatch } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { Icon, Spinner, type IconName } from '../common'
import { Markdown } from '../Markdown'
import { DiffStat, DiffView } from './DiffView'

type CardStatus = ToolCall['status'] | 'preparing' | 'cut_off' | 'interrupted'

const META: Record<string, { icon: IconName; base: string; doing: string; done: string; quiet: boolean }> = {
  list_files: { icon: 'folder', base: 'List', doing: 'Listing', done: 'Listed', quiet: true },
  read_file: { icon: 'fileText', base: 'Read', doing: 'Reading', done: 'Read', quiet: true },
  search: { icon: 'search', base: 'Search', doing: 'Searching', done: 'Searched', quiet: true },
  edit_file: { icon: 'edit', base: 'Edit', doing: 'Editing', done: 'Edited', quiet: false },
  write_file: { icon: 'filePlus', base: 'Write', doing: 'Writing', done: 'Wrote', quiet: false },
  run_command: { icon: 'terminal', base: 'Run', doing: 'Running', done: 'Ran', quiet: false },
}

const UNKNOWN_META = { icon: 'wrench' as IconName, base: 'Call', doing: 'Calling', done: 'Called', quiet: true }

function argText(args: Record<string, unknown>, key: string): string {
  const value = args[key]
  return typeof value === 'string' ? value.trim() : typeof value === 'number' || typeof value === 'boolean' ? String(value) : ''
}

function cleanPath(path: string): string {
  return path.replace(/^[`'"]+|[`'"]+$/g, '').replace(/^(\.\/)+/, '')
}

function verbFor(meta: typeof UNKNOWN_META, status: CardStatus, created: boolean): string {
  if (status === 'preparing' || status === 'pending' || status === 'running') return created ? 'Creating' : meta.doing
  if (status === 'done') return created ? 'Created' : meta.done
  return created ? 'Create' : meta.base
}

/** One line of what the call targets: the path, the pattern, the command. */
function targetOf(name: string, args: Record<string, unknown>): ReactNode {
  switch (name) {
    case 'list_files': {
      const path = cleanPath(argText(args, 'path'))
      return path && path !== '.' ? path : <span className="font-sans">the workspace</span>
    }
    case 'search': {
      const path = cleanPath(argText(args, 'path'))
      return (
        <>
          “{argText(args, 'pattern')}”{path && path !== '.' ? <span className="font-sans text-ink-400"> in {path}</span> : null}
        </>
      )
    }
    case 'run_command':
      return argText(args, 'command').split('\n')[0]
    default:
      return cleanPath(argText(args, 'path'))
  }
}

function metaOf(display: ToolDisplay | undefined): ReactNode {
  if (!display) return null
  switch (display.kind) {
    case 'read':
      if (display.totalLines === 0) return 'empty'
      return display.startLine <= 1 && display.endLine >= display.totalLines
        ? `${display.totalLines} line${display.totalLines === 1 ? '' : 's'}`
        : `${display.startLine}–${display.endLine} of ${display.totalLines}`
    case 'matches':
      return `${display.matches.length}${display.truncated ? '+' : ''} match${display.matches.length === 1 ? '' : 'es'}`
    case 'files':
      return `${display.entries.length}${display.truncated ? '+' : ''} entries`
    case 'diff':
      return <DiffStat added={display.added} removed={display.removed} />
    default:
      return null
  }
}

function ExitBadge({ display }: { display: Extract<ToolDisplay, { kind: 'command' }> }) {
  const { label, tone } = display.timedOut
    ? { label: 'timed out', tone: 'bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300' }
    : display.exitCode === null
      ? { label: 'killed', tone: 'bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300' }
      : display.exitCode === 0
        ? { label: 'exit 0', tone: 'bg-emerald-50 text-emerald-700 dark:bg-emerald-950/50 dark:text-emerald-300' }
        : { label: `exit ${display.exitCode}`, tone: 'bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300' }
  return (
    <span className="flex shrink-0 items-center gap-1.5">
      <span className={`badge mono ${tone}`}>{label}</span>
      {display.durationMs > 0 && <span className="text-[0.7rem] text-ink-400">{duration(display.durationMs)}</span>}
    </span>
  )
}

function StatusBadge({ status }: { status: CardStatus }) {
  if (status === 'awaiting_approval') {
    return (
      <span className="badge shrink-0 gap-1 bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">
        <Icon name="clock" className="size-3" />
        Waiting for approval
      </span>
    )
  }
  if (status === 'denied') return <span className="badge shrink-0 bg-ink-100 text-ink-500 dark:bg-ink-800 dark:text-ink-400">Denied</span>
  if (status === 'interrupted') return <span className="badge shrink-0 bg-ink-100 text-ink-500 dark:bg-ink-800 dark:text-ink-400">Not run</span>
  if (status === 'cut_off') return <span className="badge shrink-0 bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300">Cut off</span>
  return null
}

/** The glyph in front of a card: the tool's icon, or what is happening to it. */
function Glyph({ status, icon }: { status: CardStatus; icon: IconName }) {
  if (status === 'preparing' || status === 'pending' || status === 'running') return <Spinner className="size-3.5 shrink-0 text-arc-500" />
  if (status === 'error' || status === 'cut_off') return <Icon name="circleX" className="size-3.5 shrink-0 text-red-500" />
  return <Icon name={icon} className="size-3.5 shrink-0 text-ink-400 dark:text-ink-500" />
}

// --- details ----------------------------------------------------------------------------

function highlight(text: string, pattern: string): ReactNode {
  if (!pattern) return text
  let regex: RegExp
  try {
    regex = new RegExp(pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), 'g')
  } catch {
    return text
  }
  const parts: ReactNode[] = []
  let last = 0
  for (const match of text.matchAll(regex)) {
    const at = match.index ?? 0
    if (match[0].length === 0) break
    parts.push(text.slice(last, at))
    parts.push(
      <mark key={at} className="rounded-sm bg-amber-200/80 px-px text-inherit dark:bg-amber-400/25">
        {match[0]}
      </mark>,
    )
    last = at + match[0].length
  }
  parts.push(text.slice(last))
  return parts
}

function MatchList({ matches, pattern, truncated }: { matches: WorkspaceMatch[]; pattern: string; truncated: boolean }) {
  const groups = useMemo(() => {
    const byPath = new Map<string, WorkspaceMatch[]>()
    for (const match of matches) byPath.set(match.path, [...(byPath.get(match.path) ?? []), match])
    return [...byPath]
  }, [matches])
  if (!matches.length) return <p className="px-3 py-2 text-xs text-ink-500 dark:text-ink-400">No matches.</p>
  return (
    <div className="max-h-72 overflow-auto py-1 text-[0.75rem] leading-5">
      {groups.map(([path, items]) => (
        <div key={path}>
          <div className="mono sticky top-0 bg-white px-3 pt-1 text-[0.7rem] font-medium text-ink-600 dark:bg-ink-900 dark:text-ink-300">{path}</div>
          {items.map((match) => (
            <div key={`${match.path}:${match.line}`} className="mono flex gap-3 px-3 text-ink-700 dark:text-ink-300">
              <span className="w-9 shrink-0 select-none text-right tabular-nums text-ink-400 dark:text-ink-500">{match.line}</span>
              <span className="min-w-0 truncate">{highlight(match.text, pattern)}</span>
            </div>
          ))}
        </div>
      ))}
      {truncated && <p className="px-3 pt-1 text-[0.7rem] text-ink-400">More matches were not shown.</p>}
    </div>
  )
}

function FileList({ entries, base, truncated }: { entries: WorkspaceEntry[]; base: string; truncated: boolean }) {
  const prefix = base && base !== '.' ? `${base.replace(/\/$/, '')}/` : ''
  return (
    <div className="mono max-h-72 overflow-auto py-1.5 text-[0.75rem] leading-5">
      {entries.map((entry) => {
        const rel = prefix && entry.path.startsWith(prefix) ? entry.path.slice(prefix.length) : entry.path
        const parts = rel.split('/')
        return (
          <div key={entry.path} className="flex items-center gap-1.5 pr-3" style={{ paddingLeft: 12 + (parts.length - 1) * 14 }}>
            <Icon name={entry.is_dir ? 'folder' : 'file'} className={`size-3.5 shrink-0 ${entry.is_dir ? 'text-arc-600/70 dark:text-arc-400/70' : 'text-ink-400'}`} />
            <span className={entry.is_dir ? 'text-ink-800 dark:text-ink-100' : 'text-ink-600 dark:text-ink-300'}>{parts[parts.length - 1]}</span>
          </div>
        )
      })}
      {truncated && <p className="px-3 pt-1 font-sans text-[0.7rem] text-ink-400">The listing was cut short.</p>}
    </div>
  )
}

function Output({ text }: { text: string }) {
  return <pre className="mono max-h-72 overflow-auto whitespace-pre px-3 py-2 text-[0.75rem] leading-5 text-ink-700 dark:text-ink-300">{text}</pre>
}

/** A command and what it printed, like a terminal: the end of the output in view. */
function Terminal({ command, stdout, stderr, live }: { command: string; stdout: string; stderr: string; live: boolean }) {
  const ref = useRef<HTMLPreElement>(null)
  useLayoutEffect(() => {
    if (ref.current) ref.current.scrollTop = ref.current.scrollHeight
  }, [stdout, stderr])
  return (
    <pre
      ref={ref}
      className="mono max-h-64 overflow-auto whitespace-pre-wrap bg-ink-50 px-3 py-2 text-[0.75rem] leading-5 text-ink-800 [overflow-wrap:anywhere] dark:bg-ink-950 dark:text-ink-200"
    >
      <span className="select-none text-ink-400">$ </span>
      <span className="font-medium">{command}</span>
      {stdout && `\n${stdout.replace(/\n$/, '')}`}
      {stderr && <span className="text-red-700 dark:text-red-300">{`\n${stderr.replace(/\n$/, '')}`}</span>}
      {live && (
        <span className="mt-1 flex items-center gap-1.5 font-sans text-[0.7rem] text-ink-400">
          <Spinner className="size-3" /> running…
        </span>
      )}
    </pre>
  )
}

function Details({ name, args, call, status }: { name: string; args: Record<string, unknown>; call: ToolCall | null; status: CardStatus }) {
  const display = call?.display
  if (name === 'run_command') {
    const command = display?.kind === 'command' ? display.command : argText(args, 'command')
    const stdout = display?.kind === 'command' ? display.stdout : ''
    const stderr = display?.kind === 'command' ? display.stderr : ''
    return <Terminal command={command} stdout={stdout} stderr={stderr} live={status === 'running'} />
  }
  if (!display) {
    if (call?.output && status !== 'error') return <Output text={call.output} />
    return null
  }
  switch (display.kind) {
    case 'diff':
      return <DiffView patch={display.patch} maxHeight="20rem" />
    case 'matches':
      return <MatchList matches={display.matches} pattern={display.pattern} truncated={display.truncated} />
    case 'files':
      return <FileList entries={display.entries} base={cleanPath(argText(args, 'path')) || '.'} truncated={display.truncated} />
    case 'read':
      // The lines as the model read them, without the header the card already says.
      return call?.output ? <Output text={call.output.split('\n').slice(1).join('\n')} /> : null
    case 'text':
      return <p className="whitespace-pre-wrap px-3 py-2 text-xs text-ink-600 dark:text-ink-300">{display.text}</p>
    case 'command':
      return <Terminal command={display.command} stdout={display.stdout} stderr={display.stderr} live={false} />
  }
}

// --- approval ---------------------------------------------------------------------------

function ApprovalBar({ call, name, created }: { call: ToolCall; name: string; created: boolean }) {
  const ref = useRef<HTMLDivElement>(null)
  const workspace = useStudio((s) => s.conversations.find((c) => c.id === s.activeConversationId)?.workspace ?? null)
  useEffect(() => {
    const bar = ref.current
    if (!bar) return
    bar.scrollIntoView({ block: 'nearest' })
    // Focus comes here so Enter approves — but not out of a text field: an Enter meant for the
    // next message must never approve a command.
    if (!isTypingTarget(document.activeElement)) bar.focus({ preventScroll: true })
  }, [])
  const command = name === 'run_command'
  const question = command
    ? `Run this command${workspace ? ` in ${folderName(workspace)}` : ''}?`
    : name === 'write_file'
      ? created
        ? 'Create this file?'
        : 'Overwrite this file?'
      : 'Apply this edit?'
  return (
    <div
      ref={ref}
      tabIndex={-1}
      role="group"
      aria-label={question}
      onKeyDown={(event) => {
        if (event.key !== 'Enter' || event.target !== event.currentTarget || event.shiftKey || event.metaKey || event.ctrlKey || event.altKey) return
        if (event.nativeEvent.isComposing) return
        event.preventDefault()
        resolveApproval(call.id, 'approve')
      }}
      className="flex flex-wrap items-center justify-end gap-x-3 gap-y-1.5 border-t border-amber-200 bg-amber-50/80 px-3 py-2 outline-none focus:ring-2 focus:ring-inset focus:ring-amber-400/60 dark:border-amber-900/50 dark:bg-amber-950/25 dark:focus:ring-amber-500/40"
    >
      <span className="mr-auto flex min-w-0 items-center gap-1.5 text-xs font-medium text-amber-900 dark:text-amber-200">
        <Icon name="shield" className="size-3.5 shrink-0" />
        {question}
      </span>
      {/* One group, so a narrow column wraps the buttons together rather than one at a time. */}
      <div className="flex shrink-0 items-center gap-1.5">
        <button
          type="button"
          className="btn-ghost h-7 px-2 text-xs"
          title={`Approve this, and every ${command ? 'command' : 'edit'} after it in this thread, without asking`}
          onClick={() => resolveApproval(call.id, 'always')}
        >
          Always allow in this thread
        </button>
        <button type="button" className="btn-outline h-7 bg-white px-3 text-xs dark:bg-ink-900" onClick={() => resolveApproval(call.id, 'deny')}>
          Deny
        </button>
        <button type="button" className="btn-primary h-7 px-3 text-xs" onClick={() => resolveApproval(call.id, 'approve')}>
          Approve
          <span className="text-[0.7rem] font-normal opacity-75">{shortcutKeys('enter').join(' ')}</span>
        </button>
      </div>
    </div>
  )
}

// --- the card ---------------------------------------------------------------------------

export function ToolCallCard({ segment, call, streaming }: { segment: ToolSegment | null; call: ToolCall | null; streaming: boolean }) {
  const name = call?.name ?? canonicalToolName(segment?.name ?? '')
  const meta = META[name] ?? UNKNOWN_META
  // The text's own arguments are complete; the call keeps a shortened copy.
  const args = segment?.args ?? call?.args ?? {}
  const waitingOnThis = useAgent((s) => (call ? s.pendingApproval === call.id : false))
  const status: CardStatus = call
    ? call.status === 'awaiting_approval' && !waitingOnThis
      ? 'interrupted'
      : call.status
    : segment && !segment.complete
      ? streaming
        ? 'preparing'
        : 'cut_off'
      : 'pending'
  const display = call?.display
  const created = (display?.kind === 'diff' && display.created) || (call?.summary?.startsWith('Create ') ?? false)
  const awaiting = status === 'awaiting_approval'
  const quiet = meta.quiet && !awaiting
  const [openState, setOpen] = useState<boolean | null>(null)
  const defaultOpen = awaiting || (!quiet && (name === 'run_command' || display?.kind === 'diff' || (awaiting && display?.kind === 'text')))
  const open = openState ?? defaultOpen

  if (!name) {
    return (
      <div className="flex items-center gap-2 px-1.5 py-1 text-[0.8rem] text-ink-500 dark:text-ink-400">
        <Spinner className="size-3.5 text-arc-500" /> Preparing a tool call…
      </div>
    )
  }

  const target = targetOf(name, args)
  const verb = verbFor(meta, status, created && name === 'write_file')
  const error = status === 'error' || status === 'denied' || status === 'interrupted' ? (status === 'interrupted' ? 'The window was reloaded before this was approved, so it did not run.' : call?.error) : null
  // Red is for what went wrong; a call the person declined or stopped is said quietly.
  const errorTone = status === 'error' ? 'text-red-600 dark:text-red-400' : 'text-ink-500 dark:text-ink-400'
  const streamingWrite = status === 'preparing' && name === 'write_file' && typeof args.content === 'string'
  const hasDetails = status !== 'preparing' && (name === 'run_command' || display !== undefined || (call?.output !== undefined && status !== 'error'))

  const header = (
    <>
      <Glyph status={status} icon={meta.icon} />
      <span className={`shrink-0 font-medium ${quiet ? 'text-ink-500 dark:text-ink-400' : 'text-ink-700 dark:text-ink-200'}`}>{verb}</span>
      <span className="mono min-w-0 truncate text-[0.75rem] text-ink-700 dark:text-ink-200">{target}</span>
      {streamingWrite && <span className="shrink-0 text-[0.7rem] text-ink-400">{(args.content as string).split('\n').length} lines…</span>}
      {display?.kind === 'command' && status !== 'awaiting_approval' && call?.status === 'done' ? <ExitBadge display={display} /> : (
        <span className="shrink-0 text-[0.7rem] text-ink-400 dark:text-ink-500">{metaOf(display)}</span>
      )}
      <StatusBadge status={status} />
    </>
  )

  if (quiet) {
    return (
      <div className="text-[0.8rem]">
        <button
          type="button"
          disabled={!hasDetails}
          aria-expanded={hasDetails ? open : undefined}
          onClick={() => setOpen(!open)}
          title={call?.summary}
          className="group flex w-full min-w-0 items-center gap-2 rounded-md px-1.5 py-1 text-left transition-colors enabled:hover:bg-ink-100 disabled:cursor-default dark:enabled:hover:bg-ink-800/70"
        >
          {header}
          {hasDetails && (
            <Icon name="chevronDown" className={`ml-auto size-3.5 shrink-0 text-ink-400 transition ${open ? 'rotate-180' : 'opacity-0 group-hover:opacity-100'}`} />
          )}
        </button>
        {error && <p className={`ml-7 mt-0.5 whitespace-pre-wrap text-xs ${errorTone}`}>{error}</p>}
        {open && hasDetails && (
          <div className="mb-1 ml-7 mt-1 overflow-hidden rounded-lg border border-ink-200 bg-white dark:border-ink-800 dark:bg-ink-900">
            <Details name={name} args={args} call={call} status={status} />
          </div>
        )}
      </div>
    )
  }

  return (
    <div
      className={`overflow-hidden rounded-xl border bg-white text-[0.8rem] dark:bg-ink-900 ${
        awaiting ? 'border-amber-300 shadow-sm shadow-amber-900/5 dark:border-amber-700/60' : 'border-ink-200 dark:border-ink-800'
      }`}
    >
      <div className="flex min-w-0 items-center gap-2 px-3 py-1.5">
        <button
          type="button"
          disabled={!hasDetails}
          aria-expanded={hasDetails ? open : undefined}
          onClick={() => setOpen(!open)}
          title={call?.summary}
          className="group flex min-w-0 flex-1 items-center gap-2 text-left disabled:cursor-default"
        >
          {header}
          {hasDetails && (
            <Icon name="chevronDown" className={`ml-auto size-3.5 shrink-0 text-ink-400 transition ${open ? 'rotate-180' : 'opacity-0 group-hover:opacity-100'}`} />
          )}
        </button>
        {display?.kind === 'diff' && status === 'done' && (
          <button
            type="button"
            className="icon-btn size-6"
            title="Review in the Changes panel"
            aria-label="Review in the Changes panel"
            onClick={() => useUi.getState().setRightPanel('changes')}
          >
            <Icon name="diff" className="size-3.5" />
          </button>
        )}
      </div>
      {error && <p className={`border-t border-ink-100 px-3 py-1.5 text-xs whitespace-pre-wrap dark:border-ink-800 ${errorTone}`}>{error}</p>}
      {open && hasDetails && (
        <div className="border-t border-ink-100 dark:border-ink-800">
          <Details name={name} args={args} call={call} status={status} />
        </div>
      )}
      {awaiting && call && <ApprovalBar call={call} name={name} created={created} />}
    </div>
  )
}

/** "Working…", for a step that has not said anything it can show yet. */
function Working() {
  return (
    <div className="flex items-center gap-2 py-1 text-sm text-ink-500 dark:text-ink-400">
      <Spinner className="size-3.5" />
      Working…
    </div>
  )
}

/** The body of an assistant step in an agent thread: its text, with tool calls drawn as cards. */
export function AgentMessageBody({ message, streaming = false }: { message: ChatMessage; streaming?: boolean }) {
  const segments = useMemo(() => parseAgentText(message.content, { final: !streaming }), [message.content, streaming])
  const calls = message.toolCalls ?? []
  let toolIndex = 0
  const items: ReactNode[] = []
  segments.forEach((segment, index) => {
    if (segment.kind === 'text') {
      items.push(
        <Markdown key={`t${index}`} streaming={streaming}>
          {segment.text}
        </Markdown>,
      )
      return
    }
    const call = calls[toolIndex++] ?? null
    items.push(<ToolCallCard key={call?.id ?? `s${index}`} segment={segment} call={call} streaming={streaming && index === segments.length - 1} />)
  })
  // Calls the text no longer shows (a step kept from an older format) still get their cards.
  for (const call of calls.slice(toolIndex)) items.push(<ToolCallCard key={call.id} segment={null} call={call} streaming={false} />)

  if (items.length === 0) return streaming ? <Working /> : null
  return <div className="flex flex-col gap-2">{items}</div>
}
