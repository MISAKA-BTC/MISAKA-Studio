// The Code side's terminal, as plain logic: where a command runs, how it is quoted, what is kept
// of its output, and what ↑ brings back.
//
// The transport is one request per command (`execStream`): every command is a fresh login shell in
// the session's folder, so nothing a command does to its shell — an `export`, an alias, a `cd` —
// outlives it. `cd` is the one people expect to last, so it is done here, on the client: the
// terminal keeps a directory relative to the folder and runs each command as
// `cd '<dir>' && <command>`. It refuses to leave the folder. The runtime would happily run a command
// anywhere the shell can reach, but everything else on the Code side — the files, the changes, the
// agent — is about that folder, and a terminal that wandered off it would be the one place where a
// path meant something else.
//
// What a command prints is kept as its tail: a build log that runs to megabytes is read from the
// end, where the error is, and a window holding every byte of it would be the slow part.
//
// Plain TypeScript with explicit `.ts` imports, so the unit tests run it under Node directly.

import { MAX_ATTACHMENT_BYTES, attachmentId } from './attachments.ts'
import type { Attachment } from './types.ts'

/** The most of one command's output that is kept, in characters — the tail of it. */
export const MAX_BLOCK_CHARS = 200_000
/** Commands remembered for ↑, per folder. */
export const MAX_HISTORY = 100

/** How the shell the runtime starts reads a command line: a POSIX shell, or Windows' `cmd.exe`. */
export type ShellFlavor = 'posix' | 'windows'

/** The flavor of the runtime's shell, from the OS name the runtime reports. */
export function shellFlavor(os: string | null | undefined): ShellFlavor {
  return os && /windows/i.test(os) ? 'windows' : 'posix'
}

// --- quoting and the working directory ------------------------------------------------------

/**
 * One argument, quoted so the shell reads it back exactly. POSIX: single quotes, inside which
 * nothing is special — a `'` itself is closed, escaped and reopened (`'\''`). `cmd.exe`: double
 * quotes; a Windows file name cannot contain `"`, so there is nothing inside to escape.
 */
export function shellQuote(value: string, flavor: ShellFlavor = 'posix'): string {
  if (flavor === 'windows') return `"${value.replace(/"/g, '')}"`
  return `'${value.replace(/'/g, `'\\''`)}'`
}

/** `command`, run in `cwd` (relative to the folder; `''` is the folder itself). */
export function withCwd(command: string, cwd: string, flavor: ShellFlavor = 'posix'): string {
  if (!cwd) return command
  if (flavor === 'windows') return `cd /d ${shellQuote(cwd.replace(/\//g, '\\'), 'windows')} && ${command}`
  // A directory whose name starts with `-` would be read as an option to `cd`; `./` keeps it a path
  // in every shell, which `--` does not.
  return `cd ${shellQuote(cwd.startsWith('-') ? `./${cwd}` : cwd)} && ${command}`
}

/** What the prompt says: the folder's name, and the directory within it. */
export function promptLabel(folder: string, cwd: string): string {
  return cwd ? `${folder}/${cwd}` : folder
}

// --- `cd` -------------------------------------------------------------------------------------

export type CdParse =
  /** A lone `cd`: its one argument, or null for none. */
  | { ok: true; target: string | null }
  | { ok: false; error: string }

/** Characters that make a line more than a lone `cd` — a list, a pipe, a redirection. */
const COMPOUND = new Set([';', '&', '|', '<', '>', '(', ')', '\n'])
/** Characters the shell would expand, which this terminal cannot do for it. */
const EXPANDS = new Set(['$', '`', '*', '?', '[', '{'])

/**
 * Read `input` as a lone `cd`. Returns null when it is not one — another command, or a compound
 * line like `cd src && make`, which goes to the shell as typed (where its `cd` lasts for that one
 * line, as it would anywhere). An argument the shell would expand (`$HOME`, `src/*`) cannot be
 * resolved here, and says so rather than going somewhere unexpected.
 */
export function parseCd(input: string): CdParse | null {
  const line = input.trim()
  if (line !== 'cd' && !/^cd\s/.test(line)) return null
  const words: string[] = []
  // `''` is a word (an empty argument), so being in one is tracked apart from its text.
  let word = ''
  let inWord = false
  let quote: '"' | "'" | null = null
  for (let i = 2; i < line.length; i++) {
    const ch = line[i]!
    if (quote === "'") {
      if (ch === "'") quote = null
      else word += ch
      continue
    }
    if (quote === '"') {
      if (ch === '"') quote = null
      else if (ch === '\\' && i + 1 < line.length && '"\\$`'.includes(line[i + 1]!)) word += line[++i]
      else if (ch === '$' || ch === '`') return { ok: false, error: 'cd: variables are not expanded by this terminal — give the path itself.' }
      else word += ch
      continue
    }
    if (/\s/.test(ch)) {
      if (inWord) words.push(word)
      word = ''
      inWord = false
      continue
    }
    if (COMPOUND.has(ch)) return null
    // A `#` that starts a word starts a comment, which runs to the end of the line.
    if (ch === '#' && !inWord) break
    if (EXPANDS.has(ch)) return { ok: false, error: 'cd: variables and wildcards are not expanded by this terminal — give the path itself.' }
    inWord = true
    if (ch === "'" || ch === '"') {
      quote = ch
      continue
    }
    if (ch === '\\') {
      word += line[i + 1] ?? ''
      i++
      continue
    }
    word += ch
  }
  if (quote) return { ok: false, error: 'cd: the quote is not closed.' }
  if (inWord) words.push(word)

  const args: string[] = []
  let options = true
  for (const w of words) {
    if (options && w === '--') {
      options = false
      continue
    }
    if (options && (w === '-P' || w === '-L')) continue
    if (options && w.startsWith('-') && w !== '-') return { ok: false, error: `cd: ${w}: options other than -P and -L are not supported here.` }
    args.push(w)
  }
  if (args.length > 1) return { ok: false, error: 'cd: too many arguments.' }
  return { ok: true, target: args[0] ?? null }
}

/** `path` with `.` and `..` resolved, `/`-separated, no leading or trailing slash; null when it
 *  climbs above where it starts. */
export function normalizeRelative(path: string, flavor: ShellFlavor = 'posix'): string | null {
  // On a POSIX system a backslash is an ordinary character in a file name.
  const parts = path.split(flavor === 'windows' ? /[\\/]+/ : /\/+/)
  const out: string[] = []
  for (const part of parts) {
    if (!part || part === '.') continue
    if (part === '..') {
      if (out.length === 0) return null
      out.pop()
      continue
    }
    out.push(part)
  }
  return out.join('/')
}

function isAbsolute(path: string, flavor: ShellFlavor): boolean {
  if (flavor === 'windows') return /^[A-Za-z]:[\\/]/.test(path) || /^[\\/]/.test(path)
  return path.startsWith('/')
}

/** An absolute path with `/` separators, `.`/`..` resolved and no trailing slash. */
function canonicalAbsolute(path: string, flavor: ShellFlavor): string {
  const slashed = flavor === 'windows' ? path.replace(/\\/g, '/') : path
  const drive = /^[A-Za-z]:/.exec(slashed)?.[0] ?? ''
  const rest = slashed.slice(drive.length)
  const parts: string[] = []
  for (const part of rest.split('/')) {
    if (!part || part === '.') continue
    if (part === '..') parts.pop()
    else parts.push(part)
  }
  return `${drive}/${parts.join('/')}`
}

function lastSegment(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path
}

export type CdOptions = {
  /** The folder's absolute path. */
  root: string
  /** Where `cd -` goes back to. */
  previous?: string | null
  /** The home directory, for `~`; without it, `~` cannot be resolved. */
  home?: string | null
  flavor?: ShellFlavor
}

export type CdResult = { ok: true; cwd: string } | { ok: false; error: string }

/**
 * Where `cd target` goes from `cwd` (relative to the folder), or why it cannot. A bare `cd` goes
 * back to the folder itself — this terminal's home. Anything that would leave the folder is refused.
 */
export function resolveCd(cwd: string, target: string | null, options: CdOptions): CdResult {
  const flavor = options.flavor ?? 'posix'
  const folder = lastSegment(options.root)
  const outside = (what: string): CdResult => ({
    ok: false,
    error: `cd: ${what} is outside ${folder} — this terminal stays in the session's folder.`,
  })
  if (target === null || target === '') return { ok: true, cwd: '' }
  if (target === '-') {
    return options.previous === null || options.previous === undefined ? { ok: false, error: 'cd: there is no previous directory yet.' } : { ok: true, cwd: options.previous }
  }

  let path = target
  if (path === '~' || path.startsWith('~/') || (flavor === 'windows' && path.startsWith('~\\'))) {
    if (!options.home) return outside(target)
    path = `${options.home}${path.slice(1)}`
  } else if (path.startsWith('~')) {
    return { ok: false, error: `cd: ${target}: other users' home directories are not supported here.` }
  }

  if (isAbsolute(path, flavor)) {
    const root = canonicalAbsolute(options.root, flavor)
    const absolute = canonicalAbsolute(path, flavor)
    const same = (a: string, b: string) => (flavor === 'windows' ? a.toLowerCase() === b.toLowerCase() : a === b)
    if (same(absolute, root)) return { ok: true, cwd: '' }
    const prefix = root.endsWith('/') ? root : `${root}/`
    if (!same(absolute.slice(0, prefix.length), prefix)) return outside(target)
    return { ok: true, cwd: normalizeRelative(absolute.slice(prefix.length), flavor) ?? '' }
  }

  const resolved = normalizeRelative(cwd ? `${cwd}/${path}` : path, flavor)
  if (resolved === null) return outside(target)
  return { ok: true, cwd: resolved }
}

// --- history ------------------------------------------------------------------------------------

/**
 * `history` with `command` as its newest entry: an earlier copy of the same line goes, so ↑ never
 * walks through the same command twice, and the oldest go past `max`. A line typed with a leading
 * space is not remembered — the shells' convention for a command that carries a secret.
 */
export function pushHistory(history: readonly string[], command: string, max = MAX_HISTORY): string[] {
  const entry = command.trim()
  if (!entry || /^\s/.test(command)) return [...history]
  const next = history.filter((h) => h !== entry)
  next.push(entry)
  return next.length > max ? next.slice(next.length - max) : next
}

/**
 * ↑/↓ through `length` remembered commands. `index` is the entry on the line now — null for the
 * line being typed. ↑ from the typed line goes to the newest entry and stops at the oldest; ↓ past
 * the newest returns to the typed line.
 */
export function historyStep(length: number, index: number | null, direction: 'up' | 'down'): number | null {
  if (length === 0) return null
  if (direction === 'up') return index === null ? length - 1 : Math.max(0, index - 1)
  if (index === null) return null
  return index + 1 >= length ? null : index + 1
}

// --- output ---------------------------------------------------------------------------------------

export type OutputStream = 'stdout' | 'stderr'
export type OutputSegment = { stream: OutputStream; text: string }

/** What is kept of a command's output: runs of one stream each, and how much was let go. */
export type BlockOutput = {
  segments: OutputSegment[]
  /** Characters kept. */
  size: number
  /** Characters dropped from the start to keep the tail, and the lines among them. */
  dropped: number
  droppedLines: number
}

export const EMPTY_OUTPUT: BlockOutput = { segments: [], size: 0, dropped: 0, droppedLines: 0 }

function countLines(text: string): number {
  let n = 0
  for (let i = text.indexOf('\n'); i !== -1; i = text.indexOf('\n', i + 1)) n++
  return n
}

/**
 * `output` with `text` added on `stream`, keeping at most `max` characters — the newest. A run of
 * one stream stays one segment. The cut lands after a line break when there is one close by, so the
 * kept text starts with a whole line rather than the end of one.
 */
export function appendOutput(output: BlockOutput, stream: OutputStream, text: string, max = MAX_BLOCK_CHARS): BlockOutput {
  if (!text) return output
  const segments = output.segments.slice()
  const last = segments[segments.length - 1]
  if (last && last.stream === stream) segments[segments.length - 1] = { stream, text: last.text + text }
  else segments.push({ stream, text })
  let size = output.size + text.length
  let dropped = output.dropped
  let droppedLines = output.droppedLines
  while (size > max && segments.length > 0) {
    const first = segments[0]!
    const over = size - max
    if (first.text.length <= over) {
      segments.shift()
      size -= first.text.length
      dropped += first.text.length
      droppedLines += countLines(first.text)
      continue
    }
    let cut = over
    const newline = first.text.indexOf('\n', cut)
    if (newline !== -1 && newline - cut < 512) cut = newline + 1
    const gone = first.text.slice(0, cut)
    const kept = first.text.slice(cut)
    size -= cut
    dropped += cut
    droppedLines += countLines(gone)
    if (kept) segments[0] = { stream: first.stream, text: kept }
    else segments.shift()
  }
  return { segments, size, dropped, droppedLines }
}

// Colour and cursor escapes. The runtime asks for none (`NO_COLOR`, `TERM=dumb`), but a tool that
// insists still sends them, and they are noise as text.
// eslint-disable-next-line no-control-regex
const ANSI = /\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g

export function stripAnsi(text: string): string {
  return text.replace(ANSI, '')
}

/** A progress bar redraws its line with `\r`; only what it drew last is worth reading. */
export function applyCarriageReturns(text: string): string {
  if (!text.includes('\r')) return text
  return text
    .split('\n')
    .map((line) => {
      const body = line.endsWith('\r') ? line.slice(0, -1) : line
      const at = body.lastIndexOf('\r')
      return at === -1 ? body : body.slice(at + 1)
    })
    .join('\n')
}

/** Output as it should read: no escapes, no overdrawn progress lines. */
export function displayText(text: string): string {
  return applyCarriageReturns(stripAnsi(text))
}

/** Every kept segment, in order, as one text. */
export function outputText(output: BlockOutput): string {
  return displayText(output.segments.map((s) => s.text).join(''))
}

/** The line that says the start of the output was let go, or null when nothing was. */
export function droppedNote(output: BlockOutput): string | null {
  if (output.dropped === 0) return null
  return output.droppedLines > 0
    ? `… ${output.droppedLines.toLocaleString('en-US')} earlier line${output.droppedLines === 1 ? '' : 's'} not kept`
    : `… ${output.dropped.toLocaleString('en-US')} earlier characters not kept`
}

// --- a finished command, as text --------------------------------------------------------------------

/** `40ms`, `0.4s`, `12s`, `3m 05s`, `1h 02m`. */
export function formatDuration(ms: number): string {
  const safe = Math.max(0, Math.round(ms))
  if (safe < 100) return `${safe}ms`
  if (safe < 10_000) return `${(safe / 1000).toFixed(1)}s`
  const seconds = Math.round(safe / 1000)
  if (seconds < 60) return `${seconds}s`
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s`
  return `${Math.floor(seconds / 3600)}h ${String(Math.floor((seconds % 3600) / 60)).padStart(2, '0')}m`
}

export type BlockStatus = 'running' | 'done' | 'stopped' | 'timed_out' | 'error'

/** What a transcript needs of a command block. */
export type BlockSummary = {
  command: string
  /** The prompt it ran at: folder and directory. */
  prompt: string
  output: BlockOutput
  status: BlockStatus
  exitCode: number | null
  durationMs: number | null
  error?: string | null
}

/** How the command ended, in the words its badge and its transcript use. */
export function outcomeText(block: Pick<BlockSummary, 'status' | 'exitCode' | 'durationMs' | 'error'>): string {
  const took = block.durationMs !== null ? ` · ${formatDuration(block.durationMs)}` : ''
  switch (block.status) {
    case 'running':
      return 'running'
    case 'done':
      return `exit ${block.exitCode ?? '?'}${took}`
    case 'stopped':
      return `stopped${took}`
    case 'timed_out':
      return `timed out${took}`
    case 'error':
      return block.error ? `not run: ${block.error}` : 'not run'
  }
}

/** The command, what it printed and how it ended — what Copy puts on the clipboard. */
export function transcript(block: BlockSummary): string {
  const output = outputText(block.output)
  const note = droppedNote(block.output)
  const lines = [`${block.prompt} $ ${block.command}`]
  if (note) lines.push(`[${note}]`)
  if (output) lines.push(output.endsWith('\n') ? output.slice(0, -1) : output)
  lines.push(`[${outcomeText(block)}]`)
  return lines.join('\n')
}

/** The end of `text` that fits in `bytes` of UTF-8, never starting in the middle of a character. */
function utf8Tail(text: string, bytes: number): string {
  const encoder = new TextEncoder()
  let take = Math.min(text.length, bytes)
  let tail = text.slice(text.length - take)
  let size = encoder.encode(tail).length
  while (size > bytes && take > 0) {
    take -= Math.max(1, Math.ceil((size - bytes) / 3))
    tail = text.slice(text.length - Math.max(0, take))
    size = encoder.encode(tail).length
  }
  // A cut between the two halves of a surrogate pair leaves half a character.
  if (/^[\uDC00-\uDFFF]/.test(tail)) tail = tail.slice(1)
  return tail
}

/**
 * The block as an attachment for the composer: the command and its output, named so its chip says
 * what it is. Past the attachment limit the START of the output goes — a command's verdict is at
 * its end — and the attachment says it was cut.
 */
export function terminalAttachment(block: BlockSummary): Attachment {
  const full = transcript(block)
  const encoder = new TextEncoder()
  let content = full
  let cut = block.output.dropped > 0
  if (encoder.encode(full).length > MAX_ATTACHMENT_BYTES) {
    const head = `${block.prompt} $ ${block.command}\n[… the start of the output is not attached]\n`
    content = head + utf8Tail(full, MAX_ATTACHMENT_BYTES - encoder.encode(head).length)
    cut = true
  }
  const short = block.command.length > 40 ? `${block.command.slice(0, 39)}…` : block.command
  return {
    id: attachmentId(),
    name: `terminal output · ${short}`,
    size: encoder.encode(content).length,
    content,
    language: 'console',
    ...(cut ? { truncated: true } : {}),
  }
}
