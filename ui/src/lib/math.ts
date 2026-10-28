// Math delimiters, made uniform before Markdown sees them.
//
// Models write formulas four ways — `$x$`, `$$x$$`, and the ChatGPT-style `\(x\)` / `\[x\]` — and
// remark-math deliberately understands only the first two. So the other two are rewritten into
// them here, and every remaining lone `$` (a price, "$ (1)" in prose, a delimiter cut off by a
// stopped stream) is escaped so it cannot swallow the next paragraph as one enormous formula.
//
// The bug this file replaced: the lone-dollar escape ran over `$$…$$` as well, turning every
// display block into `$\$…$\$`. A `\[ l_1: y - y_0 = … \]` from the model, converted to `$$` first,
// then came out of the escape as `$\$ l_1: … $\$` and rendered as the raw source with stray
// backslashes — which is what a tester's copy of a geometry answer showed. Display blocks are
// protected first now, and there is a test file that feeds this function what models actually
// write.
//
// **A fourth spelling: no delimiters at all.** `misaka-palw-serve`'s A16 class is a 1.5B model
// with no chat template of its own tuning it toward LaTeX, and it answers a maths question in bare
// ASCII — `y - a^2 = (2x + p)'(x - a)`, `tan θ = tan(π/2) = ∞` — never once inside a `$`. A person
// reading that as plain Markdown gets `a^2` and `θ` as literal caret-and-letter text, not the
// notation it names (field report 2026-09-27, illegible on its own screenshot). `wrapBareFormulas`
// finds these runs and wraps them in `$…$` so KaTeX renders them the same as a model that did
// write the dollar signs. It never invents a formula: a run has to carry a "this is math, not
// English" signal — an exponent or subscript (`^`/`_`), a derivative prime, a relation
// (`= < > ≥ ≤ ≠`), a root, a Greek letter, or a named function (`tan`, `sin`, …) — before it is
// touched, and ordinary prose earns none of those. See `looksLikeMath` for the sibling check on
// text a model DID put inside `$…$`.

/** Whether the text between two single dollars is a formula rather than prose with prices in it. */
function looksLikeMath(formula: string): boolean {
  return /^[A-Za-z0-9\\^_{}()[\]<>+=*/|,:;.!?'"\- ]+$/.test(formula) && /[A-Za-z0-9\\^_+=*/]/.test(formula)
}

// --- Bare formulas: no `$`, no `\(`, just algebra sitting in the prose --------------------------

/**
 * One variable: an optional numeric coefficient glued to a single Latin letter (`3a`, `2x`), or the
 * letter alone (`a`), with an optional exponent or subscript (`a^2`, `x_1`) — or a bare number
 * (`5`, `1.5`). Never more than one letter: a real algebra variable in what these classes write is
 * always one, and bounding it here is what keeps an ordinary word (a run of several letters) from
 * ever being read as one. The derivative prime (`f'`, `(2x + p)'`) is deliberately NOT part of this
 * — see [`Token`]'s note on it — because a letter directly followed by `'` is what an English
 * contraction or possessive looks like ("it's", "model's"), and there is no way to tell those two
 * apart from the letter and the mark alone.
 */
const ATOM = String.raw`(?:[0-9]+(?:\.[0-9]+)?)?[A-Za-z](?:\^[0-9A-Za-z{}()+\-]+|_[0-9A-Za-z{}()+\-]+)?|[0-9]+(?:\.[0-9]+)?`
const GREEK = '[θπαβγδλΣ∫]'
// `log` is deliberately not here: this app's own vocabulary uses that exact English word
// constantly (an engine's log, a download log), and every one of those would otherwise read as
// the function.
const FUNC = String.raw`(?:tan|sin|cos|ln|lim|sqrt)\b`
/** Punctuation a formula strings its atoms together with, `'` included — never on its own:
 *  [`hasMathSignal`] still requires an atom, a Greek letter or a function name somewhere in the
 *  run, and [`tokenRuns`] never lets a `'` glue to a bare letter in the first place. */
const PUNCT = String.raw`[+\-*/=<>≥≤≠→°∞(),']|√`
const TOKEN_RE = new RegExp(`${FUNC}|${GREEK}|${ATOM}|${PUNCT}`, 'g')

/** One matched token, with the span it covers — kept so adjacency (touching vs. a real gap) can be
 *  read straight off the source rather than re-derived from lengths. */
interface Token {
  text: string
  start: number
  end: number
}

/** A bare, un-modified Latin letter — the one shape that must NOT be allowed to sit directly
 *  against another token of its own kind with no space between them, because that is indistin-
 *  guishable from spelling an ordinary word one letter-token at a time ("This" is T, h, i, s). Every
 *  other token — a coefficient ("3a"), a modified letter ("a^2"), an operator, a Greek letter, a
 *  function name — is unambiguous even glued to its neighbour, which is how "tan(π/2)" and "√3a"
 *  read as one run despite carrying no spaces at all.
 */
function isBareLetter(token: string): boolean {
  return /^[A-Za-z]$/.test(token)
}

/** Whether a run actually says "this is math", rather than being a stray Latin letter, a lone
 *  number, or bracket punctuation with nothing around it. An exponent, a subscript, a prime, a
 *  relation, a root, a Greek letter or a named function is the bar; two bare variables joined only
 *  by a comma or parenthesis do not clear it on their own (`(A, B)` names two points, it asserts
 *  nothing), which is what keeps this from wrapping every parenthesised abbreviation in the text.
 *  And the run has to carry an actual letter or digit — a lone `'` that never found anything to
 *  attach to is not a formula either. */
function hasMathSignal(run: string): boolean {
  return (/[\^_'=<>≥≤≠→√θπαβγδλΣ∫∞]/.test(run) || new RegExp(FUNC).test(run)) && /[A-Za-z0-9]/.test(run)
}

/**
 * Group tokens into maximal runs: consecutive tokens joined by nothing wider than one space, with
 * two exceptions.
 *
 * The first: a bare letter that is sitting inside an ordinary spelled-out word — immediately next
 * to another bare letter with no space, on either side ("This" is T next to h, "is" is i next to s)
 * — never joins a run, whichever side of it the rest of the run is on and whether the gap to it is
 * zero or one space. Without the second half of that (checked across a SPACE, not only zero
 * distance) the fragment a word leaves behind after its OTHER letters have already refused to join
 * — the "i" in "…that's fine. b^2 is next." once "t" has isolated it from "s" — would still glue
 * onto real math sitting right next to it across the one space that ordinarily separates two
 * distinct pieces of a formula, and turn "is" into part of the formula.
 *
 * The second exception is the derivative prime (`'`), which real notation writes right after a
 * closing bracket — `(2x + p)'(x - a)` — and English writes right after a letter — "it's". The two
 * are the same character in the same position and cannot be told apart by looking at the prime; so
 * a `'` glues backward only onto `)`, `]` or another prime that itself already glued (never onto a
 * bare letter), and glues forward only once it has done that — a `'` that could not attach backward
 * stays isolated on both sides, which is what keeps "it's" from picking up "s" once "t" has already
 * refused it.
 */
function tokenRuns(text: string): Token[][] {
  const tokens: Token[] = [...text.matchAll(TOKEN_RE)].map((m) => ({ text: m[0], start: m.index, end: m.index + m[0].length }))
  const wordFragment = tokens.map((token, i) => {
    if (!isBareLetter(token.text)) return false
    const prev = tokens[i - 1]
    const next = tokens[i + 1]
    const touchesLetter = (other: Token | undefined, gapBefore: boolean) =>
      other !== undefined && isBareLetter(other.text) && text.slice(gapBefore ? other.end : token.end, gapBefore ? token.start : other.start) === ''
    return touchesLetter(prev, true) || touchesLetter(next, false)
  })

  const runs: Token[][] = []
  let current: Token[] = []
  let currentHasFragment = false
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i]!
    const previous = current.at(-1)
    const gap = previous ? text.slice(previous.end, token.start) : ''
    let joins: boolean
    if (currentHasFragment || wordFragment[i]) {
      // Either end of the join is a fragment of a spelled-out word: never glue it to anything,
      // regardless of how much space separates the two.
      joins = false
    } else if (!previous || gap === ' ') {
      joins = true
    } else if (gap !== '') {
      joins = false
    } else if (token.text === "'") {
      joins = /[)\]]$/.test(previous.text) || (previous.text === "'" && current.length > 1)
    } else if (previous.text === "'") {
      joins = current.length > 1
    } else {
      joins = true // two non-fragment, zero-space tokens that are not the prime case above
    }
    if (!joins) {
      if (current.length > 0) runs.push(current)
      current = []
      currentHasFragment = false
    }
    current.push(token)
    currentHasFragment = currentHasFragment || wordFragment[i]!
  }
  if (current.length > 0) runs.push(current)
  return runs
}

/**
 * Wrap every bare run that carries [`hasMathSignal`] in `$…$`, protecting it with `keep` the same
 * way an existing `$…$` or `$$…$$` span already is — so the dollar-escape pass that runs after this
 * one treats a formula this function just wrote exactly like a formula the model wrote itself,
 * rather than re-deciding (and possibly rejecting) it under `looksLikeMath`'s narrower, ASCII-only
 * rule.
 */
function wrapBareFormulas(text: string, keep: (span: string) => string): string {
  let out = ''
  let cursor = 0
  for (const run of tokenRuns(text)) {
    const start = run[0]!.start
    const end = run.at(-1)!.end
    const span = text.slice(start, end)
    out += text.slice(cursor, start)
    out += hasMathSignal(span) ? keep(`$${span}$`) : span
    cursor = end
  }
  return out + text.slice(cursor)
}

// --- Putting it together -------------------------------------------------------------------------

/**
 * Rewrite `\[…\]` and `\(…\)` into `$$…$$` and `$…$`, wrap bare formula runs in `$…$`, keep every
 * well-formed `$$…$$` and plausible `$…$` (the model's own or one this file just wrote), and escape
 * any other dollar. Fenced code is left verbatim: a code sample that happens to contain a LaTeX
 * string, or a caret, must remain copyable source, not become a formula.
 */
export function normalizeMathDelimiters(markdown: string): string {
  return markdown
    .split(/(```[\s\S]*?```)/g)
    .map((part, index) => {
      if (index % 2 === 1) return part
      const withMarkdownDelimiters = part
        .replace(/\\\[([\s\S]*?)\\\]/g, (_, formula: string) => `$$\n${formula.trim()}\n$$`)
        .replace(/\\\(([^\n]*?)\\\)/g, (_, formula: string) => `$${formula.trim()}$`)
      return protectMath(withMarkdownDelimiters)
    })
    .join('')
}

/**
 * Keep `$$…$$` blocks, inline code spans and math-looking `$…$` pairs as they are; wrap and keep
 * bare formula runs found in what is left; escape every other dollar.
 *
 * Protected spans are swapped for private-use placeholders while the later passes run, then put
 * back — the escape's regex cannot otherwise tell a display block's closing `$$` from two stray
 * dollars, and the bare-formula pass must not read INTO a span the model already delimited itself
 * (it would find "a^2" again inside a `$…$` the model wrote and try to wrap it a second time).
 */
function protectMath(markdown: string): string {
  const kept: string[] = []
  const keep = (span: string) => `${kept.push(span) - 1}`

  const withBlocksKept = markdown.replace(/(?<!\\)\$\$([\s\S]*?)(?<!\\)\$\$/g, (whole) => keep(whole))
  // Inline code next: a code sample's `a^2` is source, never a formula, and must not be read by
  // either the bare-formula pass below or the plain dollar-escape at the end.
  const withCodeKept = withBlocksKept.replace(/`[^`\n]*`/g, (whole) => keep(whole))
  const withInlineKept = withCodeKept.replace(/(?<!\\)\$(?!\$)([^\n$]*?)(?<!\\)\$(?!\$)/g, (whole, formula: string) =>
    looksLikeMath(formula) ? keep(whole) : whole.replace(/\$/g, '\\$'),
  )
  const withBareFormulasKept = wrapBareFormulas(withInlineKept, keep)
  return withBareFormulasKept.replace(/(?<!\\)\$/g, '\\$').replace(/(\d+)/g, (_, index: string) => kept[Number(index)] ?? '')
}
