// The window's own state: what is open, what is collapsed, what was used recently.
//
// Kept apart from `studio.ts` on purpose. That store mirrors the runtime and the conversations;
// this one is chrome — a sidebar that is collapsed, a palette that is open — and none of it means
// anything to the runtime or survives being wrong for a second. Persisted separately, so a schema
// change here can never cost anyone their conversations.

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import type { Attachment } from '../lib/types'

/** What the panel on the right of the chat shows, when it shows anything. `session`, `terminal`
 *  and `files` belong to Code; the rest are shared. */
export type RightPanel = 'session' | 'changes' | 'terminal' | 'files' | 'mining' | 'context'

/**
 * The window's two sides, as in Claude's desktop app: **Chat** — conversations with the loaded
 * model — and **Code** — agent sessions working in a project folder. A thread belongs to the side
 * its `mode` says (`agent` → Code), and the window shows one side at a time.
 */
export type AppMode = 'chat' | 'code'

type UiState = {
  appMode: AppMode
  /** The thread each side had open, so switching sides comes back to where it was. */
  lastThread: Record<AppMode, string | null>
  /** Project folders collapsed in Code's sidebar. */
  collapsedProjects: string[]
  sidebarOpen: boolean
  paletteOpen: boolean
  /** What the palette's input starts with when it opens — "load model " from the status bar's
   *  model name, say. The palette takes it once, on opening, and clears it. */
  paletteQuery: string
  shortcutsOpen: boolean
  /** The folder picker for the coding agent; the conversation it will set, when one was named. */
  workspacePicker: { conversationId: string | null } | null
  rightPanel: RightPanel | null
  parametersOpen: boolean
  /** Bumped to ask the composer to take focus — from the palette, a shortcut, a new chat. */
  composerFocusTick: number
  /** Text the composer should load as a draft (a slash command's argument, a suggestion chip, the
   *  words of an agent run that could not start — with the files that were attached to them). */
  composerDraft: { text: string; attachments?: Attachment[]; tick: number } | null
  /** Whether thinking models may think before answering. Off sends `misaka.thinking: false`. */
  thinking: boolean
  /** Folders the agent was pointed at, most recent first. */
  recentWorkspaces: string[]

  /** Only the flag. Switching sides — remembering and restoring each side's thread — is
   *  `switchAppMode` in `components/shell/actions.ts`. */
  setAppMode: (mode: AppMode) => void
  rememberThread: (mode: AppMode, id: string | null) => void
  toggleProject: (path: string) => void
  setSidebarOpen: (open: boolean) => void
  toggleSidebar: () => void
  setPaletteOpen: (open: boolean) => void
  /** Open the palette, optionally with its input filled in. */
  openPalette: (query?: string) => void
  setShortcutsOpen: (open: boolean) => void
  openWorkspacePicker: (conversationId?: string | null) => void
  closeWorkspacePicker: () => void
  setRightPanel: (panel: RightPanel | null) => void
  toggleRightPanel: (panel: RightPanel) => void
  setParametersOpen: (open: boolean) => void
  focusComposer: () => void
  setComposerDraft: (text: string, attachments?: Attachment[]) => void
  setThinking: (thinking: boolean) => void
  addRecentWorkspace: (path: string) => void
  removeRecentWorkspace: (path: string) => void
}

export const useUi = create<UiState>()(
  persist(
    (set) => ({
      appMode: 'chat',
      lastThread: { chat: null, code: null },
      collapsedProjects: [],
      sidebarOpen: true,
      paletteOpen: false,
      paletteQuery: '',
      shortcutsOpen: false,
      workspacePicker: null,
      rightPanel: null,
      parametersOpen: false,
      composerFocusTick: 0,
      composerDraft: null,
      thinking: true,
      recentWorkspaces: [],

      setAppMode: (appMode) => set({ appMode }),
      rememberThread: (mode, id) => set((s) => (s.lastThread[mode] === id ? {} : { lastThread: { ...s.lastThread, [mode]: id } })),
      toggleProject: (path) =>
        set((s) => ({
          collapsedProjects: s.collapsedProjects.includes(path) ? s.collapsedProjects.filter((p) => p !== path) : [...s.collapsedProjects, path],
        })),
      setSidebarOpen: (sidebarOpen) => set({ sidebarOpen }),
      toggleSidebar: () => set((s) => ({ sidebarOpen: !s.sidebarOpen })),
      setPaletteOpen: (paletteOpen) => set({ paletteOpen }),
      openPalette: (query = '') => set({ paletteOpen: true, paletteQuery: query }),
      setShortcutsOpen: (shortcutsOpen) => set({ shortcutsOpen }),
      openWorkspacePicker: (conversationId = null) => set({ workspacePicker: { conversationId }, paletteOpen: false }),
      closeWorkspacePicker: () => set({ workspacePicker: null }),
      setRightPanel: (rightPanel) => set({ rightPanel }),
      toggleRightPanel: (panel) => set((s) => ({ rightPanel: s.rightPanel === panel ? null : panel })),
      setParametersOpen: (parametersOpen) => set({ parametersOpen }),
      focusComposer: () => set((s) => ({ composerFocusTick: s.composerFocusTick + 1 })),
      setComposerDraft: (text, attachments) =>
        set((s) => ({ composerDraft: { text, ...(attachments?.length ? { attachments } : {}), tick: (s.composerDraft?.tick ?? 0) + 1 } })),
      setThinking: (thinking) => set({ thinking }),
      addRecentWorkspace: (path) => set((s) => ({ recentWorkspaces: [path, ...s.recentWorkspaces.filter((p) => p !== path)].slice(0, 8) })),
      removeRecentWorkspace: (path) => set((s) => ({ recentWorkspaces: s.recentWorkspaces.filter((p) => p !== path) })),
    }),
    {
      name: 'misaka-studio.ui',
      partialize: (state) => ({
        appMode: state.appMode,
        lastThread: state.lastThread,
        collapsedProjects: state.collapsedProjects,
        sidebarOpen: state.sidebarOpen,
        rightPanel: state.rightPanel,
        thinking: state.thinking,
        recentWorkspaces: state.recentWorkspaces,
      }),
      version: 1,
    },
  ),
)
