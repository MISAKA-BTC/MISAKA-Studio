// The window's keyboard shortcuts.
//
// One listener, on `window`, in the bubble phase — after everything closer to the focus has had
// its say. So any control that gives a key its own meaning (a rename field's Escape, a menu's
// arrows, the composer's Enter) claims it with `preventDefault()` and this handler leaves it
// alone; and an open popover or dialog has already taken its Escape in the capture phase
// (`useLayer`), so an Escape that closed something never reaches here.

import { useEffect } from 'react'
import { isMac, matches } from '../../lib/keys'
import { useUi } from '../../store/ui'
import { hasOpenLayer } from '../ui'
import { focusComposer, goTo, isBusy, newSession, newThreadHere, stopGenerating, switchAppMode, toggleSidePanel } from './actions'
import { SHORTCUTS, VIEW_ORDER, VIEW_SHORTCUTS } from './shortcuts'

type Binding = { keys: string[]; run: () => void }

const BINDINGS: Binding[] = [
  {
    keys: [SHORTCUTS.palette],
    run: () => {
      const ui = useUi.getState()
      if (ui.paletteOpen) ui.setPaletteOpen(false)
      else {
        if (ui.shortcutsOpen) ui.setShortcutsOpen(false)
        ui.openPalette()
      }
    },
  },
  {
    keys: [SHORTCUTS.help],
    run: () => {
      const ui = useUi.getState()
      if (ui.paletteOpen) ui.setPaletteOpen(false)
      ui.setShortcutsOpen(!ui.shortcutsOpen)
    },
  },
  { keys: [SHORTCUTS.newChat, SHORTCUTS.newChatAlt], run: newThreadHere },
  { keys: [SHORTCUTS.newAgent], run: newSession },
  { keys: [SHORTCUTS.chatSide], run: () => switchAppMode('chat') },
  { keys: [SHORTCUTS.codeSide], run: () => switchAppMode('code') },
  { keys: [SHORTCUTS.toggleSidebar], run: () => useUi.getState().toggleSidebar() },
  { keys: [SHORTCUTS.settings], run: () => goTo('settings') },
  ...VIEW_ORDER.map((view) => ({ keys: [VIEW_SHORTCUTS[view]], run: () => goTo(view) })),
  { keys: [SHORTCUTS.focusComposer], run: focusComposer },
  { keys: [SHORTCUTS.miningPanel], run: () => toggleSidePanel('mining') },
  { keys: [SHORTCUTS.changesPanel], run: () => toggleSidePanel('changes') },
  { keys: [SHORTCUTS.terminalPanel], run: () => toggleSidePanel('terminal') },
  { keys: [SHORTCUTS.filesPanel], run: () => toggleSidePanel('files') },
  { keys: [SHORTCUTS.sessionPanel], run: () => toggleSidePanel('session') },
]

function onKeyDown(event: KeyboardEvent) {
  if (event.defaultPrevented) return
  // A key pressed while an input method is composing belongs to the composition: Escape there
  // cancels the candidate, it must not also stop the reply someone is typing next to.
  if (event.isComposing || event.keyCode === 229) return

  // Escape is the one key here without a modifier. It is honoured inside text fields too — it
  // types nothing, and the composer is exactly where the focus is while a reply streams — but only
  // when no layer is open and only when there is something to stop; otherwise it is left alone.
  if (matches(event, SHORTCUTS.stop)) {
    if (hasOpenLayer() || !isBusy()) return
    event.preventDefault()
    stopGenerating()
    return
  }

  // Everything else needs the platform's command key, so a mod-less key never fires anywhere —
  // in a text field or out of one.
  if (!(isMac ? event.metaKey || event.ctrlKey : event.ctrlKey)) return
  const binding = BINDINGS.find((b) => b.keys.some((key) => matches(event, key)))
  if (!binding) return
  event.preventDefault()
  // A held key repeats; a toggle that flickers on every repeat is not a shortcut anyone wants.
  if (event.repeat) return
  binding.run()
}

/** Install the window's shortcuts. Mounted once, by `App`. */
export function useGlobalShortcuts(): void {
  useEffect(() => {
    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [])
}
