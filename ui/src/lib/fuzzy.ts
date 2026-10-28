// Fuzzy matching for the command palette.
//
// The scoring is fzy's (John Hawthorn's): every character of the query must appear in order, and
// among all the ways it can, the best is the one that lands on word starts and runs of adjacent
// characters, with a small cost for every character skipped. That is what makes "nc" find "New
// chat" before "Unload model …cache", and "gtm" find "Go to Models" — the alignment is chosen by a
// dynamic programme rather than by taking the first letter that fits, which is the difference
// between a palette that ranks and one that merely filters.
//
// A query is split on whitespace and every word must match, in any order: "agent new" finds
// "New agent thread". Plain TypeScript with no imports, so the tests run it directly under Node.

/** A half-open `[start, end)` span of the text that the query matched. */
export type Range = readonly [number, number]

export type Match = {
  /** Higher is better. Only comparable between matches of the same query. */
  score: number
  /** Matched spans, sorted and merged — for highlighting. */
  ranges: Range[]
}

const GAP_LEADING = -0.005
const GAP_TRAILING = -0.005
const GAP_INNER = -0.01
const MATCH_CONSECUTIVE = 1.0
const MATCH_SLASH = 0.9
const MATCH_WORD = 0.8
const MATCH_CAPITAL = 0.7
const MATCH_DOT = 0.6
const MATCH_CLASS_CHANGE = 0.5

/** Past this length a text is matched as a substring only: a subsequence of a few letters is
 *  found somewhere in almost any long text, and that is noise, not a match. */
const MAX_FUZZY_LENGTH = 512

const SEPARATORS = new Set([' ', '\t', '\n', '-', '_', ':', ',', ';', '(', ')', '[', ']', '{', '}', '"', "'", '`', '·', '…', '|', '+', '@', '#'])

type CharClass = 'upper' | 'lower' | 'digit' | 'cjk' | 'sep' | 'other'

function charClass(ch: string): CharClass {
  if (SEPARATORS.has(ch) || ch === '/' || ch === '\\' || ch === '.') return 'sep'
  if (ch >= '0' && ch <= '9') return 'digit'
  const code = ch.charCodeAt(0)
  // Hiragana, katakana, CJK ideographs and Hangul have no case and no spaces between words, so
  // the only boundary there is to find is where a run of them begins.
  if ((code >= 0x3040 && code <= 0x30ff) || (code >= 0x3400 && code <= 0x9fff) || (code >= 0xac00 && code <= 0xd7af) || (code >= 0xf900 && code <= 0xfaff)) {
    return 'cjk'
  }
  if (ch !== ch.toLowerCase()) return 'upper'
  if (ch !== ch.toUpperCase()) return 'lower'
  return 'other'
}

/** The bonus for a match at each position of `text`, from the character before it. */
function bonuses(text: string): Float64Array {
  const out = new Float64Array(text.length)
  let prev = '/'
  let prevClass: CharClass = 'sep'
  for (let j = 0; j < text.length; j++) {
    const ch = text[j] as string
    const cls = charClass(ch)
    let bonus = 0
    if (prev === '/' || prev === '\\') bonus = MATCH_SLASH
    else if (prev === '.') bonus = MATCH_DOT
    else if (prevClass === 'sep') bonus = MATCH_WORD
    else if (prevClass === 'lower' && cls === 'upper') bonus = MATCH_CAPITAL
    else if (cls === 'cjk' && prevClass !== 'cjk') bonus = MATCH_WORD
    else if ((cls === 'digit') !== (prevClass === 'digit') && cls !== 'sep' && prevClass !== 'other') bonus = MATCH_CLASS_CHANGE
    out[j] = bonus
    prev = ch
    prevClass = cls
  }
  return out
}

/** Lower-cased one UTF-16 unit at a time, so every index still points at the same character
 *  (`'İ'.toLowerCase()` is two units long, and would shift every range after it). */
export function foldCase(text: string): string {
  const lower = text.toLowerCase()
  if (lower.length === text.length) return lower
  let out = ''
  for (const unit of text.split('')) {
    const folded = unit.toLowerCase()
    out += folded.length === 1 ? folded : unit
  }
  return out
}

/** Sort ranges and merge the ones that touch or overlap. */
export function mergeRanges(ranges: readonly Range[]): Range[] {
  if (ranges.length < 2) return [...ranges]
  const sorted = [...ranges].sort((a, b) => a[0] - b[0] || a[1] - b[1])
  const out: [number, number][] = []
  for (const [start, end] of sorted) {
    const last = out[out.length - 1]
    if (last && start <= last[1]) last[1] = Math.max(last[1], end)
    else out.push([start, end])
  }
  return out
}

function positionsToRanges(positions: number[]): Range[] {
  const out: [number, number][] = []
  for (const p of positions) {
    const last = out[out.length - 1]
    if (last && last[1] === p) last[1] = p + 1
    else out.push([p, p + 1])
  }
  return out
}

/** One word of a query against a text whose case is already folded. */
function matchWord(word: string, text: string, folded: string, bonus: () => Float64Array): Match | null {
  const n = word.length
  const m = text.length
  if (n === 0) return { score: 0, ranges: [] }
  if (n > m) return null

  if (m > MAX_FUZZY_LENGTH) {
    const at = folded.indexOf(word)
    return at < 0 ? null : { score: MATCH_CONSECUTIVE * (n - 1) + (bonus()[at] ?? 0), ranges: [[at, at + n]] }
  }

  // Cheap rejection first: most texts in a palette do not contain the word at all.
  let k = 0
  for (let j = 0; j < m && k < n; j++) if (folded[j] === word[k]) k++
  if (k < n) return null

  const b = bonus()
  // D[i][j]: the best score with word[i] matched exactly at text[j].
  // M[i][j]: the best score with word[0..=i] matched somewhere in text[0..=j].
  const D = new Float64Array(n * m)
  const M = new Float64Array(n * m)
  for (let i = 0; i < n; i++) {
    const gap = i === n - 1 ? GAP_TRAILING : GAP_INNER
    let prev = -Infinity
    const wi = word[i]
    for (let j = 0; j < m; j++) {
      const at = i * m + j
      if (folded[j] === wi) {
        let score = -Infinity
        if (i === 0) score = j * GAP_LEADING + (b[j] ?? 0)
        else if (j > 0) score = Math.max((M[at - m - 1] ?? -Infinity) + (b[j] ?? 0), (D[at - m - 1] ?? -Infinity) + MATCH_CONSECUTIVE)
        D[at] = score
        prev = Math.max(score, prev + gap)
        M[at] = prev
      } else {
        D[at] = -Infinity
        prev = prev + gap
        M[at] = prev
      }
    }
  }

  // Walk back from the end to recover where each character landed. Where two alignments score
  // the same, the later one is taken; a score that came from a consecutive run forces the
  // character before it to be matched too.
  const positions = new Array<number>(n)
  let required = false
  for (let i = n - 1, j = m - 1; i >= 0; i--) {
    for (; j >= 0; j--) {
      const at = i * m + j
      const d = D[at] as number
      if (d !== -Infinity && (required || d === M[at])) {
        required = i > 0 && j > 0 && M[at] === (D[at - m - 1] as number) + MATCH_CONSECUTIVE
        positions[i] = j--
        break
      }
    }
  }
  return { score: M[n * m - 1] as number, ranges: positionsToRanges(positions) }
}

/** The words of a query, case-folded. Empty when the query is blank. */
export function queryWords(query: string): string[] {
  return foldCase(query).trim().split(/\s+/).filter(Boolean)
}

export type MatchOptions = {
  /**
   * The least score each word must reach, per character of the word. Scores run from about 1 per
   * character (a run at a word start) down to 0 or below (letters scattered through the middles of
   * words), so 0.45 keeps prefixes, acronyms ("gtm" → Go to Models) and substrings, and drops
   * "mine" found as m…i…n…e across a long sentence. Unset accepts any subsequence.
   */
  minQuality?: number
}

/**
 * Match `query` against `text`. Every whitespace-separated word of the query must be found, as a
 * subsequence, anywhere in the text; the score is the sum of the words' scores, with a bonus when
 * the text starts with the whole query. `null` when some word is not there (or, with
 * `minQuality`, is only there badly).
 *
 * A blank query matches everything with score 0 and no ranges.
 */
export function fuzzyMatch(query: string, text: string, options: MatchOptions = {}): Match | null {
  const words = queryWords(query)
  if (words.length === 0) return { score: 0, ranges: [] }
  const folded = foldCase(text)
  let cached: Float64Array | null = null
  const bonus = () => (cached ??= bonuses(text))
  let score = 0
  const ranges: Range[] = []
  for (const word of words) {
    const match = matchWord(word, text, folded, bonus)
    if (!match) return null
    if (options.minQuality !== undefined && match.score < options.minQuality * word.length) return null
    score += match.score
    ranges.push(...match.ranges)
  }
  const whole = words.join(' ')
  if (folded.startsWith(whole)) score += folded.length === whole.length ? 2 : 1
  return { score, ranges: mergeRanges(ranges) }
}

/** Cut `text` into the runs a highlighter draws: matched and not. */
export function highlightParts(text: string, ranges: readonly Range[]): { text: string; hit: boolean }[] {
  const parts: { text: string; hit: boolean }[] = []
  let at = 0
  for (const [start, end] of ranges) {
    const s = Math.max(at, Math.min(start, text.length))
    const e = Math.min(end, text.length)
    if (e <= s) continue
    if (s > at) parts.push({ text: text.slice(at, s), hit: false })
    parts.push({ text: text.slice(s, e), hit: true })
    at = e
  }
  if (at < text.length) parts.push({ text: text.slice(at), hit: false })
  return parts
}

export type Snippet = { text: string; ranges: Range[] }

/**
 * A short excerpt of `text` around the first place every word of `query` occurs — for showing
 * where a long message matched. Words are matched literally (case-insensitively), not fuzzily:
 * in a long text a scattered subsequence is always somewhere, and finding it tells no one
 * anything. Whitespace in the excerpt is collapsed to single spaces. `null` when a word is
 * missing.
 *
 * `folded` may be passed when the caller keeps the case-folded text around (the palette caches
 * it per message), so a keystroke does not fold every message again.
 */
export function findSnippet(text: string, query: string, options: { before?: number; after?: number; folded?: string } = {}): Snippet | null {
  const words = queryWords(query)
  if (words.length === 0) return null
  const folded = options.folded ?? foldCase(text)
  let first = -1
  let firstWord = ''
  for (const word of words) {
    const at = folded.indexOf(word)
    if (at < 0) return null
    if (first < 0 || at < first) {
      first = at
      firstWord = word
    }
  }
  const before = options.before ?? 32
  const after = options.after ?? 72
  let start = Math.max(0, first - before)
  let end = Math.min(text.length, first + firstWord.length + after)
  // Start on a word boundary when one is close, so the excerpt does not open mid-word.
  if (start > 0 && !/\s/.test(text[start - 1] ?? '')) {
    const space = text.slice(start, first).search(/\s/)
    if (space >= 0 && space < 12) start += space + 1
  }
  if (end < text.length) {
    const space = text.indexOf(' ', end)
    if (space >= 0 && space - end < 12) end = space
  }

  // Collapse whitespace in the window while remembering where each kept character came from.
  let out = start > 0 ? '…' : ''
  const map: number[] = []
  let lastSpace = out.length === 0
  for (let i = start; i < end; i++) {
    const ch = text[i] as string
    if (/\s/.test(ch)) {
      if (lastSpace) continue
      map.push(i)
      out += ' '
      lastSpace = true
    } else {
      map.push(i)
      out += ch
      lastSpace = false
    }
  }
  out = out.replace(/\s+$/, '')
  if (end < text.length) out += '…'
  const offset = start > 0 ? 1 : 0

  const window = folded.slice(start, end)
  const ranges: Range[] = []
  for (const word of words) {
    let from = 0
    for (;;) {
      const at = window.indexOf(word, from)
      if (at < 0) break
      const s = map.indexOf(start + at)
      const e = map.indexOf(start + at + word.length - 1)
      if (s >= 0 && e >= 0) ranges.push([s + offset, e + offset + 1])
      from = at + word.length
    }
  }
  return { text: out, ranges: mergeRanges(ranges) }
}
