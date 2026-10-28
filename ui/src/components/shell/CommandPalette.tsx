// The command palette (⌘K): every command, every thread and every message, one search away.
//
// Three things make it worth opening instead of reaching for the mouse:
//
// * **It ranks.** Results are fuzzy-matched (`lib/fuzzy`) and the groups are ordered by their best
//   match, so the first highlighted row is the best answer to what was typed, whichever group it
//   is in. Thread content is searched literally, never fuzzily — a scattered subsequence is
//   somewhere in every long message, and matching it would bury the real hits.
// * **It says what Enter will do.** A toggle is labelled with the action, not the state ("Turn
//   thinking off"), a command that cannot run now says why in place of its hint, and the two
//   commands that destroy something ask for a second Enter.
// * **Mining is a group of its own**, always there: the mode, this thread's switch, the panel, the
//   prompt stack — mining is something a person does from the keyboard like anything else.
//
// Threads from both sides of the window are found here, each marked with its side; opening one
// takes the window to that side.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from 'react'
import { useAgent } from '../../lib/agent/run'
import { bytes, relativeTime } from '../../lib/format'
import { findSnippet, foldCase, fuzzyMatch, highlightParts, mergeRanges, queryWords, type Range, type Snippet } from '../../lib/fuzzy'
import { isMac } from '../../lib/keys'
import type { ChatMessage, Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi, type AppMode, type RightPanel } from '../../store/ui'
import { Icon, type IconName } from '../common'
import { Kbd, Modal, useLayer } from '../ui'
import {
  copyLastAnswer,
  deleteThread,
  exportThread,
  goTo,
  newChat,
  newSession,
  newSessionIn,
  openFolder,
  openSidePanel,
  openThread,
  setMiningMode,
  setPromptStack,
  setTheme,
  stopGenerating,
  switchAppMode,
  toggleSidePanel,
  type Theme,
} from './actions'
import { MINING_MODE_LABEL, useMiningSummary } from './mining'
import { folderLabel, sessionTitle } from './projects'
import { NEW_CHAT_HINT, SHORTCUTS, SIDE_ICONS, SIDE_LABELS, SIDE_SHORTCUTS, VIEW_ICONS, VIEW_LABELS, VIEW_ORDER, VIEW_SHORTCUTS } from './shortcuts'
import { useRestoreFocus } from './focus'
import { sideOf } from './sides'
import { folderName, lastAnswer } from './threads'

/** One thing the palette can do. */
type Command = {
  id: string
  group: 'actions' | 'mining'
  label: string
  /** Words that find the command without being shown: "temperature" finds the parameters. */
  keywords?: string
  icon: IconName
  shortcut?: string
  hint?: string
  /** Why it cannot run now. Set means disabled; the reason takes the hint's place. */
  disabled?: string
  danger?: boolean
  /** Ask for a second Enter first, and say so with this label in the meantime. */
  confirm?: string
  /** The palette stays open: the command changes what it shows rather than doing something. */
  keepOpen?: boolean
  /** Shown when the query is empty. */
  suggested?: boolean
  run: () => void
}

type Row = {
  key: string
  icon: IconName
  /** A running thread's spinner, in place of its icon. */
  busy?: boolean
  label: string
  ranges: Range[]
  hint?: string
  shortcut?: string
  snippet?: Snippet
  disabled?: string
  danger?: boolean
  confirm?: string
  keepOpen?: boolean
  run: () => void
  score: number
}

type Section = { id: string; label: string; rows: Row[] }

type Mode = { kind: 'search' } | { kind: 'rename'; id: string; draft: string; title: string }

const THEMES: { theme: Theme; label: string; icon: IconName }[] = [
  { theme: 'light', label: 'Light', icon: 'sun' },
  { theme: 'dark', label: 'Dark', icon: 'moon' },
  { theme: 'system', label: 'System', icon: 'monitor' },
]

const RECENT_THREADS = 6
const MAX_THREADS = 40
/** Fuzzy matches scoring below this share of the best one are dropped: with "mine" typed, "Load
 *  model …" matches through scattered letters, and that is noise next to "Mining setup". */
const RELATIVE_FLOOR = 0.2

export function CommandPalette() {
  const open = useUi((s) => s.paletteOpen)
  const setOpen = useUi((s) => s.setPaletteOpen)
  return (
    <Modal open={open} onClose={() => setOpen(false)} position="top" className="max-w-[640px]" label="Command palette">
      <PaletteBody close={() => setOpen(false)} />
    </Modal>
  )
}

/** Mounted only while the palette is open, so every opening starts from a clean slate. */
function PaletteBody({ close }: { close: () => void }) {
  const [query, setQuery] = useState(() => useUi.getState().paletteQuery)
  const [mode, setMode] = useState<Mode>({ kind: 'search' })
  const [activeKey, setActiveKey] = useState<string | null>(null)
  const [armed, setArmed] = useState<string | null>(null)
  const input = useRef<HTMLInputElement>(null)
  const list = useRef<HTMLDivElement>(null)

  // The seed is for this opening only; the next ⌘K starts empty.
  useEffect(() => {
    if (useUi.getState().paletteQuery) useUi.setState({ paletteQuery: '' })
  }, [])
  useRestoreFocus()

  // Escape in rename mode goes back to the list instead of closing the palette: this layer opens
  // above the palette's own, so it is the one Escape closes.
  useLayer(mode.kind === 'rename', () => setMode({ kind: 'search' }))
  useEffect(() => {
    input.current?.focus()
    if (mode.kind === 'rename') input.current?.select()
  }, [mode.kind])

  const enterRename = useCallback(
    (conversation: Conversation) => setMode({ kind: 'rename', id: conversation.id, draft: threadLabel(conversation), title: threadLabel(conversation) }),
    [],
  )
  const commands = useCommands(enterRename)
  const conversations = useStudio((s) => s.conversations)
  const activeId = useStudio((s) => s.activeConversationId)
  const view = useStudio((s) => s.view)
  const streamingId = useStudio((s) => s.streaming?.conversationId ?? null)
  const agentId = useAgent((s) => s.run?.conversationId ?? null)

  const side = useUi((s) => s.appMode)
  const sections = useMemo(
    () => buildSections(commands, conversations, query, view === 'chat' ? activeId : null, new Set([streamingId, agentId].filter(Boolean) as string[]), side),
    [commands, conversations, query, view, activeId, streamingId, agentId, side],
  )
  const rows = useMemo(() => sections.flatMap((s) => s.rows), [sections])
  const enabled = useMemo(() => rows.filter((r) => !r.disabled), [rows])
  const active = enabled.find((r) => r.key === activeKey) ?? enabled[0] ?? null

  const activeRowKey = active?.key ?? null
  // A second Enter must land on the same row it was asked for; moving away disarms it.
  useEffect(() => setArmed(null), [activeRowKey, query])

  useLayoutEffect(() => {
    if (activeRowKey === null) return
    list.current?.querySelector<HTMLElement>(`[data-key="${CSS.escape(activeRowKey)}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [activeRowKey])

  const run = (row: Row) => {
    if (row.disabled) return
    if (row.confirm && armed !== row.key) {
      setActiveKey(row.key)
      setArmed(row.key)
      return
    }
    if (!row.keepOpen) close()
    row.run()
  }

  const move = (delta: number) => {
    if (enabled.length === 0) return
    const index = active ? enabled.indexOf(active) : -1
    const next = Math.max(0, Math.min(enabled.length - 1, index + delta))
    // Wrapping at the ends, but only for single steps: a page jump stops at the edge.
    const wrapped = Math.abs(delta) === 1 ? (index + delta + enabled.length) % enabled.length : next
    setActiveKey(enabled[wrapped]?.key ?? null)
  }

  const onKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    // An input method's Enter picks a candidate; it is not "run the command".
    if (event.nativeEvent.isComposing || event.keyCode === 229) return
    if (mode.kind === 'rename') {
      if (event.key === 'Enter') {
        event.preventDefault()
        useStudio.getState().renameConversation(mode.id, mode.draft)
        close()
      }
      return
    }
    const ctrlOnly = isMac && event.ctrlKey && !event.metaKey && !event.altKey && !event.shiftKey
    if (event.key === 'ArrowDown' || (ctrlOnly && event.key.toLowerCase() === 'n')) {
      event.preventDefault()
      move(1)
    } else if (event.key === 'ArrowUp' || (ctrlOnly && event.key.toLowerCase() === 'p')) {
      event.preventDefault()
      move(-1)
    } else if (event.key === 'PageDown') {
      event.preventDefault()
      move(8)
    } else if (event.key === 'PageUp') {
      event.preventDefault()
      move(-8)
    } else if (event.key === 'Enter' && !event.metaKey && !event.ctrlKey && !event.altKey) {
      event.preventDefault()
      if (active) run(active)
    }
  }

  const listId = 'command-palette-list'
  const activeIndex = active ? rows.indexOf(active) : -1

  return (
    <div className="flex flex-col">
      <div className="flex items-center gap-2.5 border-b border-ink-200 px-4 dark:border-ink-800">
        <Icon name={mode.kind === 'rename' ? 'edit' : 'search'} className="size-4 shrink-0 text-ink-400 dark:text-ink-500" />
        <input
          ref={input}
          className="h-12 min-w-0 flex-1 bg-transparent text-[0.95rem] text-ink-900 outline-none placeholder:text-ink-400 dark:text-ink-50 dark:placeholder:text-ink-500"
          placeholder={mode.kind === 'rename' ? 'Thread name' : 'Search commands, threads and messages…'}
          value={mode.kind === 'rename' ? mode.draft : query}
          onChange={(event) => {
            const value = event.target.value
            if (mode.kind === 'rename') setMode({ ...mode, draft: value })
            else {
              setQuery(value)
              setActiveKey(null)
            }
          }}
          onKeyDown={onKeyDown}
          spellCheck={false}
          autoComplete="off"
          role="combobox"
          aria-expanded={mode.kind === 'search'}
          aria-controls={listId}
          aria-activedescendant={mode.kind === 'search' && activeIndex >= 0 ? `${listId}-${activeIndex}` : undefined}
          aria-label={mode.kind === 'rename' ? 'New name for the thread' : 'Search commands and threads'}
        />
        <Kbd shortcut="escape" className="shrink-0" />
      </div>

      {mode.kind === 'rename' ? (
        <div className="px-4 py-3 text-[0.8rem] text-ink-500 dark:text-ink-400">
          Renaming <span className="font-medium text-ink-700 dark:text-ink-200">“{mode.title}”</span>.{' '}
          <span className="inline-flex items-center gap-1">
            <Kbd shortcut="enter" /> saves,
          </span>{' '}
          <span className="inline-flex items-center gap-1">
            <Kbd shortcut="escape" /> goes back.
          </span>
        </div>
      ) : (
        <div
          ref={list}
          id={listId}
          role="listbox"
          aria-label="Results"
          className="max-h-[min(26rem,58vh)] overflow-y-auto overscroll-contain p-1.5"
          // Clicking a row must not take the focus out of the search field: the keyboard keeps
          // working, and a row that asked for a second Enter can still get it.
          onMouseDown={(event) => event.preventDefault()}
        >
          {rows.length === 0 && <p className="px-3 py-6 text-center text-[0.8rem] text-ink-500 dark:text-ink-400">Nothing matches “{query.trim()}”.</p>}
          {sections.map((section) => (
            <div key={section.id} role="group" aria-label={section.label} className="pb-1">
              <div className="px-2.5 pb-1 pt-2 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">{section.label}</div>
              {section.rows.map((row) => (
                <PaletteRow
                  key={row.key}
                  id={`${listId}-${rows.indexOf(row)}`}
                  row={row}
                  active={row.key === active?.key}
                  armed={row.key === armed}
                  onHover={() => !row.disabled && row.key !== activeKey && setActiveKey(row.key)}
                  onRun={() => run(row)}
                />
              ))}
            </div>
          ))}
        </div>
      )}

      {mode.kind === 'search' && (
        <div className="flex items-center gap-4 border-t border-ink-200 px-4 py-2 text-[0.7rem] text-ink-400 dark:border-ink-800 dark:text-ink-500">
          <span className="inline-flex items-center gap-1">
            <Kbd shortcut="arrowup" />
            <Kbd shortcut="arrowdown" /> to move
          </span>
          <span className="inline-flex items-center gap-1">
            <Kbd shortcut="enter" /> to run
          </span>
          <span className="ml-auto hidden truncate sm:inline">Type a thread’s words to search its messages</span>
        </div>
      )}
    </div>
  )
}

function PaletteRow({ id, row, active, armed, onHover, onRun }: { id: string; row: Row; active: boolean; armed: boolean; onHover: () => void; onRun: () => void }) {
  return (
    <div
      id={id}
      data-key={row.key}
      data-active={active}
      role="option"
      aria-selected={active}
      aria-disabled={row.disabled ? true : undefined}
      title={row.disabled}
      onMouseMove={onHover}
      onClick={onRun}
      className={`flex min-h-8 items-center gap-2.5 rounded-lg px-2.5 py-1.5 text-[0.8rem] ${
        row.disabled ? 'cursor-default opacity-45' : 'cursor-pointer'
      } ${row.danger || armed ? 'text-red-600 dark:text-red-400' : 'text-ink-700 dark:text-ink-200'} ${
        active ? (armed ? 'bg-red-50 dark:bg-red-950/40' : 'bg-ink-100 dark:bg-ink-800') : ''
      }`}
    >
      {row.busy ? (
        <span className="flex size-4 shrink-0 items-center justify-center">
          <span className="size-1.5 animate-pulse rounded-full bg-arc-500" />
        </span>
      ) : (
        <Icon name={row.icon} className="size-4 shrink-0 opacity-70" />
      )}
      <div className="min-w-0 flex-1">
        <div className="truncate">{armed && row.confirm ? row.confirm : <Highlighted text={row.label} ranges={row.ranges} />}</div>
        {row.snippet && (
          <div className="truncate text-[0.7rem] text-ink-500 dark:text-ink-400">
            <Highlighted text={row.snippet.text} ranges={row.snippet.ranges} />
          </div>
        )}
      </div>
      {(row.disabled ?? row.hint) && (
        <span className="max-w-[45%] shrink-0 truncate text-[0.7rem] text-ink-400 dark:text-ink-500">{row.disabled ?? row.hint}</span>
      )}
      {row.shortcut && <Kbd shortcut={row.shortcut} className="shrink-0" />}
    </div>
  )
}

function Highlighted({ text, ranges }: { text: string; ranges: readonly Range[] }) {
  if (ranges.length === 0) return <>{text}</>
  return (
    <>
      {highlightParts(text, ranges).map((part, index) =>
        part.hit ? (
          <mark key={index} className="bg-transparent font-semibold text-arc-700 dark:text-arc-300">
            {part.text}
          </mark>
        ) : (
          <span key={index}>{part.text}</span>
        ),
      )}
    </>
  )
}

/** Every command, as things stand. Rebuilt when what they depend on changes, not per keystroke. */
function useCommands(rename: (conversation: Conversation) => void): Command[] {
  const view = useStudio((s) => s.view)
  const models = useStudio((s) => s.models)
  const runtime = useStudio((s) => s.runtime)
  const loadingModelId = useStudio((s) => s.loadingModelId)
  const settings = useStudio((s) => s.settings)
  const generating = useStudio((s) => s.streaming !== null)
  const conversations = useStudio((s) => s.conversations)
  const activeId = useStudio((s) => s.activeConversationId)
  const agentRunning = useAgent((s) => s.run !== null)
  const thinking = useUi((s) => s.thinking)
  const rightPanel = useUi((s) => s.rightPanel)
  const side = useUi((s) => s.appMode)
  const recentFolders = useUi((s) => s.recentWorkspaces)
  const mining = useMiningSummary()

  const active = conversations.find((c) => c.id === activeId) ?? null

  return useMemo(() => {
    const ui = useUi.getState
    const studio = useStudio.getState
    const busy = generating || agentRunning
    const waitForReply = generating ? 'Wait for the reply to finish' : undefined
    const commands: Command[] = []
    const add = (command: Command) => commands.push(command)

    // --- actions ---------------------------------------------------------------
    if (busy) {
      add({ id: 'stop', group: 'actions', label: 'Stop generating', icon: 'stop', shortcut: SHORTCUTS.stop, keywords: 'cancel abort halt', suggested: true, run: stopGenerating })
    }
    // The other side first among the suggestions: it is the switch someone opening the palette
    // from this side is likelier to want.
    for (const target of (side === 'chat' ? ['code', 'chat'] : ['chat', 'code']) as AppMode[]) {
      add({
        id: `switch-${target}`,
        group: 'actions',
        label: `Switch to ${SIDE_LABELS[target]}`,
        icon: SIDE_ICONS[target],
        shortcut: SIDE_SHORTCUTS[target],
        hint: target === side ? (view === 'chat' ? 'current' : 'back to the thread') : undefined,
        keywords: target === 'code' ? 'agent coding sessions projects folder side mode' : 'conversation threads side mode',
        suggested: target !== side,
        run: () => switchAppMode(target),
      })
    }
    // ⌘N makes a thread of the side in view; the other side's "new" is named by its own key, if any.
    add({
      id: 'new-chat',
      group: 'actions',
      label: 'New chat',
      icon: 'compose',
      shortcut: side === 'chat' ? NEW_CHAT_HINT : undefined,
      keywords: 'conversation thread start create',
      suggested: side === 'chat',
      run: newChat,
    })
    add({
      id: 'new-session',
      group: 'actions',
      label: 'New session',
      icon: 'plus',
      shortcut: side === 'code' ? NEW_CHAT_HINT : SHORTCUTS.newAgent,
      keywords: 'coding agent code folder workspace project start create thread',
      suggested: side === 'code',
      run: newSession,
    })
    add({
      id: 'open-folder',
      group: 'actions',
      label: 'Open folder…',
      icon: 'folderOpen',
      hint: 'new session',
      keywords: 'workspace directory project repository agent code session',
      suggested: side === 'code',
      run: openFolder,
    })
    for (const path of recentFolders) {
      add({
        id: `new-session-in:${path}`,
        group: 'actions',
        label: `New session in ${folderLabel(path)}`,
        icon: 'folder',
        hint: path,
        keywords: 'project folder workspace agent code recent',
        run: () => newSessionIn(path),
      })
    }
    const answer = lastAnswer(active)
    if (answer) {
      add({ id: 'copy-answer', group: 'actions', label: 'Copy last answer', icon: 'copy', keywords: 'clipboard reply response', suggested: true, run: () => void copyLastAnswer(active) })
    }
    add({ id: 'toggle-sidebar', group: 'actions', label: 'Toggle sidebar', icon: 'panelLeft', shortcut: SHORTCUTS.toggleSidebar, keywords: 'hide show threads', run: () => ui().toggleSidebar() })
    for (const target of VIEW_ORDER) {
      // The thread view is the side's — "Switch to Chat/Code" above is the way to it.
      if (target === 'chat') continue
      add({
        id: `go-${target}`,
        group: 'actions',
        label: `Go to ${VIEW_LABELS[target]}`,
        icon: VIEW_ICONS[target],
        shortcut: VIEW_SHORTCUTS[target],
        hint: target === view ? 'current' : undefined,
        keywords: GO_KEYWORDS[target],
        suggested: target === 'models',
        run: () => goTo(target),
      })
    }

    for (const model of models) {
      add({
        id: `load-${model.id}`,
        group: 'actions',
        label: `Load model ${model.id}`,
        icon: 'cube',
        keywords: 'switch model engine',
        hint: loadingModelId === model.id ? 'loading…' : bytes(model.size_bytes),
        disabled: model.fit.verdict === 'does_not_fit' ? model.fit_summary : waitForReply,
        run: () => void studio().loadModel(model.id),
      })
    }
    if (runtime?.model_id) {
      add({
        id: 'unload',
        group: 'actions',
        label: 'Unload model',
        icon: 'power',
        hint: runtime.model_id,
        keywords: 'free memory eject',
        disabled: waitForReply,
        run: () => void studio().unloadModel(),
      })
    }

    const theme = settings?.ui.theme ?? 'system'
    for (const option of THEMES) {
      add({
        id: `theme-${option.theme}`,
        group: 'actions',
        label: `Theme: ${option.label}`,
        icon: option.icon,
        keywords: 'appearance colour color mode',
        hint: option.theme === theme ? 'current' : undefined,
        disabled: settings ? undefined : 'the runtime is not reachable',
        run: () => void setTheme(option.theme),
      })
    }
    add({
      id: 'thinking',
      group: 'actions',
      label: thinking ? 'Turn thinking off' : 'Turn thinking on',
      icon: 'brain',
      keywords: 'reasoning think',
      hint: thinking ? 'on' : 'off',
      run: () => ui().setThinking(!thinking),
    })
    add({
      id: 'parameters',
      group: 'actions',
      label: 'Open parameters',
      icon: 'sliders',
      keywords: 'temperature sampling top p top k max tokens seed system prompt',
      run: () => {
        studio().setView('chat')
        ui().setParametersOpen(true)
      },
    })

    if (active) {
      const session = active.mode === 'agent'
      const noun = session ? 'session' : 'thread'
      if (session) {
        // The panels beside a session, each one command away (and one key, where it has one).
        const panels: { panel: RightPanel; name: string; icon: IconName; shortcut?: string; keywords: string }[] = [
          { panel: 'session', name: 'session summary', icon: 'list', shortcut: SHORTCUTS.sessionPanel, keywords: 'environment folder branch context changes overview' },
          { panel: 'changes', name: 'changes panel', icon: 'diff', shortcut: SHORTCUTS.changesPanel, keywords: 'diff review revert files agent git' },
          { panel: 'terminal', name: 'terminal', icon: 'terminal', shortcut: SHORTCUTS.terminalPanel, keywords: 'shell command run console' },
          { panel: 'files', name: 'files', icon: 'folder', shortcut: SHORTCUTS.filesPanel, keywords: 'browse tree explorer project' },
        ]
        for (const entry of panels) {
          const shown = rightPanel === entry.panel && view === 'chat'
          add({
            id: `${entry.panel}-panel`,
            group: 'actions',
            label: `${shown ? 'Close' : 'Open'} ${entry.name}`,
            icon: entry.icon,
            shortcut: entry.shortcut,
            keywords: entry.keywords,
            run: () => toggleSidePanel(entry.panel),
          })
        }
      }
      const title = session ? sessionTitle(active) : active.title
      add({ id: 'rename', group: 'actions', label: `Rename ${noun}`, icon: 'edit', keywords: 'title name thread', keepOpen: true, run: () => rename(active) })
      add({
        id: 'star',
        group: 'actions',
        label: active.starred ? `Unstar ${noun}` : `Star ${noun}`,
        icon: 'star',
        keywords: 'favourite favorite pin keep thread',
        run: () => studio().starConversation(active.id, !active.starred),
      })
      add({ id: 'export', group: 'actions', label: `Export ${noun} as Markdown`, icon: 'download', keywords: 'save download md file thread', run: () => exportThread(active) })
      add({
        id: 'clear',
        group: 'actions',
        label: `Clear ${noun}`,
        icon: 'undo',
        keywords: 'reset empty wipe messages thread',
        danger: true,
        confirm: `Press Enter again to clear every message in this ${noun}`,
        disabled: active.messages.length === 0 ? 'already empty' : undefined,
        run: () => studio().clearConversation(active.id),
      })
      add({
        id: 'delete',
        group: 'actions',
        label: `Delete ${noun}`,
        icon: 'trash',
        keywords: 'remove trash thread',
        danger: true,
        confirm: `Press Enter again to delete “${title}”`,
        run: () => deleteThread(active.id),
      })
    }
    add({ id: 'shortcuts', group: 'actions', label: 'Keyboard shortcuts', icon: 'keyboard', shortcut: SHORTCUTS.help, keywords: 'keys hotkeys help', suggested: true, run: () => ui().setShortcutsOpen(true) })

    // --- prompt mining ---------------------------------------------------------
    const backgroundOn = mining.setting === 'background'
    add({
      id: 'mining-mode',
      group: 'mining',
      label: backgroundOn ? 'Turn background mining off' : 'Turn background mining on',
      icon: 'pickaxe',
      keywords: 'mine mining queue background inline mode',
      hint: `now ${MINING_MODE_LABEL[mining.mode].toLowerCase()}`,
      disabled:
        mining.setting === null
          ? 'the runtime is not reachable'
          : !backgroundOn && !mining.backgroundAvailable
            ? (mining.blocker ?? 'not available')
            : waitForReply,
      run: () => void setMiningMode(backgroundOn ? 'inline' : 'background'),
    })
    if (active) {
      const on = active.mine !== false
      const noun = active.mode === 'agent' ? 'session' : 'thread'
      add({
        id: 'thread-mining',
        group: 'mining',
        label: on ? `Stop mining prompts from this ${noun}` : `Mine prompts from this ${noun}`,
        icon: 'pickaxe',
        keywords: 'mine mining thread conversation',
        hint: on ? 'on' : 'off',
        run: () => studio().setConversationMining(active.id, !on),
      })
    }
    const panelOpen = rightPanel === 'mining' && view === 'chat'
    add({
      id: 'mining-panel',
      group: 'mining',
      label: panelOpen ? 'Close mining panel' : 'Open mining panel',
      icon: 'panelRight',
      shortcut: SHORTCUTS.miningPanel,
      keywords: 'mine mining queue jobs claims',
      run: () => (panelOpen ? ui().setRightPanel(null) : openSidePanel('mining')),
    })
    if (mining.stackRunning || mining.stackPartial) {
      add({
        id: 'stack-stop',
        group: 'mining',
        label: 'Stop the prompt stack',
        icon: 'stop',
        keywords: 'mine mining gateway rail node',
        hint: mining.stackRunning ? `running · ${mining.carriers} carrier${mining.carriers === 1 ? '' : 's'}` : 'partly running',
        run: () => void setPromptStack(false),
      })
    } else if (mining.stackReady) {
      add({
        id: 'stack-start',
        group: 'mining',
        label: 'Start the prompt stack',
        icon: 'play',
        keywords: 'mine mining gateway rail node',
        hint: 'stopped',
        disabled: settings?.node.free_prompt?.accept_unverified ? undefined : 'accept signing on a public node first — Network',
        run: () => void setPromptStack(true),
      })
    }
    add({
      id: 'mining-setup',
      group: 'mining',
      label: 'Mining setup',
      icon: 'globe',
      keywords: 'mine mining network gateway rail bond prompt stack configure',
      hint: mining.stackProblem && !mining.stackRunning ? 'not ready' : 'Network',
      run: () => goTo('network'),
    })

    return commands
  }, [view, models, runtime, loadingModelId, settings, generating, agentRunning, thinking, rightPanel, side, recentFolders, mining, active, rename])
}

const GO_KEYWORDS: Record<string, string> = {
  chat: 'conversation messages composer',
  models: 'download hugging face gguf library',
  image: 'diffusion generate picture',
  network: 'mining chain node gateway',
  monitor: 'gpu cpu memory metrics performance',
  settings: 'preferences config options backend',
}

const foldedContent = new WeakMap<ChatMessage, string>()
function folded(message: ChatMessage): string {
  let value = foldedContent.get(message)
  if (value === undefined) {
    value = foldCase(message.content)
    foldedContent.set(message, value)
  }
  return value
}

/** How well each word of a query must match a label or title to be shown (see `MatchOptions`). */
const MIN_QUALITY = 0.45
/** A keyword hit, per character: a little less than the same word found in the label. */
const KEYWORD_SCORE = 0.6

const keywordCache = new Map<string, string[]>()
function keywordWords(keywords: string): string[] {
  let words = keywordCache.get(keywords)
  if (!words) {
    words = queryWords(keywords)
    keywordCache.set(keywords, words)
  }
  return words
}

/**
 * Each word of the query must be in the label (fuzzily, and well enough) or start one of the
 * command's keywords. Keywords are synonyms, so they are matched as words, never as scattered
 * letters — "mine" must not find "Go to Models" through the letters of "download hugging face".
 */
function matchCommand(words: string[], command: Command): { score: number; ranges: Range[] } | null {
  const synonyms = command.keywords ? keywordWords(command.keywords) : []
  let score = 0
  const ranges: Range[] = []
  for (const word of words) {
    const onLabel = fuzzyMatch(word, command.label, { minQuality: MIN_QUALITY })
    const onKeyword = synonyms.some((k) => k.startsWith(word)) ? KEYWORD_SCORE * word.length : -Infinity
    if (onLabel && onLabel.score >= onKeyword) {
      score += onLabel.score
      ranges.push(...onLabel.ranges)
    } else if (onKeyword > -Infinity) {
      score += onKeyword
    } else {
      return null
    }
  }
  if (foldCase(command.label).startsWith(words.join(' '))) score += 1
  return { score, ranges: mergeRanges(ranges) }
}

function commandRow(command: Command, ranges: Range[], score: number): Row {
  return {
    key: `cmd:${command.id}`,
    icon: command.icon,
    label: command.label,
    ranges,
    hint: command.hint,
    shortcut: command.shortcut,
    disabled: command.disabled,
    danger: command.danger,
    confirm: command.confirm,
    keepOpen: command.keepOpen,
    run: command.run,
    score,
  }
}

/** A thread's name as the sidebar shows it — and so as it is matched and highlighted here. */
function threadLabel(conversation: Conversation): string {
  return sideOf(conversation) === 'code' ? sessionTitle(conversation) : conversation.title
}

/** A thread as a result: marked with its side (the chat mark, or Code's `</>`), and opened on it. */
function threadRow(conversation: Conversation, ranges: Range[], score: number, busy: boolean, snippet?: Snippet): Row {
  const side = sideOf(conversation)
  const folder = side === 'code' ? folderName(conversation.workspace) : null
  const when = relativeTime(conversation.updatedAt / 1000)
  return {
    key: `thread:${conversation.id}`,
    icon: SIDE_ICONS[side],
    busy,
    label: threadLabel(conversation),
    ranges,
    hint: side === 'code' ? `${folder ?? 'no folder'} · ${when}` : when,
    snippet,
    run: () => openThread(conversation.id),
    score,
  }
}

function buildSections(
  commands: Command[],
  conversations: Conversation[],
  query: string,
  currentId: string | null,
  running: Set<string>,
  side: AppMode,
): Section[] {
  const words = queryWords(query)
  const byRecency = [...conversations].sort((a, b) => b.updatedAt - a.updatedAt)

  if (words.length === 0) {
    const sections: Section[] = [
      { id: 'suggested', label: 'Suggested', rows: commands.filter((c) => c.suggested).map((c) => commandRow(c, [], 0)) },
      {
        id: 'recent',
        label: 'Recent threads',
        rows: byRecency
          .filter((c) => c.id !== currentId)
          .slice(0, RECENT_THREADS)
          .map((c) => threadRow(c, [], 0, running.has(c.id))),
      },
      { id: 'mining', label: 'Prompt mining', rows: commands.filter((c) => c.group === 'mining').map((c) => commandRow(c, [], 0)) },
    ]
    return sections.filter((s) => s.rows.length > 0)
  }

  const actions: Row[] = []
  const mining: Row[] = []
  for (const command of commands) {
    const match = matchCommand(words, command)
    if (match) (command.group === 'mining' ? mining : actions).push(commandRow(command, match.ranges, match.score))
  }

  const whole = words.join(' ')
  const threads: Row[] = []
  const literal: Row[] = []
  for (const conversation of byRecency) {
    const label = threadLabel(conversation)
    const title = fuzzyMatch(query, label, { minQuality: MIN_QUALITY })
    if (title) {
      threads.push(threadRow(conversation, title.ranges, title.score + 0.25, running.has(conversation.id)))
      continue
    }
    // A title in a script with no word breaks (Japanese, say) gets no word-start bonuses, so a
    // literal substring is a match there however the fuzzy score came out.
    const at = foldCase(label).indexOf(whole)
    if (at >= 0) {
      threads.push(threadRow(conversation, [[at, at + whole.length]], 0.5, running.has(conversation.id)))
      continue
    }
    if (whole.length < 2) continue
    for (const message of conversation.messages) {
      if (message.toolResults || message.role === 'system' || !message.content) continue
      const snippet = findSnippet(message.content, query, { folded: folded(message) })
      if (snippet) {
        literal.push(threadRow(conversation, [], 0.1, running.has(conversation.id), snippet))
        break
      }
    }
  }

  const best = Math.max(0, ...actions.map((r) => r.score), ...mining.map((r) => r.score), ...threads.map((r) => r.score))
  const floor = best * RELATIVE_FLOOR
  const keep = (rows: Row[]) => rows.filter((r) => r.score >= floor).sort((a, b) => b.score - a.score)
  const sections: Section[] = [
    { id: 'actions', label: 'Actions', rows: keep(actions) },
    { id: 'mining', label: 'Prompt mining', rows: keep(mining) },
    // Title matches by score, then message matches — those by recency, as they were found.
    { id: 'threads', label: 'Threads', rows: [...keep(threads), ...literal].slice(0, MAX_THREADS) },
  ].filter((s) => s.rows.length > 0)
  sections.sort((a, b) => (b.rows[0]?.score ?? 0) - (a.rows[0]?.score ?? 0))

  // Whatever was typed can always be asked: the last row starts a new thread of the side in view
  // — a chat, or a session — with it as the draft.
  const draft = query.trim()
  const quoted = `“${draft.length > 60 ? `${draft.slice(0, 60)}…` : draft}”`
  sections.push({
    id: 'ask',
    label: side === 'code' ? 'Task' : 'Ask',
    rows: [
      {
        key: 'ask',
        icon: SIDE_ICONS[side],
        label: side === 'code' ? `Start a session: ${quoted}` : `Ask in a new chat: ${quoted}`,
        ranges: [],
        hint: 'opens it as a draft',
        run: () => {
          if (side === 'code') newSession()
          else newChat()
          useUi.getState().setComposerDraft(draft)
        },
        score: 0,
      },
    ],
  })
  return sections
}
