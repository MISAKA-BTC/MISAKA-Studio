// Prompt mining, as the window's chrome sees it: one poll, one summary.
//
// The status bar, the palette and the sidebar all say something about mining, and they must say
// the same thing. So the reading of the runtime's two mining sources — the queue (`miningQueue`,
// background mining) and the prompt stack (the node-free gateway + rail) — happens once, here,
// and every surface draws `miningSummary`.

import { useEffect, useMemo } from 'react'
import { create } from 'zustand'
import { api } from '../../lib/api'
import type { MiningQueueView, PromptStackView } from '../../lib/types'
import { useStudio } from '../../store/studio'

type PromptStackState = {
  /** The stack as last read; null before the first read and when the runtime did not answer. */
  stack: PromptStackView | null
}

/** The prompt stack's state, kept beside the queue `useStudio` already holds. */
export const usePromptStack = create<PromptStackState>(() => ({ stack: null }))

export async function refreshPromptStack(): Promise<void> {
  try {
    usePromptStack.setState({ stack: await api.promptStack() })
  } catch {
    usePromptStack.setState({ stack: null })
  }
}

/** Read both mining sources now — after an action that changed them, or when a panel opens. */
export async function refreshMiningState(): Promise<void> {
  await Promise.all([refreshPromptStack(), useStudio.getState().refreshMining()])
}

const IDLE_MS = 15_000
/** While a prompt is waiting for the lane, its badge should not lag the queue by a quarter minute. */
const ACTIVE_MS = 5_000
/** A reply just finished: whatever it queued or committed is worth reading straight away. */
const SETTLE_MS = 1_500
/** How long a message's own word that it is queued counts as "something is in flight". Past
 *  this the queue has trimmed the job or the runtime restarted, and the badge keeps what it last
 *  heard — which must not keep the poll fast forever. */
const LOCAL_PENDING_WINDOW_MS = 10 * 60_000

function jobsInFlight(): boolean {
  const { miningQueue, conversations } = useStudio.getState()
  if (miningQueue && miningQueue.counts.queued + miningQueue.counts.running > 0) return true
  const since = Date.now() - LOCAL_PENDING_WINDOW_MS
  return conversations.some((c) =>
    c.messages.some((m) => (m.mining?.status === 'queued' || m.mining?.status === 'running') && (m.createdAt ?? 0) > since),
  )
}

/**
 * **The one mining poll in the window.** Mounted once, by `App`.
 *
 * Slow on purpose — the queue and the stack are lights, not dashboards — except while a prompt is
 * actually in the queue, and once right after a reply finishes. A hidden window is not polled at
 * all; it is read again the moment it is shown.
 */
export function useMiningPoll(): void {
  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | null = null
    let due = 0
    let reading = false
    let stopped = false

    const schedule = (ms: number) => {
      if (stopped) return
      if (timer !== null) clearTimeout(timer)
      due = Date.now() + ms
      timer = setTimeout(() => void tick(), ms)
    }
    const tick = async () => {
      timer = null
      if (stopped || reading) return
      if (document.visibilityState === 'hidden') return // `onVisibility` picks it up again
      reading = true
      try {
        await refreshMiningState()
      } finally {
        reading = false
      }
      schedule(jobsInFlight() ? ACTIVE_MS : IDLE_MS)
    }
    /** Bring the next read forward to `ms` from now, unless it is already sooner. */
    const soon = (ms: number) => {
      if (stopped || reading) return
      if (timer === null || due - Date.now() > ms) schedule(ms)
    }

    const onVisibility = () => {
      if (document.visibilityState === 'visible' && timer === null) void tick()
    }
    const unsubscribe = useStudio.subscribe((state, previous) => {
      if (previous.streaming !== null && state.streaming === null) soon(SETTLE_MS)
    })

    void tick()
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      stopped = true
      if (timer !== null) clearTimeout(timer)
      unsubscribe()
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [])
}

export type MiningSummary = {
  /**
   * What happens to the next prompt — the effective mode, not the setting:
   * - `inline`: the chat engine is the mining gateway, so the answer IS the mined work;
   * - `background`: the chat answers here and the prompt is queued and mined behind it;
   * - `off`: this chat's prompts are not mined.
   */
  mode: 'inline' | 'background' | 'off'
  /** The mode setting as written, which can say background while background is not in effect. */
  setting: MiningQueueView['mode'] | null
  /** Why background mining is not in effect, in the runtime's words. */
  blocker: string | null
  backgroundAvailable: boolean
  queued: number
  running: number
  committed: number
  /** Refused or failed: the lane said no, or the attempt broke. */
  notMined: number
  /** The node-free stack: gateway and rail both up. */
  stackRunning: boolean
  /** One of the two is up and the other is not — worth saying, it mines nothing that way. */
  stackPartial: boolean
  /** The runtime can start the stack as configured (its plan resolves). */
  stackReady: boolean
  /** Why it cannot, when it cannot. */
  stackProblem: string | null
  carriers: number
  /** Something is being mined right now: a queued job's turn, or an inline answer streaming. */
  busy: boolean
  /** Whether nothing has been read yet — the runtime has not answered either poll. */
  unknown: boolean
}

export function miningSummary(queue: MiningQueueView | null, stack: PromptStackView | null, backend: string | null | undefined, generating: boolean): MiningSummary {
  const inline = backend === 'gateway'
  const background = queue?.mode === 'background' && queue.background_available
  // Background wins when it is in effect: the runtime only reports it available when the chat is
  // answered by an engine other than the gateway, so the two cannot both be true of one prompt.
  const mode = background ? 'background' : inline ? 'inline' : 'off'
  const counts = queue?.counts ?? { queued: 0, running: 0, committed: 0, refused: 0, failed: 0 }
  const gateway = stack?.gateway.running ?? false
  const rail = stack?.rail.running ?? false
  return {
    mode,
    setting: queue?.mode ?? null,
    blocker: queue && !queue.background_available ? queue.background_blocker : null,
    backgroundAvailable: queue?.background_available ?? false,
    queued: counts.queued,
    running: counts.running,
    committed: counts.committed,
    notMined: counts.refused + counts.failed,
    stackRunning: gateway && rail,
    stackPartial: gateway !== rail,
    stackReady: stack !== null && 'Ok' in stack.planned,
    stackProblem: stack !== null && 'Err' in stack.planned ? stack.planned.Err : null,
    carriers: stack?.carriers.length ?? 0,
    busy: counts.running > 0 || (mode === 'inline' && generating),
    unknown: queue === null && stack === null,
  }
}

/** The summary, live. Re-renders only when what it says changes, not on every token. */
export function useMiningSummary(): MiningSummary {
  const queue = useStudio((s) => s.miningQueue)
  const backend = useStudio((s) => s.runtime?.backend)
  const generating = useStudio((s) => s.streaming !== null)
  const stack = usePromptStack((s) => s.stack)
  return useMemo(() => miningSummary(queue, stack, backend, generating), [queue, stack, backend, generating])
}

export const MINING_MODE_LABEL: Record<MiningSummary['mode'], string> = {
  inline: 'Inline',
  background: 'Background',
  off: 'Off',
}
