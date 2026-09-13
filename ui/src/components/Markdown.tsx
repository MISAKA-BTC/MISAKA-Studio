// Markdown rendering for assistant messages.
//
// Two things this does that a bare `react-markdown` does not:
//
// * **A copy button on every code block.** The single most common thing anyone does with a local
//   model's answer is copy the code out of it.
// * **It renders partial markdown while streaming.** Text arrives token by token, which means the
//   renderer sees `` ```py `` with no closing fence for several seconds. `react-markdown` handles
//   that gracefully — the alternative, waiting for the whole message before rendering, is the
//   thing that makes a streaming app feel like a non-streaming one.
//
// Highlighting uses `rehype-highlight`'s default set — highlight.js's `common` grammars, about
// thirty languages. Naming a subset explicitly does not shrink it: the option registers languages
// *in addition to* that set, so the honest way to trim the bundle would be a custom lowlight
// instance, and thirty grammars is not yet worth one.

import { memo, type ReactNode } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import rehypeHighlight from 'rehype-highlight'
import rehypeKatex from 'rehype-katex'
import 'katex/dist/katex.min.css'
import { CopyButton } from './common'

function textOf(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === 'boolean') return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  if (typeof node === 'object' && 'props' in (node as any)) return textOf((node as any).props?.children)
  return ''
}

/**
 * Models commonly use both Markdown math (`$x$`, `$$x$$`) and the delimiters ChatGPT emits
 * (`\\(x\\)`, `\\[x\\]`). remark-math deliberately only owns the Markdown spellings, so make
 * the latter spell the former before parsing. Fenced code is left verbatim: a code sample that
 * happens to contain a LaTex string must remain copyable source, not become a formula.
 */
function normalizeInlineMath(markdown: string): string {
  const formulas: string[] = []
  // A lone `$` is surprisingly common in partially streamed text (and in prose such as
  // "$ (1)").  remark-math then treats everything up to the next dollar as one enormous
  // formula.  Only keep a pair when its contents look like math; quote every other dollar so it
  // stays readable source instead of swallowing the next paragraph.
  const protectedMath = markdown.replace(/(?<!\\)\$(?!\$)([^\n$]*?)(?<!\\)\$(?!\$)/g, (whole, formula: string) => {
    if (!/^[A-Za-z0-9\\^_{}()[\]<>+=*/|,:;.!?'"\- ]+$/.test(formula) || !/[A-Za-z0-9\\^_+=*/]/.test(formula)) {
      return whole.replace(/\$/g, '\\$')
    }
    const token = formulas.push(`$${formula}$`) - 1
    return `\uE000${token}\uE001`
  })
  return protectedMath
    .replace(/(?<!\\)\$(?!\$)/g, '\\$')
    .replace(/\uE000(\d+)\uE001/g, (_, token: string) => formulas[Number(token)] ?? '')
}

function normalizeMathDelimiters(markdown: string): string {
  return markdown
    .split(/(```[\s\S]*?```)/g)
    .map((part, index) => {
      if (index % 2 === 1) return part
      const normalized = part
        .replace(/\\\[([\s\S]*?)\\\]/g, (_, formula: string) => `$$\n${formula.trim()}\n$$`)
        .replace(/\\\(([^\n]*?)\\\)/g, (_, formula: string) => `$${formula.trim()}$`)
      return normalizeInlineMath(normalized)
    })
    .join('')
}

export const Markdown = memo(function Markdown({ children, streaming = false }: { children: string; streaming?: boolean }) {
  // An incomplete reply has unmatched delimiters by definition.  Rendering it as ordinary
  // Markdown until the stream commits avoids repeatedly rebuilding KaTeX's tree for every token
  // and makes the in-progress source legible.
  const markdown = streaming ? children : normalizeMathDelimiters(children)
  return (
    <div className="prose-chat">
      <ReactMarkdown
        remarkPlugins={streaming ? [remarkGfm] : [remarkGfm, remarkMath]}
        rehypePlugins={streaming ? [[rehypeHighlight, { detect: true, ignoreMissing: true }]] : [[rehypeHighlight, { detect: true, ignoreMissing: true }], rehypeKatex]}
        components={{
          pre({ children }) {
            const code = textOf(children)
            return (
              <div className="group relative">
                <div className="absolute right-2 top-2 opacity-0 transition-opacity group-hover:opacity-100">
                  <CopyButton
                    text={code}
                    label="Copy code"
                    className="btn rounded-md bg-ink-800/80 px-2 py-1 text-ink-200 hover:bg-ink-700"
                  />
                </div>
                <pre>{children}</pre>
              </div>
            )
          },
          // Links open outside the app. A local-LLM window is not a browser, and a model's
          // hallucinated URL should not replace the app with a 404.
          a({ href, children }) {
            return (
              <a href={href} target="_blank" rel="noreferrer noopener">
                {children}
              </a>
            )
          },
        }}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  )
})
