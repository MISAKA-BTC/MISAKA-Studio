// The panel beside the thread. Its tabs depend on the side of the window:
//
// * **Chat** — the thread's mining and its context, as before.
// * **Code** — the session's summary, its changes, a terminal and the files of its folder, and
//   mining. Wider, and resizable from its left edge, because a diff or a terminal needs the room.
//
// Each tab is about the thread on screen, and each is the long form of something the transcript
// says briefly — the diff behind a tool card, the claim behind a mining badge, the full report
// behind a context line — so the transcript can stay short without hiding anything.
//
// There is one `rightPanel` setting for the window; a tab one side does not have is shown as its
// nearest neighbour on the other (`effectivePanel`), so switching sides never leaves a blank panel.

import { memo, useRef, useState, type KeyboardEvent, type PointerEvent } from 'react'
import { api } from '../../lib/api'
import { contentForModel, historyForModel } from '../../lib/history'
import type { ContextReport, Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi, type RightPanel as Panel } from '../../store/ui'
import { FilesPanel } from '../code/FilesPanel'
import { GitChangesPanel } from '../code/GitChangesPanel'
import { SessionPanel } from '../code/SessionPanel'
import { TerminalPanel } from '../code/TerminalPanel'
import { PANEL_DEFAULT_WIDTH, PANEL_MAX_WIDTH, PANEL_MIN_WIDTH, clampPanelWidth, useCodeUi } from '../code/codeStore'
import { useGitStatus } from '../code/gitStore'
import { PANE_META, effectivePanel, noteTabShown, useSide } from '../code/shared'
import { useTerminalBusy } from '../code/terminalStore'
import { Icon, Spinner, type IconName } from '../common'
import { ContextReportView, PinnedNotesEditor } from '../ContextPanel'
import { IconButton } from '../ui'
import { draftKey, useDraft } from './composerStore'
import { MiningPanel } from './MiningPanel'

const CODE_TABS: Panel[] = ['session', 'changes', 'terminal', 'files', 'mining']

/** Memoised for the same reason as the header: a streamed token changes nothing in here. */
export const RightPanel = memo(function RightPanel({ conversation }: { conversation: Conversation | null }) {
  const panel = useUi((s) => s.rightPanel)
  const side = useSide(conversation)
  const tab = effectivePanel(panel, side)
  // A pane that takes the focus when opened (the terminal) needs to know it was opened just now —
  // by a click or a key — and not merely restored with the window or carried across a side switch.
  const lastPanel = useRef(panel)
  if (lastPanel.current !== panel) {
    lastPanel.current = panel
    noteTabShown(tab)
  }
  if (!tab) return null
  return side === 'code' ? <CodePanel conversation={conversation} tab={tab} /> : <ChatPanel conversation={conversation} tab={tab} />
})

function Tab({ id, active, label, icon, count, dot, compact }: { id: Panel; active: boolean; label: string; icon: IconName; count?: number; dot?: boolean; compact?: boolean }) {
  const setRightPanel = useUi((s) => s.setRightPanel)
  return (
    <button
      type="button"
      role="tab"
      id={`panel-tab-${id}`}
      aria-selected={active}
      aria-controls="thread-panel-body"
      title={label}
      onClick={() => setRightPanel(id)}
      className={`relative inline-flex h-7 shrink-0 items-center gap-1.5 rounded-md px-2 text-xs font-medium transition-colors ${
        active
          ? 'bg-ink-100 text-ink-900 dark:bg-ink-800 dark:text-ink-50'
          : 'text-ink-500 hover:bg-ink-100 hover:text-ink-800 dark:text-ink-400 dark:hover:bg-ink-800 dark:hover:text-ink-100'
      }`}
    >
      <Icon name={icon} className="size-3.5" />
      {/* In a narrow panel only the open tab spells its name; the rest are their icons. */}
      <span className={compact && !active ? 'sr-only @min-[30rem]:not-sr-only' : ''}>{label}</span>
      {count ? <span className="rounded bg-ink-200 px-1 text-[0.6rem] tabular-nums text-ink-600 dark:bg-ink-700 dark:text-ink-300">{count}</span> : null}
      {dot && <span className="absolute right-1 top-1 size-1.5 rounded-full bg-amber-500" aria-hidden />}
    </button>
  )
}

// --- Code ---------------------------------------------------------------------------------------------

/** The panel's left edge, dragged (or arrowed) to resize it. Double-click puts it back. */
function Resizer() {
  const width = useCodeUi((s) => s.panelWidth)
  const setPanelWidth = useCodeUi((s) => s.setPanelWidth)
  const [dragging, setDragging] = useState(false)

  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0) return
    const handle = event.currentTarget
    const aside = handle.parentElement
    if (!aside) return
    event.preventDefault()
    const startX = event.clientX
    const startWidth = aside.getBoundingClientRect().width
    // Beside the thread, the thread keeps room to be read; over it (a narrow window), nearly all.
    const room = (aside.parentElement?.clientWidth ?? window.innerWidth) - (window.innerWidth >= 1280 ? 352 : 32)
    const max = Math.max(PANEL_MIN_WIDTH, Math.min(PANEL_MAX_WIDTH, room))
    let next = startWidth
    handle.setPointerCapture(event.pointerId)
    setDragging(true)
    const body = document.body.style
    const restore = { cursor: body.cursor, userSelect: body.userSelect }
    body.cursor = 'col-resize'
    body.userSelect = 'none'
    // The width is set on the element while dragging and saved once at the end: a store write per
    // pixel would redraw every pane — and write localStorage — sixty times a second.
    const move = (e: globalThis.PointerEvent) => {
      next = Math.min(max, clampPanelWidth(startWidth + (startX - e.clientX)))
      aside.style.width = `${next}px`
    }
    const end = () => {
      handle.removeEventListener('pointermove', move)
      handle.removeEventListener('pointerup', end)
      handle.removeEventListener('pointercancel', end)
      body.cursor = restore.cursor
      body.userSelect = restore.userSelect
      setDragging(false)
      setPanelWidth(next)
    }
    handle.addEventListener('pointermove', move)
    handle.addEventListener('pointerup', end)
    handle.addEventListener('pointercancel', end)
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? 64 : 16
    const keys: Record<string, number> = {
      ArrowLeft: width + step,
      ArrowRight: width - step,
      Home: PANEL_MIN_WIDTH,
      End: PANEL_MAX_WIDTH,
      Enter: PANEL_DEFAULT_WIDTH,
    }
    const target = keys[event.key]
    if (target === undefined) return
    event.preventDefault()
    setPanelWidth(target)
  }

  return (
    <div
      role="separator"
      aria-orientation="vertical"
      aria-label="Resize the panel"
      aria-valuenow={width}
      aria-valuemin={PANEL_MIN_WIDTH}
      aria-valuemax={PANEL_MAX_WIDTH}
      tabIndex={0}
      title="Drag to resize · double-click to reset"
      onPointerDown={onPointerDown}
      onDoubleClick={() => setPanelWidth(PANEL_DEFAULT_WIDTH)}
      onKeyDown={onKeyDown}
      className="group absolute inset-y-0 -left-1 z-20 w-2 cursor-col-resize touch-none outline-none"
    >
      <span
        className={`absolute inset-y-0 left-1/2 w-px -translate-x-1/2 transition-colors ${
          dragging ? 'bg-arc-500' : 'bg-transparent group-hover:bg-arc-500/50 group-focus-visible:bg-arc-500'
        }`}
      />
    </div>
  )
}

function CodePanel({ conversation, tab }: { conversation: Conversation | null; tab: Panel }) {
  const width = useCodeUi((s) => s.panelWidth)
  const setRightPanel = useUi((s) => s.setRightPanel)
  const root = conversation?.workspace ?? null
  const git = useGitStatus(root)
  const terminalBusy = useTerminalBusy(root)
  const changes = git?.status?.is_git ? git.status.files.length : (conversation?.changes?.length ?? 0)
  const fills = tab === 'terminal' || tab === 'files'

  return (
    <aside
      style={{ width }}
      className="@container relative flex max-w-full shrink-0 flex-col border-l border-ink-200 bg-white xl:max-w-[calc(100%-22rem)] dark:border-ink-800 dark:bg-ink-900 max-xl:absolute max-xl:inset-y-0 max-xl:right-0 max-xl:z-30 max-xl:max-w-[calc(100%-2rem)] max-xl:shadow-2xl max-xl:shadow-ink-950/20 dark:max-xl:shadow-black/50"
      aria-label="Session panel"
    >
      <Resizer />
      <div className="flex h-11 shrink-0 items-center gap-0.5 overflow-hidden border-b border-ink-200 px-2 dark:border-ink-800" role="tablist" aria-label="Panels">
        {CODE_TABS.map((id) => (
          <Tab
            key={id}
            id={id}
            active={tab === id}
            label={PANE_META[id].label}
            icon={PANE_META[id].icon}
            count={id === 'changes' ? changes : undefined}
            dot={id === 'terminal' && terminalBusy}
            compact
          />
        ))}
        <IconButton icon="x" label="Close panel" onClick={() => setRightPanel(null)} className="ml-auto" />
      </div>
      <div id="thread-panel-body" className={`min-h-0 flex-1 ${fills ? 'overflow-hidden' : 'overflow-y-auto'}`} role="tabpanel" aria-labelledby={`panel-tab-${tab}`}>
        {tab === 'session' && <SessionPanel conversation={conversation} />}
        {tab === 'changes' && <GitChangesPanel conversation={conversation} />}
        {tab === 'terminal' && <TerminalPanel conversation={conversation} />}
        {tab === 'files' && <FilesPanel conversation={conversation} />}
        {tab === 'mining' && <MiningPanel conversation={conversation} />}
      </div>
    </aside>
  )
}

// --- Chat ---------------------------------------------------------------------------------------------

function ChatPanel({ conversation, tab }: { conversation: Conversation | null; tab: Panel }) {
  const setRightPanel = useUi((s) => s.setRightPanel)
  const pins = conversation?.pinned?.length ?? 0

  return (
    <aside
      className="flex w-[360px] max-w-full shrink-0 flex-col border-l border-ink-200 bg-white dark:border-ink-800 dark:bg-ink-900 max-xl:absolute max-xl:inset-y-0 max-xl:right-0 max-xl:z-30 max-xl:shadow-2xl max-xl:shadow-ink-950/20 dark:max-xl:shadow-black/50"
      aria-label="Thread panel"
    >
      <div className="flex h-11 shrink-0 items-center gap-0.5 border-b border-ink-200 px-2 dark:border-ink-800" role="tablist" aria-label="Panels">
        <Tab id="mining" active={tab === 'mining'} label="Mining" icon="pickaxe" />
        <Tab id="context" active={tab === 'context'} label="Context" icon="info" count={pins} />
        <IconButton icon="x" label="Close panel" onClick={() => setRightPanel(null)} className="ml-auto" />
      </div>
      <div id="thread-panel-body" className="min-h-0 flex-1 overflow-y-auto" role="tabpanel" aria-labelledby={`panel-tab-${tab}`}>
        {tab === 'mining' && <MiningPanel conversation={conversation} />}
        {tab === 'context' && <ContextTab conversation={conversation} />}
      </div>
    </aside>
  )
}

function SectionTitle({ children }: { children: string }) {
  return <h3 className="mb-1.5 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">{children}</h3>
}

/** Pinned notes, the last reply's context report, and a preview of the next request's. */
function ContextTab({ conversation }: { conversation: Conversation | null }) {
  const settings = useStudio((s) => s.settings)
  const draft = useDraft(draftKey(conversation?.id))
  const [preview, setPreview] = useState<{ report: ContextReport | null; error: string | null; loading: boolean } | null>(null)
  const last = [...(conversation?.messages ?? [])].reverse().find((m) => m.role === 'assistant' && m.context)?.context ?? null
  const agent = conversation?.mode === 'agent'
  // Without a next message there is no next request: a thread that ends on a reply would be read
  // by the runtime as "continue that reply", which is not what anyone previewing means.
  const hasDraft = draft.text.trim().length > 0 || draft.attachments.length > 0

  // What the next request would carry — the thread as it stands plus the draft in the composer —
  // asked of the runtime's own context manager, so the answer is the one the request will get.
  const previewNext = async () => {
    if (!conversation || !hasDraft) return
    setPreview({ report: null, error: null, loading: true })
    const history = historyForModel(conversation.messages)
    history.push({ role: 'user', content: contentForModel({ content: draft.text.trim(), attachments: draft.attachments }) })
    const system = settings?.generation.system_prompt?.trim()
    const messages = system ? [{ role: 'system' as const, content: system }, ...history] : history
    try {
      const report = await api.contextPlan(messages, conversation.pinned ?? [], settings?.generation.max_tokens)
      setPreview({ report, error: null, loading: false })
    } catch (error) {
      setPreview({ report: null, error: (error as Error).message, loading: false })
    }
  }

  return (
    <div className="space-y-6 p-3">
      <section>
        <SectionTitle>Pinned notes</SectionTitle>
        <p className="mb-2 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
          Standing facts for this thread. Every request carries them ahead of the history, so a small model window never drops them.
        </p>
        <PinnedNotesEditor />
      </section>

      <section>
        <SectionTitle>Last reply</SectionTitle>
        {last ? (
          <ContextReportView report={last} />
        ) : (
          <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">No reply in this thread has a context report yet — one comes with every answer.</p>
        )}
      </section>

      {conversation && !agent && (
        <section>
          <SectionTitle>Next request</SectionTitle>
          <p className="mb-2 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
            {hasDraft
              ? 'What the loaded model would be sent for this thread and the message in the composer — without sending it.'
              : 'Type the next message in the composer, then preview what its request would carry — without sending it.'}
          </p>
          <button type="button" className="btn-outline px-2 py-1 text-xs" onClick={() => void previewNext()} disabled={!hasDraft || preview?.loading}>
            {preview?.loading ? <Spinner className="size-3.5" /> : <Icon name="eye" className="size-3.5" />}
            Preview
          </button>
          {preview?.error && <p className="mt-2 text-[0.7rem] text-red-700 dark:text-red-300">{preview.error}</p>}
          {preview?.report && (
            <div className="mt-3">
              <ContextReportView report={preview.report} />
            </div>
          )}
        </section>
      )}
    </div>
  )
}
