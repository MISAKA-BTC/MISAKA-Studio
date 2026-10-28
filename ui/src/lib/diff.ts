// Line diffs: what an edit changed, as a unified patch to keep and as hunks to draw.
//
// The coding agent stores a patch per edit rather than two copies of the file — a conversation
// lives in localStorage, and a thread that touched a 2,000-line file twenty times must not hold
// forty copies of it. The diff view draws the same patch back, so the two forms round-trip:
// `structuredDiff` → `unifiedPatch` → `parsePatch` gives the same hunks and line numbers.
//
// The algorithm is Myers' O(ND) on lines, after the common prefix and suffix are set aside — an
// agent's edit touches a few lines of a long file, and the cost is in what differs. When what
// differs is too large to search (a rewritten 50,000-line file), the middle is reported as one
// replacement: still a correct patch, only not a minimal one, and the caller is told so.
//
// Lines keep their line endings while they are compared, so a last line that gained or lost its
// newline is a change (as git sees it) and is marked "\ No newline at end of file" in the patch.

export type DiffLineKind = 'context' | 'add' | 'del'

export type DiffLine = {
  kind: DiffLineKind
  /** The line's text, without its line ending. */
  text: string
  /** 1-based line number in the old file; null for an added line. */
  oldLine: number | null
  /** 1-based line number in the new file; null for a removed line. */
  newLine: number | null
  /** The file's last line, with no newline after it. */
  noEol?: boolean
}

export type DiffHunk = {
  /** Unified-format numbers: when a side has no lines, its start is the line *before* the hunk. */
  oldStart: number
  oldLines: number
  newStart: number
  newLines: number
  lines: DiffLine[]
}

export type FileDiff = {
  hunks: DiffHunk[]
  added: number
  removed: number
  /** Lines in the old and the new file (from a parsed patch: as far as the patch says). */
  oldTotal: number
  newTotal: number
  /** The difference was too large to search, and is shown as one block replaced by another. */
  approximate: boolean
}

export type ParsedPatch = FileDiff & { created: boolean; deleted: boolean }

const EQUAL = 0
const DEL = 1
const ADD = 2

/** A text's lines, each keeping its `\n` (the last one has none when the file does not end in one). */
export function splitLines(text: string): string[] {
  const lines: string[] = []
  let start = 0
  while (start < text.length) {
    const newline = text.indexOf('\n', start)
    if (newline === -1) {
      lines.push(text.slice(start))
      break
    }
    lines.push(text.slice(start, newline + 1))
    start = newline + 1
  }
  return lines
}

/** The number of lines in `text`, counted the way an editor does. */
export function lineCount(text: string | null): number {
  return text ? splitLines(text).length : 0
}

function displayText(line: string): string {
  return line.endsWith('\r\n') ? line.slice(0, -2) : line.endsWith('\n') ? line.slice(0, -1) : line
}

/**
 * The furthest a diff may search before it settles for "replaced": the work is the lines that
 * differ times the edit distance, and the memory the edit distance squared — a few megabytes at
 * the cap, a few milliseconds for a real edit.
 */
function searchBudget(size: number): number {
  return Math.max(64, Math.min(1500, size, Math.floor(30_000_000 / Math.max(1, size))))
}

/**
 * Myers' shortest edit script between `a` and `b` (interned lines), or `null` past `maxD` edits.
 *
 * `v[k]` is the furthest x reached on diagonal k = x − y. Each round extends every diagonal by one
 * edit — a deletion (right) or an insertion (down), deletions preferred on a tie so removed lines
 * come before added ones, as every diff tool prints them — then follows the "snake" of equal lines.
 * A move that would leave the grid is not taken, which is what keeps every point a real position.
 * The state after each round is kept for the walk back.
 */
function myers(a: Int32Array, b: Int32Array, maxD: number): Uint8Array | null {
  const n = a.length
  const m = b.length
  if (n === 0 && m === 0) return new Uint8Array(0)
  const offset = maxD + 1
  const v = new Int32Array(2 * maxD + 3).fill(-1)
  const trace: Int32Array[] = []
  for (let d = 0; d <= maxD; d++) {
    for (let k = -d; k <= d; k += 2) {
      let x: number
      if (d === 0) {
        x = 0
      } else {
        const down = k + 1 <= d - 1 ? v[offset + k + 1]! : -1
        const right = k - 1 >= -(d - 1) ? v[offset + k - 1]! : -1
        const canDown = down >= 0 && down - k <= m
        const canRight = right >= 0 && right + 1 <= n
        if (canDown && (!canRight || right < down)) x = down
        else if (canRight) x = right + 1
        else {
          v[offset + k] = -1
          continue
        }
      }
      let y = x - k
      while (x < n && y < m && a[x] === b[y]) {
        x++
        y++
      }
      v[offset + k] = x
      if (x >= n && y >= m) {
        trace.push(v.slice(offset - d, offset + d + 1))
        return walkBack(trace, n, m)
      }
    }
    trace.push(v.slice(offset - d, offset + d + 1))
  }
  return null
}

/** The edit script, from the rounds `myers` kept: from (n, m) back to (0, 0), one edit per round. */
function walkBack(trace: Int32Array[], n: number, m: number): Uint8Array {
  const ops: number[] = []
  let x = n
  let y = m
  for (let d = trace.length - 1; d > 0; d--) {
    const previous = trace[d - 1]!
    const at = (k: number) => (k >= -(d - 1) && k <= d - 1 ? previous[k + d - 1]! : -1)
    const k = x - y
    const down = at(k + 1)
    const right = at(k - 1)
    const canDown = down >= 0 && down - k <= m
    const canRight = right >= 0 && right + 1 <= n
    // The same choice the forward pass made from the same state.
    const fromDown = canDown && (!canRight || right < down)
    const prevK = fromDown ? k + 1 : k - 1
    const prevX = fromDown ? down : right
    const moveX = fromDown ? prevX : prevX + 1
    while (x > moveX) {
      ops.push(EQUAL)
      x--
      y--
    }
    ops.push(fromDown ? ADD : DEL)
    x = prevX
    y = prevX - prevK
  }
  while (x > 0) {
    ops.push(EQUAL)
    x--
  }
  return Uint8Array.from(ops.reverse())
}

/** The edit script between two lists of lines. */
function editScript(a: string[], b: string[]): { ops: Uint8Array; approximate: boolean } {
  const n = a.length
  const m = b.length
  let prefix = 0
  while (prefix < n && prefix < m && a[prefix] === b[prefix]) prefix++
  let suffix = 0
  while (suffix < n - prefix && suffix < m - prefix && a[n - 1 - suffix] === b[m - 1 - suffix]) suffix++

  // Interned, so the inner loop compares integers rather than strings.
  const ids = new Map<string, number>()
  const intern = (line: string) => {
    let id = ids.get(line)
    if (id === undefined) {
      id = ids.size
      ids.set(line, id)
    }
    return id
  }
  const midA = Int32Array.from(a.slice(prefix, n - suffix), intern)
  const midB = Int32Array.from(b.slice(prefix, m - suffix), intern)
  let middle = myers(midA, midB, searchBudget(midA.length + midB.length))
  const approximate = middle === null
  if (middle === null) {
    middle = new Uint8Array(midA.length + midB.length)
    middle.fill(DEL, 0, midA.length)
    middle.fill(ADD, midA.length)
  }
  const ops = new Uint8Array(prefix + middle.length + suffix) // EQUAL is 0: the prefix and suffix
  ops.set(middle, prefix)
  return { ops, approximate }
}

/**
 * The difference between two versions of a file, as hunks with `context` unchanged lines around
 * each change. `null` is a file that does not exist (created, or deleted).
 */
export function structuredDiff(before: string | null, after: string | null, context = 3): FileDiff {
  const a = splitLines(before ?? '')
  const b = splitLines(after ?? '')
  const { ops, approximate } = editScript(a, b)

  // Which ops each hunk covers: every change, with `context` lines either side, merged with the
  // next when the unchanged run between them is too short to be worth hiding.
  const ranges: { start: number; end: number }[] = []
  let added = 0
  let removed = 0
  for (let i = 0; i < ops.length; i++) {
    const op = ops[i]
    if (op === EQUAL) continue
    if (op === ADD) added++
    else removed++
    const start = Math.max(0, i - context)
    const end = Math.min(ops.length, i + 1 + context)
    const last = ranges[ranges.length - 1]
    if (last && start <= last.end) last.end = Math.max(last.end, end)
    else ranges.push({ start, end })
  }

  const hunks: DiffHunk[] = []
  let oldNo = 0
  let newNo = 0
  let r = 0
  let current: DiffHunk | null = null
  for (let i = 0; i < ops.length && r < ranges.length; i++) {
    const range = ranges[r]!
    if (i === range.start) current = { oldStart: oldNo + 1, oldLines: 0, newStart: newNo + 1, newLines: 0, lines: [] }
    const op = ops[i]
    if (current) {
      if (op === EQUAL) {
        const line = a[oldNo]!
        current.lines.push({ kind: 'context', text: displayText(line), oldLine: oldNo + 1, newLine: newNo + 1, ...(line.endsWith('\n') ? {} : { noEol: true }) })
        current.oldLines++
        current.newLines++
      } else if (op === DEL) {
        const line = a[oldNo]!
        current.lines.push({ kind: 'del', text: displayText(line), oldLine: oldNo + 1, newLine: null, ...(line.endsWith('\n') ? {} : { noEol: true }) })
        current.oldLines++
      } else {
        const line = b[newNo]!
        current.lines.push({ kind: 'add', text: displayText(line), oldLine: null, newLine: newNo + 1, ...(line.endsWith('\n') ? {} : { noEol: true }) })
        current.newLines++
      }
    }
    if (op !== ADD) oldNo++
    if (op !== DEL) newNo++
    if (current && i === range.end - 1) {
      if (current.oldLines === 0) current.oldStart -= 1
      if (current.newLines === 0) current.newStart -= 1
      hunks.push(current)
      current = null
      r++
    }
  }
  return { hunks, added, removed, oldTotal: a.length, newTotal: b.length, approximate }
}

/** Lines added and removed between two versions of a file. */
export function diffStats(before: string | null, after: string | null): { added: number; removed: number } {
  const { added, removed } = structuredDiff(before, after, 0)
  return { added, removed }
}

function rangeText(start: number, count: number): string {
  return count === 1 ? String(start) : `${start},${count}`
}

/** Hunks as unified-diff text, under `--- a/path` / `+++ b/path` headers. */
export function formatPatch(path: string, diff: FileDiff, options: { created?: boolean; deleted?: boolean } = {}): string {
  const out = [`--- ${options.created ? '/dev/null' : `a/${path}`}`, `+++ ${options.deleted ? '/dev/null' : `b/${path}`}`]
  for (const hunk of diff.hunks) {
    out.push(`@@ -${rangeText(hunk.oldStart, hunk.oldLines)} +${rangeText(hunk.newStart, hunk.newLines)} @@`)
    for (const line of hunk.lines) {
      out.push(`${line.kind === 'add' ? '+' : line.kind === 'del' ? '-' : ' '}${line.text}`)
      if (line.noEol) out.push('\\ No newline at end of file')
    }
  }
  return `${out.join('\n')}\n`
}

/** A unified patch from `before` to `after`. `null` before is a created file; `null` after, a deleted one. */
export function unifiedPatch(path: string, before: string | null, after: string | null, context = 3): string {
  return formatPatch(path, structuredDiff(before, after, context), { created: before === null, deleted: after === null })
}

/**
 * A unified patch read back into hunks with line numbers — what the diff view draws for an edit
 * whose patch is all that was kept. Lenient about what it does not understand: a line outside any
 * hunk is a header or noise, and is skipped.
 */
export function parsePatch(patch: string): ParsedPatch {
  const hunks: DiffHunk[] = []
  let created = false
  let deleted = false
  let added = 0
  let removed = 0
  let current: DiffHunk | null = null
  let nextOld = 0
  let nextNew = 0
  let remainingOld = 0
  let remainingNew = 0
  let oldTotal = 0
  let newTotal = 0
  const lines = patch.split('\n')
  if (lines[lines.length - 1] === '') lines.pop()
  for (const raw of lines) {
    const header = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/.exec(raw)
    if (header) {
      const oldStart = Number(header[1])
      const oldLines = header[2] === undefined ? 1 : Number(header[2])
      const newStart = Number(header[3])
      const newLines = header[4] === undefined ? 1 : Number(header[4])
      current = { oldStart, oldLines, newStart, newLines, lines: [] }
      hunks.push(current)
      nextOld = oldLines === 0 ? oldStart + 1 : oldStart
      nextNew = newLines === 0 ? newStart + 1 : newStart
      remainingOld = oldLines
      remainingNew = newLines
      oldTotal = Math.max(oldTotal, oldLines === 0 ? oldStart : oldStart + oldLines - 1)
      newTotal = Math.max(newTotal, newLines === 0 ? newStart : newStart + newLines - 1)
      continue
    }
    if (current && raw.startsWith('\\')) {
      const last = current.lines[current.lines.length - 1]
      if (last) last.noEol = true
      continue
    }
    if (current && (remainingOld > 0 || remainingNew > 0)) {
      const tag = raw.charAt(0)
      const text = raw.slice(1)
      if (tag === '+' && remainingNew > 0) {
        current.lines.push({ kind: 'add', text, oldLine: null, newLine: nextNew++ })
        remainingNew--
        added++
      } else if (tag === '-' && remainingOld > 0) {
        current.lines.push({ kind: 'del', text, oldLine: nextOld++, newLine: null })
        remainingOld--
        removed++
      } else if (tag === ' ' || raw === '') {
        // An empty context line, from a tool that strips trailing whitespace, is still one.
        current.lines.push({ kind: 'context', text, oldLine: nextOld++, newLine: nextNew++ })
        remainingOld--
        remainingNew--
      }
      continue
    }
    if (raw.startsWith('--- ')) created = raw.slice(4).trim() === '/dev/null'
    else if (raw.startsWith('+++ ')) deleted = raw.slice(4).trim() === '/dev/null'
  }
  return { hunks, added, removed, oldTotal, newTotal, approximate: false, created, deleted }
}

/** Lines added and removed by a patch. */
export function patchStats(patch: string): { added: number; removed: number } {
  let added = 0
  let removed = 0
  for (const hunk of parsePatch(patch).hunks) {
    for (const line of hunk.lines) {
      if (line.kind === 'add') added++
      else if (line.kind === 'del') removed++
    }
  }
  return { added, removed }
}

/** Apply hunks to `before` — the inverse the tests check every diff against. */
export function applyHunks(before: string, diff: Pick<FileDiff, 'hunks'>): string {
  const a = splitLines(before)
  const out: string[] = []
  let at = 0
  for (const hunk of diff.hunks) {
    const first = hunk.oldLines === 0 ? hunk.oldStart : hunk.oldStart - 1
    while (at < first) out.push(a[at++]!)
    for (const line of hunk.lines) {
      if (line.kind === 'context') out.push(a[at++]!)
      else if (line.kind === 'del') at++
      else out.push(line.noEol ? line.text : `${line.text}\n`)
    }
  }
  while (at < a.length) out.push(a[at++]!)
  return out.join('')
}
