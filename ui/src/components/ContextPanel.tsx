// The context manager, as the chat shows it.
//
// A class registered at 512 tokens holds a question and an answer, not a conversation, so the
// runtime decides what each request carries: the system prompt and the question, the pinned notes,
// recent turns whole, and a memory of the older ones. Two things here make that decision visible
// and steerable:
//
// * **Pinned notes** — standing facts for this conversation, kept in every request ahead of
//   history. What a person pins is what no amount of later chat may push out.
// * **The context line** under a reply — what that reply was produced from, with the messages
//   exactly as sent a click away. "The model forgot" and "the app did not send it" look the same
//   from the chat, and only one of them is fixed by pinning.

import { useState } from 'react'
import { tokens } from '../lib/format'
import type { ContextReport } from '../lib/types'
import { useStudio } from '../store/studio'
import { Icon } from './common'

/** How long a pin taken from a message is before the person trims it. */
const PIN_FROM_MESSAGE_CHARS = 280

export function pinTextFromMessage(content: string): string {
  const flat = content.replace(/\s+/g, ' ').trim()
  return flat.length > PIN_FROM_MESSAGE_CHARS ? `${flat.slice(0, PIN_FROM_MESSAGE_CHARS - 1)}…` : flat
}

export function PinnedNotes() {
  const conversation = useStudio((s) => s.conversations.find((c) => c.id === s.activeConversationId) ?? null)
  const addPin = useStudio((s) => s.addPin)
  const updatePin = useStudio((s) => s.updatePin)
  const removePin = useStudio((s) => s.removePin)
  const [open, setOpen] = useState(false)
  const [draft, setDraft] = useState('')
  const pins = conversation?.pinned ?? []

  if (!conversation) return null

  const add = () => {
    if (!draft.trim()) return
    addPin(draft)
    setDraft('')
  }

  return (
    <div className="border-b border-ink-200 px-4 py-1.5 dark:border-ink-800">
      <div className="mx-auto max-w-3xl">
        <button
          type="button"
          className="flex items-center gap-1.5 text-xs text-ink-600 hover:text-ink-900 dark:text-ink-300 dark:hover:text-ink-100"
          onClick={() => setOpen((v) => !v)}
          title="Standing facts for this conversation. Every request carries them ahead of the chat history, so a small model window never drops them."
        >
          <Icon name="pin" className="size-3.5" />
          Pinned notes
          <span className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300">{pins.length}</span>
          <Icon name="chevron" className={`size-3 transition-transform ${open ? 'rotate-90' : ''}`} />
        </button>

        {open && (
          <div className="mt-2 space-y-1.5 pb-1.5">
            {pins.length === 0 && (
              <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">
                Nothing pinned. Pin a fact the model must keep using — a result, a constraint, a name — here or with the pin button under
                any message.
              </p>
            )}
            {pins.map((pin, index) => (
              <div key={`${index}-${pin.slice(0, 16)}`} className="flex items-start gap-1.5">
                <textarea
                  className="input min-h-[2.25rem] flex-1 resize-y py-1.5 text-xs"
                  rows={1}
                  defaultValue={pin}
                  onBlur={(event) => {
                    if (event.target.value !== pin) updatePin(index, event.target.value)
                  }}
                />
                <button type="button" className="btn-ghost px-1.5 py-1.5" title="Unpin" onClick={() => removePin(index)}>
                  <Icon name="x" className="size-3.5" />
                </button>
              </div>
            ))}
            <div className="flex items-center gap-1.5">
              <input
                className="input flex-1 py-1.5 text-xs"
                placeholder="Add a pinned note…"
                value={draft}
                onChange={(event) => setDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter' && !event.nativeEvent.isComposing) {
                    event.preventDefault()
                    add()
                  }
                }}
              />
              <button type="button" className="btn-outline px-2 py-1.5 text-xs" onClick={add} disabled={!draft.trim()}>
                <Icon name="plus" className="size-3.5" />
                Pin
              </button>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

/** One line under a reply: what the model was given to produce it. */
export function ContextLine({ report }: { report: ContextReport }) {
  const [open, setOpen] = useState(false)
  const warnings: string[] = []
  if (report.pinned_omitted > 0) warnings.push(`${report.pinned_omitted} pinned note${report.pinned_omitted === 1 ? '' : 's'} did not fit`)
  if (report.question_over_budget) warnings.push('the question alone is over the window by the estimate')
  if (report.memory?.note) warnings.push(report.memory.note)
  if (report.older_messages > 0 && !report.memory) warnings.push(`${report.older_messages} earlier messages were not sent and there was no room to summarise them`)

  // Nothing to say about a conversation that fitted as sent.
  if (!report.managed && warnings.length === 0) return null

  const parts: string[] = [`${tokens(report.window)}-token window`, `prompt ${report.prompt_tokens}/${report.prompt_budget}`]
  if (report.pinned_included > 0) parts.push(`${report.pinned_included} pinned`)
  if (report.continuation) parts.push('continuing the cut-off reply')
  else {
    if (report.recent_messages > 0) parts.push(`${report.recent_messages} recent message${report.recent_messages === 1 ? '' : 's'} whole`)
    if (report.memory) parts.push(`${report.memory.messages_covered} earlier as ${report.memory.source === 'summary' ? 'a summary' : 'an extract'}`)
  }

  return (
    <div className="mt-2 text-[0.7rem] text-ink-500 dark:text-ink-400">
      <button
        type="button"
        className="flex flex-wrap items-center gap-1 text-left hover:text-ink-800 dark:hover:text-ink-200"
        onClick={() => setOpen((v) => !v)}
        title={report.counter.kind === 'tokenizer' ? `Counted with ${report.counter.path}` : 'Counted with an estimate: no tokenizer for this model was found'}
      >
        <Icon name="layers" className="size-3.5" />
        <span>{parts.join(' · ')}</span>
        <span className="mono opacity-70">{report.counter.kind === 'tokenizer' ? '· exact' : '· estimate'}</span>
        <Icon name="chevron" className={`size-3 transition-transform ${open ? 'rotate-90' : ''}`} />
      </button>
      {warnings.map((warning) => (
        <p key={warning} className="mt-1 flex items-start gap-1 text-amber-700 dark:text-amber-400">
          <Icon name="warning" className="mt-0.5 size-3 shrink-0" />
          {warning}
        </p>
      ))}
      {open && report.sent && (
        <div className="mt-1.5 space-y-1 rounded-lg border border-ink-200 p-2 dark:border-ink-800">
          <div className="font-medium text-ink-600 dark:text-ink-300">What the model saw</div>
          {report.sent.map((message, index) => (
            <div key={index}>
              <span className="mono text-ink-400">{message.role}</span>
              <p className="whitespace-pre-wrap break-words text-ink-700 dark:text-ink-300">{message.content}</p>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}
