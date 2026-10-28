// An attached file, as a chip: in the composer (removable) and above the message that carried it.
// Clicking the name previews what the model is sent — the file as attached, cut and all.

import { useRef, useState } from 'react'
import { bytes } from '../../lib/format'
import type { Attachment } from '../../lib/types'
import { Icon } from '../common'
import { Popover } from '../ui'

export function AttachmentChip({ attachment, onRemove }: { attachment: Attachment; onRemove?: () => void }) {
  const anchor = useRef<HTMLSpanElement>(null)
  const [open, setOpen] = useState(false)
  const label = attachment.path ?? attachment.name
  return (
    <>
      <span
        ref={anchor}
        className="inline-flex h-7 max-w-full items-center gap-1.5 rounded-lg border border-ink-200 bg-white pl-2 pr-1 text-xs text-ink-700 dark:border-ink-700 dark:bg-ink-900 dark:text-ink-200"
        title={`${label} · ${bytes(attachment.size)}${attachment.truncated ? ' · only the first 200 KB is attached' : ''}`}
      >
        <Icon name={attachment.path ? 'fileText' : 'file'} className="size-3.5 shrink-0 text-ink-400" />
        <button
          type="button"
          className="min-w-0 truncate text-left hover:underline"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
          aria-label={`Preview ${label}`}
        >
          {label}
        </button>
        <span className="shrink-0 text-[0.65rem] text-ink-400 dark:text-ink-500">{bytes(attachment.size)}</span>
        {attachment.truncated && (
          <span className="badge shrink-0 bg-amber-100 px-1 py-0 text-[0.6rem] text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">cut</span>
        )}
        {onRemove ? (
          <button type="button" className="icon-btn size-5 rounded" onClick={onRemove} aria-label={`Remove ${label}`} title="Remove">
            <Icon name="x" className="size-3" />
          </button>
        ) : (
          <span className="w-1" />
        )}
      </span>
      <Popover anchor={anchor} open={open} onClose={() => setOpen(false)} placement="top-end" className="w-[min(34rem,85vw)] p-0">
        <div className="flex items-center gap-2 border-b border-ink-200 px-3 py-1.5 text-[0.7rem] text-ink-500 dark:border-ink-700 dark:text-ink-400">
          <Icon name="fileText" className="size-3.5 shrink-0" />
          <span className="mono min-w-0 flex-1 truncate">{label}</span>
          {attachment.truncated && <span className="shrink-0">first 200 KB of {bytes(attachment.size)}</span>}
        </div>
        <pre className="mono max-h-80 overflow-auto whitespace-pre p-3 text-[0.7rem] leading-relaxed text-ink-700 dark:text-ink-300">
          {attachment.content.length > 20_000 ? `${attachment.content.slice(0, 20_000)}\n…` : attachment.content || '(empty file)'}
        </pre>
      </Popover>
    </>
  )
}
