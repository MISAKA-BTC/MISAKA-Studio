// The window's shortcuts, written down once.
//
// The handler (`useGlobalShortcuts`), the help sheet, the palette's hints and the sidebar's key
// caps all read this table, so a key that is changed here is changed everywhere it is shown.

import type { View } from '../../store/studio'
import type { IconName } from '../common'

/**
 * Whether this page is running inside the desktop shell. The shell's initialization script sets
 * the API base before the bundle loads; a browser tab pointed at the runtime never has it.
 *
 * It decides one thing: which new-chat key to show. A browser keeps ⌘N for itself (a new window,
 * before the page ever sees the key), the Tauri webview does not — so both keys work in the shell,
 * and the hint names the one that works where it is read.
 */
export const inDesktopShell: boolean =
  typeof globalThis !== 'undefined' && ('__MISAKA_STUDIO_API__' in globalThis || '__TAURI_INTERNALS__' in globalThis)

export const SHORTCUTS = {
  palette: 'mod+k',
  help: 'mod+/',
  toggleSidebar: 'mod+b',
  settings: 'mod+,',
  /** "New" on the side in view: a chat on Chat's side, a session on Code's. */
  newChat: 'mod+n',
  newChatAlt: 'mod+shift+o',
  /** A new Code session from anywhere. */
  newAgent: 'mod+shift+a',
  chatSide: 'mod+shift+1',
  codeSide: 'mod+shift+2',
  focusComposer: 'mod+l',
  miningPanel: 'mod+shift+m',
  // The Code side's panes, on the keys an editor's hands already know: VS Code's source control
  // (⌃⇧G there), its panel (⌘J), its explorer (⌘⇧E) and its secondary side bar (⌘⌥B). Letters, not
  // the backtick, because the backtick moves on a Japanese keyboard and needs Shift there.
  changesPanel: 'mod+shift+g',
  terminalPanel: 'mod+j',
  filesPanel: 'mod+shift+e',
  sessionPanel: 'mod+alt+b',
  stop: 'escape',
} as const

/** The new-chat key to show: ⌘N where it works, ⌘⇧O in a browser tab. On Code's side the same
 *  key makes a session. */
export const NEW_CHAT_HINT: string = inDesktopShell ? SHORTCUTS.newChat : SHORTCUTS.newChatAlt

export const SIDE_LABELS = { chat: 'Chat', code: 'Code' } as const
export const SIDE_ICONS = { chat: 'chat', code: 'code' } as const satisfies Record<keyof typeof SIDE_LABELS, IconName>
export const SIDE_SHORTCUTS = { chat: SHORTCUTS.chatSide, code: SHORTCUTS.codeSide } as const

export const VIEW_SHORTCUTS: Record<View, string> = {
  chat: 'mod+1',
  models: 'mod+2',
  image: 'mod+3',
  network: 'mod+4',
  monitor: 'mod+5',
  settings: 'mod+6',
}

export const VIEW_LABELS: Record<View, string> = {
  chat: 'Chat',
  models: 'Models',
  image: 'Image',
  network: 'Network',
  monitor: 'Monitor',
  settings: 'Settings',
}

export const VIEW_ICONS: Record<View, IconName> = {
  chat: 'chat',
  models: 'cube',
  image: 'image',
  network: 'globe',
  monitor: 'gauge',
  settings: 'settings',
}

/** In the order of their number keys. */
export const VIEW_ORDER: View[] = ['chat', 'models', 'image', 'network', 'monitor', 'settings']
