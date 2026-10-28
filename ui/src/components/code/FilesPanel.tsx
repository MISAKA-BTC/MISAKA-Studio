// The Files tab: the session's folder as a tree, a filter over every file in it, and a viewer.
//
// The tree is read a level at a time as folders open (`filesStore.ts`); the filter asks the
// runtime's own file finder — the one behind the composer's `@` — rather than searching what the
// tree happens to have loaded. The viewer replaces the tree rather than splitting the pane with it:
// at a panel's width a file needs every line of height it can get, and the tree comes back as it
// was left, scrolled where it was.
//
// "Mention in composer" does what typing `@path` in the composer does: the path goes into the
// message and the file's content is attached for the agent to read. Attachments are never mined.

import { useEffect, useLayoutEffect, useMemo, useRef, useState, type KeyboardEvent, type RefObject } from 'react'
import { api } from '../../lib/api'
import { folderName } from '../../lib/agent/workspace'
import { MAX_ATTACHMENT_BYTES, attachmentFromText, baseName, languageFor } from '../../lib/attachments'
import { bytes } from '../../lib/format'
import type { Conversation, WorkspaceEntry, WorkspaceFile } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { draftKey, useComposer } from '../chat/composerStore'
import { Icon, Spinner } from '../common'
import { EMPTY_TREE, loadDir, refreshTree, rememberScroll, selectFile, setFilesQuery, toggleDir, useFileTree, useFiles, type FileTree } from './filesStore'
import { NoFolder, copyText, splitPath } from './shared'

/** The most of a file the viewer reads. */
const VIEW_MAX_BYTES = 512 * 1024
/** Past this many characters a file is shown without highlighting — highlighting is the slow part. */
const HIGHLIGHT_LIMIT = 200_000
/** Lines drawn before "Show all". */
const RENDER_LINES = 5000

// --- the composer -----------------------------------------------------------------------------------

/** Put `@path` in the session's draft and attach the file, as the composer's own `@` does. */
async function mentionInComposer(root: string, path: string, conversationId: string): Promise<void> {
  const key = draftKey(conversationId)
  const composer = useComposer.getState()
  const toast = useStudio.getState().toast
  const text = composer.drafts[key]?.text ?? ''
  const token = `@${path}`
  if (!text.split(/\s+/).includes(token)) composer.setText(key, `${text}${text && !/\s$/.test(text) ? ' ' : ''}${token} `)
  try {
    const file = await api.workspace.read(root, path, { max_bytes: MAX_ATTACHMENT_BYTES })
    if (file.binary) toast('info', `${baseName(path)} is a binary file — it is mentioned, but its content is not attached.`)
    else useComposer.getState().addAttachments(key, [attachmentFromText(path, file.content, { size: file.size, truncated: file.truncated, workspacePath: path })])
  } catch (error) {
    toast('info', `${path} is mentioned, but it could not be read: ${(error as Error).message}`)
  }
  useUi.getState().focusComposer()
}

// --- highlighting -----------------------------------------------------------------------------------

type Highlighter = { highlight: (code: string, options: { language: string; ignoreIllegals?: boolean }) => { value: string }; getLanguage: (name: string) => unknown }
let highlighter: Promise<Highlighter> | null = null

/** highlight.js's common grammars, loaded the first time a file is viewed — not with the window. */
function loadHighlighter(): Promise<Highlighter> {
  highlighter ??= import('highlight.js/lib/common').then((module) => module.default as unknown as Highlighter)
  return highlighter
}

// The window's stylesheet colours tokens for a dark code block. The viewer follows the theme, so in
// the light one it needs light-theme colours (GitHub's) — scoped to it, so chat code blocks keep theirs.
const LIGHT_TOKENS = `
html:not(.dark) .code-view :is(.hljs-doctag,.hljs-keyword,.hljs-meta .hljs-keyword,.hljs-template-tag,.hljs-template-variable,.hljs-type,.hljs-variable.language_){color:#cf222e}
html:not(.dark) .code-view :is(.hljs-title,.hljs-title.class_,.hljs-title.class_.inherited__,.hljs-title.function_){color:#8250df}
html:not(.dark) .code-view :is(.hljs-attr,.hljs-attribute,.hljs-literal,.hljs-meta,.hljs-number,.hljs-operator,.hljs-variable,.hljs-selector-attr,.hljs-selector-class,.hljs-selector-id){color:#0550ae}
html:not(.dark) .code-view :is(.hljs-regexp,.hljs-string,.hljs-meta .hljs-string){color:#0a3069}
html:not(.dark) .code-view :is(.hljs-built_in,.hljs-symbol){color:#953800}
html:not(.dark) .code-view :is(.hljs-comment,.hljs-code,.hljs-formula){color:#6e7781}
html:not(.dark) .code-view :is(.hljs-name,.hljs-quote,.hljs-selector-tag,.hljs-selector-pseudo){color:#116329}
html:not(.dark) .code-view .hljs-subst{color:#24292f}
html:not(.dark) .code-view .hljs-section{color:#0550ae;font-weight:bold}
html:not(.dark) .code-view .hljs-bullet{color:#953800}
html:not(.dark) .code-view .hljs-emphasis{color:#24292f;font-style:italic}
html:not(.dark) .code-view .hljs-strong{color:#24292f;font-weight:bold}
html:not(.dark) .code-view .hljs-addition{color:#116329;background-color:#f0fff4}
html:not(.dark) .code-view .hljs-deletion{color:#82071e;background-color:#ffebe9}
`

function CodeView({ content, language, startLine }: { content: string; language: string | null; startLine: number }) {
  const [showAll, setShowAll] = useState(false)
  const lines = useMemo(() => {
    const all = content.replace(/\r\n/g, '\n').split('\n')
    if (all.length > 1 && all[all.length - 1] === '') all.pop()
    return all
  }, [content])
  const shown = showAll ? lines : lines.slice(0, RENDER_LINES)
  const text = useMemo(() => shown.join('\n'), [shown])
  const numbers = useMemo(() => Array.from({ length: shown.length }, (_, i) => i + startLine).join('\n'), [shown.length, startLine])
  const [html, setHtml] = useState<string | null>(null)

  useEffect(() => {
    setHtml(null)
    if (!language || text.length > HIGHLIGHT_LIMIT) return
    let live = true
    void loadHighlighter()
      .then((hljs) => {
        if (!live || !hljs.getLanguage(language)) return
        setHtml(hljs.highlight(text, { language, ignoreIllegals: true }).value)
      })
      .catch(() => {
        /* plain text, then */
      })
    return () => {
      live = false
    }
  }, [text, language])

  return (
    <>
      <style href="misaka-code-view" precedence="medium">
        {LIGHT_TOKENS}
      </style>
      <div className="code-view mono flex min-w-max text-[0.75rem] leading-5" style={{ tabSize: 4 }}>
        <pre aria-hidden className="sticky left-0 z-[1] select-none border-r border-ink-100 bg-white px-2 text-right text-ink-300 dark:border-ink-800 dark:bg-ink-900 dark:text-ink-600">
          {numbers}
        </pre>
        {html !== null ? (
          <pre className="px-3 text-ink-800 dark:text-ink-200" dangerouslySetInnerHTML={{ __html: html }} />
        ) : (
          <pre className="px-3 text-ink-800 dark:text-ink-200">{text}</pre>
        )}
      </div>
      {shown.length < lines.length && (
        <div className="sticky left-0 px-3 py-2">
          <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => setShowAll(true)}>
            Show all {lines.length.toLocaleString('en-US')} lines
          </button>
        </div>
      )}
    </>
  )
}

// --- the viewer -------------------------------------------------------------------------------------

/** Files read recently, so going back and forth between two is instant. */
const viewed = new Map<string, WorkspaceFile>()
const VIEWED_KEEP = 12

function remember(key: string, file: WorkspaceFile) {
  viewed.delete(key)
  viewed.set(key, file)
  while (viewed.size > VIEWED_KEEP) viewed.delete(viewed.keys().next().value!)
}

function FileViewer({ root, path, conversationId }: { root: string; path: string; conversationId: string }) {
  const key = `${root}\n${path}`
  const [file, setFile] = useState<WorkspaceFile | null>(() => viewed.get(key) ?? null)
  const [error, setError] = useState<string | null>(null)
  const [loading, setLoading] = useState(false)
  const [nonce, setNonce] = useState(0)
  const backRef = useRef<HTMLButtonElement>(null)
  const toast = useStudio((s) => s.toast)

  useEffect(() => {
    const cached = nonce === 0 ? viewed.get(key) : undefined
    if (cached) {
      setFile(cached)
      setError(null)
      return
    }
    let live = true
    setLoading(true)
    setError(null)
    api.workspace
      .read(root, path, { max_bytes: VIEW_MAX_BYTES })
      .then((read) => {
        if (!live) return
        remember(key, read)
        setFile(read)
      })
      .catch((e: unknown) => {
        if (live) setError((e as Error).message)
      })
      .finally(() => {
        if (live) setLoading(false)
      })
    return () => {
      live = false
    }
  }, [key, root, path, nonce])

  // Opened from the tree or the filter, the keyboard follows into the viewer — but a viewer that
  // appears because another session in the folder was opened leaves the focus where it is.
  useEffect(() => {
    const back = backRef.current
    const active = document.activeElement
    if (back && (active === document.body || back.closest('aside')?.contains(active))) back.focus({ preventScroll: true })
  }, [])

  const { dir, name } = splitPath(path)
  const language = languageFor(path)
  const back = () => selectFile(root, null)

  return (
    <div
      className="flex h-full min-h-0 flex-col"
      onKeyDown={(event) => {
        // Escape goes back to the tree — claimed, so the window does not also read it as "stop".
        if (event.key === 'Escape' && !event.defaultPrevented) {
          event.preventDefault()
          back()
        }
      }}
    >
      <div className="flex shrink-0 items-center gap-1 border-b border-ink-200 px-2 py-1.5 dark:border-ink-800">
        <button ref={backRef} type="button" className="icon-btn" onClick={back} aria-label="Back to the files" title="Back to the files (Esc)">
          <Icon name="chevronLeft" className="size-4" />
        </button>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[0.8rem]" title={path}>
            <span className="text-ink-400 dark:text-ink-500">{dir}</span>
            <span className="font-medium text-ink-800 dark:text-ink-100">{name}</span>
          </div>
          {file && (
            <div className="truncate text-[0.65rem] text-ink-400 dark:text-ink-500">
              {file.binary ? 'binary' : `${file.total_lines.toLocaleString('en-US')} line${file.total_lines === 1 ? '' : 's'}`} · {bytes(file.size)}
              {language && !file.binary ? ` · ${language}` : ''}
            </div>
          )}
        </div>
        <button type="button" className="btn-ghost h-7 gap-1 px-2 py-0 text-[0.7rem]" onClick={() => void mentionInComposer(root, path, conversationId)} title="Mention this file in the composer and attach its content">
          <Icon name="at" className="size-3.5" />
          Mention
        </button>
        <button type="button" className="icon-btn" onClick={() => void copyText(path, 'the path', toast)} aria-label="Copy path" title="Copy the path">
          <Icon name="copy" className="size-3.5" />
        </button>
        <button type="button" className="icon-btn" onClick={() => setNonce((n) => n + 1)} aria-label="Read the file again" title="Read the file again" disabled={loading}>
          {loading ? <Spinner className="size-3.5" /> : <Icon name="refresh" className="size-3.5" />}
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-auto bg-white dark:bg-ink-900" tabIndex={0} aria-label={`Contents of ${path}`}>
        {error ? (
          <p className="p-3 text-xs text-red-600 dark:text-red-400">{error}</p>
        ) : !file ? (
          <div className="flex items-center gap-1.5 p-3 text-xs text-ink-400">
            <Spinner className="size-3.5" />
            Reading…
          </div>
        ) : file.binary ? (
          <p className="p-6 text-center text-xs text-ink-500 dark:text-ink-400">A binary file ({bytes(file.size)}) — there is nothing to show as text.</p>
        ) : file.content === '' ? (
          <p className="p-6 text-center text-xs text-ink-500 dark:text-ink-400">An empty file.</p>
        ) : (
          <CodeView content={file.content} language={language} startLine={file.start_line || 1} />
        )}
      </div>
      {file?.truncated && !file.binary && (
        <p className="shrink-0 border-t border-ink-200 px-3 py-1.5 text-[0.7rem] text-ink-500 dark:border-ink-800 dark:text-ink-400">
          Showing lines {file.start_line.toLocaleString('en-US')}–{file.end_line.toLocaleString('en-US')} of {file.total_lines.toLocaleString('en-US')}: the
          viewer reads the first {bytes(VIEW_MAX_BYTES, 0)} of a file.
        </p>
      )}
    </div>
  )
}

// --- the tree ---------------------------------------------------------------------------------------

type Row =
  | { kind: 'entry'; entry: WorkspaceEntry; depth: number; open: boolean }
  | { kind: 'note'; key: string; depth: number; text: string; tone: 'quiet' | 'error' }

function rowsOf(tree: FileTree): Row[] {
  const rows: Row[] = []
  const expanded = new Set(tree.expanded)
  const walk = (path: string, depth: number) => {
    const dir = tree.dirs[path]
    if (!dir || (dir.status === 'loading' && dir.entries.length === 0)) {
      if (path !== '.') rows.push({ kind: 'note', key: `${path}:loading`, depth, text: 'Reading…', tone: 'quiet' })
      return
    }
    if (dir.status === 'error') {
      rows.push({ kind: 'note', key: `${path}:error`, depth, text: dir.error ?? 'This folder could not be read.', tone: 'error' })
      return
    }
    // Folders first, as the runtime lists them — sorted again here so that holds whatever it sends.
    const entries = [...dir.entries].sort((a, b) => Number(b.is_dir) - Number(a.is_dir))
    if (entries.length === 0 && path !== '.') rows.push({ kind: 'note', key: `${path}:empty`, depth, text: 'Empty', tone: 'quiet' })
    for (const entry of entries) {
      const open = entry.is_dir && expanded.has(entry.path)
      rows.push({ kind: 'entry', entry, depth, open })
      if (open) walk(entry.path, depth + 1)
    }
    if (dir.truncated) rows.push({ kind: 'note', key: `${path}:more`, depth, text: 'More entries than are listed here — use the filter to find one.', tone: 'quiet' })
  }
  walk('.', 0)
  return rows
}

function parentOf(path: string): string | null {
  const at = path.lastIndexOf('/')
  return at === -1 ? null : path.slice(0, at)
}

function TreeView({ root, tree }: { root: string; tree: FileTree }) {
  const rows = useMemo(() => rowsOf(tree), [tree])
  const entries = rows.filter((r): r is Extract<Row, { kind: 'entry' }> => r.kind === 'entry')
  const [active, setActive] = useState<string | null>(tree.lastViewed)
  const scrollRef = useRef<HTMLDivElement>(null)
  const top = tree.dirs['.']

  useEffect(() => {
    void loadDir(root, '.')
  }, [root])

  // Back from the viewer: where the tree was, with the file just viewed in focus for the keyboard.
  // Only on mount (and for another folder): from then on the scroll position is the person's.
  useLayoutEffect(() => {
    const element = scrollRef.current
    const { scrollTop, lastViewed } = useFiles.getState().byRoot[root] ?? EMPTY_TREE
    if (element) element.scrollTop = scrollTop
    if (document.activeElement === document.body && lastViewed) {
      element?.querySelector<HTMLElement>(`[data-tree-row="${CSS.escape(lastViewed)}"]`)?.focus({ preventScroll: true })
    }
    return () => {
      if (element) rememberScroll(root, element.scrollTop)
    }
  }, [root])

  const activate = (entry: WorkspaceEntry) => (entry.is_dir ? toggleDir(root, entry.path) : selectFile(root, entry.path))
  const focusRow = (path: string | undefined) => {
    if (!path) return
    setActive(path)
    scrollRef.current?.querySelector<HTMLElement>(`[data-tree-row="${CSS.escape(path)}"]`)?.focus()
  }

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const current = (event.target as HTMLElement).dataset.treeRow
    if (current === undefined) return
    const index = entries.findIndex((r) => r.entry.path === current)
    const row = entries[index]
    if (!row) return
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault()
        focusRow(entries[index + 1]?.entry.path)
        break
      case 'ArrowUp':
        event.preventDefault()
        focusRow(entries[index - 1]?.entry.path)
        break
      case 'Home':
        event.preventDefault()
        focusRow(entries[0]?.entry.path)
        break
      case 'End':
        event.preventDefault()
        focusRow(entries[entries.length - 1]?.entry.path)
        break
      case 'ArrowRight':
        event.preventDefault()
        if (row.entry.is_dir && !row.open) toggleDir(root, row.entry.path, true)
        else if (row.open) focusRow(entries[index + 1]?.entry.path)
        break
      case 'ArrowLeft':
        event.preventDefault()
        if (row.open) toggleDir(root, row.entry.path, false)
        else focusRow(parentOf(row.entry.path) ?? undefined)
        break
      case 'Enter':
      case ' ':
        event.preventDefault()
        activate(row.entry)
        break
    }
  }

  if (!top || (top.status === 'loading' && top.entries.length === 0)) {
    return (
      <div className="flex items-center gap-1.5 p-3 text-xs text-ink-400">
        <Spinner className="size-3.5" />
        Reading the folder…
      </div>
    )
  }
  if (top.status === 'error') {
    return (
      <div className="space-y-2 p-3 text-xs">
        <p className="text-red-600 dark:text-red-400">{top.error}</p>
        <button type="button" className="btn-outline px-2 py-1 text-xs" onClick={() => void loadDir(root, '.', true)}>
          Try again
        </button>
      </div>
    )
  }
  if (entries.length === 0) return <p className="p-3 text-xs text-ink-500 dark:text-ink-400">This folder is empty.</p>

  const tabStop = active && entries.some((r) => r.entry.path === active) ? active : entries[0]?.entry.path

  return (
    <div ref={scrollRef} className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1" role="tree" aria-label={`Files in ${folderName(root)}`} onKeyDown={onKeyDown}>
      {rows.map((row) => {
        if (row.kind === 'note') {
          return (
            <div
              key={row.key}
              className={`truncate py-1 pr-2 text-[0.7rem] ${row.tone === 'error' ? 'text-red-600 dark:text-red-400' : 'text-ink-400 dark:text-ink-500'}`}
              style={{ paddingLeft: 30 + row.depth * 14 }}
            >
              {row.text}
            </div>
          )
        }
        const { entry, depth, open } = row
        const name = baseName(entry.path)
        const marked = tree.lastViewed === entry.path
        return (
          <div
            key={entry.path}
            role="treeitem"
            aria-level={depth + 1}
            aria-expanded={entry.is_dir ? open : undefined}
            aria-selected={marked}
            data-tree-row={entry.path}
            tabIndex={tabStop === entry.path ? 0 : -1}
            onFocus={() => setActive(entry.path)}
            onClick={() => {
              setActive(entry.path)
              activate(entry)
            }}
            title={entry.is_dir ? entry.path : `${entry.path} · ${bytes(entry.size)}`}
            className={`group flex h-7 cursor-default items-center gap-1.5 rounded-md pr-2 text-[0.8rem] outline-none hover:bg-ink-100 focus-visible:ring-2 focus-visible:ring-arc-500/40 dark:hover:bg-ink-800 ${
              marked ? 'bg-ink-100 text-ink-900 dark:bg-ink-800 dark:text-ink-50' : 'text-ink-700 dark:text-ink-200'
            }`}
            style={{ paddingLeft: 6 + depth * 14 }}
          >
            {entry.is_dir ? (
              <Icon name="chevron" className={`size-3 shrink-0 text-ink-400 transition-transform ${open ? 'rotate-90' : ''}`} />
            ) : (
              <span className="w-3 shrink-0" />
            )}
            <Icon
              name={entry.is_dir ? (open ? 'folderOpen' : 'folder') : 'file'}
              className={`size-3.5 shrink-0 ${entry.is_dir ? 'text-arc-600/80 dark:text-arc-400/80' : 'text-ink-400 dark:text-ink-500'}`}
            />
            <span className="min-w-0 flex-1 truncate">{name}</span>
            {!entry.is_dir && entry.size !== null && (
              <span className="shrink-0 text-[0.65rem] tabular-nums text-ink-400 opacity-0 group-hover:opacity-100 group-focus-visible:opacity-100 dark:text-ink-500">
                {bytes(entry.size)}
              </span>
            )}
          </div>
        )
      })}
    </div>
  )
}

// --- the filter -------------------------------------------------------------------------------------

function FilterResults({ root, query, listRef }: { root: string; query: string; listRef: RefObject<HTMLDivElement | null> }) {
  const [result, setResult] = useState<{ query: string; paths: string[]; error: string | null } | null>(null)

  useEffect(() => {
    let live = true
    // Debounced: a walk of a large tree per keystroke would outrun the typing.
    const timer = setTimeout(() => {
      api.workspace
        .files(root, query, 100)
        .then(({ paths }) => live && setResult({ query, paths, error: null }))
        .catch((error: unknown) => live && setResult({ query, paths: [], error: (error as Error).message }))
    }, 150)
    return () => {
      live = false
      clearTimeout(timer)
    }
  }, [root, query])

  if (!result) {
    return (
      <div className="flex items-center gap-1.5 p-3 text-xs text-ink-400">
        <Spinner className="size-3.5" />
        Finding files…
      </div>
    )
  }
  if (result.error) return <p className="p-3 text-xs text-red-600 dark:text-red-400">{result.error}</p>
  if (result.paths.length === 0) return <p className="p-3 text-xs text-ink-500 dark:text-ink-400">No file matches “{result.query}”.</p>

  const move = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
    const buttons = Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>('button[data-result]') ?? [])
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement)
    const next = buttons[event.key === 'ArrowDown' ? index + 1 : index - 1]
    if (next) {
      event.preventDefault()
      next.focus()
    } else if (event.key === 'ArrowUp' && index === 0) {
      event.preventDefault()
      listRef.current?.closest('[data-files]')?.querySelector<HTMLInputElement>('input')?.focus()
    }
  }

  return (
    <div ref={listRef} className="min-h-0 flex-1 overflow-y-auto px-1.5 py-1" onKeyDown={move} role="list" aria-label="Matching files">
      {result.paths.map((path) => {
        const { dir, name } = splitPath(path)
        return (
          <button
            key={path}
            type="button"
            role="listitem"
            data-result
            className="flex h-7 w-full items-center gap-1.5 rounded-md px-2 text-left text-[0.8rem] outline-none hover:bg-ink-100 focus-visible:bg-ink-100 dark:hover:bg-ink-800 dark:focus-visible:bg-ink-800"
            onClick={() => selectFile(root, path)}
            title={path}
          >
            <Icon name="file" className="size-3.5 shrink-0 text-ink-400 dark:text-ink-500" />
            <span className="min-w-0 truncate">
              <span className="font-medium text-ink-800 dark:text-ink-100">{name}</span>
              {dir && <span className="ml-1.5 text-[0.7rem] text-ink-400 dark:text-ink-500">{dir.replace(/\/$/, '')}</span>}
            </span>
          </button>
        )
      })}
    </div>
  )
}

// --- the tab ----------------------------------------------------------------------------------------

function Files({ root, conversationId }: { root: string; conversationId: string }) {
  const tree = useFileTree(root)
  const [refreshing, setRefreshing] = useState(false)
  const listRef = useRef<HTMLDivElement>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const query = tree.query

  // Back from the viewer to a filtered list: the keyboard lands in the filter, where it left.
  useEffect(() => {
    if (!tree.selected && query.trim() && document.activeElement === document.body) inputRef.current?.focus({ preventScroll: true })
    // Only when the viewer closes — not while the filter is typed into, hence not on `query`.
  }, [tree.selected])

  if (tree.selected) return <FileViewer key={tree.selected} root={root} path={tree.selected} conversationId={conversationId} />

  const refresh = async () => {
    setRefreshing(true)
    await refreshTree(root)
    setRefreshing(false)
  }

  return (
    <div className="flex h-full min-h-0 flex-col" data-files>
      <div className="flex shrink-0 items-center gap-1.5 border-b border-ink-200 px-2 py-1.5 dark:border-ink-800">
        <div className="relative min-w-0 flex-1">
          <Icon name="search" className="pointer-events-none absolute left-2 top-1/2 size-3.5 -translate-y-1/2 text-ink-400" />
          <input
            ref={inputRef}
            className="input h-7 py-0 pl-7 text-xs"
            placeholder={`Find a file in ${folderName(root)}`}
            value={query}
            aria-label="Find a file"
            spellCheck={false}
            onChange={(event) => setFilesQuery(root, event.target.value)}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing || event.keyCode === 229) return
              if (event.key === 'Escape' && query) {
                event.preventDefault()
                setFilesQuery(root, '')
              } else if (event.key === 'ArrowDown' && query.trim()) {
                event.preventDefault()
                listRef.current?.querySelector<HTMLButtonElement>('button[data-result]')?.focus()
              } else if (event.key === 'Enter' && query.trim()) {
                event.preventDefault()
                listRef.current?.querySelector<HTMLButtonElement>('button[data-result]')?.click()
              }
            }}
          />
        </div>
        <button type="button" className="icon-btn" onClick={() => void refresh()} aria-label="Read the folder again" title="Read the folder again" disabled={refreshing}>
          {refreshing ? <Spinner className="size-3.5" /> : <Icon name="refresh" className="size-3.5" />}
        </button>
      </div>
      {query.trim() ? <FilterResults root={root} query={query.trim()} listRef={listRef} /> : <TreeView root={root} tree={tree} />}
    </div>
  )
}

export function FilesPanel({ conversation }: { conversation: Conversation | null }) {
  const root = conversation?.workspace ?? null
  if (!conversation || !root) return <NoFolder conversation={conversation} what="its files" />
  return <Files root={root} conversationId={conversation.id} />
}
