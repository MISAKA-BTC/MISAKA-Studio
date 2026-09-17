import assert from 'node:assert/strict'
import { test } from 'node:test'
import { joinContinuation } from '../src/lib/continuation.ts'

test('a true continuation is appended as is', () => {
  assert.equal(joinContinuation('これらを $x_1$ と', ' $y_1$ について解くと'), 'これらを $x_1$ と $y_1$ について解くと')
})

test('a continuation that restates the end of the answer loses the restatement', () => {
  assert.equal(joinContinuation('これらを解いて x_1 と y_1 を求める', 'x_1 と y_1 を求める：\nx_1 = 1'), 'これらを解いて x_1 と y_1 を求める：\nx_1 = 1')
})

test('a short coincidence is ordinary text, not an overlap', () => {
  assert.equal(joinContinuation('abc の', 'の値'), 'abc のの値')
})

test('works while the continuation is still arriving', () => {
  const base = 'The key is 1234567890'
  assert.equal(joinContinuation(base, '1234'), 'The key is 12345678901234', 'too short to call an overlap yet')
  assert.equal(joinContinuation(base, '1234567890 and more'), 'The key is 1234567890 and more')
})
