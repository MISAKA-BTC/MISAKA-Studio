// A thinking model's reasoning, above its answer.
//
// While the model thinks and nothing of the answer exists yet, the block says so — with a running
// count, because a reasoning model can think for a minute and a silent spinner for that long reads
// as a hang — and shows the last lines of the thought, muted, so it is visibly moving. Once the
// answer starts the block folds to one line, "Thought for 12s", and stays out of the answer's way.
// The reasoning is the model's scratch work, not something it said: it is never sent back as
// history (`lib/history`), and copy and pin take the answer alone.

import { useEffect, useRef, useState } from 'react'
import { reasoningTail, seconds, thoughtLabel } from '../../lib/reasoning'
import { Icon } from '../common'

/** `performance.now()`, refreshed every second while `active`. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => performance.now())
  useEffect(() => {
    if (!active) return
    setNow(performance.now())
    const timer = setInterval(() => setNow(performance.now()), 1000)
    return () => clearInterval(timer)
  }, [active])
  return now
}

export function ReasoningBlock({
  text,
  live,
  startedAt,
  durationMs,
}: {
  text: string
  /** Still thinking: no answer yet. */
  live: boolean
  /** `performance.now()` when the request was sent, for the live count. */
  startedAt?: number
  /** How long the model thought, when the engine's stream says (`message.reasoningMs`). */
  durationMs?: number | null
}) {
  const [open, setOpen] = useState(false)
  const now = useNow(live)
  // An inline `<think>` block carries no timing of its own. The moment it closes is measured here,
  // so the folded line can still say how long it took — for as long as this reply stays on screen.
  const [measured, setMeasured] = useState<number | null>(null)
  const wasLive = useRef(live)
  useEffect(() => {
    if (wasLive.current && !live && startedAt !== undefined) setMeasured(performance.now() - startedAt)
    wasLive.current = live
  }, [live, startedAt])

  if (!live && !text.trim()) return null

  if (live) {
    return (
      <div className="mb-3">
        <button
          type="button"
          className="inline-flex items-center gap-1.5 rounded-md py-0.5 text-xs font-medium text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-100"
          onClick={() => setOpen((v) => !v)}
          aria-expanded={open}
        >
          <Icon name="brain" className="size-3.5 animate-pulse" />
          <span>Thinking…</span>
          {startedAt !== undefined && <span className="tabular-nums text-ink-400 dark:text-ink-500">{seconds(now - startedAt)}</span>}
          {text && <Icon name="chevron" className={`size-3 transition-transform ${open ? 'rotate-90' : ''}`} />}
        </button>
        {text &&
          (open ? (
            <ReasoningText text={text} />
          ) : (
            // A fixed height, bottom-aligned: new lines push the old ones up and out of view, and
            // nothing below the block moves while the model thinks.
            <div
              className="mt-1.5 flex h-[3.9rem] flex-col justify-end overflow-hidden border-l-2 border-ink-200 pl-3 text-xs leading-[1.3rem] text-ink-400 [mask-image:linear-gradient(to_bottom,transparent,black_55%)] dark:border-ink-700 dark:text-ink-500"
              aria-hidden
            >
              <p className="whitespace-pre-wrap break-words">{reasoningTail(text)}</p>
            </div>
          ))}
      </div>
    )
  }

  return (
    <div className="mb-3">
      <button
        type="button"
        className="inline-flex items-center gap-1.5 rounded-md py-0.5 text-xs font-medium text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-100"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
      >
        <Icon name="brain" className="size-3.5" />
        <span>{thoughtLabel(durationMs ?? measured)}</span>
        <Icon name="chevron" className={`size-3 transition-transform ${open ? 'rotate-90' : ''}`} />
      </button>
      {open && <ReasoningText text={text} />}
    </div>
  )
}

function ReasoningText({ text }: { text: string }) {
  return (
    <div className="mt-1.5 max-h-96 overflow-y-auto border-l-2 border-ink-200 pl-3 text-xs leading-relaxed text-ink-500 dark:border-ink-700 dark:text-ink-400">
      <p className="whitespace-pre-wrap break-words">{text.trim()}</p>
    </div>
  )
}
