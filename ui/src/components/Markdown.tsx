// Markdown rendering for assistant messages.
//
// What this does that a bare `react-markdown` does not:
//
// * **A header on every code block** — its language, a Copy that is always there, and a wrap
//   toggle. The single most common thing anyone does with a local model's answer is copy the code
//   out of it, and a copy button that only appears on hover is one nobody finds on a long block.
// * **Math in every spelling models use.** `$x$`, `$$x$$`, `\(x\)` and `\[x\]` all render; see
//   `lib/math.ts` for the normalisation and the bug it replaced.
// * **It renders partial markdown while streaming.** Text arrives token by token, which means the
//   renderer sees `` ```py `` with no closing fence for several seconds. `react-markdown` handles
//   that gracefully — the alternative, waiting for the whole message before rendering, is the
//   thing that makes a streaming app feel like a non-streaming one.
// * **Tables scroll** inside the answer instead of widening it past the window.
//
// The renderers below are defined once, at module level, on purpose. An inline `components={{…}}`
// is a new component type on every render — and a streamed reply renders on every token — so each
// code block was unmounted and rebuilt per token, and a wrap toggle or a "Copied" confirmation
// could not survive the next one.
//
// Highlighting uses `rehype-highlight`'s default set — highlight.js's `common` grammars, about
// thirty languages. Naming a subset explicitly does not shrink it: the option registers languages
// *in addition to* that set, so the honest way to trim the bundle would be a custom lowlight
// instance, and thirty grammars is not yet worth one.

import { isValidElement, memo, useEffect, useState, type ReactNode } from 'react'
import ReactMarkdown, { type Components, type Options } from 'react-markdown'
import remarkGfm from 'remark-gfm'
import remarkMath from 'remark-math'
import rehypeHighlight from 'rehype-highlight'
import rehypeKatex from 'rehype-katex'
import 'katex/dist/katex.min.css'
import { normalizeMathDelimiters } from '../lib/math'
import { Icon } from './common'

function textOf(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === 'boolean') return ''
  if (typeof node === 'string' || typeof node === 'number') return String(node)
  if (Array.isArray(node)) return node.map(textOf).join('')
  if (typeof node === 'object' && 'props' in (node as any)) return textOf((node as any).props?.children)
  return ''
}

/** The fence's language — or the one highlight.js detected — from the `language-xxx` class. */
function languageOf(children: ReactNode): string | null {
  const child = Array.isArray(children) ? children[0] : children
  if (!isValidElement(child)) return null
  const className = (child.props as { className?: unknown }).className
  const match = typeof className === 'string' ? /(?:^|\s)language-([^\s]+)/.exec(className) : null
  return match?.[1] ?? null
}

function CopyCode({ text }: { text: string }) {
  const [copied, setCopied] = useState(false)
  useEffect(() => {
    if (!copied) return
    const timer = setTimeout(() => setCopied(false), 1500)
    return () => clearTimeout(timer)
  }, [copied])
  return (
    <button
      type="button"
      className="inline-flex h-6 items-center gap-1 rounded-md px-1.5 text-[0.7rem] font-medium text-ink-300 transition-colors hover:bg-white/10 hover:text-white"
      aria-label={copied ? 'Copied' : 'Copy code'}
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text)
          setCopied(true)
        } catch {
          // Clipboard access can be denied; the label simply does not change.
          setCopied(false)
        }
      }}
    >
      <Icon name={copied ? 'check' : 'copy'} className="size-3.5" />
      {copied ? 'Copied' : 'Copy'}
    </button>
  )
}

function CodeBlock({ children }: { children?: ReactNode }) {
  const [wrap, setWrap] = useState(false)
  const language = languageOf(children)
  return (
    <div className="overflow-hidden rounded-lg border border-ink-800 bg-ink-900 dark:border-ink-800 dark:bg-black/40">
      <div className="flex h-8 items-center gap-0.5 border-b border-white/10 pl-3 pr-1 text-[0.7rem] text-ink-400">
        <span className="mono min-w-0 flex-1 truncate">{language ?? 'text'}</span>
        <button
          type="button"
          className={`inline-flex size-6 items-center justify-center rounded-md transition-colors hover:bg-white/10 hover:text-white ${wrap ? 'bg-white/10 text-white' : 'text-ink-400'}`}
          aria-label="Wrap long lines"
          aria-pressed={wrap}
          title={wrap ? 'Stop wrapping long lines' : 'Wrap long lines'}
          onClick={() => setWrap((v) => !v)}
        >
          <Icon name="wrap" className="size-3.5" />
        </button>
        <CopyCode text={textOf(children)} />
      </div>
      <pre className={`rounded-none bg-transparent dark:bg-transparent ${wrap ? 'whitespace-pre-wrap break-words' : ''}`}>{children}</pre>
    </div>
  )
}

const COMPONENTS: Components = {
  pre: CodeBlock,
  // Links open outside the app. A local-LLM window is not a browser, and a model's hallucinated URL
  // should not replace the app with a 404.
  a({ href, children }) {
    return (
      <a href={href} target="_blank" rel="noreferrer noopener">
        {children}
      </a>
    )
  },
  table({ children }) {
    return (
      <div className="max-w-full overflow-x-auto">
        <table>{children}</table>
      </div>
    )
  },
}

// The same plugin lists as always — streaming leaves out math, see below — built once.
const REMARK_STREAMING: Options['remarkPlugins'] = [remarkGfm]
const REMARK: Options['remarkPlugins'] = [remarkGfm, remarkMath]
const REHYPE_STREAMING: Options['rehypePlugins'] = [[rehypeHighlight, { detect: true, ignoreMissing: true }]]
const REHYPE: Options['rehypePlugins'] = [[rehypeHighlight, { detect: true, ignoreMissing: true }], rehypeKatex]

export const Markdown = memo(function Markdown({ children, streaming = false }: { children: string; streaming?: boolean }) {
  // An incomplete reply has unmatched delimiters by definition.  Rendering it as ordinary
  // Markdown until the stream commits avoids repeatedly rebuilding KaTeX's tree for every token
  // and makes the in-progress source legible.
  const markdown = streaming ? children : normalizeMathDelimiters(children)
  return (
    <div className="prose-chat">
      <ReactMarkdown
        remarkPlugins={streaming ? REMARK_STREAMING : REMARK}
        rehypePlugins={streaming ? REHYPE_STREAMING : REHYPE}
        components={COMPONENTS}
      >
        {markdown}
      </ReactMarkdown>
    </div>
  )
})
