// A thinking model's reasoning, as the transcript shows it.
//
// The reasoning reaches the window one of two ways: as its own stream (`reasoning_content`, which a
// recent llama.cpp separates from the answer — `streamReply` keeps it in `streaming.reasoning` and
// commits it as `message.reasoning`), or inline, as a `<think>…</think>` block at the start of the
// content, from an engine that does not separate it. Both are drawn the same way: a collapsed
// "Thought for 12s" above the answer, never mixed into it. `history.stripThinking` is the other
// half of this — what is sent back to the model.
//
// Plain TypeScript with no imports, so the unit tests run it directly under Node.

const OPEN = '<think>'
const CLOSE = '</think>'

export type SplitThinking = {
  /** The reasoning, or null when the content carries none (an empty block counts as none). */
  thinking: string | null
  /** The content without its reasoning: what the model actually said. */
  answer: string
  /** The reasoning began and has not ended — still thinking, or cut off while thinking. */
  open: boolean
}

/** `text` without a closing tag that is still arriving at its end (`…</th`). */
function withoutPartialTag(text: string, tag: string): string {
  for (let k = Math.min(tag.length - 1, text.length); k > 0; k--) {
    if (text.endsWith(tag.slice(0, k))) return text.slice(0, -k)
  }
  return text
}

/** Whether a position after `prefix` is outside inline code and fences: backticks come in pairs. */
function outsideCode(prefix: string): boolean {
  return (prefix.match(/`/g)?.length ?? 0) % 2 === 0
}

/**
 * Split a reply into its inline reasoning and its answer.
 *
 * Three shapes are recognised, and only these, because a reply is free to *talk about* the tags:
 *
 * 1. A `<think>` block at the very start (after whitespace) — the common case. Unclosed, it is the
 *    model still thinking; a closing tag still arriving token by token is not shown as text.
 * 2. The opening tag itself still arriving (`<`, `<thi`) — shown as thinking, not as a stray `<`.
 * 3. A lone `</think>` on a line boundary, outside code, with no opening tag before it: a chat
 *    template that writes `<think>` into the prompt leaves only the closing tag in the reply.
 */
export function splitThinking(content: string): SplitThinking {
  const rest = content.trimStart()
  if (rest.startsWith(OPEN)) {
    const body = rest.slice(OPEN.length)
    const end = body.indexOf(CLOSE)
    if (end === -1) return { thinking: withoutPartialTag(body, CLOSE).trimStart(), answer: '', open: true }
    const thinking = body.slice(0, end).trim()
    return { thinking: thinking || null, answer: body.slice(end + CLOSE.length).trimStart(), open: false }
  }
  if (rest.length > 0 && OPEN.startsWith(rest)) return { thinking: '', answer: '', open: true }

  const close = content.indexOf(CLOSE)
  if (close > 0) {
    const before = content.slice(0, close)
    const after = content.slice(close + CLOSE.length)
    const onLineBoundary = /\n[ \t]*$/.test(before) || /^[ \t]*(\n|$)/.test(after)
    if (!before.includes(OPEN) && onLineBoundary && outsideCode(before)) {
      const thinking = before.trim()
      return { thinking: thinking || null, answer: after.trimStart(), open: false }
    }
  }
  return { thinking: null, answer: content, open: false }
}

/**
 * The end of a reasoning stream, for the live preview: at most `maxChars`, cut at a word or line
 * boundary so the preview never starts mid-word.
 */
export function reasoningTail(text: string, maxChars = 480): string {
  const trimmed = text.trimEnd()
  if (trimmed.length <= maxChars) return trimmed.trimStart()
  const slice = trimmed.slice(-maxChars)
  const boundary = slice.search(/[\s]/)
  return (boundary > 0 && boundary < 40 ? slice.slice(boundary) : slice).trimStart()
}

/** Whole seconds, as the live count says them: `7s`, `1m 05s`. */
export function seconds(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  if (s < 60) return `${s}s`
  return `${Math.floor(s / 60)}m ${String(s % 60).padStart(2, '0')}s`
}

/** The collapsed block's label: how long the model thought, when that is known. */
export function thoughtLabel(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms) || ms < 0) return 'Thought process'
  if (ms < 1000) return 'Thought for a moment'
  return `Thought for ${seconds(ms)}`
}
