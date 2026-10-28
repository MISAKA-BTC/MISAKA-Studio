// Threads as the sidebar and the palette list them: grouped by date, and marked when they were
// mined.

import { stripThinking } from '../../lib/history'
import type { Conversation } from '../../lib/types'

export type ThreadGroup = { id: string; label: string; threads: Conversation[] }

const DAY_MS = 86_400_000

/**
 * Starred first, then by how recently each thread changed: Today, Yesterday, Previous 7 days,
 * Previous 30 days, Older. Days are the person's own calendar days — local midnight — not 24-hour
 * windows, so "Yesterday" means what it says at 00:05.
 */
export function groupThreads(conversations: Conversation[], now: number = Date.now()): ThreadGroup[] {
  const midnight = new Date(now)
  midnight.setHours(0, 0, 0, 0)
  const today = midnight.getTime()
  const bounds: { id: string; label: string; from: number }[] = [
    { id: 'today', label: 'Today', from: today },
    { id: 'yesterday', label: 'Yesterday', from: today - DAY_MS },
    { id: 'week', label: 'Previous 7 days', from: today - 7 * DAY_MS },
    { id: 'month', label: 'Previous 30 days', from: today - 30 * DAY_MS },
    { id: 'older', label: 'Older', from: -Infinity },
  ]
  const sorted = [...conversations].sort((a, b) => b.updatedAt - a.updatedAt)
  const groups: ThreadGroup[] = [{ id: 'starred', label: 'Starred', threads: sorted.filter((c) => c.starred) }]
  for (const bound of bounds) groups.push({ id: bound.id, label: bound.label, threads: [] })
  for (const conversation of sorted) {
    if (conversation.starred) continue
    const index = bounds.findIndex((b) => conversation.updatedAt >= b.from)
    groups[index + 1]?.threads.push(conversation)
  }
  return groups.filter((g) => g.threads.length > 0)
}

export type ThreadMining = {
  /** A prompt from this thread is queued or being mined now. */
  pending: number
  /** Prompts from this thread the lane committed. Committed, not paid: a claim pays at Final. */
  committed: number
}

const miningCache = new WeakMap<Conversation, ThreadMining>()

/** What became of this thread's prompts on the mining lane, from the badges on its messages. */
export function threadMining(conversation: Conversation): ThreadMining {
  const cached = miningCache.get(conversation)
  if (cached) return cached
  let pending = 0
  let committed = 0
  for (const message of conversation.messages) {
    const status = message.mining?.status
    if (status === 'queued' || status === 'running') pending++
    else if (status === 'committed') committed++
  }
  const result = { pending, committed }
  miningCache.set(conversation, result)
  return result
}

/** The last path segment of a folder, for a thread's subtitle. */
export function folderName(path: string | null | undefined): string | null {
  if (!path) return null
  const trimmed = path.replace(/[\\/]+$/, '')
  return trimmed.split(/[\\/]/).pop() || trimmed || null
}

/** The last thing the assistant said in a thread, as a person would copy it. */
export function lastAnswer(conversation: Conversation | null | undefined): string | null {
  if (!conversation) return null
  for (let i = conversation.messages.length - 1; i >= 0; i--) {
    const message = conversation.messages[i]
    if (message?.role !== 'assistant') continue
    const text = stripThinking(message.content).trim()
    if (text) return text
  }
  return null
}
