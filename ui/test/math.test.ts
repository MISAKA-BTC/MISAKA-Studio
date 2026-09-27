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
  // A display block cut off by a stopped stream: the opening `$$` alone must not start a formula —
  // it is escaped on its own, same as before. `x = 1` is bare after it, so the bare-formula pass
  // still wraps it; the two concerns are independent and this is the correct combination of them.
  assert.equal(normalizeMathDelimiters('途中で $$\nx = 1'), '途中で \\$\\$\n$x = 1$')
})

test('fenced code is never touched', () => {
  const code = '```sh\necho "$HOME $$"\n```'
  assert.equal(normalizeMathDelimiters(`before \\( a \\)\n${code}\nafter`), `before $a$\n${code}\nafter`)
})

test('a formula next to prose with a price keeps only the formula', () => {
  assert.equal(normalizeMathDelimiters('$x^2$ for $5'), '$x^2$ for \\$5')
})

// The field report (2026-09-27): the A16 class answers in bare ASCII, no `$` anywhere, and the
// unwrapped `a^2` and `θ` read as literal caret-and-letter text on screen. These are the actual
// lines from that answer.
test('bare exponents, subscripts and Greek letters in an untouched reply are wrapped', () => {
  assert.equal(normalizeMathDelimiters('接点は B(b, b^2 + p)'), '接点は $B(b, b^2 + p)$')
  assert.equal(normalizeMathDelimiters('θ = π/2 のとき'), '$θ = π/2$ のとき')
  assert.equal(normalizeMathDelimiters('tan θ = tan(π/2) = ∞'), '$tan θ = tan(π/2) = ∞$')
  assert.equal(normalizeMathDelimiters('p ≥ a^2 + √3a + 1'), '$p ≥ a^2 + √3a + 1$')
  assert.equal(
    normalizeMathDelimiters('y - a^2 = (2x + p)\'(x - a) = 2x + p'),
    '$y - a^2 = (2x + p)\'(x - a) = 2x + p$',
  )
})

test('a sentence with more than one bare formula wraps each and keeps the Japanese between them', () => {
  assert.equal(
    normalizeMathDelimiters('点 A(a, a^2) から Q(x, x^2 + p) に引いた接線'),
    '点 $A(a, a^2)$ から $Q(x, x^2 + p)$ に引いた接線',
  )
})

test('a bare formula and a model-written $…$ formula in the same reply are both handled once', () => {
  assert.equal(normalizeMathDelimiters('接線 $l_1$ の傾きは 2x + p^2 である'), '接線 $l_1$ の傾きは $2x + p^2$ である')
})

// What must NOT be wrapped: ordinary words, isolated letters and numbers with no signal, code, and
// point labels that assert nothing on their own.
test('ordinary prose, lone letters and numbers, and unmarked point labels are left alone', () => {
  assert.equal(normalizeMathDelimiters('This is the MISAKA Studio mock runtime.'), 'This is the MISAKA Studio mock runtime.')
  assert.equal(normalizeMathDelimiters("it's a model's answer, not the engine's"), "it's a model's answer, not the engine's")
  assert.equal(normalizeMathDelimiters('点 A から Q に引いた接線は 2 本ある'), '点 A から Q に引いた接線は 2 本ある')
  assert.equal(normalizeMathDelimiters('the file is 1.7 GiB, M4 Pro で動く'), 'the file is 1.7 GiB, M4 Pro で動く')
  // Two labels named together assert nothing by themselves — only an operator between them would.
  assert.equal(normalizeMathDelimiters('線分 AB と 点 (A, B) の関係'), '線分 AB と 点 (A, B) の関係')
})

test('a prime after a bare letter is not a derivative — it is what a contraction looks like', () => {
  assert.equal(normalizeMathDelimiters("the engine's own log"), "the engine's own log")
  assert.equal(normalizeMathDelimiters("that's fine. b^2 is next."), "that's fine. $b^2$ is next.")
  // The trade-off this makes: a prime right after a closing bracket is a derivative and keeps its
  // run going on both sides (the shape the field report's answer actually used) —
  assert.equal(normalizeMathDelimiters("(2x)'(x - a) は導関数"), "$(2x)'(x - a)$ は導関数")
  // — but a prime directly after a BARE letter never joins, contraction or not, because the two
  // cannot be told apart from the mark alone; f'(x) on its own is therefore left as plain text.
  assert.equal(normalizeMathDelimiters("f'(x) は 1 階微分"), "f'(x) は 1 階微分")
})

test('inline code is read as source, never as a bare formula', () => {
  assert.equal(normalizeMathDelimiters('`a^2 + b^2` を計算する関数'), '`a^2 + b^2` を計算する関数')
  const code = '```py\nreturn a**2 + b**2\n```'
  assert.equal(normalizeMathDelimiters(`前置き\n${code}\n後書き a^2`), `前置き\n${code}\n後書き $a^2$`)
})
