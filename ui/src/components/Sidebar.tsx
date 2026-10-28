// Navigation and threads, for whichever side of the window is in view.
//
// The window has two sides, as in Claude's desktop app, and the switch at the top says which:
// **Chat** lists conversations by date; **Code** lists agent sessions under the folder each works
// in, like Claude Code. The rest of the window — Models, Image, Network (where mining is set up),
// Monitor, Settings — is the same from both sides, kept small at the bottom so the threads get the
// room.
//
// The connection dot is not decoration: this window talks to a separate process, and when that
// process is not there every other part of the UI is showing stale data. Saying so in one place,
// permanently, beats a toast that has already faded by the time someone looks up.
//
// Collapsed, the sidebar becomes a rail of icons rather than nothing at all: a sidebar that can
// only be brought back with a shortcut is one a person who never learnt the shortcut loses.

import { memo, useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from 'react'
import logo from '../assets/misaka-logo.png'
import { useAgent } from '../lib/agent/run'
import { useWorkspaceInfo } from '../lib/agent/workspace'
import { formatShortcut } from '../lib/keys'
import type { Conversation } from '../lib/types'
import { useStudio, type View } from '../store/studio'
import { useUi, type AppMode } from '../store/ui'
import { useGitStatus } from './code/gitStore'
import { Icon, Spinner, type IconName } from './common'
import { IconButton, Kbd, MenuItem, MenuSeparator, Popover, StatusDot } from './ui'
import {
  deleteThread,
  exportThread,
  goTo,
  newChat,
  newSession,
  newSessionIn,
  nextTheme,
  openFolder,
  openThread,
  setTheme,
  switchAppMode,
  type Theme,
} from './shell/actions'
import { dirtySummary, folderLabel, groupProjects, sessionTitle, shortAge, type Project } from './shell/projects'
import { NEW_CHAT_HINT, SHORTCUTS, SIDE_ICONS, SIDE_LABELS, SIDE_SHORTCUTS, VIEW_ICONS, VIEW_LABELS, VIEW_SHORTCUTS } from './shell/shortcuts'
import { sideOf, threadsOf } from './shell/sides'
import { groupThreads, threadMining } from './shell/threads'
import { useNewSessionProject } from './shell/useSessionProject'

/** The views beside the threads. The thread view itself is the switch's; Settings is in the footer. */
const NAV: View[] = ['models', 'image', 'network', 'monitor']

const NAV_NOTES: Partial<Record<View, string>> = {
  models: 'find, download and load models',
  image: 'generate images',
  network: 'prompt mining setup, the classes and the chain',
  monitor: 'this machine and the engine, live',
}

const SIDES: AppMode[] = ['chat', 'code']

/** The small buttons a row shows under the pointer or the keyboard: a row's menu, a project's `+`. */
const ROW_BUTTON =
  'inline-flex size-6 items-center justify-center rounded-md text-ink-500 hover:bg-ink-200 hover:text-ink-900 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-arc-500/50 dark:text-ink-400 dark:hover:bg-ink-700 dark:hover:text-ink-50'

const THEME_ICON = { system: 'monitor', light: 'sun', dark: 'moon' } as const
const THEME_LABEL: Record<Theme, string> = { system: 'System', light: 'Light', dark: 'Dark' }

export function Sidebar() {
  const open = useUi((s) => s.sidebarOpen)
  return open ? <ExpandedSidebar /> : <SidebarRail />
}

function useActiveDownloads(): number {
  return useStudio((s) => s.downloads.filter((d) => d.status === 'downloading' || d.status === 'verifying').length)
}

type Activity = 'running' | 'approval' | null

/** What is happening on each side — for a side not in view, so work going on there is not missed. */
function useSideActivity(): Record<AppMode, Activity> {
  const run = useAgent((s) => s.run)
  const streamingSide = useStudio((s) => {
    const id = s.streaming?.conversationId
    if (!id) return null
    const thread = s.conversations.find((c) => c.id === id)
    return thread ? sideOf(thread) : null
  })
  return {
    chat: streamingSide === 'chat' ? 'running' : null,
    code: run ? (run.status === 'awaiting_approval' ? 'approval' : 'running') : streamingSide === 'code' ? 'running' : null,
  }
}

function sideTitle(mode: AppMode, activity: Activity): string {
  const base = `${SIDE_LABELS[mode]} (${formatShortcut(SIDE_SHORTCUTS[mode])})`
  if (activity === 'approval') return `${base} — the agent is waiting for your approval`
  if (activity === 'running') return mode === 'code' ? `${base} — the agent is working` : `${base} — a reply is being written`
  return base
}

/** A light on a side that is not in view: something is going on over there. */
function ActivityDot({ activity, className = '' }: { activity: Activity; className?: string }) {
  if (!activity) return null
  return <StatusDot tone={activity === 'approval' ? 'amber' : 'arc'} pulse className={`pointer-events-none absolute ${className}`} />
}

function ExpandedSidebar() {
  const toggleSidebar = useUi((s) => s.toggleSidebar)
  const openPalette = useUi((s) => s.openPalette)
  const side = useUi((s) => s.appMode)

  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-ink-200 bg-white dark:border-ink-800 dark:bg-ink-900" aria-label="Sidebar">
      <div className="flex h-11 shrink-0 items-center gap-2 pl-3.5 pr-2">
        {/* The project's own mark, not a letter in a box. It carries its own colour in both
            themes, so it is drawn at its native aspect rather than cropped into a square. */}
        <img src={logo} alt="" className="h-6 w-auto shrink-0" />
        <span className="min-w-0 flex-1 truncate text-[0.85rem] font-semibold tracking-tight">MISAKA Studio</span>
        <IconButton icon="panelLeft" label="Collapse sidebar" shortcut={SHORTCUTS.toggleSidebar} onClick={toggleSidebar} />
      </div>

      <div className="shrink-0 px-2 pb-2">
        <SideSwitch />
      </div>

      <div className="shrink-0 space-y-px px-2 pb-1">
        {side === 'chat' ? (
          <SidebarButton icon="compose" label="New chat" shortcut={NEW_CHAT_HINT} onClick={newChat} />
        ) : (
          <>
            <NewSessionButton />
            <SidebarButton icon="folderOpen" label="Open folder…" title="Open a folder and start a session in it" onClick={openFolder} />
          </>
        )}
        <SidebarButton icon="search" label="Search" shortcut={SHORTCUTS.palette} onClick={() => openPalette()} />
      </div>

      {side === 'chat' ? <ChatThreadList /> : <ProjectList />}

      <div className="shrink-0 border-t border-ink-200 dark:border-ink-800">
        <NavGrid />
        <SidebarFooter />
      </div>
    </aside>
  )
}

/**
 * Chat | Code, as in Claude's desktop app. The side in view is the raised one; the other carries a
 * light while something is going on there. Clicking the side in view is the way back to its thread
 * from Models or Network.
 */
function SideSwitch() {
  const side = useUi((s) => s.appMode)
  const view = useStudio((s) => s.view)
  const activity = useSideActivity()
  return (
    <div className="grid grid-cols-2 gap-0.5 rounded-lg bg-ink-100 p-0.5 dark:bg-ink-800" role="group" aria-label="Chat or Code">
      {SIDES.map((mode) => {
        const on = side === mode
        return (
          <button
            key={mode}
            type="button"
            aria-pressed={on}
            title={sideTitle(mode, on ? null : activity[mode])}
            onClick={() => switchAppMode(mode)}
            className={`relative flex h-7 min-w-0 items-center justify-center gap-1.5 rounded-md text-[0.8rem] font-medium transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-arc-500/40 ${
              on
                ? `bg-white shadow-sm shadow-ink-950/5 dark:bg-ink-950 ${view === 'chat' ? 'text-ink-900 dark:text-ink-50' : 'text-ink-700 dark:text-ink-200'}`
                : 'text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-100'
            }`}
          >
            <Icon name={SIDE_ICONS[mode]} className="size-3.5 shrink-0" />
            {SIDE_LABELS[mode]}
            {!on && <ActivityDot activity={activity[mode]} className="right-2 top-1/2 -translate-y-1/2" />}
          </button>
        )
      })}
    </div>
  )
}

function SidebarButton({
  icon,
  label,
  shortcut,
  title,
  onClick,
}: {
  icon: IconName
  label: string
  shortcut?: string
  /** The tooltip, when it should say more than the label and its key. */
  title?: string
  onClick: () => void
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title ?? (shortcut ? `${label} (${formatShortcut(shortcut)})` : label)}
      className="flex h-8 w-full items-center gap-2.5 rounded-lg px-2.5 text-[0.8rem] text-ink-700 transition-colors hover:bg-ink-100 hover:text-ink-900 focus-visible:bg-ink-100 focus-visible:outline-none dark:text-ink-200 dark:hover:bg-ink-800 dark:hover:text-ink-50 dark:focus-visible:bg-ink-800"
    >
      <Icon name={icon} className="size-4 shrink-0 opacity-80" />
      <span className="min-w-0 flex-1 truncate text-left">{label}</span>
      {shortcut && <Kbd shortcut={shortcut} className="shrink-0" />}
    </button>
  )
}

function NewSessionButton() {
  const project = useNewSessionProject()
  const keys = formatShortcut(NEW_CHAT_HINT)
  return (
    <SidebarButton
      icon="plus"
      label="New session"
      shortcut={NEW_CHAT_HINT}
      title={project ? `New session in ${folderLabel(project)} (${keys})\n${project}` : `New session — choose its folder first (${keys})`}
      onClick={newSession}
    />
  )
}

// --- the lists -------------------------------------------------------------------------------------

const ROWS = '[data-thread-row], [data-project-row]'

/** Up and down walk the rows like a list, once one of them has the focus; left from a session goes
 *  to its project's name. */
function onListKeyDown(event: ReactKeyboardEvent<HTMLDivElement>) {
  const target = event.target as HTMLElement
  if (!target.matches(ROWS)) return
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    const rows = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(ROWS))
    const next = rows[rows.indexOf(target) + (event.key === 'ArrowDown' ? 1 : -1)]
    if (next) {
      event.preventDefault()
      next.focus()
    }
  } else if (event.key === 'ArrowLeft' && target.matches('[data-thread-row]')) {
    const header = target.closest('[data-project-group]')?.querySelector<HTMLElement>('[data-project-row]')
    if (header) {
      event.preventDefault()
      header.focus()
    }
  }
}

function runningOf(id: string, agentRun: { conversationId: string; status: string } | null, streamingId: string | null): Activity {
  if (agentRun?.conversationId === id) return agentRun.status === 'awaiting_approval' ? 'approval' : 'running'
  return streamingId === id ? 'running' : null
}

/** Chat's side: the conversations, starred first and then by date — as the sidebar always listed them. */
function ChatThreadList() {
  const conversations = useStudio((s) => s.conversations)
  const activeId = useStudio((s) => s.activeConversationId)
  const view = useStudio((s) => s.view)
  const streamingId = useStudio((s) => s.streaming?.conversationId ?? null)
  const agentRun = useAgent((s) => s.run)
  const groups = useMemo(() => groupThreads(threadsOf(conversations, 'chat')), [conversations])

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2" onKeyDown={onListKeyDown}>
      {groups.length === 0 && (
        <p className="px-2.5 py-4 text-[0.8rem] leading-relaxed text-ink-500 dark:text-ink-400">
          No chats yet. Start one with <Kbd shortcut={NEW_CHAT_HINT} />.
        </p>
      )}
      {groups.map((group) => (
        <section key={group.id} aria-label={group.label}>
          <h3 className="sticky top-0 z-[1] bg-white px-2.5 pb-1 pt-3 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:bg-ink-900 dark:text-ink-500">
            {group.label}
          </h3>
          <ul className="space-y-px">
            {group.threads.map((conversation) => {
              const mining = threadMining(conversation)
              return (
                <ThreadRow
                  key={conversation.id}
                  conversation={conversation}
                  kind="chat"
                  active={view === 'chat' && conversation.id === activeId}
                  running={runningOf(conversation.id, agentRun, streamingId)}
                  pending={mining.pending}
                  committed={mining.committed}
                />
              )
            })}
          </ul>
        </section>
      ))}
    </div>
  )
}

/** A clock that moves once a minute — enough for "5m" to become "6m" without a timer per row. */
function useMinuteClock(): number {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 60_000)
    return () => clearInterval(timer)
  }, [])
  return now
}

/** Code's side: every folder with a session, and the folders opened recently, sessions beneath. */
function ProjectList() {
  const conversations = useStudio((s) => s.conversations)
  const activeId = useStudio((s) => s.activeConversationId)
  const view = useStudio((s) => s.view)
  const streamingId = useStudio((s) => s.streaming?.conversationId ?? null)
  const agentRun = useAgent((s) => s.run)
  const recent = useUi((s) => s.recentWorkspaces)
  const collapsed = useUi((s) => s.collapsedProjects)
  const now = useMinuteClock()
  const projects = useMemo(() => groupProjects(threadsOf(conversations, 'code'), recent), [conversations, recent])

  if (projects.length === 0) {
    return (
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-4 text-[0.8rem] leading-relaxed text-ink-500 dark:text-ink-400">
        <p>No projects yet. Open a folder and the agent works in it — reading, editing and running commands, with your approval.</p>
        <button type="button" className="btn-outline mt-3 px-2.5 py-1 text-xs" onClick={openFolder}>
          <Icon name="folderOpen" className="size-3.5" />
          Open folder…
        </button>
      </div>
    )
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-2" onKeyDown={onListKeyDown}>
      {projects.map((project) => {
        const folded = collapsed.includes(project.key)
        // A folded project still shows the session in view: the thread on screen is never one the
        // sidebar has hidden.
        const shown = folded ? project.sessions.filter((s) => view === 'chat' && s.id === activeId) : project.sessions
        const busy = project.sessions.some((s) => s.id === agentRun?.conversationId || s.id === streamingId)
        return (
          <section key={project.key || '(no folder)'} data-project-group aria-label={project.name} className="pt-2 first:pt-1">
            <ProjectHeader project={project} collapsed={folded} busy={busy} />
            {shown.length > 0 && (
              <ul className="space-y-px">
                {shown.map((session) => {
                  const mining = threadMining(session)
                  return (
                    <ThreadRow
                      key={session.id}
                      conversation={session}
                      kind="session"
                      active={view === 'chat' && session.id === activeId}
                      running={runningOf(session.id, agentRun, streamingId)}
                      pending={mining.pending}
                      committed={mining.committed}
                      age={shortAge(session.updatedAt, now)}
                      changes={session.changes?.length ?? 0}
                    />
                  )
                })}
              </ul>
            )}
          </section>
        )
      })}
    </div>
  )
}

/** A project's name row: folds its sessions; on hover, a new session there and the folder's menu. */
function ProjectHeader({ project, collapsed, busy }: { project: Project; collapsed: boolean; busy: boolean }) {
  // The runtime's account of the folder — its branch — cached app-wide and asked again only once
  // it is stale (`lib/agent/workspace`), not on every render of this row.
  const entry = useWorkspaceInfo(project.path)
  const [menuOpen, setMenuOpen] = useState(false)
  const more = useRef<HTMLButtonElement>(null)
  const info = entry?.status === 'ok' ? entry.info : null
  const path = project.path
  const count = project.sessions.length
  // The working tree, from the one reading the Code panes share (`code/gitStore`) — refreshed after
  // agent steps and terminal commands, so the branch follows a checkout made there. Asked only for
  // folders with sessions: a folder merely opened once is not worth a `git status` per visit.
  const git = useGitStatus(path && count > 0 ? path : null)
  const repo = git?.status?.is_git ? git.status : null
  const branch = repo?.branch ?? repo?.head ?? info?.branch ?? null
  const dirty = repo ? dirtySummary(repo) : null
  const toggle = () => useUi.getState().toggleProject(project.key)

  const title = path
    ? [`${info?.root ?? path}${branch ? ` · ${branch}` : ''}`, dirty, entry?.status === 'error' ? entry.error : null, count === 0 ? 'No sessions yet' : null]
        .filter(Boolean)
        .join('\n')
    : 'Sessions without a folder — choose one from the folder chip in the composer'

  return (
    <div className="group/header relative">
      <button
        type="button"
        data-project-row
        aria-expanded={!collapsed}
        title={title}
        onClick={toggle}
        onKeyDown={(event) => {
          if (event.key === 'ArrowLeft' && !collapsed) {
            event.preventDefault()
            toggle()
          } else if (event.key === 'ArrowRight') {
            event.preventDefault()
            if (collapsed) toggle()
            else event.currentTarget.closest('[data-project-group]')?.querySelector<HTMLElement>('[data-thread-row]')?.focus()
          }
        }}
        className={`peer flex h-8 w-full items-center gap-2 rounded-lg pl-2.5 pr-2 text-left text-[0.8rem] font-medium transition-colors hover:bg-ink-100 focus-visible:bg-ink-100 focus-visible:outline-none dark:hover:bg-ink-800 dark:focus-visible:bg-ink-800 ${
          menuOpen ? 'bg-ink-100 dark:bg-ink-800' : ''
        } ${path ? 'text-ink-700 dark:text-ink-200' : 'text-ink-500 dark:text-ink-400'}`}
      >
        {/* The folder, open or shut; under the pointer, the chevron that says a click folds it. */}
        <span className="relative flex size-4 shrink-0 items-center justify-center text-ink-400 dark:text-ink-500" aria-hidden>
          <Icon name={collapsed ? 'folder' : 'folderOpen'} className="size-4 transition-opacity group-hover/header:opacity-0" />
          <Icon
            name="chevronDown"
            className={`absolute size-3.5 opacity-0 transition-[opacity,transform] group-hover/header:opacity-100 ${collapsed ? '-rotate-90' : ''}`}
          />
        </span>
        <span className="min-w-0 shrink truncate">{project.name}</span>
        {branch && (
          <span className="flex min-w-0 flex-1 items-center gap-0.5 text-[0.7rem] font-normal text-ink-400 dark:text-ink-500">
            <Icon name="gitBranch" className="size-3 shrink-0" />
            <span className="min-w-0 truncate">{branch}</span>
            {dirty && <span className="ml-1 size-1.5 shrink-0 rounded-full bg-amber-500" role="img" aria-label={dirty} />}
          </span>
        )}
        <span className="ml-auto flex shrink-0 items-center pl-1 text-[0.7rem] font-normal tabular-nums text-ink-400 transition-opacity group-hover/header:opacity-0 dark:text-ink-500">
          {collapsed && busy ? <Spinner className="size-3 text-arc-600 dark:text-arc-400" /> : collapsed && count > 0 ? count : null}
        </span>
      </button>

      {path && (
        <div
          className={`absolute inset-y-0 right-0 flex items-center gap-0.5 rounded-r-lg bg-gradient-to-l from-ink-100 from-60% to-transparent pl-5 pr-1 transition-opacity dark:from-ink-800 ${
            menuOpen
              ? 'opacity-100'
              : 'pointer-events-none opacity-0 focus-within:pointer-events-auto focus-within:opacity-100 group-hover/header:pointer-events-auto group-hover/header:opacity-100 peer-focus-visible:pointer-events-auto peer-focus-visible:opacity-100'
          }`}
        >
          <button
            type="button"
            className={ROW_BUTTON}
            aria-label={`New session in ${project.name}`}
            title={`New session in ${project.name}`}
            onClick={() => newSessionIn(path)}
          >
            <Icon name="plus" className="size-3.5" />
          </button>
          <button
            ref={more}
            type="button"
            className={ROW_BUTTON}
            aria-label={`More actions for ${project.name}`}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            title="More"
            onClick={() => setMenuOpen((v) => !v)}
          >
            <Icon name="more" className="size-4" />
          </button>
        </div>
      )}

      {path && (
        <Popover anchor={more} open={menuOpen} onClose={() => setMenuOpen(false)} placement="bottom-end" className="w-60">
          <MenuItem
            icon="plus"
            label="New session here"
            onSelect={() => {
              setMenuOpen(false)
              newSessionIn(path)
            }}
          />
          <MenuItem
            icon="copy"
            label="Copy path"
            onSelect={() => {
              setMenuOpen(false)
              void copyPath(path)
            }}
          />
          {/* Only a folder with no sessions leaves the list: one with sessions is listed because of
              them, and would be back the moment it was removed. */}
          {count === 0 && project.recent && (
            <>
              <MenuSeparator />
              <MenuItem
                icon="x"
                label="Remove from recent"
                onSelect={() => {
                  setMenuOpen(false)
                  useUi.getState().removeRecentWorkspace(path)
                }}
              />
            </>
          )}
        </Popover>
      )}
    </div>
  )
}

async function copyPath(path: string) {
  const toast = useStudio.getState().toast
  try {
    await navigator.clipboard.writeText(path)
    toast('success', `Copied ${path}`)
  } catch {
    toast('error', 'The clipboard is not available here.')
  }
}

// --- one thread ------------------------------------------------------------------------------------

type RowProps = {
  conversation: Conversation
  /** A chat row leads with the chat mark; a session's lead is empty until it runs, and it says how
   *  long ago it changed and what the agent changed. */
  kind: 'chat' | 'session'
  active: boolean
  running: Activity
  pending: number
  committed: number
  age?: string
  changes?: number
}

/** Only what a row draws: a poll that rewrites every conversation object must not redraw them all. */
function sameRow(a: RowProps, b: RowProps): boolean {
  const x = a.conversation
  const y = b.conversation
  return (
    a.kind === b.kind &&
    a.active === b.active &&
    a.running === b.running &&
    a.pending === b.pending &&
    a.committed === b.committed &&
    a.age === b.age &&
    a.changes === b.changes &&
    x.id === y.id &&
    x.title === y.title &&
    x.mode === y.mode &&
    x.workspace === y.workspace &&
    x.starred === y.starred &&
    x.mine === y.mine &&
    x.messages.length === y.messages.length
  )
}

const ThreadRow = memo(function ThreadRow({ conversation, kind, active, running, pending, committed, age, changes = 0 }: RowProps) {
  const [renaming, setRenaming] = useState(false)
  const [menuOpen, setMenuOpen] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const more = useRef<HTMLButtonElement>(null)
  const row = useRef<HTMLButtonElement>(null)
  const session = kind === 'session'
  const title = session ? sessionTitle(conversation) : conversation.title
  const mining = conversation.mine !== false
  const noun = session ? 'session' : 'thread'

  const closeMenu = () => {
    setMenuOpen(false)
    setConfirmDelete(false)
  }
  const finishRename = (next: string | null, byKeyboard: boolean) => {
    setRenaming(false)
    if (next !== null && next.trim() && next.trim() !== title) useStudio.getState().renameConversation(conversation.id, next)
    // After Enter or Escape the keyboard carries on from the row. After a click elsewhere the focus
    // is wherever that click put it, and taking it back would be a surprise.
    if (byKeyboard) requestAnimationFrame(() => row.current?.focus({ preventScroll: true }))
  }

  return (
    <li className="group relative">
      {renaming ? (
        <RenameField initial={title} label={session ? 'Session name' : 'Thread name'} onDone={finishRename} />
      ) : (
        <button
          ref={row}
          type="button"
          data-thread-row
          onClick={() => openThread(conversation.id)}
          onDoubleClick={() => setRenaming(true)}
          onKeyDown={(event) => {
            if (event.key === 'F2') {
              event.preventDefault()
              setRenaming(true)
            }
          }}
          aria-current={active ? 'page' : undefined}
          title={title}
          className={`peer group/row flex h-8 w-full items-center gap-2 rounded-lg pl-2.5 pr-2 text-left text-[0.8rem] transition-colors focus-visible:bg-ink-100 focus-visible:outline-none dark:focus-visible:bg-ink-800 ${
            active || menuOpen
              ? 'bg-ink-100 text-ink-900 dark:bg-ink-800 dark:text-ink-50'
              : 'text-ink-600 hover:bg-ink-100 hover:text-ink-900 dark:text-ink-300 dark:hover:bg-ink-800 dark:hover:text-ink-50'
          }`}
        >
          <span className="flex size-4 shrink-0 items-center justify-center" aria-hidden>
            {running === 'running' ? (
              <Spinner className="size-3.5 text-arc-600 dark:text-arc-400" />
            ) : running === 'approval' ? (
              <StatusDot tone="amber" pulse />
            ) : session ? null : (
              <Icon name="chat" className="size-3.5 text-ink-400 dark:text-ink-500" />
            )}
          </span>
          <span className="min-w-0 flex-1 truncate">{title}</span>
          {/* The trailing marks give way to the row's menu button on hover, in the same space,
              so nothing in the row moves when the pointer crosses it. */}
          <span
            className={`flex shrink-0 items-center gap-1.5 transition-opacity group-hover:opacity-0 group-focus-visible/row:opacity-0 ${menuOpen ? 'opacity-0' : ''}`}
          >
            {session && conversation.starred && (
              <span title="Starred" className="flex">
                <Icon name="star" className="size-3 text-amber-500" />
              </span>
            )}
            {session && changes > 0 && (
              <span
                className="flex items-center gap-0.5 text-[0.7rem] tabular-nums text-ink-400 dark:text-ink-500"
                title={`The agent changed ${changes} file${changes === 1 ? '' : 's'} — review them in Changes`}
              >
                <Icon name="diff" className="size-3" />
                {changes}
              </span>
            )}
            {pending > 0 ? (
              <span title={`${pending} prompt${pending === 1 ? '' : 's'} from this ${noun} waiting to be mined`} className="flex">
                <Icon name="pickaxe" className="size-3 animate-pulse text-amber-600 dark:text-amber-400" />
              </span>
            ) : committed > 0 ? (
              <span title={`${committed} prompt${committed === 1 ? '' : 's'} from this ${noun} committed by the mining lane`} className="flex">
                <Icon name="pickaxe" className="size-3 text-emerald-600 dark:text-emerald-400" />
              </span>
            ) : null}
            {age && (
              <span className="min-w-[1.6rem] text-right text-[0.7rem] tabular-nums text-ink-400 dark:text-ink-500" title={new Date(conversation.updatedAt).toLocaleString()}>
                {age}
              </span>
            )}
          </span>
        </button>
      )}

      {!renaming && (
        <div
          className={`absolute inset-y-0 right-0 flex items-center rounded-r-lg bg-gradient-to-l from-ink-100 from-60% to-transparent pl-4 pr-1 transition-opacity dark:from-ink-800 ${
            menuOpen
              ? 'opacity-100'
              : 'pointer-events-none opacity-0 focus-within:pointer-events-auto focus-within:opacity-100 group-hover:pointer-events-auto group-hover:opacity-100 peer-focus-visible:pointer-events-auto peer-focus-visible:opacity-100'
          }`}
        >
          <button
            ref={more}
            type="button"
            className={ROW_BUTTON}
            aria-label={`More actions for ${title}`}
            aria-haspopup="menu"
            aria-expanded={menuOpen}
            title="More"
            onClick={() => (menuOpen ? closeMenu() : setMenuOpen(true))}
          >
            <Icon name="more" className="size-4" />
          </button>
        </div>
      )}

      <Popover anchor={more} open={menuOpen} onClose={closeMenu} placement="bottom-end" className="w-72">
        <MenuItem
          icon="edit"
          label="Rename"
          hint="F2"
          onSelect={() => {
            closeMenu()
            setRenaming(true)
          }}
        />
        <MenuItem
          icon="star"
          label={conversation.starred ? 'Unstar' : 'Star'}
          onSelect={() => {
            closeMenu()
            useStudio.getState().starConversation(conversation.id, !conversation.starred)
          }}
        />
        <MenuItem
          icon="pickaxe"
          label={`Mine prompts from this ${noun}`}
          hint={mining ? 'on' : 'off'}
          onSelect={() => {
            closeMenu()
            useStudio.getState().setConversationMining(conversation.id, !mining)
          }}
        />
        <MenuItem
          icon="download"
          label="Export as Markdown"
          onSelect={() => {
            closeMenu()
            // Read at the click: this row redraws only for what it shows, so the conversation it
            // holds can be a reply behind the one in the store.
            const latest = useStudio.getState().conversations.find((c) => c.id === conversation.id)
            if (latest) exportThread(latest)
          }}
        />
        <MenuSeparator />
        {confirmDelete ? (
          <div className="flex items-center gap-1.5 px-2.5 py-1 text-[0.8rem]" role="group" aria-label="Confirm delete">
            <Icon name="trash" className="size-4 shrink-0 text-red-600 opacity-80 dark:text-red-400" />
            <span className="min-w-0 flex-1 font-medium text-red-600 dark:text-red-400">Delete?</span>
            <button
              type="button"
              className="rounded-md bg-red-600 px-2 py-0.5 text-xs font-medium text-white hover:bg-red-500"
              autoFocus
              onClick={() => {
                closeMenu()
                deleteThread(conversation.id)
              }}
            >
              Yes
            </button>
            <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => setConfirmDelete(false)}>
              No
            </button>
          </div>
        ) : (
          <MenuItem icon="trash" label="Delete" danger onSelect={() => setConfirmDelete(true)} />
        )}
      </Popover>
    </li>
  )
}, sameRow)

/** The title in place, editable. Enter or leaving the field saves; Escape puts it back. */
function RenameField({ initial, label, onDone }: { initial: string; label: string; onDone: (title: string | null, byKeyboard: boolean) => void }) {
  const [value, setValue] = useState(initial)
  const finished = useRef(false)
  const finish = (title: string | null, byKeyboard = false) => {
    if (finished.current) return
    finished.current = true
    onDone(title, byKeyboard)
  }
  return (
    <input
      autoFocus
      value={value}
      aria-label={label}
      onChange={(event) => setValue(event.target.value)}
      onFocus={(event) => event.target.select()}
      onBlur={() => finish(value)}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing || event.keyCode === 229) return
        if (event.key === 'Enter') {
          event.preventDefault()
          finish(value, true)
        } else if (event.key === 'Escape') {
          // Claimed here, so the window's Escape (stop generating) leaves it alone.
          event.preventDefault()
          finish(null, true)
        }
      }}
      className="h-8 w-full rounded-lg border border-arc-500 bg-white px-2.5 text-[0.8rem] text-ink-900 outline-none ring-2 ring-arc-500/20 dark:bg-ink-950 dark:text-ink-50"
    />
  )
}

// --- the rest of the window ----------------------------------------------------------------------

/** Models, Image, Network, Monitor — two by two, so the threads above keep the height. */
function NavGrid() {
  const view = useStudio((s) => s.view)
  const downloads = useActiveDownloads()
  return (
    <nav className="grid grid-cols-2 gap-px px-2 pt-1.5" aria-label="Views">
      {NAV.map((item) => (
        <button
          key={item}
          type="button"
          onClick={() => goTo(item)}
          aria-current={view === item ? 'page' : undefined}
          title={`${VIEW_LABELS[item]} — ${NAV_NOTES[item] ?? ''} (${formatShortcut(VIEW_SHORTCUTS[item])})`}
          className={`flex h-7 min-w-0 items-center gap-2 rounded-md px-2 text-[0.75rem] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-arc-500/40 ${
            view === item
              ? 'bg-arc-500/10 font-medium text-arc-700 dark:bg-arc-400/10 dark:text-arc-300'
              : 'text-ink-600 hover:bg-ink-100 hover:text-ink-900 dark:text-ink-300 dark:hover:bg-ink-800 dark:hover:text-ink-50'
          }`}
        >
          <Icon name={VIEW_ICONS[item]} className="size-3.5 shrink-0 opacity-80" />
          <span className="min-w-0 flex-1 truncate text-left">{VIEW_LABELS[item]}</span>
          {item === 'models' && downloads > 0 && (
            <span className="badge bg-arc-600 px-1 py-0 text-[0.6rem] text-white" title={`${downloads} download${downloads === 1 ? '' : 's'} in progress`}>
              {downloads}
            </span>
          )}
        </button>
      ))}
    </nav>
  )
}

function ConnectionStatus({ compact = false }: { compact?: boolean }) {
  const connected = useStudio((s) => s.connected)
  const modelId = useStudio((s) => s.runtime?.model_id ?? null)
  const text = connected ? (modelId ? 'Model loaded' : 'Runtime ready') : 'Runtime unreachable'
  const title = connected ? (modelId ? `Model loaded: ${modelId}` : 'Runtime ready — no model loaded') : 'The MISAKA Runtime is not answering; what the window shows may be stale'
  if (compact) {
    return (
      <span className="flex size-7 items-center justify-center" title={title} role="status" aria-label={text}>
        <StatusDot tone={connected ? 'green' : 'red'} />
      </span>
    )
  }
  return (
    <div className="flex min-w-0 flex-1 items-center gap-2 px-1.5 text-[0.75rem] text-ink-500 dark:text-ink-400" title={title} role="status">
      <StatusDot tone={connected ? 'green' : 'red'} />
      <span className="truncate">{text}</span>
    </div>
  )
}

function ThemeButton() {
  const theme = useStudio((s) => s.settings?.ui.theme ?? 'system')
  const available = useStudio((s) => s.settings !== null)
  const next = nextTheme(theme)
  return (
    <IconButton
      icon={THEME_ICON[theme]}
      label={available ? `Theme: ${THEME_LABEL[theme]} — switch to ${THEME_LABEL[next]}` : 'Theme (the runtime is not reachable)'}
      disabled={!available}
      onClick={() => void setTheme(next)}
    />
  )
}

function SidebarFooter() {
  const view = useStudio((s) => s.view)
  return (
    <div className="flex h-10 items-center gap-0.5 px-2">
      <ConnectionStatus />
      <ThemeButton />
      <IconButton icon="settings" label="Settings" shortcut={SHORTCUTS.settings} active={view === 'settings'} onClick={() => goTo('settings')} />
    </div>
  )
}

/** The collapsed sidebar: every way back in, as icons. */
function SidebarRail() {
  const toggleSidebar = useUi((s) => s.toggleSidebar)
  const openPalette = useUi((s) => s.openPalette)
  const side = useUi((s) => s.appMode)
  const view = useStudio((s) => s.view)
  const downloads = useActiveDownloads()
  const activity = useSideActivity()
  const project = useNewSessionProject()

  return (
    <aside className="flex h-full w-12 shrink-0 flex-col items-center gap-0.5 border-r border-ink-200 bg-white py-2 dark:border-ink-800 dark:bg-ink-900" aria-label="Sidebar">
      <IconButton icon="panelLeft" label="Expand sidebar" shortcut={SHORTCUTS.toggleSidebar} onClick={toggleSidebar} />
      <div className="my-1.5 h-px w-6 bg-ink-200 dark:bg-ink-800" />
      <div className="flex flex-col gap-0.5 rounded-lg bg-ink-100 p-0.5 dark:bg-ink-800" role="group" aria-label="Chat or Code">
        {SIDES.map((mode) => {
          const on = side === mode
          return (
            <span key={mode} className="relative flex">
              <button
                type="button"
                aria-pressed={on}
                aria-label={SIDE_LABELS[mode]}
                title={sideTitle(mode, on ? null : activity[mode])}
                onClick={() => switchAppMode(mode)}
                className={`inline-flex size-7 items-center justify-center rounded-md transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-arc-500/40 ${
                  on
                    ? 'bg-white text-ink-900 shadow-sm shadow-ink-950/5 dark:bg-ink-950 dark:text-ink-50'
                    : 'text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-100'
                }`}
              >
                <Icon name={SIDE_ICONS[mode]} className="size-4" />
              </button>
              {!on && <ActivityDot activity={activity[mode]} className="right-0.5 top-0.5" />}
            </span>
          )
        })}
      </div>
      <div className="my-1.5 h-px w-6 bg-ink-200 dark:bg-ink-800" />
      {side === 'chat' ? (
        <IconButton icon="compose" label="New chat" shortcut={NEW_CHAT_HINT} onClick={newChat} />
      ) : (
        <>
          <IconButton icon="plus" label={project ? `New session in ${folderLabel(project)}` : 'New session'} shortcut={NEW_CHAT_HINT} onClick={newSession} />
          <IconButton icon="folderOpen" label="Open folder…" onClick={openFolder} />
        </>
      )}
      <IconButton icon="search" label="Search" shortcut={SHORTCUTS.palette} onClick={() => openPalette()} />
      <div className="my-1.5 h-px w-6 bg-ink-200 dark:bg-ink-800" />
      <nav className="flex flex-col items-center gap-0.5" aria-label="Views">
        {NAV.map((item) => (
          <span key={item} className="relative">
            <IconButton
              icon={VIEW_ICONS[item]}
              label={VIEW_LABELS[item]}
              shortcut={VIEW_SHORTCUTS[item]}
              className={view === item ? 'bg-arc-500/10 text-arc-700 hover:text-arc-700 dark:bg-arc-400/10 dark:text-arc-300 dark:hover:text-arc-300' : ''}
              onClick={() => goTo(item)}
            />
            {item === 'models' && downloads > 0 && (
              <span className="pointer-events-none absolute right-0.5 top-0.5 size-1.5 rounded-full bg-arc-500" title={`${downloads} downloading`} />
            )}
          </span>
        ))}
      </nav>
      <div className="flex-1" />
      <ConnectionStatus compact />
      <ThemeButton />
      <IconButton icon="settings" label="Settings" shortcut={SHORTCUTS.settings} active={view === 'settings'} onClick={() => goTo('settings')} />
    </aside>
  )
}
