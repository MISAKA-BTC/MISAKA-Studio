// What models actually write, through the normalizer. Run with `npm test` (Node's own runner;
// Node strips the types itself, so there is no build step and no test framework to install).

import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeMathDelimiters } from '../src/lib/math.ts'

test('ChatGPT-style display math becomes a $$ block and survives the stray-dollar escape', () => {
  const answer = 'まず \\[ l_1: y - y_0 = \\frac{y_0 - 0}{x_0 - 1}(x - 1) \\] と \\[ l_2: y = 2 \\] です。'
  assert.equal(
    normalizeMathDelimiters(answer),
    'まず $$\nl_1: y - y_0 = \\frac{y_0 - 0}{x_0 - 1}(x - 1)\n$$ と $$\nl_2: y = 2\n$$ です。',
  )
})

test('a $$ block the model wrote itself is left exactly as it is', () => {
  const answer = '解：\n\n$$\nx_1 = \\frac{2(x_0 - y_0)}{y_0^2 - 1}\n$$\n\nよって'
  assert.equal(normalizeMathDelimiters(answer), answer)
  assert.equal(normalizeMathDelimiters('inline $$y$$ block'), 'inline $$y$$ block')
})

test('inline forms: \\( \\) becomes $ $, and math-looking $ $ pairs are kept', () => {
  assert.equal(normalizeMathDelimiters('座標 \\( x_0 , y_0 \\) を用いて'), '座標 $x_0 , y_0$ を用いて')
  assert.equal(normalizeMathDelimiters('接線 $l_1$ と $l_2$'), '接線 $l_1$ と $l_2$')
  assert.equal(normalizeMathDelimiters('$OP \\cdot OQ = 1$ を示す'), '$OP \\cdot OQ = 1$ を示す')
})

test('dollars that are not delimiters are escaped so they cannot swallow a paragraph', () => {
  assert.equal(normalizeMathDelimiters('it costs $5 today'), 'it costs \\$5 today')
  assert.equal(normalizeMathDelimiters('$ (1) と $ (2) の両方'), '\\$ (1) と \\$ (2) の両方')
  // A display block cut off by a stopped stream: the opening `$$` alone must not start a formula.
  assert.equal(normalizeMathDelimiters('途中で $$\nx = 1'), '途中で \\$\\$\nx = 1')
})

test('fenced code is never touched', () => {
  const code = '```sh\necho "$HOME $$"\n```'
  assert.equal(normalizeMathDelimiters(`before \\( a \\)\n${code}\nafter`), `before $a$\n${code}\nafter`)
})

test('a formula next to prose with a price keeps only the formula', () => {
  assert.equal(normalizeMathDelimiters('$x^2$ for $5'), '$x^2$ for \\$5')
})
