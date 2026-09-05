// The mining queue, as a panel: mode switch, counts, and the jobs with the lane's own words.
//
// This is where mining conditions belong — not in the chat. A refused job says why here, can be
// retried here after the cause is fixed, and a queued one can be dropped here. The chat only
// carries a badge that points back to this.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import { relativeTime } from '../lib/format'
import type { MiningJob, MiningQueueView } from '../lib/types'
import { useStudio } from '../store/studio'
import { Icon, Toggle } from './common'

function statusBadge(job: MiningJob) {
  switch (job.status) {
    case 'committed':
      return <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">committed</span>
    case 'running':
      return <span className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">mining…</span>
    case 'queued':
      return <span className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300">queued{job.attempts > 0 ? ` · retry ${job.attempts}` : ''}</span>
    case 'refused':
      return <span className="badge bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300">refused</span>
    case 'failed':
      return <span className="badge bg-red-50 text-red-700 dark:bg-red-950/40 dark:text-red-300">failed</span>
  }
}

export function MiningQueuePanel() {
  const toast = useStudio((s) => s.toast)
  const refreshMining = useStudio((s) => s.refreshMining)
  const [view, setView] = useState<MiningQueueView | null>(null)
  const [busy, setBusy] = useState(false)

  const read = useCallback(async () => {
    try {
      setView(await api.miningQueue())
    } catch {
      setView(null)
    }
  }, [])

  useEffect(() => {
    void read()
    const timer = setInterval(() => void read(), 10_000)
    return () => clearInterval(timer)
  }, [read])

  const setMode = async (background: boolean) => {
    setBusy(true)
    try {
      const next = await api.miningMode(background ? 'background' : 'inline')
      setView(next)
      await refreshMining()
      toast(
        'success',
        background
          ? 'Background mining: the chat answers locally and every prompt is queued for the slot'
          : 'Inline mining: the chat waits for the lane, and its answer is the mined answer',
      )
    } catch (error) {
      toast('error', (error as Error).message)
    } finally {
      setBusy(false)
    }
  }

  const act = async (label: string, fn: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await fn()
      await read()
      await refreshMining()
    } catch (error) {
      toast('error', `${label}: ${(error as Error).message}`)
    } finally {
      setBusy(false)
    }
  }

  if (!view) return null
  const { counts } = view
  const live = counts.queued + counts.running

  return (
    <div className="rounded-lg border border-ink-200 p-2 dark:border-ink-800">
      <div className="flex flex-wrap items-center gap-2">
        <span className="text-[0.7rem] font-medium">Mining queue</span>
        <span className="text-[0.65rem] text-ink-500 dark:text-ink-400">
          {live > 0 ? `${counts.running} mining · ${counts.queued} queued · ` : ''}
          {counts.committed} committed
          {counts.refused + counts.failed > 0 ? ` · ${counts.refused + counts.failed} not mined` : ''}
        </span>
      </div>

      <div className="mt-2 flex items-start justify-between gap-3">
        <div className="min-w-0 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
          <strong className="text-ink-700 dark:text-ink-200">Mine behind the chat.</strong> The chat answers from the engine that can
          answer now; each prompt is queued and mined on the slot&apos;s gateway at its own pace, like a hash miner accumulating
          and submitting. The mined answer is the one the chain holds — it appears under the message.
          {view.mode === 'background' && !view.background_available && view.background_blocker && (
            <p className="mt-1 flex items-start gap-1 text-amber-700 dark:text-amber-300">
              <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
              Not in effect yet: {view.background_blocker}. Until then the chat mines inline.
            </p>
          )}
        </div>
        <Toggle checked={view.mode === 'background'} onChange={(checked) => !busy && void setMode(checked)} label="Background" />
      </div>

      {view.jobs.length > 0 && (
        <ul className="mt-2 max-h-56 space-y-1 overflow-y-auto">
          {view.jobs.slice(0, 20).map((job) => (
            <li key={job.id} className="rounded-md bg-ink-50 px-2 py-1.5 text-[0.7rem] dark:bg-ink-900/60">
              <div className="flex items-center gap-2">
                {statusBadge(job)}
                <span className="min-w-0 flex-1 truncate" title={job.prompt}>
                  {job.prompt}
                </span>
                <span className="shrink-0 text-ink-500 dark:text-ink-400">{relativeTime(Math.floor(job.created_ms / 1000))}</span>
                {(job.status === 'refused' || job.status === 'failed') && (
                  <button type="button" className="btn-ghost px-1.5 py-0.5" disabled={busy} onClick={() => void act('retry', () => api.miningRetry(job.id))} title="Queue it again">
                    retry
                  </button>
                )}
                {job.status === 'queued' && (
                  <button type="button" className="btn-ghost px-1.5 py-0.5" disabled={busy} onClick={() => void act('remove', () => api.miningRemove(job.id))} title="Drop it">
                    drop
                  </button>
                )}
              </div>
              {job.claim_id && (
                <div className="mono mt-0.5 truncate text-[0.65rem] text-ink-500 dark:text-ink-400" title={job.claim_id}>
                  claim {job.claim_id.slice(0, 16)}…
                </div>
              )}
              {job.error && <div className="mt-0.5 whitespace-pre-wrap text-[0.65rem] text-red-700 dark:text-red-300">{job.error}</div>}
            </li>
          ))}
        </ul>
      )}
    </div>
  )
}
