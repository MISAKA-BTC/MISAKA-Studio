// The window's interaction primitives: popovers, menus, modals, key hints, segmented controls.
//
// One implementation of each, because the Cursor-style shell lives on them — the model picker,
// the mining chip, the conversation menu and the command palette are all a popover or a modal —
// and five hand-rolled click-outside handlers are five different answers to "does Esc close this
// or stop the generation". That question has one answer here, in `useLayer`.
//
// **Escape.** Every open popover and modal is a layer. A single keydown listener on `document`,
// in the capture phase, closes only the topmost layer and stops the event there, so the window's
// own shortcuts — which listen on `window`, in the bubble phase — never see an Escape that closed
// something. When nothing is open the Escape reaches them, and stops a running generation.

import { useEffect, useLayoutEffect, useRef, useState, type ReactNode, type RefObject } from 'react'
import { createPortal } from 'react-dom'
import { shortcutKeys } from '../lib/keys'
import { Icon, type IconName } from './common'

// --- layers -----------------------------------------------------------------

const layers: { id: number; close: () => void }[] = []
let nextLayer = 1
let listening = false

function onLayerKey(event: KeyboardEvent) {
  // An Escape while an IME is composing cancels the composition — Japanese input uses it all the
  // time — and must reach the IME, not close the menu the person is typing into.
  if (event.key !== 'Escape' || layers.length === 0 || event.isComposing || event.keyCode === 229) return
  event.preventDefault()
  event.stopPropagation()
  layers[layers.length - 1]?.close()
}

/** Register an open popover/modal as a layer that Escape closes. */
export function useLayer(open: boolean, close: () => void) {
  const closeRef = useRef(close)
  closeRef.current = close
  useEffect(() => {
    if (!open) return
    if (!listening) {
      document.addEventListener('keydown', onLayerKey, true)
      listening = true
    }
    const layer = { id: nextLayer++, close: () => closeRef.current() }
    layers.push(layer)
    return () => {
      const index = layers.findIndex((l) => l.id === layer.id)
      if (index >= 0) layers.splice(index, 1)
    }
  }, [open])
}

/** Whether any popover or modal is open — for shortcuts that must not fire under one. */
export function hasOpenLayer(): boolean {
  return layers.length > 0
}

// --- key hints ----------------------------------------------------------------

/** A shortcut drawn as keycaps: `<Kbd shortcut="mod+k" />` → ⌘ K on a Mac, Ctrl K elsewhere. */
export function Kbd({ shortcut, className = '' }: { shortcut: string; className?: string }) {
  return (
    <span className={`inline-flex items-center gap-0.5 ${className}`}>
      {shortcutKeys(shortcut).map((key, index) => (
        <kbd key={index} className="kbd">
          {key}
        </kbd>
      ))}
    </span>
  )
}

// --- popover --------------------------------------------------------------------

export type Placement = 'bottom-start' | 'bottom-end' | 'top-start' | 'top-end'

/**
 * A floating panel anchored to an element, drawn in a portal so no `overflow: hidden` ancestor can
 * clip it. Positioned against the viewport and flipped to the other side when it would not fit.
 * Closes on Escape, on a click outside both it and its anchor, and when the window resizes.
 */
export function Popover({
  anchor,
  open,
  onClose,
  placement = 'bottom-start',
  offset = 6,
  className = '',
  children,
}: {
  anchor: RefObject<HTMLElement | null>
  open: boolean
  onClose: () => void
  placement?: Placement
  offset?: number
  className?: string
  children: ReactNode
}) {
  const panel = useRef<HTMLDivElement>(null)
  const [position, setPosition] = useState<{ top: number; left: number; maxHeight: number } | null>(null)
  useLayer(open, onClose)

  useLayoutEffect(() => {
    if (!open) {
      setPosition(null)
      return
    }
    const place = () => {
      const a = anchor.current?.getBoundingClientRect()
      const p = panel.current
      if (!a || !p) return
      const width = p.offsetWidth
      const height = p.offsetHeight
      const margin = 8
      const below = window.innerHeight - a.bottom - offset - margin
      const above = a.top - offset - margin
      const wantsTop = placement.startsWith('top')
      const top = (wantsTop ? above >= height || above > below : below < height && above > below)
        ? Math.max(margin, a.top - offset - Math.min(height, above))
        : a.bottom + offset
      const maxHeight = Math.max(160, top < a.top ? a.top - offset - margin : below)
      let left = placement.endsWith('end') ? a.right - width : a.left
      left = Math.min(Math.max(margin, left), window.innerWidth - width - margin)
      setPosition({ top, left, maxHeight })
    }
    place()
    const observer = new ResizeObserver(place)
    if (panel.current) observer.observe(panel.current)
    window.addEventListener('resize', place)
    window.addEventListener('scroll', place, true)
    return () => {
      observer.disconnect()
      window.removeEventListener('resize', place)
      window.removeEventListener('scroll', place, true)
    }
  }, [open, anchor, placement, offset])

  useEffect(() => {
    if (!open) return
    const onDown = (event: MouseEvent) => {
      const target = event.target as Node
      if (panel.current?.contains(target) || anchor.current?.contains(target)) return
      onClose()
    }
    document.addEventListener('mousedown', onDown)
    return () => document.removeEventListener('mousedown', onDown)
  }, [open, anchor, onClose])

  if (!open) return null
  return createPortal(
    <div
      ref={panel}
      className={`menu pop-in fixed z-50 overflow-y-auto ${className}`}
      style={position ? { top: position.top, left: position.left, maxHeight: position.maxHeight } : { top: -9999, left: -9999 }}
    >
      {children}
    </div>,
    document.body,
  )
}

/** A button that opens a popover — the common case, with the anchor and open state wired up. */
export function PopoverButton({
  button,
  className,
  title,
  label,
  placement,
  panelClassName,
  children,
}: {
  button: ReactNode
  className?: string
  title?: string
  /** The accessible name, for a trigger that is only an icon. */
  label?: string
  placement?: Placement
  panelClassName?: string
  /** The panel's content; given `close` so an item can close the popover it lives in. */
  children: (close: () => void) => ReactNode
}) {
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const close = () => setOpen(false)
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={className}
        title={title}
        aria-label={label}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
      >
        {button}
      </button>
      <Popover anchor={anchor} open={open} onClose={close} placement={placement} className={panelClassName}>
        {children(close)}
      </Popover>
    </>
  )
}

// --- menus ------------------------------------------------------------------------

export function MenuItem({
  icon,
  label,
  hint,
  shortcut,
  danger = false,
  checked,
  disabled = false,
  active = false,
  onSelect,
}: {
  icon?: IconName
  label: ReactNode
  /** Secondary text on the right, e.g. a model's size. */
  hint?: ReactNode
  shortcut?: string
  danger?: boolean
  /** A check mark column: `true` draws it, `false` reserves its space. */
  checked?: boolean
  disabled?: boolean
  /** Keyboard highlight, for menus driven by arrow keys. */
  active?: boolean
  onSelect: () => void
}) {
  return (
    <button
      type="button"
      className={`menu-item ${danger ? 'text-red-600 dark:text-red-400' : ''}`}
      data-active={active}
      disabled={disabled}
      onClick={onSelect}
    >
      {checked !== undefined && <Icon name="check" className={`size-3.5 shrink-0 ${checked ? '' : 'invisible'}`} />}
      {icon && <Icon name={icon} className="size-4 shrink-0 opacity-80" />}
      <span className="min-w-0 flex-1 truncate">{label}</span>
      {hint !== undefined && <span className="shrink-0 text-[0.7rem] text-ink-400 dark:text-ink-500">{hint}</span>}
      {shortcut && <Kbd shortcut={shortcut} className="shrink-0" />}
    </button>
  )
}

export function MenuLabel({ children }: { children: ReactNode }) {
  return <div className="menu-label">{children}</div>
}

export function MenuSeparator() {
  return <div className="menu-sep" />
}

// --- modal ------------------------------------------------------------------------

/** A dialog over the window. `position="top"` is the command palette's place; `center` the rest. */
export function Modal({
  open,
  onClose,
  children,
  className = '',
  position = 'center',
  label,
}: {
  open: boolean
  onClose: () => void
  children: ReactNode
  className?: string
  position?: 'top' | 'center'
  label?: string
}) {
  useLayer(open, onClose)
  if (!open) return null
  return createPortal(
    <div
      className={`fixed inset-0 z-50 flex justify-center bg-ink-950/30 px-4 backdrop-blur-[2px] dark:bg-black/50 ${
        position === 'top' ? 'items-start pt-[12vh]' : 'items-center'
      }`}
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose()
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label={label}
        className={`pop-in w-full overflow-hidden rounded-2xl border border-ink-200 bg-white shadow-2xl shadow-ink-950/20 dark:border-ink-700 dark:bg-ink-900 dark:shadow-black/50 ${className}`}
      >
        {children}
      </div>
    </div>,
    document.body,
  )
}

// --- small controls ---------------------------------------------------------------

/** A square icon button with a tooltip. `active` keeps it pressed (a toggled panel). */
export function IconButton({
  icon,
  label,
  onClick,
  active = false,
  disabled = false,
  shortcut,
  className = '',
}: {
  icon: IconName
  label: string
  onClick: () => void
  active?: boolean
  disabled?: boolean
  shortcut?: string
  className?: string
}) {
  const title = shortcut ? `${label} (${shortcutKeys(shortcut).join(' ')})` : label
  return (
    <button
      type="button"
      className={`icon-btn ${active ? 'bg-ink-100 text-ink-900 dark:bg-ink-800 dark:text-ink-50' : ''} ${className}`}
      title={title}
      aria-label={label}
      aria-pressed={active}
      disabled={disabled}
      onClick={onClick}
    >
      <Icon name={icon} className="size-4" />
    </button>
  )
}

export type SegmentOption<T extends string> = { value: T; label: ReactNode; icon?: IconName; title?: string }

/** A row of mutually exclusive choices — Chat / Agent, Light / Dark / System. */
export function Segmented<T extends string>({
  value,
  options,
  onChange,
  className = '',
}: {
  value: T
  options: SegmentOption<T>[]
  onChange: (value: T) => void
  className?: string
}) {
  return (
    <div className={`inline-flex items-center gap-0.5 rounded-lg bg-ink-100 p-0.5 dark:bg-ink-800 ${className}`} role="radiogroup">
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={option.value === value}
          title={option.title}
          onClick={() => onChange(option.value)}
          className={`inline-flex h-6 items-center gap-1 rounded-md px-2 text-xs font-medium transition-colors ${
            option.value === value
              ? 'bg-white text-ink-900 shadow-sm dark:bg-ink-950 dark:text-ink-50'
              : 'text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-100'
          }`}
        >
          {option.icon && <Icon name={option.icon} className="size-3.5" />}
          {option.label}
        </button>
      ))}
    </div>
  )
}

/** A status light. `pulse` for something running now. */
export function StatusDot({ tone, pulse = false, className = '' }: { tone: 'green' | 'amber' | 'red' | 'gray' | 'arc'; pulse?: boolean; className?: string }) {
  const color = { green: 'bg-emerald-500', amber: 'bg-amber-500', red: 'bg-red-500', gray: 'bg-ink-400', arc: 'bg-arc-500' }[tone]
  return (
    <span className={`relative inline-flex size-2 shrink-0 ${className}`}>
      {pulse && <span className={`absolute inline-flex size-full animate-ping rounded-full opacity-60 ${color}`} />}
      <span className={`relative inline-flex size-2 rounded-full ${color}`} />
    </span>
  )
}
