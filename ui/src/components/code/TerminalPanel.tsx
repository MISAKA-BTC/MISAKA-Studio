// The Terminal tab: a block terminal in the session's folder.
//
// Each command is a block — the prompt it was typed at, what it printed (stderr in red), how it
// ended and how long it took — rather than one undivided scroll, because the thing anyone does
// with a command's output here is hand it on: copy it, or send it to the agent as an attachment.
// A block is that unit.
//
// The work happens in `terminalStore.ts` (one terminal per folder, outliving this pane) and
// `lib/terminal.ts` (cd, quoting, history, what is kept of the output); this file draws them.

import { memo, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent } from 'react'
import { folderName, useWorkspaceInfo } from '../../lib/agent/workspace'
import { isMac } from '../../lib/keys'
import { displayText, droppedNote, formatDuration, historyStep, promptLabel, shellFlavor, terminalAttachment, transcript, type BlockSummary } from '../../lib/terminal'
import type { Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { draftKey, useComposer } from '../chat/composerStore'
import { Icon, Spinner } from '../common'
import { useTerminalHistory } from './codeStore'
import { NeedsRuntime, NoFolder, consumeTabShown, useHomeDir } from './shared'
import { clearTerminal, runCommand, stopCommand, useTerminal, type TerminalBlock } from './terminalStore'

/** Closer to the bottom than this counts as following the output. */
const FOLLOW_SLACK_PX = 40

/** A prompt short enough for the input row: the folder, or the last two directories within it. */
function shortPrompt(folder: string, cwd: string): string {
  const full = promptLabel(folder, cwd)
  if (full.length <= 32) return full
  const parts = cwd.split('/')
  return `…/${parts.slice(-2).join('/')}`
}

/** Seconds since `since`, ticking while `active`. */
function useElapsed(since: number, active: boolean): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (!active) return
    const timer = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [active])
  return Math.max(0, now - since)
}

function summary(block: TerminalBlock, folder: string): BlockSummary {
  return {
    command: block.command,
    prompt: promptLabel(folder, block.cwd),
    output: block.output,
    status: block.status,
    exitCode: block.exitCode,
    durationMs: block.durationMs,
    error: block.error,
  }
}

function Outcome({ block }: { block: TerminalBlock }) {
  const elapsed = useElapsed(block.startedAt, block.status === 'running')
  const took = block.durationMs !== null ? formatDuration(block.durationMs) : null
  const base = 'inline-flex h-5 items-center gap-1 rounded-md px-1.5 text-[0.65rem] font-medium tabular-nums'
  switch (block.status) {
    case 'running':
      return (
        <span className={`${base} text-ink-500 dark:text-ink-400`} role="status">
          <Spinner className="size-3" />
          {formatDuration(elapsed)}
        </span>
      )
    case 'done':
      if (block.local && block.exitCode === 0) return null
      return block.exitCode === 0 ? (
        <span className={`${base} text-emerald-700 dark:text-emerald-400`} title={`Exit code 0${took ? ` after ${took}` : ''}`}>
          <Icon name="check" className="size-3" />
          {took}
        </span>
      ) : (
        <span className={`${base} bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300`} title={`Exit code ${block.exitCode ?? 'unknown'}`}>
          exit {block.exitCode ?? '?'}
          {took && <span className="font-normal opacity-75">· {took}</span>}
        </span>
      )
    case 'stopped':
      return <span className={`${base} bg-amber-50 text-amber-800 dark:bg-amber-950/40 dark:text-amber-300`}>stopped{took && ` · ${took}`}</span>
    case 'timed_out':
      return <span className={`${base} bg-amber-50 text-amber-800 dark:bg-amber-950/40 dark:text-amber-300`}>timed out{took && ` · ${took}`}</span>
    case 'error':
      return block.unsupported ? null : <span className={`${base} bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300`}>not run</span>
  }
}

function BlockActions({ block, folder, conversationId }: { block: TerminalBlock; folder: string; conversationId: string }) {
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    if (!copied) return
    const timer = setTimeout(() => setCopied(false), 1500)
    return () => clearTimeout(timer)
  }, [copied])

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(transcript(summary(block, folder)))
      setCopied(true)
    } catch {
      useStudio.getState().toast('error', 'The clipboard is not available here.')
    }
  }
  const send = () => {
    const attachment = terminalAttachment(summary(block, folder))
    useComposer.getState().addAttachments(draftKey(conversationId), [attachment])
    useUi.getState().focusComposer()
    useStudio.getState().toast('success', `Attached the output of “${block.command.length > 48 ? `${block.command.slice(0, 47)}…` : block.command}” to your message`)
  }

  return (
    <span className="flex items-center opacity-0 transition-opacity focus-within:opacity-100 group-hover:opacity-100">
      <button type="button" className="icon-btn size-6" title={copied ? 'Copied' : 'Copy the command and its output'} aria-label="Copy the command and its output" onClick={() => void copy()}>
        <Icon name={copied ? 'check' : 'copy'} className="size-3.5" />
      </button>
      <button type="button" className="icon-btn size-6" title="Send to the composer, as an attachment" aria-label="Send the output to the composer" onClick={send}>
        <Icon name="paperclip" className="size-3.5" />
      </button>
    </span>
  )
}

const Block = memo(function Block({ block, folder, conversationId }: { block: TerminalBlock; folder: string; conversationId: string }) {
  const note = droppedNote(block.output)
  const hasOutput = block.output.segments.length > 0
  return (
    <div className="group border-b border-ink-100 px-3 py-2 dark:border-ink-800/70">
      <div className="flex items-start gap-2">
        <p className="min-w-0 flex-1 whitespace-pre-wrap [overflow-wrap:anywhere]">
          <span className="select-none text-ink-400 dark:text-ink-500">{promptLabel(folder, block.cwd)} $ </span>
          <span className="font-medium text-ink-900 dark:text-ink-50">{block.command}</span>
        </p>
        <span className="-my-0.5 flex shrink-0 items-center gap-0.5">
          {!block.local && <BlockActions block={block} folder={folder} conversationId={conversationId} />}
          <Outcome block={block} />
        </span>
      </div>
      {note && <p className="mt-1 select-none text-[0.65rem] italic text-ink-400 dark:text-ink-500">{note}</p>}
      {hasOutput && (
        <pre className="mt-1 whitespace-pre-wrap break-words text-ink-700 dark:text-ink-300">
          {block.output.segments.map((segment, index) => (
            <span key={index} className={segment.stream === 'stderr' ? 'text-red-600 dark:text-red-400' : undefined}>
              {displayText(segment.text)}
            </span>
          ))}
        </pre>
      )}
      {block.unsupported ? (
        <div className="mt-1.5 font-sans">
          <NeedsRuntime title="The terminal needs the updated runtime">
            It streams commands through an endpoint this runtime does not have yet. Restart MISAKA Studio once it is updated.
          </NeedsRuntime>
        </div>
      ) : block.status === 'error' && block.error ? (
        <p className="mt-1 text-red-600 dark:text-red-400">{block.error}</p>
      ) : null}
    </div>
  )
})

function Terminal({ root, conversationId }: { root: string; conversationId: string }) {
  const session = useTerminal(root)
  const history = useTerminalHistory(root)
  const os = useStudio((s) => s.system?.hardware.os ?? null)
  const home = useHomeDir()
  const info = useWorkspaceInfo(root)
  const folder = info?.status === 'ok' ? info.info.name : folderName(root)
  const [input, setInput] = useState('')
  const [historyIndex, setHistoryIndex] = useState<number | null>(null)
  const typed = useRef('')
  const inputRef = useRef<HTMLInputElement>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const follow = useRef(true)
  const running = session.running !== null
  const runningLocal = running && session.blocks.find((b) => b.id === session.running)?.local === true

  useEffect(() => {
    follow.current = true
    setInput('')
    setHistoryIndex(null)
  }, [root])

  // Opened to type into — but a panel that was simply left on this tab, coming back with the window,
  // leaves the focus in the composer.
  useEffect(() => {
    if (consumeTabShown('terminal')) inputRef.current?.focus({ preventScroll: true })
  }, [])

  // Follow the output while the view is at its end; leave it alone once someone scrolls up to read.
  useLayoutEffect(() => {
    const element = scrollRef.current
    if (element && follow.current) element.scrollTop = element.scrollHeight
  }, [session.blocks])

  const onScroll = () => {
    const element = scrollRef.current
    if (element) follow.current = element.scrollHeight - element.scrollTop - element.clientHeight < FOLLOW_SLACK_PX
  }

  const submit = () => {
    // A `cd` being checked does not hold the prompt: the line waits for it in the store.
    if (running && !runningLocal) return
    const line = input
    setInput('')
    setHistoryIndex(null)
    typed.current = ''
    follow.current = true
    void runCommand(root, line, { flavor: shellFlavor(os), home })
  }

  const onKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing || event.keyCode === 229) return
    const key = event.key.toLowerCase()
    if (event.key === 'Enter') {
      event.preventDefault()
      submit()
    } else if ((event.key === 'ArrowUp' || event.key === 'ArrowDown') && !event.altKey && !event.metaKey && !event.ctrlKey) {
      if (history.length === 0) return
      event.preventDefault()
      const next = historyStep(history.length, historyIndex, event.key === 'ArrowUp' ? 'up' : 'down')
      if (historyIndex === null) typed.current = input
      setHistoryIndex(next)
      const value = next === null ? typed.current : (history[next] ?? '')
      setInput(value)
      requestAnimationFrame(() => inputRef.current?.setSelectionRange(value.length, value.length))
    } else if (event.ctrlKey && !event.metaKey && !event.altKey && key === 'c') {
      // Ctrl+C: stop the command, as in any terminal. Off a Mac, Ctrl+C over selected text is Copy.
      const element = event.currentTarget
      if (!isMac && element.selectionStart !== element.selectionEnd) return
      event.preventDefault()
      if (running) stopCommand(root)
      else {
        setInput('')
        setHistoryIndex(null)
      }
    } else if (event.ctrlKey && !event.metaKey && !event.altKey && key === 'l') {
      event.preventDefault()
      clearTerminal(root)
    } else if (event.key === 'Escape' && input) {
      // Claimed only when it clears something, so an Escape in an empty prompt still stops the agent.
      event.preventDefault()
      setInput('')
      setHistoryIndex(null)
    }
  }

  // A click on the scrollback that selects nothing puts the cursor back at the prompt.
  const focusPrompt = () => {
    const selection = window.getSelection()
    if (selection && !selection.isCollapsed) return
    inputRef.current?.focus({ preventScroll: true })
  }

  const prompt = promptLabel(folder, session.cwd)

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-9 shrink-0 items-center gap-2 border-b border-ink-200 px-3 text-[0.7rem] dark:border-ink-800">
        <Icon name="terminal" className="size-3.5 shrink-0 text-ink-400" />
        <span className="mono min-w-0 flex-1 truncate text-ink-600 dark:text-ink-300" title={`${root}${session.cwd ? `/${session.cwd}` : ''}`}>
          {prompt}
        </span>
        {running && !runningLocal && (
          <button type="button" className="btn-outline h-6 gap-1 px-2 py-0 text-[0.7rem]" onClick={() => stopCommand(root)} title="Stop the command (Ctrl+C)">
            <Icon name="stop" className="size-3" />
            Stop
          </button>
        )}
        <button
          type="button"
          className="btn-ghost h-6 px-2 py-0 text-[0.7rem]"
          onClick={() => clearTerminal(root)}
          disabled={session.blocks.length === 0 || (running && session.blocks.length === 1)}
          title="Clear the scrollback (Ctrl+L)"
        >
          Clear
        </button>
      </div>

      <div ref={scrollRef} onScroll={onScroll} onMouseUp={focusPrompt} className="mono min-h-0 flex-1 overflow-y-auto text-[0.75rem] leading-[1.15rem]" role="log" aria-label="Terminal output" aria-live="off">
        {session.blocks.length === 0 ? (
          <div className="px-4 py-8 font-sans text-xs leading-relaxed text-ink-500 dark:text-ink-400">
            <p>
              Commands run in <span className="font-medium text-ink-700 dark:text-ink-200">{folder}</span> through your login shell, and their output streams in
              here.
            </p>
            <p className="mt-2">
              Each command starts a fresh shell: <span className="mono">cd</span> is remembered by this terminal, but <span className="mono">export</span>, aliases
              and other shell state last for one command only. Commands get no keyboard input, so an editor or a password prompt will not work here, and
              one still running after an hour is stopped.
            </p>
            <p className="mt-2 text-ink-400 dark:text-ink-500">↑ ↓ history · Ctrl+C stops · Ctrl+L or clear clears · each block can be copied or sent to the composer</p>
          </div>
        ) : (
          session.blocks.map((block) => <Block key={block.id} block={block} folder={folder} conversationId={conversationId} />)
        )}
      </div>

      <div className="flex shrink-0 items-center gap-2 border-t border-ink-200 px-3 py-2 dark:border-ink-800">
        <label htmlFor={`terminal-input-${conversationId}`} className="mono max-w-[45%] shrink-0 truncate text-[0.75rem] text-ink-400 dark:text-ink-500" title={prompt}>
          {shortPrompt(folder, session.cwd)} $
        </label>
        <input
          id={`terminal-input-${conversationId}`}
          ref={inputRef}
          className="mono min-w-0 flex-1 bg-transparent text-[0.75rem] text-ink-900 outline-none placeholder:text-ink-300 dark:text-ink-50 dark:placeholder:text-ink-600"
          value={input}
          onChange={(event) => {
            setInput(event.target.value)
            setHistoryIndex(null)
          }}
          onKeyDown={onKeyDown}
          placeholder={running ? 'Running — Ctrl+C stops it' : 'Type a command'}
          aria-label={`Command to run in ${prompt}`}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          autoComplete="off"
        />
      </div>
    </div>
  )
}

export function TerminalPanel({ conversation }: { conversation: Conversation | null }) {
  const root = conversation?.workspace ?? null
  if (!conversation || !root) return <NoFolder conversation={conversation} what="a terminal in it" />
  return <Terminal root={root} conversationId={conversation.id} />
}
