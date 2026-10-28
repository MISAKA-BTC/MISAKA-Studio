import assert from 'node:assert/strict'
import { test } from 'node:test'
import { findSnippet, foldCase, fuzzyMatch, highlightParts, mergeRanges } from '../src/lib/fuzzy.ts'

const score = (query: string, text: string) => fuzzyMatch(query, text)?.score ?? -Infinity
const hits = (text: string, query: string) =>
  highlightParts(text, fuzzyMatch(query, text)?.ranges ?? [])
    .filter((part) => part.hit)
    .map((part) => part.text)

test('every character of the query must appear, in order', () => {
  assert.equal(fuzzyMatch('xyz', 'New chat'), null)
  assert.equal(fuzzyMatch('tahc', 'New chat'), null, 'order matters within a word')
  assert.notEqual(fuzzyMatch('nwct', 'New chat'), null)
})

test('a blank query matches everything, with nothing to highlight', () => {
  assert.deepEqual(fuzzyMatch('', 'New chat'), { score: 0, ranges: [] })
  assert.deepEqual(fuzzyMatch('   ', 'New chat'), { score: 0, ranges: [] })
})

test('word starts beat letters in the middle of words', () => {
  assert.ok(score('nc', 'New chat') > score('nc', 'Unicode'))
  assert.deepEqual(hits('Go to Models', 'gtm'), ['G', 't', 'M'])
})

test('a contiguous run beats the same letters spread over word starts', () => {
  assert.ok(score('mod', 'Models') > score('mod', 'Mining on demand'))
  assert.ok(score('chat', 'Go to Chat') > score('chat', 'Clear the history and thread'))
})

test('the alignment is the best one, not the first that fits', () => {
  // Greedy matching takes the leading "a" and then the "b"; the run at the end scores higher.
  assert.deepEqual(fuzzyMatch('ab', 'a xab')?.ranges, [[3, 5]])
})

test('matching ignores case and highlights exactly the matched text', () => {
  assert.deepEqual(fuzzyMatch('MODELS', 'Go to Models')?.ranges, [[6, 12]])
  assert.deepEqual(hits('Load model Huihui-Qwen3.5-9B', 'qwen'), ['Qwen'])
})

test('a query that is the start of the text ranks above the same letters further in', () => {
  assert.ok(score('go', 'Go to Models') > score('go', 'Turn background mining on'))
  assert.ok(score('new chat', 'New chat') > score('new chat', 'New chat in a folder'))
})

test('the words of a query match in any order, and all of them must', () => {
  assert.deepEqual(hits('New agent thread', 'agent new'), ['New', 'agent'])
  assert.equal(fuzzyMatch('agent banana', 'New agent thread'), null)
})

test('Japanese titles match, and the ranges stay on the right characters', () => {
  const title = '原点を中心とする円の接線'
  assert.deepEqual(hits(title, '円の接線'), ['円の接線'])
  assert.notEqual(fuzzyMatch('中心', title), null)
})

test('case folding never moves an index', () => {
  assert.equal(foldCase('İAb').length, 3)
  assert.deepEqual(fuzzyMatch('ab', 'İAb')?.ranges, [[1, 3]])
})

test('a long text is matched as a substring, not a scattered subsequence', () => {
  const long = `x${'a'.repeat(600)}y z`
  assert.equal(fuzzyMatch('xyz', long), null)
  const withNeedle = `${'lorem ipsum '.repeat(60)}needle in here`
  const match = fuzzyMatch('needle', withNeedle)
  assert.ok(match)
  const [start, end] = match.ranges[0] ?? [0, 0]
  assert.equal(withNeedle.slice(start, end), 'needle')
})

test('a quality floor keeps prefixes, acronyms and runs, and drops scattered letters', () => {
  const floor = { minQuality: 0.45 }
  assert.notEqual(fuzzyMatch('mine', 'Mine prompts from this thread', floor), null)
  assert.notEqual(fuzzyMatch('gtm', 'Go to Models', floor), null)
  assert.notEqual(fuzzyMatch('odel', 'Go to Models', floor), null)
  assert.notEqual(fuzzyMatch('nc', 'New chat', floor), null)
  const sentence = 'An extremely long thread title that will certainly need to be truncated in the sidebar'
  assert.notEqual(fuzzyMatch('mine', sentence), null, 'it is a subsequence…')
  assert.equal(fuzzyMatch('mine', sentence, floor), null, '…but not a match worth showing')
  assert.equal(fuzzyMatch('c', 'Unicode', floor), null, 'a single letter counts at a word start only')
  assert.notEqual(fuzzyMatch('c', 'Clear thread', floor), null)
})

test('highlight parts cover the whole text, alternating', () => {
  assert.deepEqual(highlightParts('Go to Models', [[0, 1], [6, 12]]), [
    { text: 'G', hit: true },
    { text: 'o to ', hit: false },
    { text: 'Models', hit: true },
  ])
  assert.deepEqual(highlightParts('abc', []), [{ text: 'abc', hit: false }])
  assert.deepEqual(mergeRanges([[4, 6], [0, 2], [2, 3], [5, 9]]), [[0, 3], [4, 9]])
})

test('a snippet is cut around the hit, with whitespace collapsed and the hit marked', () => {
  const text = `${'The quick brown fox jumps over the lazy dog. '.repeat(4)}\n\nHere the borrow checker\nrejects the second mutable borrow of the vector. ${'More words follow here. '.repeat(6)}`
  const snippet = findSnippet(text, 'borrow checker')
  assert.ok(snippet)
  assert.ok(snippet.text.startsWith('…'))
  assert.ok(snippet.text.endsWith('…'))
  assert.ok(!snippet.text.includes('\n'))
  const marked = snippet.ranges.map(([s, e]) => snippet.text.slice(s, e))
  assert.ok(marked.includes('checker'), JSON.stringify(marked))
  assert.ok(marked.every((m) => m.toLowerCase() === 'borrow' || m.toLowerCase() === 'checker'), JSON.stringify(marked))
})

test('a snippet keeps the words before the hit, starting on a word boundary', () => {
  const text = 'Because Rust forbids aliasing a mutable reference: the second mutable borrow of the vector overlaps the first. Split the borrow with split_at_mut or restructure.'
  const snippet = findSnippet(text, 'split_at')
  assert.ok(snippet)
  assert.ok(/…\S/.test(snippet.text.slice(0, 2)), snippet.text)
  const before = snippet.text.slice(1, snippet.text.indexOf('split_at'))
  assert.ok(before.length > 15, `context before the hit: ${JSON.stringify(before)}`)
  assert.ok(text.includes(` ${before.split(' ')[0]}`), 'the excerpt opens on a whole word')
})

test('a snippet needs every word of the query', () => {
  assert.equal(findSnippet('the borrow checker', 'borrow lifetime'), null)
  assert.equal(findSnippet('anything', ''), null)
  const short = findSnippet('Short answer about Rust.', 'rust')
  assert.deepEqual(short, { text: 'Short answer about Rust.', ranges: [[19, 23]] })
})
