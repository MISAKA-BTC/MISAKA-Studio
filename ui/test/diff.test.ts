import assert from 'node:assert/strict'
import { test } from 'node:test'
import { applyHunks, diffStats, lineCount, parsePatch, patchStats, splitLines, structuredDiff, unifiedPatch } from '../src/lib/diff.ts'

const lines = (count: number, label = 'line') => Array.from({ length: count }, (_, i) => `${label} ${i + 1}\n`).join('')

test('identical texts have no hunks', () => {
  const text = lines(10)
  const diff = structuredDiff(text, text)
  assert.equal(diff.hunks.length, 0)
  assert.deepEqual(diffStats(text, text), { added: 0, removed: 0 })
  assert.equal(unifiedPatch('a.txt', text, text), '--- a/a.txt\n+++ b/a.txt\n')
})

test('one changed line in the middle: one hunk, three lines of context either side', () => {
  const before = lines(20)
  const after = before.replace('line 10\n', 'line ten\n')
  const diff = structuredDiff(before, after)
  assert.equal(diff.hunks.length, 1)
  const hunk = diff.hunks[0]!
  assert.deepEqual([hunk.oldStart, hunk.oldLines, hunk.newStart, hunk.newLines], [7, 7, 7, 7])
  assert.deepEqual(
    hunk.lines.map((l) => `${l.kind[0]}${l.oldLine ?? '-'}:${l.newLine ?? '-'} ${l.text}`),
    ['c7:7 line 7', 'c8:8 line 8', 'c9:9 line 9', 'd10:- line 10', 'a-:10 line ten', 'c11:11 line 11', 'c12:12 line 12', 'c13:13 line 13'],
  )
  assert.equal(
    unifiedPatch('src/a.txt', before, after),
    '--- a/src/a.txt\n+++ b/src/a.txt\n@@ -7,7 +7,7 @@\n line 7\n line 8\n line 9\n-line 10\n+line ten\n line 11\n line 12\n line 13\n',
  )
  assert.deepEqual(diffStats(before, after), { added: 1, removed: 1 })
})

test('removed lines come before added ones in a replacement', () => {
  const diff = structuredDiff('a\nb\nc\n', 'a\nx\ny\nc\n', 0)
  assert.deepEqual(
    diff.hunks[0]!.lines.map((l) => `${l.kind}:${l.text}`),
    ['del:b', 'add:x', 'add:y'],
  )
})

test('changes close together share a hunk; far apart they do not', () => {
  const before = lines(40)
  const near = before.replace('line 10\n', 'X\n').replace('line 16\n', 'Y\n') // 5 unchanged between
  assert.equal(structuredDiff(before, near).hunks.length, 1)
  const exactlySix = before.replace('line 10\n', 'X\n').replace('line 17\n', 'Y\n') // 6 unchanged between
  assert.equal(structuredDiff(before, exactlySix).hunks.length, 1)
  const far = before.replace('line 10\n', 'X\n').replace('line 18\n', 'Y\n') // 7 unchanged between
  assert.equal(structuredDiff(before, far).hunks.length, 2)
})

test('a created file is all additions from /dev/null', () => {
  const patch = unifiedPatch('new.rs', null, 'fn main() {}\n')
  assert.equal(patch, '--- /dev/null\n+++ b/new.rs\n@@ -0,0 +1 @@\n+fn main() {}\n')
  const parsed = parsePatch(patch)
  assert.equal(parsed.created, true)
  assert.equal(parsed.deleted, false)
  assert.deepEqual(patchStats(patch), { added: 1, removed: 0 })
})

test('a deleted file is all removals to /dev/null', () => {
  const patch = unifiedPatch('old.rs', 'a\nb\n', null)
  assert.equal(patch, '--- a/old.rs\n+++ /dev/null\n@@ -1,2 +0,0 @@\n-a\n-b\n')
  assert.equal(parsePatch(patch).deleted, true)
})

test('a last line that gains its newline is a change, marked in the patch', () => {
  const patch = unifiedPatch('a.txt', 'a\nb', 'a\nb\n')
  assert.equal(patch, '--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n a\n-b\n\\ No newline at end of file\n+b\n')
  const parsed = parsePatch(patch)
  const del = parsed.hunks[0]!.lines.find((l) => l.kind === 'del')!
  assert.equal(del.noEol, true)
  assert.equal(applyHunks('a\nb', parsed), 'a\nb\n')
})

test('insertion at the top of a file and deletion at its end', () => {
  const before = lines(8)
  const after = `header\n${before.split('\n').slice(0, 7).join('\n')}\n`
  const diff = structuredDiff(before, after)
  assert.equal(applyHunks(before, diff), after)
  assert.equal(diff.hunks[0]!.oldStart, 1)
  assert.deepEqual(diffStats(before, after), { added: 1, removed: 1 })
})

test('a patch reads back to the same hunks and line numbers', () => {
  const before = lines(60)
  const after = before.replace('line 3\n', '').replace('line 30\n', 'thirty\nthirty-and-a-half\n').replace('line 58\n', 'line 58 changed\n')
  const diff = structuredDiff(before, after)
  const parsed = parsePatch(unifiedPatch('f.txt', before, after))
  assert.deepEqual(parsed.hunks, diff.hunks)
  assert.equal(applyHunks(before, parsed), after)
})

/** A small deterministic generator, so a failure here reproduces. */
function random(seed: number) {
  let s = seed >>> 0
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0
    return s / 2 ** 32
  }
}

test('every diff applies back to its target, on hundreds of random edits', () => {
  const next = random(42)
  const pick = (n: number) => Math.floor(next() * n)
  const alphabet = ['a', 'b', 'c', 'd', 'e', '']
  for (let round = 0; round < 400; round++) {
    const base = Array.from({ length: pick(30) }, () => alphabet[pick(alphabet.length)]!)
    const edited = [...base]
    for (let edits = pick(6); edits > 0; edits--) {
      const at = pick(edited.length + 1)
      const kind = pick(3)
      if (kind === 0) edited.splice(at, 0, alphabet[pick(alphabet.length)]!)
      else if (kind === 1) edited.splice(at, 1)
      else edited[at] = 'z'
    }
    const join = (xs: string[], eol: boolean) => (xs.length ? xs.join('\n') + (eol ? '\n' : '') : '')
    const before = join(base, pick(2) === 0)
    const after = join(edited, pick(2) === 0)
    for (const context of [0, 1, 3]) {
      const diff = structuredDiff(before, after, context)
      assert.equal(applyHunks(before, diff), after, `round ${round}, context ${context}: ${JSON.stringify({ before, after })}`)
      assert.equal(applyHunks(before, parsePatch(unifiedPatch('f', before, after, context))), after)
    }
  }
})

test('the diff is minimal on a classic example', () => {
  // ABCABBA → CBABAC: the shortest edit script has 5 edits (Myers 1986, figure 1).
  const before = 'A\nB\nC\nA\nB\nB\nA\n'
  const after = 'C\nB\nA\nB\nA\nC\n'
  const { added, removed } = diffStats(before, after)
  assert.equal(added + removed, 5)
})

test('a huge rewrite settles for one replacement, quickly, and still applies', () => {
  const before = lines(20_000, 'old')
  const after = lines(20_000, 'new')
  const started = performance.now()
  const diff = structuredDiff(before, after)
  const elapsed = performance.now() - started
  assert.equal(diff.approximate, true)
  assert.equal(diff.added, 20_000)
  assert.equal(diff.removed, 20_000)
  assert.equal(applyHunks(before, diff), after)
  assert.ok(elapsed < 3000, `took ${elapsed.toFixed(0)} ms`)
})

test('a small edit to a long file is exact and fast', () => {
  const before = lines(5_000)
  const after = before.replace('line 2500\n', 'changed\n').replace('line 4000\n', '')
  const started = performance.now()
  const diff = structuredDiff(before, after)
  assert.ok(performance.now() - started < 500)
  assert.equal(diff.approximate, false)
  assert.deepEqual([diff.added, diff.removed], [1, 2])
  assert.equal(diff.oldTotal, 5_000)
  assert.equal(diff.newTotal, 4_999)
})

test('line splitting keeps endings and counts like an editor', () => {
  assert.deepEqual(splitLines('a\nb'), ['a\n', 'b'])
  assert.deepEqual(splitLines('a\r\nb\n'), ['a\r\n', 'b\n'])
  assert.deepEqual(splitLines(''), [])
  assert.equal(lineCount('a\nb\n'), 2)
  assert.equal(lineCount(null), 0)
})

test('CRLF lines are compared with their endings but drawn without them', () => {
  const diff = structuredDiff('a\r\nb\r\n', 'a\r\nc\r\n', 0)
  assert.deepEqual(
    diff.hunks[0]!.lines.map((l) => l.text),
    ['b', 'c'],
  )
})
