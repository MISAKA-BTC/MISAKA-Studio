// The Code side's own chrome state: how wide its panel is, which summary cards are folded, what was
// typed in each folder's terminal — and the few one-off signals between its panes.
//
// Kept apart from `store/ui.ts` (the shell's) for the same reason that one is kept apart from the
// conversations: none of this is worth a schema change anywhere else, and losing it costs nothing
// but a panel at its default width.

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import { pushHistory } from '../../lib/terminal'

export const PANEL_DEFAULT_WIDTH = 420
export const PANEL_MIN_WIDTH = 320
export const PANEL_MAX_WIDTH = 960
/** Folders whose terminal history is kept; the least recently used goes first. */
const HISTORY_FOLDERS = 24

type CodeUiState = {
  panelWidth: number
  /** Session-panel cards folded shut. */
  collapsed: string[]
  /** Whether a Code session has ever been opened in this window's storage — the first one opens
   *  the Session panel, so the summary is found; after that the panel stays as it was left. */
  introduced: boolean
  /** Terminal history per folder, oldest first. */
  history: Record<string, string[]>
  /** Bumped when the Session panel's "Revert all…" sends someone to the agent's edits; the Changes
   *  tab scrolls to them when it sees a new value. Not persisted. */
  agentEditsTick: number

  setPanelWidth: (width: number) => void
  toggleCard: (id: string) => void
  markIntroduced: () => void
  remember: (root: string, command: string) => void
  showAgentEdits: () => void
}

export function clampPanelWidth(width: number): number {
  return Math.round(Math.min(PANEL_MAX_WIDTH, Math.max(PANEL_MIN_WIDTH, width)))
}

export const useCodeUi = create<CodeUiState>()(
  persist(
    (set) => ({
      panelWidth: PANEL_DEFAULT_WIDTH,
      collapsed: [],
      introduced: false,
      history: {},
      agentEditsTick: 0,

      setPanelWidth: (width) => set({ panelWidth: clampPanelWidth(width) }),
      toggleCard: (id) => set((s) => ({ collapsed: s.collapsed.includes(id) ? s.collapsed.filter((c) => c !== id) : [...s.collapsed, id] })),
      markIntroduced: () => set({ introduced: true }),
      remember: (root, command) =>
        set((s) => {
          const next = pushHistory(s.history[root] ?? [], command)
          // Re-inserted, so the object's key order is the order folders were last used in.
          const rest = { ...s.history }
          delete rest[root]
          const history: Record<string, string[]> = { ...rest, [root]: next }
          const roots = Object.keys(history)
          for (const old of roots.slice(0, Math.max(0, roots.length - HISTORY_FOLDERS))) delete history[old]
          return { history }
        }),
      showAgentEdits: () => set((s) => ({ agentEditsTick: s.agentEditsTick + 1 })),
    }),
    {
      name: 'misaka-studio.code',
      partialize: (s) => ({ panelWidth: s.panelWidth, collapsed: s.collapsed, introduced: s.introduced, history: s.history }),
      version: 1,
    },
  ),
)

const NO_HISTORY: string[] = []

export function useTerminalHistory(root: string | null): string[] {
  return useCodeUi((s) => (root ? (s.history[root] ?? NO_HISTORY) : NO_HISTORY))
}
