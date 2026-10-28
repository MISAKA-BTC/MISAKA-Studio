// Every shortcut in the window, on one sheet (⌘/).
//
// Drawn from the same table the handler reads, so the sheet cannot advertise a key that does not
// work — except the composer's, the agent's and the sidebar's keys, which their own components
// handle and which are listed here by hand.

import { useEffect, useRef, type ReactNode } from 'react'
import { useUi } from '../../store/ui'
import { Icon } from '../common'
import { Kbd, Modal } from '../ui'
import { useRestoreFocus } from './focus'
import { NEW_CHAT_HINT, SHORTCUTS, VIEW_LABELS, VIEW_ORDER, VIEW_SHORTCUTS, inDesktopShell } from './shortcuts'

type Row = { label: ReactNode; keys: string[]; note?: string }
type Group = { title: string; rows: Row[] }

const GROUPS: Group[] = [
  {
    title: 'General',
    rows: [
      { label: 'Command palette', keys: [SHORTCUTS.palette] },
      { label: 'Keyboard shortcuts', keys: [SHORTCUTS.help] },
      { label: 'Toggle sidebar', keys: [SHORTCUTS.toggleSidebar] },
      { label: 'Settings', keys: [SHORTCUTS.settings] },
      { label: 'Stop generating', keys: [SHORTCUTS.stop], note: 'when no menu or dialog is open' },
    ],
  },
  {
    title: 'Navigation',
    rows: [
      { label: 'Switch to Chat', keys: [SHORTCUTS.chatSide] },
      { label: 'Switch to Code', keys: [SHORTCUTS.codeSide] },
      {
        label: 'New chat or session',
        keys: inDesktopShell ? [SHORTCUTS.newChat, SHORTCUTS.newChatAlt] : [NEW_CHAT_HINT],
        note: 'on this side',
      },
      { label: 'New session', keys: [SHORTCUTS.newAgent], note: 'from anywhere' },
      { label: 'Move between threads', keys: ['arrowup', 'arrowdown'], note: 'in the sidebar' },
      { label: 'Rename a thread', keys: ['f2'], note: 'in the sidebar' },
      { label: 'Focus the composer', keys: [SHORTCUTS.focusComposer] },
      // The thread view is Chat's or Code's, whichever side is in view.
      ...VIEW_ORDER.map((view) => ({
        label: view === 'chat' ? 'Back to the thread' : VIEW_LABELS[view],
        keys: [VIEW_SHORTCUTS[view]],
        ...(view === 'chat' ? { note: 'Chat or Code' } : {}),
      })),
    ],
  },
  {
    title: 'Composer',
    rows: [
      { label: 'Send', keys: ['enter'] },
      { label: 'New line', keys: ['shift+enter'] },
      { label: 'Edit your last message', keys: ['arrowup'], note: 'in an empty composer' },
      { label: 'Commands', keys: ['/'] },
      { label: 'Mention a file', keys: ['@'], note: 'Code sessions' },
      { label: 'Stop generating', keys: [SHORTCUTS.stop] },
    ],
  },
  {
    title: 'Code',
    rows: [
      { label: 'Approve the waiting step', keys: ['enter'], note: 'on the approval card' },
      { label: 'Session summary', keys: [SHORTCUTS.sessionPanel] },
      { label: 'Changes panel', keys: [SHORTCUTS.changesPanel] },
      { label: 'Terminal', keys: [SHORTCUTS.terminalPanel] },
      { label: 'Files', keys: [SHORTCUTS.filesPanel] },
      { label: 'Fold or unfold a project', keys: ['arrowleft', 'arrowright'], note: 'on its name in the sidebar' },
    ],
  },
  {
    title: 'Mining',
    rows: [
      { label: 'Mining panel', keys: [SHORTCUTS.miningPanel] },
      { label: 'Mining commands', keys: [SHORTCUTS.palette], note: 'then type “mine”' },
    ],
  },
]

export function ShortcutsHelp() {
  const open = useUi((s) => s.shortcutsOpen)
  const setOpen = useUi((s) => s.setShortcutsOpen)
  return (
    <Modal open={open} onClose={() => setOpen(false)} position="top" className="max-w-[44rem]" label="Keyboard shortcuts">
      <ShortcutsSheet close={() => setOpen(false)} />
    </Modal>
  )
}

/** Mounted while the sheet is open, so it can take the focus and give it back. */
function ShortcutsSheet({ close }: { close: () => void }) {
  useRestoreFocus()
  const body = useRef<HTMLDivElement>(null)
  useEffect(() => body.current?.focus({ preventScroll: true }), [])
  return (
    <>
      <div className="flex items-center gap-2 border-b border-ink-200 px-4 py-3 dark:border-ink-800">
        <Icon name="keyboard" className="size-4 text-ink-400 dark:text-ink-500" />
        <h2 className="text-sm font-semibold">Keyboard shortcuts</h2>
        <button type="button" className="icon-btn ml-auto" aria-label="Close" title="Close (Esc)" onClick={close}>
          <Icon name="x" className="size-4" />
        </button>
      </div>
      {/* The sheet takes the focus itself — so typing does not land in the composer behind it, and
          the arrow keys scroll it — rather than ringing the close button on every opening. */}
      <div className="grid max-h-[min(34rem,70vh)] gap-x-8 gap-y-5 overflow-y-auto px-5 py-4 outline-none sm:grid-cols-2" tabIndex={-1} ref={body}>
        {GROUPS.map((group) => (
          <section key={group.title} aria-label={group.title}>
            <h3 className="mb-1.5 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">{group.title}</h3>
            <dl className="space-y-0.5">
              {group.rows.map((row, index) => (
                <div key={index} className="flex min-h-7 items-center gap-3 text-[0.8rem]">
                  <dt className="min-w-0 flex-1 text-ink-700 dark:text-ink-200">
                    {row.label}
                    {row.note && <span className="ml-1.5 text-[0.7rem] text-ink-400 dark:text-ink-500">{row.note}</span>}
                  </dt>
                  <dd className="flex shrink-0 items-center gap-1.5">
                    {row.keys.map((key, i) => (
                      <span key={key} className="flex items-center gap-1.5">
                        {i > 0 && <span className="text-[0.65rem] text-ink-400 dark:text-ink-500">or</span>}
                        <Kbd shortcut={key} />
                      </span>
                    ))}
                  </dd>
                </div>
              ))}
            </dl>
          </section>
        ))}
      </div>
    </>
  )
}
