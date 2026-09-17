import assert from 'node:assert/strict'
import { test } from 'node:test'
import { hasRepeatedTail, historyForModel, loops, trimRepeatedTail } from '../src/lib/history.ts'
import type { ChatMessage } from '../src/lib/types.ts'

let n = 0
const msg = (role: ChatMessage['role'], content: string, extra: Partial<ChatMessage> = {}): ChatMessage => ({ id: String(n++), role, content, ...extra })

const QUESTION = '原点 O(0, 0) を中心とする半径 1 の円に, 円外の点 P(x0, y0) から 2 本の接線を引く。(1) 2 つの接点の中点を Q とするとき, 点 Q の座標を表せ。'
const LOOP_BLOCK = '(1)さらに具体的な解き方を説明します：まず、点 P の座標 (x0, y0) を用いて l1 と l2 の接線の方程式を書きましょう：l1: y - y0 = (y0 - 0)/(x0 - 1)(x - 1)。これらを解くと x1 = (x0 - 1)/(y0 - 0)(y - 1) + 1。'
const LOOPED = `解説：まず、原点 O から引いた 2 本の接線はそれぞれ l1 と l2 です。${LOOP_BLOCK}${LOOP_BLOCK}${LOOP_BLOCK.slice(0, 60)}`

test('a looping reply is detected and cut back to before the loop', () => {
  assert.equal(hasRepeatedTail(LOOPED), true)
  const trimmed = trimRepeatedTail(`解説：まず。${LOOP_BLOCK}${LOOP_BLOCK}${LOOP_BLOCK}`)
  assert.ok(trimmed !== null)
  assert.equal(hasRepeatedTail(trimmed!), false, trimmed!)
  assert.ok(trimmed!.startsWith('解説：まず。'))
  assert.ok(trimmed!.includes('さらに具体的な解き方'), 'one copy of the block stays: it is the answer as far as it got')
  assert.equal(trimmed!.split('さらに具体的な解き方').length - 1, 1, 'and only one')
  assert.equal(trimRepeatedTail('short answer'), 'short answer')
})

/** The measured conversation, in shape: one question asked fifteen times, replies that failed,
 *  looped or never arrived among them. Only the last asking goes to the model. */
test('the fifteen-times conversation sends the question once and none of the failures', () => {
  const messages: ChatMessage[] = []
  const replies: Partial<ChatMessage>[] = [
    { content: '(1) まず接点を A, B とします。これらを', stats: undefined },
    { content: LOOPED, error: 'Minified React error #185' },
    { content: '接点を A, B とする。よって Q = (x0/(x0²+y0²), y0/(x0²+y0²)) となる。' },
    { content: `${LOOP_BLOCK}${LOOP_BLOCK}${LOOP_BLOCK}` },
    { content: LOOPED, error: '同じ文章の反復を検出したため、この表示を停止しました。' },
    { content: '' },
    { content: '途中まで', error: 'llamacpp: stream broke' },
  ]
  for (const reply of replies) {
    messages.push(msg('user', QUESTION))
    messages.push(msg('assistant', reply.content ?? '', reply))
  }
  messages.push(msg('user', QUESTION))

  const sent = historyForModel(messages)
  assert.deepEqual(sent, [{ role: 'user', content: QUESTION }])
})

test('different questions keep their good replies, and failures between them are dropped', () => {
  const messages = [
    msg('user', 'Q1'),
    msg('assistant', 'Q = (x0/r², y0/r²)'),
    msg('user', 'Q2'),
    msg('assistant', LOOPED, { error: '同じ文章の反復を検出したため' }),
    msg('user', 'Q3'),
    msg('assistant', '', { streaming: true }),
  ]
  assert.deepEqual(historyForModel(messages), [
    { role: 'user', content: 'Q1' },
    { role: 'assistant', content: 'Q = (x0/r², y0/r²)' },
    { role: 'user', content: 'Q2' },
    { role: 'user', content: 'Q3' },
  ])
})

test('a reply being continued is kept, and an asked-again question with a good answer keeps only the newest pair', () => {
  const messages = [
    msg('user', 'same'),
    msg('assistant', 'old answer'),
    msg('user', 'same'),
    msg('assistant', 'new partial answer that was cut at the ceiling'),
  ]
  assert.deepEqual(historyForModel(messages), [
    { role: 'user', content: 'same' },
    { role: 'assistant', content: 'new partial answer that was cut at the ceiling' },
  ])
})

test('a short phrase over and over is a loop too, and is cut to one copy', () => {
  const phrase = 'まず、点 P の座標を用いて、'
  const text = `前置き。${phrase.repeat(9)}`
  assert.equal(loops(text), true)
  assert.equal(trimRepeatedTail(text), `前置き。${phrase}`.trimEnd())
  assert.equal(loops('ababab is not a sentence loop of any length that matters'), false)
  assert.equal(loops(`Title\n${'='.repeat(40)}`), false, 'an underline is formatting')
  assert.equal(loops(`zeros: ${'[0, 0, 0, 0], '.repeat(5)}`), false, 'so is a zero-filled array')
})

test('a long answer that loops only at its end keeps its answer', () => {
  const answer = 'Q は OP と AB の交点なので、よって Q = (x0/(x0²+y0²), y0/(x0²+y0²)) となる。次に OP・OQ を計算すると、OP = √(x0²+y0²)、OQ = 1/√(x0²+y0²) であり、したがって OP・OQ = 1 が成り立つ。'.repeat(1)
  const ending = 'したがって答えは上の通りです。'
  const messages = [msg('user', 'Q'), msg('assistant', `${answer}${answer.slice(0, 80)}${ending}${ending}${ending}`), msg('user', 'next')]
  const sent = historyForModel(messages)
  assert.equal(sent.length, 3)
  assert.equal(loops(sent[1]!.content), false, sent[1]!.content)
  assert.ok(sent[1]!.content.includes('OP・OQ = 1'))
})

test('a reply that is only a loop leaves nothing worth sending', () => {
  const messages = [msg('user', 'Q'), msg('assistant', `${LOOP_BLOCK.slice(0, 30)}`.repeat(12)), msg('user', 'Q again differently')]
  const sent = historyForModel(messages)
  assert.deepEqual(sent.map((m) => m.role), ['user', 'user'], JSON.stringify(sent))
})
