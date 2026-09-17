// What of a conversation is sent back to the model as history.
//
// The window shows every turn, failures included, because a person should see what happened. The
// model must not: a reply that looped, broke off with an error, or never arrived is not context,
// it is a template for the next failure. Measured on a real conversation (2026-09-17): one
// question asked fifteen times, with nine replies stopped for repetition or errors among them, was
// sent back whole — 11,101 tokens — and the model reproduced the looping paragraph word for word.
// The same question on its own produced no loop at all.
//
// So history is filtered before it is sent, by three rules:
//
// 1. A reply that failed (it carries an error), is empty, or is still streaming is left out.
// 2. A reply that repeats itself is cut back to before the repetition, and left out if what remains
//    is a fragment, less than half of it, or if the repetition cannot be cut cleanly.
// 3. A question asked again replaces its earlier asking: the earlier copy and the replies to it are
//    left out. Someone who sends the same words again is retrying, not adding context.

import type { ChatMessage } from './types'

/** The length of text compared when looking for a repeated tail. */
const TAIL = 120
/** Below this, a reply cut back to before its repetition is a fragment, not an answer. */
const MIN_KEPT_REPLY = 40

function compact(text: string): string {
  return text.replace(/\s+/g, ' ').trim()
}

/** Whether the last stretch of `text` already appeared earlier in it: a model looping. */
export function hasRepeatedTail(text: string): boolean {
  const flat = compact(text)
  if (flat.length < TAIL * 2) return false
  return flat.slice(0, -TAIL).includes(flat.slice(-TAIL))
}

/** Shortest and longest loop period looked for directly at the end of a text. */
const SHORT_PERIOD = { min: 8, max: 300 }

/** Whether `text` ends in the same block three times running — a loop too short for the tail test. */
function shortLoopPeriod(text: string): number | null {
  const max = Math.min(SHORT_PERIOD.max, Math.floor(text.length / 3))
  for (let p = SHORT_PERIOD.min; p <= max; p++) {
    const last = text.slice(-p)
    // A run of symbols or numbers is formatting — an underline, a zero-filled array — not a model
    // saying the same thing again.
    if (!/\p{L}/u.test(last)) continue
    if (text.slice(-2 * p, -p) === last && text.slice(-3 * p, -2 * p) === last) return p
  }
  return null
}

/** Whether a reply loops, by either test. */
export function loops(text: string): boolean {
  const trimmed = text.trimEnd()
  return hasRepeatedTail(trimmed) || shortLoopPeriod(trimmed) !== null
}

/**
 * `text` without the repetition at its end, or `null` when it cannot be cut cleanly.
 *
 * A long loop is found by its tail: the tail's earlier occurrence ends one period before the text
 * does, so a period at a time is removed until the tail no longer repeats. A short loop — a phrase
 * over and over — is found directly, as the same block three times at the end, and cut to one copy.
 */
export function trimRepeatedTail(text: string): string | null {
  let current = text.trimEnd()
  let shortCut: number | null = null
  for (let guard = 0; guard < 4096; guard++) {
    const short = shortLoopPeriod(current)
    if (short !== null) {
      current = current.slice(0, current.length - short)
      shortCut = short
      continue
    }
    // A short loop cut down to where it no longer runs three times still ends in two copies.
    if (shortCut !== null && current.slice(-shortCut) === current.slice(-2 * shortCut, -shortCut)) {
      current = current.slice(0, current.length - shortCut)
    }
    shortCut = null
    current = current.trimEnd()
    if (!hasRepeatedTail(current)) return current
    const tail = current.slice(-TAIL)
    const earlier = current.indexOf(tail)
    if (earlier < 0 || earlier + TAIL >= current.length) return null
    const period = current.length - (earlier + TAIL)
    current = current.slice(0, current.length - period).trimEnd()
  }
  return null
}

/** The conversation as the model should see it. */
export function historyForModel(messages: ChatMessage[]): { role: ChatMessage['role']; content: string }[] {
  // Rule 3 first: the last asking of each question wins.
  const lastAsking = new Map<string, number>()
  messages.forEach((m, i) => {
    if (m.role === 'user') lastAsking.set(m.content.trim(), i)
  })
  const out: { role: ChatMessage['role']; content: string }[] = []
  let skippingReplies = false
  messages.forEach((m, i) => {
    if (m.role === 'user') {
      skippingReplies = lastAsking.get(m.content.trim()) !== i
      if (!skippingReplies) out.push({ role: m.role, content: m.content })
      return
    }
    if (m.role === 'system') {
      out.push({ role: m.role, content: m.content })
      return
    }
    // An assistant reply.
    if (skippingReplies || m.error || m.streaming || !m.content.trim()) return
    if (loops(m.content)) {
      // Kept only when the loop was an ending: most of the reply must survive the cut, or it was a
      // loop with a preamble and sending the preamble teaches the loop's opening.
      const trimmed = trimRepeatedTail(m.content)
      if (trimmed && trimmed.length >= MIN_KEPT_REPLY && trimmed.length * 2 >= m.content.trimEnd().length) {
        out.push({ role: m.role, content: trimmed })
      }
      return
    }
    out.push({ role: m.role, content: m.content })
  })
  return out
}
