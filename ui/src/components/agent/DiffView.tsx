// A unified diff, drawn: old and new line numbers, removed lines then added ones, the unchanged
// runs between hunks folded away.
//
// Two sources. A tool call's card has only the patch it kept (`patch`), so the folds there are
// counts. The Changes panel has the file's original and current text (`before`/`after`), so its
// folds open — the unchanged lines are read from the original, where they are the same by
// definition.
//
// The gutters stay put while long lines scroll sideways; their tints are opaque for that reason
// (a translucent gutter would show the text sliding underneath it).

import { useMemo, useState } from 'react'
import { parsePatch, splitLines, structuredDiff, type DiffHunk, type DiffLine, type FileDiff } from '../../lib/diff'

/** More lines than this are drawn only on request — a diff view is not a file viewer. */
const RENDER_LIMIT = 1500

type Row =
  | { kind: 'line'; line: DiffLine }
  | { kind: 'hunk'; hunk: DiffHunk }
  /** Unchanged lines left out between hunks: `count` when known, `from` its first old line. */
  | { kind: 'gap'; id: string; count: number | null; oldFrom: number; newFrom: number }

const TONE = {
  add: {
    row: 'bg-emerald-50 text-emerald-950 dark:bg-[oklch(0.22_0.045_160)] dark:text-emerald-100',
    gutter: 'bg-emerald-100 text-emerald-700 dark:bg-[oklch(0.26_0.055_160)] dark:text-emerald-300',
    sign: '+',
  },
  del: {
    row: 'bg-red-50 text-red-950 dark:bg-[oklch(0.22_0.05_22)] dark:text-red-100',
    gutter: 'bg-red-100 text-red-700 dark:bg-[oklch(0.26_0.06_22)] dark:text-red-300',
    sign: '−',
  },
  context: {
    row: 'bg-white text-ink-700 dark:bg-ink-900 dark:text-ink-300',
    gutter: 'bg-ink-50 text-ink-400 dark:bg-ink-850 dark:text-ink-500',
    sign: ' ',
  },
} as const

function oldEnd(hunk: DiffHunk): number {
  return hunk.oldLines === 0 ? hunk.oldStart : hunk.oldStart + hunk.oldLines - 1
}

function newEnd(hunk: DiffHunk): number {
  return hunk.newLines === 0 ? hunk.newStart : hunk.newStart + hunk.newLines - 1
}

function firstOld(hunk: DiffHunk): number {
  return hunk.oldLines === 0 ? hunk.oldStart + 1 : hunk.oldStart
}

/** The rows to draw: hunks, their lines, and the folds before, between and after them. */
function rowsOf(diff: FileDiff, knownTotal: boolean): Row[] {
  const rows: Row[] = []
  diff.hunks.forEach((hunk, index) => {
    const previous = diff.hunks[index - 1]
    const gapOld = previous ? oldEnd(previous) + 1 : 1
    const gapNew = previous ? newEnd(previous) + 1 : 1
    const count = firstOld(hunk) - gapOld
    if (count > 0) rows.push({ kind: 'gap', id: `g${index}`, count, oldFrom: gapOld, newFrom: gapNew })
    rows.push({ kind: 'hunk', hunk })
    for (const line of hunk.lines) rows.push({ kind: 'line', line })
  })
  // After the last hunk only a full diff knows whether anything follows; a patch does not say.
  const last = diff.hunks[diff.hunks.length - 1]
  if (last && knownTotal) {
    const tailFrom = oldEnd(last) + 1
    const tail = diff.oldTotal - tailFrom + 1
    if (tail > 0) rows.push({ kind: 'gap', id: 'tail', count: tail, oldFrom: tailFrom, newFrom: newEnd(last) + 1 })
  }
  return rows
}

function Gutter({ value, tone }: { value: number | null; tone: string }) {
  return <span className={`w-10 shrink-0 select-none px-1.5 text-right tabular-nums ${tone}`}>{value ?? ''}</span>
}

function LineRow({ line }: { line: DiffLine }) {
  const tone = TONE[line.kind]
  return (
    <div className={`flex min-w-full w-max ${tone.row}`}>
      <div className={`sticky left-0 z-[1] flex ${tone.gutter}`}>
        <Gutter value={line.oldLine} tone="" />
        <Gutter value={line.newLine} tone="" />
      </div>
      <span className="w-5 shrink-0 select-none text-center opacity-70">{tone.sign}</span>
      <span className="whitespace-pre pr-6">
        {line.text || ' '}
        {line.noEol && <span className="ml-2 select-none text-[0.65rem] italic opacity-60">no newline at end</span>}
      </span>
    </div>
  )
}

export function DiffView({
  patch,
  before,
  after,
  className = '',
  maxHeight = '24rem',
}: {
  /** A unified patch — what a tool call kept. */
  patch?: string
  /** Or both versions of the file, when they are at hand: the folds can then be opened. */
  before?: string | null
  after?: string | null
  className?: string
  maxHeight?: string
}) {
  const full = before !== undefined || after !== undefined
  const diff = useMemo<FileDiff>(() => (full ? structuredDiff(before ?? null, after ?? null) : parsePatch(patch ?? '')), [full, patch, before, after])
  const oldLines = useMemo(() => (full && before ? splitLines(before) : null), [full, before])
  const [opened, setOpened] = useState<Set<string>>(() => new Set())
  const [showAll, setShowAll] = useState(false)

  const rows = useMemo(() => {
    const base = rowsOf(diff, full)
    if (!oldLines || opened.size === 0) return base
    // An opened fold is the original's lines, numbered on both sides.
    return base.flatMap((row): Row[] => {
      if (row.kind !== 'gap' || !opened.has(row.id)) return [row]
      const count = row.count ?? 0
      return Array.from({ length: count }, (_, i) => {
        const text = oldLines[row.oldFrom - 1 + i] ?? ''
        return { kind: 'line' as const, line: { kind: 'context' as const, text: text.replace(/\r?\n$/, ''), oldLine: row.oldFrom + i, newLine: row.newFrom + i } }
      })
    })
  }, [diff, full, oldLines, opened])

  if (diff.hunks.length === 0) {
    return <div className={`px-3 py-2 text-xs text-ink-500 dark:text-ink-400 ${className}`}>No changes.</div>
  }

  let drawn = 0
  const limited = !showAll && rows.filter((r) => r.kind === 'line').length > RENDER_LIMIT
  const visible: Row[] = []
  for (const row of rows) {
    if (limited && row.kind === 'line' && drawn >= RENDER_LIMIT) break
    if (row.kind === 'line') drawn++
    visible.push(row)
  }
  const hidden = rows.filter((r) => r.kind === 'line').length - drawn

  return (
    <div className={`mono overflow-auto text-[0.75rem] leading-5 ${className}`} style={{ maxHeight }}>
      {diff.approximate && (
        <div className="sticky left-0 bg-amber-50 px-3 py-1 text-[0.7rem] text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          Too different to match line by line — shown as one block replaced by another.
        </div>
      )}
      {visible.map((row, index) => {
        if (row.kind === 'line') return <LineRow key={index} line={row.line} />
        if (row.kind === 'hunk') {
          return (
            <div key={index} className="flex min-w-full w-max bg-ink-50 text-[0.7rem] text-ink-400 dark:bg-ink-850 dark:text-ink-500">
              <span className="sticky left-0 select-none px-3 py-0.5">
                @@ −{row.hunk.oldStart},{row.hunk.oldLines} +{row.hunk.newStart},{row.hunk.newLines} @@
              </span>
            </div>
          )
        }
        const expandable = oldLines !== null && row.count !== null && row.count > 0
        const label = row.count === null ? '⋯' : `⋯ ${row.count.toLocaleString('en-US')} unchanged line${row.count === 1 ? '' : 's'}`
        return (
          <div key={index} className="flex min-w-full w-max bg-ink-50/70 text-[0.7rem] text-ink-400 dark:bg-ink-900 dark:text-ink-500">
            {expandable ? (
              <button
                type="button"
                className="sticky left-0 px-3 py-0.5 text-left hover:text-arc-600 dark:hover:text-arc-400"
                onClick={() => setOpened((s) => new Set(s).add(row.id))}
                title="Show these lines"
              >
                {label}
              </button>
            ) : (
              <span className="sticky left-0 select-none px-3 py-0.5">{label}</span>
            )}
          </div>
        )
      })}
      {hidden > 0 && (
        <div className="sticky left-0 px-3 py-1.5">
          <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => setShowAll(true)}>
            Show {hidden.toLocaleString('en-US')} more lines
          </button>
        </div>
      )}
    </div>
  )
}

/** "+12 −3", tinted — the size of a change at a glance. */
export function DiffStat({ added, removed, className = '' }: { added: number; removed: number; className?: string }) {
  return (
    <span className={`mono inline-flex shrink-0 items-center gap-1 text-[0.7rem] tabular-nums ${className}`}>
      {added > 0 && <span className="text-emerald-600 dark:text-emerald-400">+{added}</span>}
      {removed > 0 && <span className="text-red-600 dark:text-red-400">−{removed}</span>}
      {added === 0 && removed === 0 && <span className="text-ink-400">±0</span>}
    </span>
  )
}
