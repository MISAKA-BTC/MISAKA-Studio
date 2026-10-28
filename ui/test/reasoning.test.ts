import assert from 'node:assert/strict'
import { test } from 'node:test'
import { reasoningTail, seconds, splitThinking, thoughtLabel } from '../src/lib/reasoning.ts'

test('a reply with no reasoning is all answer', () => {
  assert.deepEqual(splitThinking('Paris is the capital of France.'), { thinking: null, answer: 'Paris is the capital of France.', open: false })
  assert.deepEqual(splitThinking(''), { thinking: null, answer: '', open: false })
})

test('a closed think block at the start is split off, and the answer starts at its first character', () => {
  assert.deepEqual(splitThinking('<think>\nThe user wants the capital.\n</think>\n\nParis.'), {
    thinking: 'The user wants the capital.',
    answer: 'Paris.',
    open: false,
  })
  // Leading whitespace before the tag, as some templates leave it.
  assert.equal(splitThinking('\n  <think>x</think>y').answer, 'y')
})

test('an unclosed block is the model still thinking, and a closing tag still arriving is not shown', () => {
  assert.deepEqual(splitThinking('<think>Let me work out'), { thinking: 'Let me work out', answer: '', open: true })
  assert.deepEqual(splitThinking('<think>Let me work out</thi'), { thinking: 'Let me work out', answer: '', open: true })
  assert.deepEqual(splitThinking('<think>a < b'), { thinking: 'a < b', answer: '', open: true })
})

test('the opening tag arriving token by token is thinking, not a stray "<" in the answer', () => {
  for (const partial of ['<', '<t', '<thin', '<think']) {
    assert.deepEqual(splitThinking(partial), { thinking: '', answer: '', open: true }, partial)
  }
  // A reply that merely starts with a tag-like character is an answer once it stops matching.
  assert.equal(splitThinking('<div>hi</div>').thinking, null)
})

test('an empty think block — a model asked not to think — leaves no reasoning behind', () => {
  assert.deepEqual(splitThinking('<think>\n\n</think>\n\nHello!'), { thinking: null, answer: 'Hello!', open: false })
})

test('a template that pre-fills <think> leaves only the closing tag, on its own line', () => {
  assert.deepEqual(splitThinking('Okay, the user greets me.\n</think>\n\nHi there!'), {
    thinking: 'Okay, the user greets me.',
    answer: 'Hi there!',
    open: false,
  })
})

test('a reply that talks about the tags keeps them as text', () => {
  const inCode = 'Close the block with `</think>` when you are done.'
  assert.deepEqual(splitThinking(inCode), { thinking: null, answer: inCode, open: false })
  const inProse = 'The model emits </think> when it is done thinking.'
  assert.deepEqual(splitThinking(inProse), { thinking: null, answer: inProse, open: false })
  const fenced = '```xml\n<think>\n</think>\n```'
  assert.deepEqual(splitThinking(fenced), { thinking: null, answer: fenced, open: false })
})

test('the live preview is the end of the reasoning, cut at a word boundary', () => {
  assert.equal(reasoningTail('  short  '), 'short')
  const long = 'word '.repeat(200)
  const tail = reasoningTail(long, 50)
  assert.ok(tail.length <= 50)
  assert.ok(tail.startsWith('word'), tail)
})

test('durations read the way the block says them', () => {
  assert.equal(seconds(0), '0s')
  assert.equal(seconds(12_400), '12s')
  assert.equal(seconds(65_000), '1m 05s')
  assert.equal(thoughtLabel(undefined), 'Thought process')
  assert.equal(thoughtLabel(400), 'Thought for a moment')
  assert.equal(thoughtLabel(12_000), 'Thought for 12s')
})
