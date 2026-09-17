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

/** Whether the text between two single dollars is a formula rather than prose with prices in it. */
function looksLikeMath(formula: string): boolean {
  return /^[A-Za-z0-9\\^_{}()[\]<>+=*/|,:;.!?'"\- ]+$/.test(formula) && /[A-Za-z0-9\\^_+=*/]/.test(formula)
}

/**
 * Rewrite `\[…\]` and `\(…\)` into `$$…$$` and `$…$`, keep every well-formed `$$…$$` and
 * plausible `$…$`, and escape any other dollar. Fenced code is left verbatim: a code sample that
 * happens to contain a LaTeX string must remain copyable source, not become a formula.
 */
export function normalizeMathDelimiters(markdown: string): string {
  return markdown
    .split(/(```[\s\S]*?```)/g)
    .map((part, index) => {
      if (index % 2 === 1) return part
      const withMarkdownDelimiters = part
        .replace(/\\\[([\s\S]*?)\\\]/g, (_, formula: string) => `$$\n${formula.trim()}\n$$`)
        .replace(/\\\(([^\n]*?)\\\)/g, (_, formula: string) => `$${formula.trim()}$`)
      return escapeStrayDollars(withMarkdownDelimiters)
    })
    .join('')
}

/**
 * Keep `$$…$$` blocks and math-looking `$…$` pairs; escape every other dollar.
 *
 * Protected spans are swapped for private-use placeholders while the escape runs, then put back —
 * the escape's regex cannot otherwise tell a display block's closing `$$` from two stray dollars.
 */
function escapeStrayDollars(markdown: string): string {
  const kept: string[] = []
  const keep = (span: string) => `\uE000${kept.push(span) - 1}\uE001`

  const withBlocksKept = markdown.replace(/(?<!\\)\$\$([\s\S]*?)(?<!\\)\$\$/g, (whole) => keep(whole))
  const withInlineKept = withBlocksKept.replace(/(?<!\\)\$(?!\$)([^\n$]*?)(?<!\\)\$(?!\$)/g, (whole, formula: string) =>
    looksLikeMath(formula) ? keep(`$${formula}$`) : whole.replace(/\$/g, '\\$'),
  )
  return withInlineKept.replace(/(?<!\\)\$/g, '\\$').replace(/\uE000(\d+)\uE001/g, (_, index: string) => kept[Number(index)] ?? '')
}
