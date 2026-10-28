// What the window's chrome does, in one place.
//
// The sidebar, the command palette, the status bar and the keyboard all offer "new chat", "stop",
// "switch mining mode". Each of those is written once, here, so the four entry points cannot drift
// into four slightly different meanings of the same command.

import { api } from '../../lib/api'
import { stopAgentRun, useAgent } from '../../lib/agent/run'
import type { Conversation, MiningMode, Settings } from '../../lib/types'
import { useStudio, type View } from '../../store/studio'
import { useUi, type AppMode, type RightPanel } from '../../store/ui'
import { downloadMarkdown } from '../chat/exportMarkdown'
import { sideOf as paneSide, togglePane } from '../code/shared'
import { refreshMiningState } from './mining'
import { projectForNewSession as pickProject, restoreTarget, sideOf } from './sides'
import { lastAnswer } from './threads'

export type Theme = Settings['ui']['theme']

/** Close the chrome's own dialogs before a command takes the window somewhere else. */
function closeChromeLayers() {
  const ui = useUi.getState()
  if (ui.paletteOpen) ui.setPaletteOpen(false)
  if (ui.shortcutsOpen) ui.setShortcutsOpen(false)
}

export function goTo(view: View) {
  closeChromeLayers()
  useStudio.getState().setView(view)
}

/**
 * Switch the window to Chat or to Code, and to the thread that side had open — else its most
 * recent, else its empty page — with the composer ready.
 *
 * The thread is chosen here, not left to the side sync (`sideSync.ts`): the sync only ever follows
 * the thread in view to its side, and a switch is the one move that starts from the side.
 * Switching to the side already in view is how the switch takes someone back from Models or
 * Network to their thread.
 */
export function switchAppMode(mode: AppMode) {
  closeChromeLayers()
  const ui = useUi.getState()
  const studio = useStudio.getState()
  const current = activeConversation()
  if (current && sideOf(current) === ui.appMode && ui.lastThread[ui.appMode] !== current.id) ui.rememberThread(ui.appMode, current.id)
  if (ui.appMode !== mode) {
    ui.setAppMode(mode)
    const target = restoreTarget(studio.conversations, useUi.getState().lastThread[mode], mode)
    if (target !== studio.activeConversationId) useStudio.setState({ activeConversationId: target })
  }
  studio.setView('chat')
  useUi.getState().focusComposer()
}

export function newChat() {
  closeChromeLayers()
  const studio = useStudio.getState()
  studio.newConversation({ mode: 'chat' })
  studio.setView('chat')
  useUi.getState().focusComposer()
}

/** The folder a new session starts in, as things stand (see `sides.projectForNewSession`). */
export function projectForNewSession(): string | null {
  const { conversations, activeConversationId } = useStudio.getState()
  const ui = useUi.getState()
  const find = (id: string | null) => (id ? conversations.find((c) => c.id === id) : undefined)
  return pickProject({ active: find(activeConversationId), last: find(ui.lastThread.code), recent: ui.recentWorkspaces })
}

/**
 * A new Code session in `path`. An untouched empty session is reused rather than stacked (the
 * store's rule for every new thread), and moved to this folder; the folder becomes the one used
 * last, which is where the next "New session" goes when nothing else says.
 */
export function newSessionIn(path: string) {
  closeChromeLayers()
  const studio = useStudio.getState()
  studio.newConversation({ mode: 'agent', workspace: path })
  useUi.getState().addRecentWorkspace(path)
  studio.setView('chat')
  useUi.getState().focusComposer()
}

/**
 * The folder picker, for a new session in the folder chosen. Nothing is made until a folder is: a
 * cancelled picker leaves no folderless session behind. (The picker reuses an untouched session
 * the same way `newSessionIn` does.)
 */
export function openFolder() {
  closeChromeLayers()
  // The picker makes the session but leaves the view alone; the session is where to be.
  useStudio.getState().setView('chat')
  useUi.getState().openWorkspacePicker(null)
}

/** A new session in the project of the session in view or the last one, else the folder used
 *  last; with none of those, the folder picker. */
export function newSession() {
  const project = projectForNewSession()
  if (project) newSessionIn(project)
  else openFolder()
}

/** "New" on the side in view: a chat on Chat's side, a session on Code's — ⌘N, `/new`, the rail. */
export function newThreadHere() {
  if (useUi.getState().appMode === 'code') newSession()
  else newChat()
}

/** @deprecated The Code side's own name for it is `newSession`; kept for callers of the old one. */
export const newAgentThread = newSession
/** @deprecated `openFolder`, under its old name. */
export const openFolderForAgent = openFolder

export function openThread(id: string) {
  closeChromeLayers()
  const studio = useStudio.getState()
  studio.selectConversation(id)
  studio.setView('chat')
}

export function focusComposer() {
  closeChromeLayers()
  useStudio.getState().setView('chat')
  useUi.getState().focusComposer()
}

/**
 * Toggle a panel beside the chat. From another view the panel is opened rather than toggled: the
 * shortcut is pressed to see the panel, and switching to the chat only to close it would read as
 * the key doing nothing.
 */
export function toggleSidePanel(panel: RightPanel) {
  closeChromeLayers()
  const studio = useStudio.getState()
  const ui = useUi.getState()
  if (studio.view !== 'chat') {
    studio.setView('chat')
    ui.setRightPanel(panel)
    return
  }
  // The header's rule: a pane's key closes the panel when that pane is what it shows on this side
  // (Code's Session is Chat's Context), and opens it otherwise.
  const active = studio.conversations.find((c) => c.id === studio.activeConversationId) ?? null
  togglePane(panel, paneSide(active, ui.appMode))
}

export function openSidePanel(panel: RightPanel) {
  closeChromeLayers()
  useStudio.getState().setView('chat')
  useUi.getState().setRightPanel(panel)
}

/** A reply is streaming or the agent is running — something Esc would stop. */
export function isBusy(): boolean {
  return useStudio.getState().streaming !== null || useAgent.getState().run !== null
}

export function stopGenerating() {
  useStudio.getState().stop()
  stopAgentRun()
}

export function activeConversation(): Conversation | null {
  const { conversations, activeConversationId } = useStudio.getState()
  return conversations.find((c) => c.id === activeConversationId) ?? null
}

/** Delete a thread. When it was the one in view, the side sync opens the newest thread left on the
 *  same side (or its empty page) — never one from the other side. */
export function deleteThread(id: string) {
  const studio = useStudio.getState()
  // A reply still streaming into a thread that no longer exists would have nowhere to land.
  if (studio.streaming?.conversationId === id) studio.stop()
  if (useAgent.getState().run?.conversationId === id) stopAgentRun()
  studio.deleteConversation(id)
}

/** Download the thread as Markdown — the chat header's export, so every way of exporting a
 *  thread writes the same file. */
export function exportThread(conversation: Conversation) {
  useStudio.getState().toast('success', `Exported ${downloadMarkdown(conversation)}`)
}

export async function copyLastAnswer(conversation: Conversation | null) {
  const text = lastAnswer(conversation)
  const toast = useStudio.getState().toast
  if (!text) {
    toast('info', 'There is no answer in this thread to copy yet.')
    return
  }
  try {
    await navigator.clipboard.writeText(text)
    toast('success', 'Copied the last answer')
  } catch {
    toast('error', 'The clipboard is not available here.')
  }
}

/**
 * Switch the theme. It changes at once in the window, then the runtime is told.
 *
 * The save writes the theme over the runtime's CURRENT settings rather than the window's copy of
 * them. That copy can be behind — the mining mode, for one, is switched through its own endpoint —
 * and putting a stale mode back would switch the chat's engine, which unloads the model, all for a
 * click on a sun icon.
 */
export async function setTheme(theme: Theme) {
  const { settings, toast } = useStudio.getState()
  if (!settings) {
    toast('error', 'The runtime is not reachable, so the theme cannot be saved.')
    return
  }
  const previous = settings.ui.theme
  if (previous === theme) return
  useStudio.setState({ settings: { ...settings, ui: { ...settings.ui, theme } } })
  try {
    const current = await api.settings()
    const saved = await api.saveSettings({ ...current, ui: { ...current.ui, theme } })
    useStudio.setState({ settings: saved })
  } catch (error) {
    useStudio.setState((s) => (s.settings && s.settings.ui.theme === theme ? { settings: { ...s.settings, ui: { ...s.settings.ui, theme: previous } } } : {}))
    toast('error', `The theme was not saved: ${(error as Error).message}`)
  }
}

export const THEME_ORDER: Theme[] = ['system', 'light', 'dark']

export function nextTheme(theme: Theme | undefined): Theme {
  const index = THEME_ORDER.indexOf(theme ?? 'system')
  return THEME_ORDER[(index + 1) % THEME_ORDER.length] ?? 'system'
}

let switchingMode = false

/**
 * Switch the mining mode. The runtime moves the chat to the engine the new mode needs (and loads
 * the model back into it), so the engine, the settings and both mining lights are read again
 * afterwards — every one of them can have changed.
 */
export async function setMiningMode(mode: MiningMode): Promise<boolean> {
  const studio = useStudio.getState()
  if (switchingMode) return false
  if (studio.streaming) {
    studio.toast('info', 'Wait for the reply to finish: switching the mining mode changes the engine that is answering it.')
    return false
  }
  switchingMode = true
  try {
    await api.miningMode(mode)
    const [settings] = await Promise.all([api.settings().catch(() => null), studio.refreshRuntime(), refreshMiningState()])
    if (settings) useStudio.setState({ settings })
    studio.toast(
      'success',
      mode === 'background'
        ? 'Background mining: the chat answers here, and every prompt is queued for the gateway'
        : 'Inline mining: the chat waits for the gateway, and its answer is the mined answer',
    )
    return true
  } catch (error) {
    studio.toast('error', `The mining mode did not change: ${(error as Error).message}`)
    return false
  } finally {
    switchingMode = false
  }
}

/** Start or stop the node-free prompt stack (gateway + rail). */
export async function setPromptStack(run: boolean): Promise<void> {
  const toast = useStudio.getState().toast
  try {
    if (run) {
      await api.startPromptStack()
      toast('success', 'Prompt mining started: the gateway and the rail are running.')
    } else {
      await api.stopPromptStack()
      toast('info', 'Prompt mining stopped. Jobs already committed stay in the outbox for the next start.')
    }
  } catch (error) {
    toast('error', `${run ? 'Prompt mining did not start' : 'Prompt mining did not stop'}: ${(error as Error).message}`)
  } finally {
    await refreshMiningState()
  }
}
