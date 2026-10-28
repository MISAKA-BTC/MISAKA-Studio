// The window's two sides, as data: which side a thread is on, and which thread a side opens on.
//
// The rule they serve is one sentence — **the thread in view always belongs to the side in view**
// — and every way of moving between threads (the switch, the palette, a deleted thread, a status
// bar link) settles it with these few functions, so the sides cannot each grow their own idea of
// "the last thread". `sideSync.ts` applies them to the stores; this file stays free of the stores.
//
// Plain TypeScript with type-only imports, so the unit tests run it directly under Node.

import type { Conversation } from '../../lib/types'
import type { AppMode } from '../../store/ui'

type Sided = Pick<Conversation, 'mode'>
type Ranked = Pick<Conversation, 'id' | 'mode' | 'updatedAt'>
type Placed = Pick<Conversation, 'mode' | 'workspace'>

/** A coding-agent thread is a Code session; everything else is a chat. */
export function sideOf(thread: Sided): AppMode {
  return thread.mode === 'agent' ? 'code' : 'chat'
}

export function threadsOf<T extends Sided>(threads: readonly T[], side: AppMode): T[] {
  return threads.filter((thread) => sideOf(thread) === side)
}

/** The side's most recently changed thread — what it falls back to when its own is gone. */
export function mostRecent<T extends Ranked>(threads: readonly T[], side: AppMode, except?: string | null): T | null {
  let best: T | null = null
  for (const thread of threads) {
    if (thread.id === except || sideOf(thread) !== side) continue
    if (best === null || thread.updatedAt > best.updatedAt) best = thread
  }
  return best
}

/**
 * The thread a side opens on when the window switches to it: the one it had open, while that still
 * exists and is still on that side (an empty chat the folder picker turned into a session is not),
 * else its most recent thread, else none — the side's empty page.
 */
export function restoreTarget(threads: readonly Ranked[], remembered: string | null | undefined, side: AppMode): string | null {
  if (remembered) {
    const thread = threads.find((t) => t.id === remembered)
    if (thread && sideOf(thread) === side) return thread.id
  }
  return mostRecent(threads, side)?.id ?? null
}

/**
 * The folder a new session starts in: the folder of the session in view, else of the session Code
 * had open last, else the folder used last. Null when there is none of those — then the person is
 * asked for one.
 */
export function projectForNewSession(input: { active: Placed | null | undefined; last: Placed | null | undefined; recent: readonly string[] }): string | null {
  for (const thread of [input.active, input.last]) {
    if (thread && sideOf(thread) === 'code' && thread.workspace) return thread.workspace
  }
  return input.recent[0] ?? null
}
