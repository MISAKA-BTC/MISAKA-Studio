// What became of a prompt on the chain, and the tones every mining surface in the chat shares.

import { useState } from 'react'
import type { MessageMining, MiningJobStatus } from '../../lib/types'
import { CopyButton } from '../common'

/** Amber while in progress, emerald once committed, red for the lane's refusal. */
export function miningTone(status: MiningJobStatus): string {
  return status === 'committed'
    ? 'bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300'
    : status === 'refused' || status === 'failed'
      ? 'bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300'
      : 'bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300'
}

export function miningLabel(mining: Pick<MessageMining, 'status' | 'claimId'>): string {
  switch (mining.status) {
    case 'queued':
      return 'queued for mining'
    case 'running':
      return 'mining…'
    case 'committed':
      return `mined · claim ${mining.claimId ? mining.claimId.slice(0, 8) + '…' : 'committed'}`
    case 'refused':
      return 'not mined — the lane refused'
    case 'failed':
      return 'not mined — gave up'
  }
}

/**
 * **What became of this prompt on the chain** — under the message, always visible, never a toast.
 *
 * Background mining answers the chat here and mines the prompt behind it; the badge is the only
 * place the two halves meet. It says exactly what the runtime's queue says: queued, mining, a
 * committed claim (with its id), or the lane's own refusal — and, when there is one, the mined
 * answer, because that is the text misakascan will show and it need not match the chat's.
 */
export function MiningBadge({ mining }: { mining: MessageMining }) {
  const [open, setOpen] = useState(false)
  const expandable = Boolean(mining.error || mining.answer)
  return (
    <div className="mt-1.5 flex max-w-full flex-col items-end gap-1 text-[0.7rem]">
      <button
        type="button"
        className={`inline-flex items-center gap-1 rounded-md px-1.5 py-0.5 ${miningTone(mining.status)} ${expandable ? '' : 'cursor-default'}`}
        onClick={() => expandable && setOpen((v) => !v)}
        aria-expanded={expandable ? open : undefined}
        title={mining.error ?? (mining.answer ? 'Show the mined answer' : undefined)}
      >
        <span aria-hidden>⛏</span>
        {miningLabel(mining)}
      </button>
      {open && expandable && (
        <div className="max-w-prose rounded-lg border border-ink-200 bg-white p-2 text-left text-xs text-ink-700 dark:border-ink-800 dark:bg-ink-900 dark:text-ink-300">
          {mining.error ? (
            <p className="whitespace-pre-wrap">{mining.error}</p>
          ) : (
            <>
              <p className="mb-1 text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">Mined answer — what the chain holds</p>
              <p className="whitespace-pre-wrap">{mining.answer}</p>
            </>
          )}
          {mining.claimId && (
            <div className="mt-1.5 flex items-center gap-1 border-t border-ink-100 pt-1.5 text-[0.65rem] text-ink-500 dark:border-ink-800 dark:text-ink-400">
              <span className="mono min-w-0 flex-1 truncate" title={mining.claimId}>
                claim {mining.claimId}
              </span>
              <CopyButton text={mining.claimId} label="Copy claim id" className="icon-btn size-6" />
            </div>
          )}
        </div>
      )}
    </div>
  )
}
