// What the Code side's panes share: which side of the window is showing, how the one right-panel
// setting maps onto each side's tabs, and the quiet states a pane falls back to.

import { useEffect, useState, type ReactNode } from 'react'
import { api } from '../../lib/api'
import type { Conversation } from '../../lib/types'
import { useUi, type AppMode, type RightPanel } from '../../store/ui'
import { Icon, type IconName } from '../common'
import { SHORTCUTS } from '../shell/shortcuts'

export type Side = AppMode

/** The side a thread belongs to — its own `mode` when there is one, the window's otherwise. */
export function sideOf(conversation: Conversation | null, appMode: AppMode): Side {
  if (conversation) return conversation.mode === 'agent' ? 'code' : 'chat'
  return appMode
}

export function useSide(conversation: Conversation | null): Side {
  const appMode = useUi((s) => s.appMode)
  return sideOf(conversation, appMode)
}

/**
 * The tab the panel shows for the `rightPanel` setting on this side. There is one setting for the
 * window, so a tab one side does not have becomes its nearest neighbour on the other: Code's
 * summary is Chat's Context, and the Code-only panes fall back to Mining, which both sides have.
 */
export function effectivePanel(panel: RightPanel | null, side: Side): RightPanel | null {
  if (!panel) return null
  if (side === 'code') return panel === 'context' ? 'session' : panel
  if (panel === 'session') return 'context'
  if (panel === 'changes' || panel === 'terminal' || panel === 'files') return 'mining'
  return panel
}

/** Open a pane, or close the panel when that pane is what it shows. */
export function togglePane(panel: RightPanel, side: Side): void {
  const ui = useUi.getState()
  ui.setRightPanel(effectivePanel(ui.rightPanel, side) === panel ? null : panel)
}

/**
 * The tab the panel was just switched to (or opened on), for the one pane that takes the focus when
 * it is opened — the terminal's prompt. Noted by the panel while it renders, taken once by the pane
 * as it mounts: a panel that is merely still open when the window loads does not count.
 */
let shownTab: RightPanel | null = null

export function noteTabShown(tab: RightPanel | null): void {
  shownTab = tab
}

export function consumeTabShown(tab: RightPanel): boolean {
  if (shownTab !== tab) return false
  shownTab = null
  return true
}

// Read loosely: the shell owns the table and adds keys as it binds them, and a pane whose key is not
// bound yet simply shows none.
const KEYS = SHORTCUTS as unknown as Partial<Record<string, string>>

export const PANE_SHORTCUTS: Partial<Record<RightPanel, string>> = {
  session: KEYS.sessionPanel,
  changes: KEYS.changesPanel,
  terminal: KEYS.terminalPanel,
  files: KEYS.filesPanel,
  mining: KEYS.miningPanel,
  context: KEYS.contextPanel,
}

export const PANE_META: Record<RightPanel, { label: string; icon: IconName }> = {
  session: { label: 'Session', icon: 'panelRight' },
  changes: { label: 'Changes', icon: 'diff' },
  terminal: { label: 'Terminal', icon: 'terminal' },
  files: { label: 'Files', icon: 'folder' },
  mining: { label: 'Mining', icon: 'pickaxe' },
  context: { label: 'Context', icon: 'info' },
}

let home: Promise<string | null> | null = null

/** The home directory, for writing paths as `~/…` and for `cd ~` — asked of the runtime once. */
export function useHomeDir(): string | null {
  const [value, setValue] = useState<string | null>(null)
  useEffect(() => {
    let live = true
    home ??= api.workspace
      .browse()
      .then((view) => view.home)
      .catch(() => {
        // Asked again next time: the runtime may simply not have been up yet.
        home = null
        return null
      })
    void home.then((h) => {
      if (live) setValue(h)
    })
    return () => {
      live = false
    }
  }, [])
  return value
}

/** A pane that needs an endpoint this runtime does not have yet: said calmly, not as an error. */
export function NeedsRuntime({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <div className="flex items-start gap-2.5 rounded-xl border border-dashed border-ink-200 px-3 py-2.5 text-xs dark:border-ink-700">
      <Icon name="refresh" className="mt-0.5 size-3.5 shrink-0 text-ink-400" />
      <div className="min-w-0">
        <p className="font-medium text-ink-700 dark:text-ink-200">{title}</p>
        <p className="mt-0.5 leading-relaxed text-ink-500 dark:text-ink-400">
          {children ?? 'This needs the updated runtime. Restart MISAKA Studio once it is installed; everything else here keeps working.'}
        </p>
      </div>
    </div>
  )
}

/** A pane about the session's folder, in a session that has none yet. */
export function NoFolder({ conversation, what }: { conversation: Conversation | null; what: string }) {
  return (
    <div className="flex flex-col items-center gap-3 px-6 py-14 text-center">
      <div className="rounded-xl bg-ink-100 p-3 text-ink-400 dark:bg-ink-800 dark:text-ink-500">
        <Icon name="folder" className="size-5" />
      </div>
      <p className="max-w-64 text-xs leading-relaxed text-ink-500 dark:text-ink-400">
        {conversation ? `Choose the folder this session works in to see ${what}.` : `Start a session in a folder to see ${what}.`}
      </p>
      <button type="button" className="btn-outline px-2.5 py-1 text-xs" onClick={() => useUi.getState().openWorkspacePicker(conversation?.id ?? null)}>
        <Icon name="folderOpen" className="size-3.5" />
        Choose folder…
      </button>
    </div>
  )
}

/** A quiet heading inside a pane. */
export function PaneHeading({ children, aside }: { children: ReactNode; aside?: ReactNode }) {
  return (
    <div className="flex items-center gap-2">
      <h3 className="text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">{children}</h3>
      {aside && <div className="ml-auto flex items-center gap-1">{aside}</div>}
    </div>
  )
}

/** A path split for drawing: the directory quiet, the name plain. */
export function splitPath(path: string): { dir: string; name: string } {
  const at = path.lastIndexOf('/')
  return at === -1 ? { dir: '', name: path } : { dir: path.slice(0, at + 1), name: path.slice(at + 1) }
}

/** Copy text and say so in a toast — for menu items, which close before a button could say "Copied". */
export async function copyText(text: string, what: string, toast: (kind: 'success' | 'error', text: string) => void): Promise<void> {
  try {
    await navigator.clipboard.writeText(text)
    toast('success', `Copied ${what}`)
  } catch {
    toast('error', 'The clipboard is not available here.')
  }
}
