// Keyboard shortcuts, spelled once.
//
// A shortcut is written as `mod+k`, `mod+shift+o`, `escape`, `alt+arrowup`: `mod` is ⌘ on a Mac
// and Ctrl everywhere else, which is the one difference that matters between the platforms the
// Studio ships on. The same string drives the handler and the hint shown beside the action, so the
// key a menu says is the key that works.

// Case-blind: Chromium's `userAgentData.platform` says "macOS", the older `navigator.platform`
// says "MacIntel" — a case-sensitive test read every Chrome on a Mac as a PC, with Ctrl everywhere.
export const isMac: boolean =
  typeof navigator !== 'undefined' &&
  /mac|iphone|ipad|ipod/i.test((navigator as { userAgentData?: { platform?: string } }).userAgentData?.platform || navigator.platform || '')

type Parsed = { mod: boolean; shift: boolean; alt: boolean; ctrl: boolean; key: string }

function parse(shortcut: string): Parsed {
  const parts = shortcut.toLowerCase().split('+')
  const key = parts.pop() ?? ''
  return {
    mod: parts.includes('mod'),
    shift: parts.includes('shift'),
    alt: parts.includes('alt'),
    // A literal Ctrl, distinct from `mod`, for the rare binding that is Ctrl on every platform.
    ctrl: parts.includes('ctrl'),
    key,
  }
}

/** The key an event names, normalised: letters lower-case, `Escape` → `escape`, `,` stays `,`. */
function eventKey(event: KeyboardEvent): string {
  // `event.key` under Alt on a Mac is the composed character (⌥K → ˚), so letters and digits are
  // read from the physical key instead.
  if (/^Key[A-Z]$/.test(event.code)) return event.code.slice(3).toLowerCase()
  if (/^Digit[0-9]$/.test(event.code)) return event.code.slice(5)
  return event.key.toLowerCase()
}

/** Whether `event` is `shortcut` — exactly: `mod+k` does not fire on `mod+shift+k`. */
export function matches(event: KeyboardEvent, shortcut: string): boolean {
  const want = parse(shortcut)
  // On a Mac `mod` is ⌘ and `ctrl` the separate Control key. Elsewhere both are the one Ctrl key,
  // so a binding written with `ctrl` wants Ctrl there too — it used to want a Ctrl that a PC can
  // never report apart from `mod`, and never fired.
  const wantMeta = isMac ? want.mod : false
  const wantCtrl = isMac ? want.ctrl : want.mod || want.ctrl
  if (event.metaKey !== wantMeta || event.ctrlKey !== wantCtrl) return false
  if (want.shift !== event.shiftKey || want.alt !== event.altKey) return false
  const key = eventKey(event)
  return key === want.key || (want.key === '/' && key === '?' && want.shift)
}

const MAC_SYMBOL: Record<string, string> = {
  mod: '⌘',
  shift: '⇧',
  alt: '⌥',
  ctrl: '⌃',
  enter: '↩',
  escape: 'Esc',
  backspace: '⌫',
  arrowup: '↑',
  arrowdown: '↓',
  arrowleft: '←',
  arrowright: '→',
  tab: '⇥',
}
const PC_NAME: Record<string, string> = {
  mod: 'Ctrl',
  shift: 'Shift',
  alt: 'Alt',
  ctrl: 'Ctrl',
  enter: 'Enter',
  escape: 'Esc',
  backspace: 'Backspace',
  arrowup: '↑',
  arrowdown: '↓',
  arrowleft: '←',
  arrowright: '→',
  tab: 'Tab',
}

/** The keys of a shortcut as the platform writes them: `['⌘', 'K']` or `['Ctrl', 'K']`. */
export function shortcutKeys(shortcut: string): string[] {
  const table = isMac ? MAC_SYMBOL : PC_NAME
  return shortcut.split('+').map((part) => {
    const lower = part.toLowerCase()
    return table[lower] ?? (lower.length === 1 ? lower.toUpperCase() : part.charAt(0).toUpperCase() + part.slice(1))
  })
}

/** A shortcut as one string for a `title` attribute: `⌘K`, `Ctrl+K`. */
export function formatShortcut(shortcut: string): string {
  return shortcutKeys(shortcut).join(isMac ? '' : '+')
}

/** Whether the event started in a place that types text, where single-key shortcuts must not fire. */
export function isTypingTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  return target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)
}
