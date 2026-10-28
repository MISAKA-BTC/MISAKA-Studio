// The coding agent's tools: a parsed call checked, decided on by the approval policy, run through
// the runtime's workspace API, and reported twice — as text for the model, and as a card for the
// person (`ToolDisplay`).
//
// Two rules hold everywhere in this file:
//
// * **Nothing the policy says to ask about runs before it is approved.** A call is first
//   *prepared* — arguments coerced, the policy consulted, and for an edit the would-be diff worked
//   out from the file as it is now — and only `run` touches the disk. The loop decides between the
//   two, after asking.
// * **What the model reads is written for a small model.** Numbered lines it can copy from, one
//   match per line, errors that say what to do next, and a cap on every result so one long file
//   cannot push the task out of an 8K window.
//
// The workspace API is passed in (`ToolContext.api`, shaped like `api.workspace`), so the tests run
// this against a fake folder.

import type {
  ApprovalPolicy,
  FileChange,
  ToolDisplay,
  WorkspaceEdit,
  WorkspaceEntry,
  WorkspaceExec,
  WorkspaceFile,
  WorkspaceMatch,
  WorkspaceSearch,
  WorkspaceTree,
  WorkspaceWrite,
} from '../types.ts'
import { diffStats, lineCount, structuredDiff, unifiedPatch } from '../diff.ts'
import { planEdit } from './edit.ts'
import { TOOL_NAMES, toolSpec, truncateMiddle, type ToolKind, type ToolSpec } from './protocol.ts'

/** The workspace endpoints, as `api.workspace` has them. */
export type WorkspaceApi = {
  tree: (root: string, path?: string, depth?: number, limit?: number) => Promise<WorkspaceTree>
  read: (root: string, path: string, range?: { start_line?: number; end_line?: number; max_bytes?: number }) => Promise<WorkspaceFile>
  search: (
    root: string,
    pattern: string,
    options?: { path?: string; glob?: string; regex?: boolean; case_insensitive?: boolean; max_results?: number },
  ) => Promise<WorkspaceSearch>
  write: (root: string, path: string, content: string) => Promise<WorkspaceWrite>
  edit: (root: string, path: string, oldString: string, newString: string, replaceAll?: boolean) => Promise<WorkspaceEdit>
  exec: (root: string, command: string, timeoutSecs?: number, signal?: AbortSignal) => Promise<WorkspaceExec>
}

/** The kinds of call the policy can be asked about. Reads never are. */
export type ApprovalKind = Exclude<ToolKind, 'read'>

/** Characters of tool output the model is sent, per tool. */
export type OutputLimits = { read: number; search: number; list: number; exec: number }

export type ToolContext = {
  /** The workspace's absolute path. */
  root: string
  api: WorkspaceApi
  policy: ApprovalPolicy
  /** Kinds the person chose "Always allow" for in this thread. */
  allowed: ReadonlySet<ApprovalKind>
  limits: OutputLimits
}

/** What a call came to. */
export type ToolOutcome = {
  status: 'done' | 'error'
  /** Exactly what the model is sent. */
  output: string
  /** One line for the card's header. */
  summary: string
  display?: ToolDisplay
  error?: string
  /** A file this call changed, for the conversation's Changes list. */
  change?: { path: string; previous: string | null; content: string; created: boolean }
}

/** A call ready to run — or already settled, when there is nothing to run (bad arguments, a
 *  refusal, an edit that cannot apply, a write that changes nothing). */
export type PreparedCall =
  | { kind: 'settled'; outcome: ToolOutcome }
  | {
      kind: 'run'
      toolKind: ToolKind
      /** `ask`: wait for the person before calling `run`. */
      approval: 'auto' | 'ask'
      summary: string
      /** The arguments as coerced, for the card. */
      args: Record<string, unknown>
      /** What the approval card shows: the diff the edit would make, or the command. */
      preview?: ToolDisplay
      run: (signal: AbortSignal) => Promise<ToolOutcome>
    }

/** An original or current file content over this size is not kept: the change is listed, not revertible. */
export const TRACK_LIMIT_BYTES = 256 * 1024
/** Whole files are read up to this size for previews and revert checks — the most `edit` changes. */
export const WHOLE_FILE_BYTES = 4 * 1024 * 1024
/** Kept on a card, beyond what the model was sent: enough to review, not a second copy of a build log. */
const DISPLAY_STREAM_CHARS = 20_000
const DISPLAY_MATCHES = 300
const DISPLAY_ENTRIES = 500
const DISPLAY_PATCH_CHARS = 200_000
/** A line longer than this is cut in `read_file` output — a minified bundle, not code to edit. */
const LINE_CHARS = 1000

/**
 * Output caps for a context window: about a quarter of it per result, at roughly three characters
 * a token for code, within 2K–16K characters. Commands, searches and listings get less — their
 * long outputs are rarely all needed, and the head and tail say the most.
 */
export function outputLimits(contextTokens: number | null | undefined): OutputLimits {
  const window = contextTokens && contextTokens > 0 ? contextTokens : 16_384
  const budget = Math.max(2000, Math.min(16_000, Math.floor(window * 0.75)))
  return { read: budget, search: Math.min(budget, 6000), list: Math.min(budget, 6000), exec: Math.min(budget, 8000) }
}

/** What the policy says about a call of this kind. */
export function approvalFor(kind: ToolKind, policy: ApprovalPolicy, allowed: ReadonlySet<ApprovalKind> = new Set()): 'auto' | 'ask' | 'refuse' {
  if (kind === 'read') return 'auto'
  if (policy === 'read_only') return 'refuse'
  if (kind === 'edit') return policy === 'ask' && !allowed.has('edit') ? 'ask' : 'auto'
  return policy === 'full_auto' || allowed.has('command') ? 'auto' : 'ask'
}

/** The names small models reach for instead of ours. */
const ALIASES: Record<string, string> = {
  read: 'read_file',
  readfile: 'read_file',
  cat: 'read_file',
  view: 'read_file',
  open_file: 'read_file',
  write: 'write_file',
  writefile: 'write_file',
  create_file: 'write_file',
  edit: 'edit_file',
  editfile: 'edit_file',
  str_replace: 'edit_file',
  replace: 'edit_file',
  ls: 'list_files',
  list: 'list_files',
  list_dir: 'list_files',
  list_directory: 'list_files',
  grep: 'search',
  search_files: 'search',
  rg: 'search',
  bash: 'run_command',
  shell: 'run_command',
  exec: 'run_command',
  run: 'run_command',
  execute_command: 'run_command',
}

/** A call's tool name as ours: `functions.read_file`, `readFile` and `cat` all mean read_file. */
export function canonicalToolName(name: string): string {
  const bare = name
    .trim()
    .replace(/^functions\./, '')
    .replace(/([a-z])([A-Z])/g, '$1_$2')
    .toLowerCase()
  if (TOOL_NAMES.includes(bare)) return bare
  return ALIASES[bare.replace(/_/g, '')] ?? ALIASES[bare] ?? bare
}

/** Arguments as the tool's parameters say they are: "12" → 12, "true" → true, an object → JSON. */
export function coerceArgs(spec: ToolSpec, args: Record<string, unknown>): { ok: true; args: Record<string, unknown> } | { ok: false; error: string } {
  const out: Record<string, unknown> = {}
  for (const param of spec.params) {
    const value = args[param.name]
    const missing = value === undefined || value === null || (param.type !== 'string' && typeof value === 'string' && value.trim() === '')
    if (missing) {
      if (param.required) {
        return { ok: false, error: `${spec.name} needs the parameter "${param.name}" (${param.description}). Required parameters MUST be specified.` }
      }
      continue
    }
    if (param.type === 'integer') {
      const number = typeof value === 'number' ? value : Number(String(value).trim())
      if (!Number.isFinite(number) || !Number.isInteger(number)) return { ok: false, error: `${param.name} must be a whole number, not ${JSON.stringify(value)}.` }
      out[param.name] = number
    } else if (param.type === 'boolean') {
      if (typeof value === 'boolean') out[param.name] = value
      else {
        const text = String(value).trim().toLowerCase()
        if (['true', '1', 'yes', 'on'].includes(text)) out[param.name] = true
        else if (['false', '0', 'no', 'off'].includes(text)) out[param.name] = false
        else return { ok: false, error: `${param.name} must be true or false, not ${JSON.stringify(value)}.` }
      }
    } else if (typeof value === 'string') {
      out[param.name] = value
    } else if (typeof value === 'number' || typeof value === 'boolean') {
      out[param.name] = String(value)
    } else {
      // A JSON file's content written as an object, by a model calling in JSON.
      out[param.name] = JSON.stringify(value, null, 2)
    }
  }
  return { ok: true, args: out }
}

/** A path as the runtime should see it: no quotes or backticks around it, no `./` in front. */
function cleanPath(path: unknown): string {
  const text = String(path ?? '')
    .trim()
    .replace(/^[`'"]+|[`'"]+$/g, '')
  return text.replace(/^(\.\/)+/, '') || '.'
}

/** The runtime's errors name its endpoints; the model knows the tools. */
export function forModel(message: string): string {
  return message
    .replace(/use write to create/g, 'use write_file to create')
    .replace(/\buse write;/g, 'use write_file;')
    .replace(/list it with tree/g, 'list it with list_files')
    .replace(/run git commands with exec instead/g, 'run git commands with run_command instead')
}

function errorText(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}

function isAbort(error: unknown): boolean {
  return (error as { name?: string } | null)?.name === 'AbortError'
}

/** UTF-8 length, without encoding the string. */
function utf8Bytes(text: string): number {
  let bytes = 0
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i)
    if (code < 0x80) bytes += 1
    else if (code < 0x800) bytes += 2
    else if (code >= 0xd800 && code <= 0xdbff) {
      bytes += 4
      i++
    } else bytes += 3
  }
  return bytes
}

function plural(count: number, one: string, many = `${one}s`): string {
  return `${count} ${count === 1 ? one : many}`
}

function settled(outcome: ToolOutcome): PreparedCall {
  return { kind: 'settled', outcome }
}

function failure(summary: string, error: string, display?: ToolDisplay): ToolOutcome {
  return { status: 'error', output: `Error: ${error}`, summary, error, ...(display ? { display } : {}) }
}

/** A string argument cut for keeping on the card: the full text is in the message already. */
export function compactArgs(args: Record<string, unknown>): Record<string, unknown> {
  const out: Record<string, unknown> = {}
  for (const [key, value] of Object.entries(args)) {
    out[key] = typeof value === 'string' && value.length > 800 ? `${value.slice(0, 800)}… (${value.length.toLocaleString('en-US')} characters)` : value
  }
  return out
}

/** One line for a call's header, before it has run: "Read src/main.rs · 1–120". */
export function summarizeCall(name: string, args: Record<string, unknown>): string {
  const path = typeof args.path === 'string' && args.path.trim() ? cleanPath(args.path) : null
  switch (name) {
    case 'list_files':
      return `List ${path && path !== '.' ? path : 'the workspace'}`
    case 'read_file': {
      const start = args.start_line
      const end = args.end_line
      return `Read ${path ?? 'a file'}${start || end ? ` · ${start ?? 1}–${end ?? '…'}` : ''}`
    }
    case 'search':
      return `Search ${JSON.stringify(String(args.pattern ?? ''))}${path && path !== '.' ? ` in ${path}` : ''}${args.glob ? ` (${String(args.glob)})` : ''}`
    case 'edit_file':
      return `Edit ${path ?? 'a file'}`
    case 'write_file':
      return `Write ${path ?? 'a file'}`
    case 'run_command':
      return `Run ${String(args.command ?? '').split('\n')[0]}`
    default:
      return name || 'Tool call'
  }
}

// --- reading a whole file -------------------------------------------------------------

type WholeFile = { exists: false } | { exists: true; content: string; complete: boolean; binary: boolean; size: number }

function isMissing(error: unknown): boolean {
  const status = (error as { status?: number } | null)?.status
  return status === 404 || /does not exist|no such file|not found/i.test(errorText(error))
}

/** The file as it is now, when it exists: `complete` when `content` is all of it. */
export async function readWhole(api: WorkspaceApi, root: string, path: string): Promise<WholeFile> {
  try {
    const file = await api.read(root, path, { max_bytes: WHOLE_FILE_BYTES })
    const complete = !file.binary && !file.truncated && (file.total_lines === 0 || (file.start_line <= 1 && file.end_line >= file.total_lines))
    return { exists: true, content: file.content, complete, binary: file.binary, size: file.size }
  } catch (error) {
    if (isMissing(error)) return { exists: false }
    throw error
  }
}

// --- formatting for the model ---------------------------------------------------------

/** `cat -n`-style lines, numbered from `start`: the format the model copies old_string out of. */
export function numberLines(content: string, start: number): string {
  const lines = content.split('\n')
  if (lines[lines.length - 1] === '') lines.pop()
  const width = Math.max(3, String(start + lines.length - 1).length)
  return lines
    .map((raw, i) => {
      const line = raw.endsWith('\r') ? raw.slice(0, -1) : raw
      const text = line.length > LINE_CHARS ? `${line.slice(0, LINE_CHARS)} ... [line cut at ${LINE_CHARS} of ${line.length} characters]` : line
      return `${String(start + i).padStart(width, ' ')}\t${text}`
    })
    .join('\n')
}

/** A listing as an indented tree, relative to the directory that was listed. */
export function formatTree(tree: WorkspaceTree, max: number): string {
  const base = tree.path === '.' || tree.path === '' ? '' : `${tree.path.replace(/\/$/, '')}/`
  const lines = tree.entries.map((entry) => {
    const rel = base && entry.path.startsWith(base) ? entry.path.slice(base.length) : entry.path
    const parts = rel.split('/')
    return `${'  '.repeat(parts.length - 1)}${parts[parts.length - 1]}${entry.is_dir ? '/' : ''}`
  })
  const title = `${base || './'} (${plural(tree.entries.length, 'entry', 'entries')}${tree.truncated ? ', more not listed' : ''})`
  let body = ''
  let shown = 0
  for (const line of lines) {
    if (body.length + line.length + 1 > max) break
    body += `${line}\n`
    shown++
  }
  const notes: string[] = []
  if (shown < lines.length) notes.push(`${lines.length - shown} more entries not shown: list a subfolder, or use a smaller depth.`)
  else if (tree.truncated) notes.push('The listing stopped early: list a subfolder, or use a smaller depth.')
  if (tree.entries.length === 0) return `${title}\n(empty)`
  return `${title}\n${body.trimEnd()}${notes.length ? `\n[${notes.join(' ')}]` : ''}`
}

/** Search results, one `path:line: text` per match, capped. */
export function formatMatches(pattern: string, result: WorkspaceSearch, max: number, where: string | null): string {
  if (result.matches.length === 0) {
    return `No matches for ${JSON.stringify(pattern)}${where ? ` in ${where}` : ''} (${plural(result.files_scanned, 'file')} searched).`
  }
  const files = new Set(result.matches.map((m) => m.path)).size
  let body = ''
  let shown = 0
  for (const match of result.matches) {
    const line = `${match.path}:${match.line}: ${match.text}`
    if (body.length + line.length + 1 > max) break
    body += `${line}\n`
    shown++
  }
  const head = `${plural(result.matches.length, 'match', 'matches')}${result.truncated ? '+' : ''} in ${plural(files, 'file')}:`
  const notes: string[] = []
  if (shown < result.matches.length) notes.push(`${result.matches.length - shown} more matches not shown.`)
  if (result.truncated || shown < result.matches.length) notes.push('Narrow the search with path or glob, or a more specific pattern.')
  return `${head}\n${body.trimEnd()}${notes.length ? `\n[${notes.join(' ')}]` : ''}`
}

/** A command's result: exit code and both streams, head and tail kept within `max`. */
export function formatExec(result: WorkspaceExec, max: number): string {
  const seconds = (result.duration_ms / 1000).toFixed(1)
  const status = result.timed_out
    ? `Timed out after ${seconds} s and was stopped.`
    : result.exit_code === null
      ? `Killed by a signal after ${seconds} s.`
      : `Exit code ${result.exit_code} (${seconds} s).`
  const stdout = result.stdout.trimEnd()
  const stderr = result.stderr.trimEnd()
  if (!stdout && !stderr) return `${status}\n(no output)`
  // Shared between the streams: one that is short leaves its room to the other.
  const room = Math.max(400, max - status.length - 40)
  const stderrRoom = stdout ? Math.max(Math.min(stderr.length, Math.floor(room * 0.4)), room - stdout.length) : room
  const stdoutRoom = room - Math.min(stderr.length, stderrRoom)
  const hint = 'rerun with less output, e.g. redirect to a file and search it'
  const parts = [status]
  if (stdout) parts.push(`stdout:\n${truncateMiddle(stdout, stdoutRoom, hint)}`)
  if (stderr) parts.push(`stderr:\n${truncateMiddle(stderr, stderrRoom, hint)}`)
  if (result.truncated) parts.push('[The runtime kept only the start and end of a very long output.]')
  return parts.join('\n')
}

/** The changed region of a file after an edit, numbered — so the next edit there can be made
 *  without reading the file again. */
function editedRegion(previous: string, content: string): string {
  const diff = structuredDiff(previous, content, 2)
  const out: string[] = []
  let count = 0
  for (const hunk of diff.hunks) {
    if (out.length) out.push('...')
    for (const line of hunk.lines) {
      if (line.kind === 'del') continue
      if (count >= 40) {
        out.push('... (more changed lines not shown)')
        return out.join('\n')
      }
      out.push(`${String(line.newLine).padStart(3, ' ')}\t${line.text.length > LINE_CHARS ? `${line.text.slice(0, LINE_CHARS)} ...` : line.text}`)
      count++
    }
  }
  return out.join('\n')
}

// --- displays for the card ---------------------------------------------------------------

/** A diff card for a change, the patch capped for storage (its counts are always the full ones). */
export function diffDisplay(path: string, previous: string | null, content: string, created: boolean): ToolDisplay {
  let patch = unifiedPatch(path, created ? null : previous, content)
  if (patch.length > DISPLAY_PATCH_CHARS) {
    const cut = patch.lastIndexOf('\n@@ ', DISPLAY_PATCH_CHARS)
    patch = `${patch.slice(0, cut > 0 ? cut + 1 : DISPLAY_PATCH_CHARS)}`
  }
  const { added, removed } = diffStats(created ? null : previous, content)
  return { kind: 'diff', path, patch, added, removed, created }
}

function capStream(text: string): string {
  return text.length > DISPLAY_STREAM_CHARS ? truncateMiddle(text, DISPLAY_STREAM_CHARS) : text
}

// --- the tools ---------------------------------------------------------------------

/** Check a call and decide what it needs. Reads nothing that changes; may read the file an edit
 *  would change, to show its diff. */
export async function prepareCall(call: { name: string; args: Record<string, unknown> }, context: ToolContext): Promise<PreparedCall> {
  const name = canonicalToolName(call.name)
  const spec = toolSpec(name)
  if (!spec) {
    const known = TOOL_NAMES.filter((n) => context.policy !== 'read_only' || toolSpec(n)?.kind === 'read').join(', ')
    return settled(failure(call.name || 'Tool call', `there is no tool called ${JSON.stringify(call.name)}. The tools are: ${known}.`))
  }
  const coerced = coerceArgs(spec, call.args)
  const summary = summarizeCall(name, call.args)
  if (!coerced.ok) return settled(failure(summary, coerced.error))
  const args = coerced.args
  const approval = approvalFor(spec.kind, context.policy, context.allowed)
  if (approval === 'refuse') {
    return settled(
      failure(
        summary,
        `${name} is not available: the approval policy is "Read only", so files cannot be changed and commands cannot be run. Tell the user what you would change, and that they can switch the policy to let you do it.`,
      ),
    )
  }
  try {
    switch (spec.name) {
      case 'list_files':
        return prepareList(args, context, summary)
      case 'read_file':
        return prepareRead(args, context, summary)
      case 'search':
        return prepareSearch(args, context, summary)
      case 'edit_file':
        return await prepareEdit(args, context, summary, approval)
      case 'write_file':
        return await prepareWrite(args, context, summary, approval)
      case 'run_command':
        return prepareCommand(args, context, summary, approval)
    }
  } catch (error) {
    return settled(failure(summary, forModel(errorText(error))))
  }
}

function runReading(args: Record<string, unknown>, summary: string, run: (signal: AbortSignal) => Promise<ToolOutcome>): PreparedCall {
  return {
    kind: 'run',
    toolKind: 'read',
    approval: 'auto',
    summary,
    args,
    run: async (signal) => {
      try {
        return await run(signal)
      } catch (error) {
        return failure(summary, forModel(errorText(error)))
      }
    },
  }
}

function prepareList(args: Record<string, unknown>, context: ToolContext, summary: string): PreparedCall {
  const path = cleanPath(args.path ?? '.')
  const depth = Math.max(1, Math.min(6, (args.depth as number | undefined) ?? 2))
  return runReading(args, summary, async () => {
    const tree = await context.api.tree(context.root, path, depth, 400)
    const entries: WorkspaceEntry[] = tree.entries.slice(0, DISPLAY_ENTRIES)
    return {
      status: 'done',
      output: formatTree(tree, context.limits.list),
      summary: `List ${tree.path === '.' ? 'the workspace' : tree.path} · ${plural(tree.entries.length, 'entry', 'entries')}`,
      display: { kind: 'files', entries, truncated: tree.truncated || entries.length < tree.entries.length },
    }
  })
}

function prepareRead(args: Record<string, unknown>, context: ToolContext, summary: string): PreparedCall {
  const path = cleanPath(args.path)
  const start = args.start_line as number | undefined
  const end = args.end_line as number | undefined
  if (start !== undefined && start < 1) return settled(failure(summary, 'start_line is 1-based: the first line is 1.'))
  if (start !== undefined && end !== undefined && end < start) return settled(failure(summary, `end_line ${end} is before start_line ${start}.`))
  return runReading(args, summary, async () => {
    const file = await context.api.read(context.root, path, { start_line: start, end_line: end, max_bytes: context.limits.read })
    if (file.binary) {
      return failure(`Read ${file.path}`, `${file.path} is a binary file (${plural(file.size, 'byte')}); it cannot be read as text.`)
    }
    if (file.total_lines === 0 || file.end_line === 0) {
      return {
        status: 'done',
        output: `${file.path} is empty.`,
        summary: `Read ${file.path} · empty`,
        display: { kind: 'read', path: file.path, startLine: 0, endLine: 0, totalLines: 0 },
      }
    }
    const whole = file.start_line === 1 && file.end_line >= file.total_lines
    const header = whole ? `${file.path} (${plural(file.total_lines, 'line')})` : `${file.path} (lines ${file.start_line}-${file.end_line} of ${file.total_lines})`
    const more = file.total_lines - file.end_line
    const note =
      more > 0 && (file.truncated || end === undefined || end > file.end_line)
        ? `\n[${plural(more, 'more line')}. To read on, call read_file with start_line ${file.end_line + 1}.]`
        : ''
    return {
      status: 'done',
      output: `${header}\n${numberLines(file.content, file.start_line)}${note}`,
      summary: whole ? `Read ${file.path} · ${plural(file.total_lines, 'line')}` : `Read ${file.path} · ${file.start_line}–${file.end_line}`,
      display: { kind: 'read', path: file.path, startLine: file.start_line, endLine: file.end_line, totalLines: file.total_lines },
    }
  })
}

function prepareSearch(args: Record<string, unknown>, context: ToolContext, summary: string): PreparedCall {
  const pattern = String(args.pattern ?? '')
  if (!pattern) return settled(failure(summary, 'pattern is empty: give the text to search for.'))
  const path = args.path === undefined ? undefined : cleanPath(args.path)
  const glob = typeof args.glob === 'string' && args.glob.trim() ? args.glob.trim() : undefined
  return runReading(args, summary, async () => {
    const result = await context.api.search(context.root, pattern, {
      ...(path && path !== '.' ? { path } : {}),
      ...(glob ? { glob } : {}),
      regex: args.regex === true,
      max_results: 200,
    })
    const matches: WorkspaceMatch[] = result.matches.slice(0, DISPLAY_MATCHES)
    return {
      status: 'done',
      output: formatMatches(pattern, result, context.limits.search, path && path !== '.' ? path : null),
      summary: `${summary} · ${plural(result.matches.length, 'match', 'matches')}${result.truncated ? '+' : ''}`,
      display: { kind: 'matches', pattern, matches, truncated: result.truncated || matches.length < result.matches.length },
    }
  })
}

async function prepareEdit(args: Record<string, unknown>, context: ToolContext, summary: string, approval: 'auto' | 'ask'): Promise<PreparedCall> {
  const path = cleanPath(args.path)
  if (path === '.') return settled(failure(summary, 'path is empty: name the file to edit.'))
  const oldString = String(args.old_string ?? '')
  const newString = String(args.new_string ?? '')
  const replaceAll = args.replace_all === true
  const file = await readWhole(context.api, context.root, path)
  if (!file.exists) return settled(failure(summary, `${path} does not exist. To create it, use write_file.`))
  if (file.binary) return settled(failure(summary, `${path} is a binary file; edit_file only changes text.`))
  let plan: ReturnType<typeof planEdit> | null = null
  let preview: ToolDisplay | undefined
  if (file.complete) {
    plan = planEdit(file.content, oldString, newString, replaceAll, path)
    // Answered now, not after the person approves something that cannot apply.
    if (!plan.ok) return settled(failure(summary, plan.error))
    preview = diffDisplay(path, file.content, plan.content, false)
  } else {
    preview = { kind: 'text', text: `${path} is too large to preview; the edit replaces ${replaceAll ? 'every occurrence' : 'one occurrence'} of old_string.` }
  }
  const sendOld = plan?.ok ? plan.oldString : oldString
  const sendNew = plan?.ok ? plan.newString : newString
  const stripped = plan?.ok === true && plan.strippedLineNumbers
  return {
    kind: 'run',
    toolKind: 'edit',
    approval,
    summary,
    args,
    preview,
    run: async () => {
      try {
        const result = await context.api.edit(context.root, path, sendOld, sendNew, replaceAll)
        const display = diffDisplay(result.path, result.previous, result.content, false)
        const counts = display.kind === 'diff' ? `+${display.added} -${display.removed}` : ''
        const region = editedRegion(result.previous, result.content)
        return {
          status: 'done',
          output:
            `Edited ${result.path}: ${plural(result.replacements, 'replacement')} (${counts}).` +
            (stripped ? ' (The line-number prefixes copied from read_file were taken off old_string and new_string.)' : '') +
            (region ? `\nThe changed part now reads:\n${region}` : ''),
          summary: `Edit ${result.path}`,
          display,
          change: { path: result.path, previous: result.previous, content: result.content, created: false },
        }
      } catch (error) {
        return failure(summary, forModel(errorText(error)), preview)
      }
    },
  }
}

async function prepareWrite(args: Record<string, unknown>, context: ToolContext, summary: string, approval: 'auto' | 'ask'): Promise<PreparedCall> {
  const path = cleanPath(args.path)
  if (path === '.') return settled(failure(summary, 'path is empty: name the file to write.'))
  const content = String(args.content ?? '')
  let preview: ToolDisplay | undefined
  // Only an edit the person must approve is read first: an automatic one learns what was there
  // from the write itself, which returns it.
  if (approval === 'ask') {
    const file = await readWhole(context.api, context.root, path)
    if (file.exists && file.binary) return settled(failure(summary, `${path} is a binary file; it was not overwritten.`))
    if (file.exists && file.complete && file.content === content) {
      return settled({ status: 'done', output: `${path} already has exactly this content; nothing was written.`, summary: `Write ${path} · unchanged` })
    }
    preview = file.exists
      ? file.complete
        ? diffDisplay(path, file.content, content, false)
        : { kind: 'text', text: `${path} is too large to preview; write_file replaces all of it (${plural(lineCount(content), 'line')}).` }
      : diffDisplay(path, null, content, true)
  }
  return {
    kind: 'run',
    toolKind: 'edit',
    approval,
    summary: preview && preview.kind === 'diff' && preview.created ? `Create ${path}` : summary,
    args,
    ...(preview ? { preview } : {}),
    run: async () => {
      try {
        const result = await context.api.write(context.root, path, content)
        const lines = plural(lineCount(result.content), 'line')
        // The runtime returns no `previous` for a file over 2 MiB: listed, but there is no diff.
        const known = result.created || result.previous !== null
        const display: ToolDisplay = known
          ? diffDisplay(result.path, result.previous, result.content, result.created)
          : { kind: 'text', text: `Wrote ${result.path} (${lines}). The file was too large to keep a diff of.` }
        const counts = display.kind === 'diff' ? `, +${display.added} -${display.removed}` : ''
        return {
          status: 'done',
          output: result.created ? `Created ${result.path} (${lines}).` : `Wrote ${result.path} (${lines}${counts}).`,
          summary: `${result.created ? 'Create' : 'Write'} ${result.path}`,
          display,
          change: { path: result.path, previous: result.previous, content: result.content, created: result.created },
        }
      } catch (error) {
        return failure(summary, forModel(errorText(error)), preview)
      }
    },
  }
}

function prepareCommand(args: Record<string, unknown>, context: ToolContext, summary: string, approval: 'auto' | 'ask'): PreparedCall {
  const command = String(args.command ?? '').trim()
  if (!command) return settled(failure(summary, 'command is empty.'))
  const timeout = args.timeout_secs === undefined ? undefined : Math.max(1, Math.min(900, args.timeout_secs as number))
  const preview: ToolDisplay = { kind: 'command', command, exitCode: null, stdout: '', stderr: '', timedOut: false, durationMs: 0 }
  return {
    kind: 'run',
    toolKind: 'command',
    approval,
    summary,
    args,
    preview,
    run: async (signal) => {
      try {
        const result = await context.api.exec(context.root, command, timeout, signal)
        return {
          status: 'done',
          output: formatExec(result, context.limits.exec),
          summary,
          display: {
            kind: 'command',
            command,
            exitCode: result.exit_code,
            stdout: capStream(result.stdout),
            stderr: capStream(result.stderr),
            timedOut: result.timed_out,
            durationMs: result.duration_ms,
          },
        }
      } catch (error) {
        if (isAbort(error)) {
          // Aborting the request is what stops the command: the runtime kills the command's whole
          // process group when its client goes away, as it does on a timeout.
          return failure(summary, 'Stopped by the user before the command finished; it was cancelled.', preview)
        }
        return failure(summary, forModel(errorText(error)), preview)
      }
    },
  }
}

// --- the conversation's list of changed files ----------------------------------------------

/**
 * The Changes list after a call changed a file. The first original per path is kept — it is what
 * Revert restores — and a file that ends up exactly as it started drops off the list. A file whose
 * original or current content is too large to keep (or that the runtime did not return) is listed
 * as untracked: shown, never revertible, because a revert that cannot check what it overwrites
 * could throw away someone's work.
 */
export function recordChange(
  changes: FileChange[] | undefined,
  change: { path: string; previous: string | null; content: string; created: boolean },
  now: number,
): FileChange[] {
  const list = changes ?? []
  const big = (text: string | null) => text !== null && (text.length > TRACK_LIMIT_BYTES || utf8Bytes(text) > TRACK_LIMIT_BYTES)
  const untracked: FileChange = { path: change.path, original: null, current: null, untracked: true, updatedAt: now }
  const existing = list.find((c) => c.path === change.path)
  if (existing) {
    if (existing.untracked || big(change.content)) return list.map((c) => (c.path === change.path ? untracked : c))
    if (existing.original !== null && change.content === existing.original) return list.filter((c) => c.path !== change.path)
    return list.map((c) => (c.path === change.path ? { ...c, current: change.content, updatedAt: now } : c))
  }
  if (!change.created && change.previous === change.content) return list
  if ((!change.created && change.previous === null) || big(change.previous) || big(change.content)) return [...list, untracked]
  return [...list, { path: change.path, original: change.created ? null : change.previous, current: change.content, updatedAt: now }]
}
