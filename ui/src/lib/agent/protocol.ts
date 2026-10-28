// The coding agent's protocol: the tools it is offered, the system prompt that offers them, how a
// reply is read back into text and tool calls, and how tool results go back to the model.
//
// The call format is the one Qwen3-Coder and Qwen3.5 are trained on — the system prompt below
// mirrors their chat template word for word — XML-ish tags, one parameter per tag, every value
// verbatim:
//
//     <tool_call>
//     <function=edit_file>
//     <parameter=path>
//     src/main.rs
//     </parameter>
//     …
//     </function>
//     </tool_call>
//
// JSON would make a small model escape every quote and newline of the code it writes, and that is
// exactly where small models go wrong; a value inside <parameter=…> needs no escaping at all. The
// Hermes JSON form (`<tool_call>{"name": …, "arguments": …}</tool_call>`) that other models emit is
// read as well, leniently, and so is a <function=…> block whose <tool_call> wrapper was dropped.
//
// Tools are offered as text in the system prompt, not through the API's `tools` field: the runtime
// serves engines whose templates differ, and a prompt it can pass through unchanged behaves the
// same on all of them.
//
// Plain TypeScript with type-only imports, so `node --test` can load it.

import type { ApprovalPolicy } from '../types.ts'

// --- tools ------------------------------------------------------------------------

export type ToolName = 'list_files' | 'read_file' | 'search' | 'edit_file' | 'write_file' | 'run_command'

/** What a tool does, which is what the approval policy decides on. */
export type ToolKind = 'read' | 'edit' | 'command'

export type ParamSpec = { name: string; type: 'string' | 'integer' | 'boolean'; description: string; required?: boolean }

export type ToolSpec = { name: ToolName; kind: ToolKind; description: string; params: ParamSpec[] }

const PATH = (what: string): ParamSpec => ({ name: 'path', type: 'string', description: `${what}, relative to the workspace root.`, required: true })

export const TOOLS: readonly ToolSpec[] = [
  {
    name: 'list_files',
    kind: 'read',
    description: 'List the files and folders under a directory as an indented tree. Ignored files (.gitignore, node_modules, build output) are left out.',
    params: [
      { name: 'path', type: 'string', description: 'Directory to list, relative to the workspace root. Default: the root.' },
      { name: 'depth', type: 'integer', description: 'How many levels deep to list, 1-6. Default: 2.' },
    ],
  },
  {
    name: 'read_file',
    kind: 'read',
    description: 'Read a text file. Lines come back numbered ("  12\\t..."); the numbers are not part of the file. A long file comes back in parts: read on with start_line.',
    params: [
      PATH('The file'),
      { name: 'start_line', type: 'integer', description: 'First line to read, 1-based. Default: 1.' },
      { name: 'end_line', type: 'integer', description: 'Last line to read, inclusive. Default: as many as fit.' },
    ],
  },
  {
    name: 'search',
    kind: 'read',
    description: 'Search the contents of the files, like ripgrep. Returns each matching line as "path:line: text".',
    params: [
      { name: 'pattern', type: 'string', description: 'The text to find, case-sensitive (a regular expression when regex is true).', required: true },
      { name: 'path', type: 'string', description: 'File or directory to search in. Default: the whole workspace.' },
      { name: 'glob', type: 'string', description: 'Only search files whose name matches, e.g. "*.ts" or "*.{rs,toml}".' },
      { name: 'regex', type: 'boolean', description: 'Treat pattern as a regular expression. Default: false.' },
    ],
  },
  {
    name: 'edit_file',
    kind: 'edit',
    description:
      'Replace one exact piece of a file with new text. old_string must match the file exactly (copy it from read_file, without the line numbers) and occur once: include enough surrounding lines to make it unique.',
    params: [
      PATH('The file'),
      { name: 'old_string', type: 'string', description: 'The exact text to replace.', required: true },
      { name: 'new_string', type: 'string', description: 'The text to put in its place.', required: true },
      { name: 'replace_all', type: 'boolean', description: 'Replace every occurrence instead of exactly one. Default: false.' },
    ],
  },
  {
    name: 'write_file',
    kind: 'edit',
    description: 'Create a file, or replace all of its content. Missing folders are created. To change part of an existing file, use edit_file.',
    params: [PATH('The file'), { name: 'content', type: 'string', description: 'The complete content of the file.', required: true }],
  },
  {
    name: 'run_command',
    kind: 'command',
    description:
      'Run a shell command in the workspace root and return its exit code and output: tests, builds, linters, git. Nothing can be typed into it, and it is stopped at the timeout.',
    params: [
      { name: 'command', type: 'string', description: 'The command line, e.g. "npm test" or "cargo check".', required: true },
      { name: 'timeout_secs', type: 'integer', description: 'Seconds before it is stopped, 1-900. Default: 120.' },
    ],
  },
]

export const TOOL_NAMES: readonly string[] = TOOLS.map((t) => t.name)

export function toolSpec(name: string): ToolSpec | undefined {
  return TOOLS.find((t) => t.name === name)
}

/** The tools a policy offers. Read-only offers none that change anything — not even by name. */
export function toolsFor(policy: ApprovalPolicy): ToolSpec[] {
  return TOOLS.filter((t) => policy !== 'read_only' || t.kind === 'read')
}

/**
 * JSON the way the template's `tojson` writes it — Python's `json.dumps`, with `", "` and `": "`
 * between items — so the tool list reads exactly like the ones the model was trained on.
 */
function pyJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(pyJson).join(', ')}]`
  if (value !== null && typeof value === 'object') {
    return `{${Object.entries(value as Record<string, unknown>)
      .map(([key, item]) => `${JSON.stringify(key)}: ${pyJson(item)}`)
      .join(', ')}}`
  }
  return JSON.stringify(value)
}

function toolSchema(tool: ToolSpec) {
  return {
    type: 'function',
    function: {
      name: tool.name,
      description: tool.description,
      parameters: {
        type: 'object',
        properties: Object.fromEntries(tool.params.map((p) => [p.name, { type: p.type, description: p.description }])),
        required: tool.params.filter((p) => p.required).map((p) => p.name),
      },
    },
  }
}

// --- the system prompt --------------------------------------------------------------

export type AgentPromptContext = {
  /** The workspace's absolute path. */
  root: string
  name?: string | null
  branch?: string | null
  isGit?: boolean
  /** The machine's OS as the runtime names it (`macos`, `linux`, `windows`). */
  os?: string | null
  /** Today, as YYYY-MM-DD. */
  date: string
  policy: ApprovalPolicy
  /** The user's own system prompt from Settings, kept as standing instructions. */
  userInstructions?: string | null
}

function osName(os: string | null | undefined): string | null {
  if (!os) return null
  const key = os.toLowerCase()
  if (key.includes('mac') || key === 'darwin') return 'macOS'
  if (key.includes('win')) return 'Windows'
  if (key.includes('linux')) return 'Linux'
  return os
}

const POLICY_TEXT: Record<ApprovalPolicy, string> = {
  read_only:
    'Approval policy: read only. You can list, read and search, but you cannot change files or run commands. When the task needs changes, say exactly what to change and where, and tell the user they can switch the approval policy to let you make them.',
  ask: 'Approval policy: ask. Each edit and each command waits for the user to approve it. If the user declines one, do not repeat it: change your approach, or ask.',
  auto_edit:
    'Approval policy: auto edit. Your edits apply at once (the user reviews them and can revert them); each command waits for the user to approve it. If the user declines one, do not repeat it.',
  full_auto:
    'Approval policy: full auto. Edits and commands run without asking, so never run anything destructive (deleting files, resetting git, installing globally) unless the user asked for it.',
}

const RULES = [
  'Make one tool call per message, then wait for its result.',
  'Look before you change anything: find the code with search or list_files, then read_file the parts you need.',
  'Read a file before you edit it. Never guess at file contents, names or APIs.',
  'Edit with edit_file: copy old_string exactly from your latest read_file of that file (without the line-number prefix), and keep each edit small. Use write_file only for a new file or to rewrite a short one.',
  'After changing code, run the tests, the type checker or the build when the project has them, and fix what they report.',
  'When a tool returns an error, read it and correct the call; never repeat a call that failed in the same way.',
  'If you are blocked, or the request is unclear, ask the user instead of guessing.',
  'When the task is done, reply without a tool call: a short summary of what you changed and how you checked it.',
]

const READ_ONLY_RULES = [
  'Make one tool call per message, then wait for its result.',
  'Find the code with search or list_files, then read_file the parts you need. Never guess at file contents.',
  'If the request is unclear, ask the user instead of guessing.',
  'When you have the answer, reply without a tool call.',
]

/**
 * The agent's system prompt: the tool block exactly as Qwen3.5's template renders `tools`, then who
 * the agent is, where it works and how. Compact on purpose — the window may be 8–16K tokens, and
 * every token here is one fewer for the code.
 */
export function buildSystemPrompt(context: AgentPromptContext): string {
  const tools = toolsFor(context.policy)
  const toolBlock =
    '# Tools\n\nYou have access to the following functions:\n\n<tools>\n' +
    tools.map((tool) => pyJson(toolSchema(tool))).join('\n') +
    '\n</tools>\n\n' +
    'If you choose to call a function ONLY reply in the following format with NO suffix:\n\n' +
    '<tool_call>\n<function=example_function_name>\n<parameter=example_parameter_1>\nvalue_1\n</parameter>\n' +
    '<parameter=example_parameter_2>\nThis is the value for the second parameter\nthat can span\nmultiple lines\n</parameter>\n</function>\n</tool_call>\n\n' +
    '<IMPORTANT>\nReminder:\n' +
    '- Function calls MUST follow the specified format: an inner <function=...></function> block must be nested within <tool_call></tool_call> XML tags\n' +
    '- Required parameters MUST be specified\n' +
    '- You may provide optional reasoning for your function call in natural language BEFORE the function call, but NOT after\n' +
    '- If there is no function call available, answer the question like normal with your current knowledge and do not tell the user about function calls\n' +
    '</IMPORTANT>'

  const name = context.name || context.root.split(/[\\/]/).filter(Boolean).pop() || context.root
  const where = [
    `You are working in the folder \`${name}\` (${context.root})${context.branch ? `, on git branch \`${context.branch}\`` : context.isGit ? ', a git repository' : ''}.`,
    'Every path you give a tool is relative to this folder, and nothing outside it can be reached.',
    [osName(context.os) ? `The OS is ${osName(context.os)}.` : '', `Today is ${context.date}.`].filter(Boolean).join(' '),
  ].join(' ')
  const rules = (context.policy === 'read_only' ? READ_ONLY_RULES : RULES).map((rule) => `- ${rule}`).join('\n')

  const parts = [
    toolBlock,
    'You are MISAKA Agent, a coding agent running locally in MISAKA Studio, on the user\'s own machine. You help with the code in one folder by using the functions above. Reply in the language the user writes in.',
    `# Workspace\n${where}\n${POLICY_TEXT[context.policy]}`,
    `# Rules\n${rules}`,
  ]
  const own = context.userInstructions?.trim()
  if (own) parts.push(`# The user's standing instructions\n${own}`)
  return parts.join('\n\n')
}

// --- reading a reply ---------------------------------------------------------------

export type TextSegment = { kind: 'text'; text: string }

export type ToolSegment = {
  kind: 'tool'
  /** The tool's name as written; '' while it is still arriving. */
  name: string
  args: Record<string, unknown>
  /** The whole call is there, so it can be run. False while it streams, or when it was cut off. */
  complete: boolean
  /** The call as the model wrote it, tags included. */
  raw: string
  /** Set when a call that is all there cannot be read; the loop tells the model why. */
  error?: string
}

export type Segment = TextSegment | ToolSegment

const CALL_OPEN = '<tool_call>'
const CALL_CLOSE = '</tool_call>'
const FN_OPEN = '<function='
const FN_CLOSE = '</function>'
const PARAM_OPEN = '<parameter='
const PARAM_CLOSE = '</parameter>'

/** The format reminder sent back with a call that could not be read. */
export const CALL_FORMAT_HINT =
  'Write a tool call exactly like this, with nothing after it:\n<tool_call>\n<function=read_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n</function>\n</tool_call>'

/**
 * A reply without its reasoning. A reply whose `<think>` was opened by the template starts in the
 * middle of a thought and closes it; a block that never closes is a reply still thinking. Tool
 * calls drafted inside reasoning are not calls.
 */
export function removeThinking(text: string): string {
  let out = text
  const close = out.indexOf('</think>')
  const open = out.indexOf('<think>')
  if (close !== -1 && (open === -1 || close < open)) out = out.slice(close + '</think>'.length)
  out = out.replace(/<think>[\s\S]*?<\/think>/g, '')
  const unclosed = out.indexOf('<think>')
  return unclosed === -1 ? out : out.slice(0, unclosed)
}

/** How much of `token` the end of `text` could be the beginning of (a tag still arriving). */
function partialSuffix(text: string, token: string): number {
  for (let length = Math.min(token.length - 1, text.length); length > 0; length--) {
    if (text.endsWith(token.slice(0, length))) return length
  }
  return 0
}

function atLineStart(text: string, index: number): boolean {
  let i = index - 1
  while (i >= 0 && (text[i] === ' ' || text[i] === '\t')) i--
  return i < 0 || text[i] === '\n'
}

function findOpener(text: string, from: number): { index: number; bare: boolean } | null {
  const call = text.indexOf(CALL_OPEN, from)
  let fn = text.indexOf(FN_OPEN, from)
  while (fn !== -1 && !atLineStart(text, fn)) fn = text.indexOf(FN_OPEN, fn + 1)
  if (call === -1 && fn === -1) return null
  if (fn !== -1 && (call === -1 || fn < call)) return { index: fn, bare: true }
  return { index: call, bare: false }
}

/**
 * Where a parameter's value ends: the first `</parameter>` followed by the next tag (or the end) —
 * so a value that itself mentions `</parameter>`, as this file does, is not cut short there.
 */
function parameterClose(body: string, from: number): number {
  let first = -1
  for (let at = body.indexOf(PARAM_CLOSE, from); at !== -1; at = body.indexOf(PARAM_CLOSE, at + 1)) {
    if (first === -1) first = at
    const rest = body.slice(at + PARAM_CLOSE.length).trimStart()
    if (rest === '' || rest.startsWith(PARAM_OPEN) || rest.startsWith(FN_CLOSE) || rest.startsWith(CALL_CLOSE)) return at
  }
  return first
}

/** A value as written between its tags: the template puts it on its own lines, so one newline at
 *  each end is the format's, not the value's. */
function cleanValue(value: string, trailing: boolean): string {
  let out = value.replace(/^\r?\n/, '')
  if (trailing) out = out.replace(/\r?\n$/, '')
  return out
}

type XmlCall = { name: string; nameDone: boolean; args: Record<string, string>; closed: boolean; end: number; rawArgs: string }

/** Read `<function=name>` and its parameters from `body`, starting at `start`. `closedBody`: the
 *  body's end is the call's end (its `</tool_call>` arrived), so a value running to it is whole. */
function readXml(body: string, start: number, closedBody = false): XmlCall {
  let i = start + FN_OPEN.length
  const gt = body.indexOf('>', i)
  const newline = body.indexOf('\n', i)
  let name: string
  if (gt !== -1 && (newline === -1 || gt < newline)) {
    name = body.slice(i, gt).trim()
    i = gt + 1
  } else if (newline !== -1) {
    // `<function=read_file` with its `>` forgotten: the name is the rest of the line.
    name = body.slice(i, newline).trim()
    i = newline + 1
  } else {
    return { name: '', nameDone: false, args: {}, closed: false, end: body.length, rawArgs: '' }
  }
  const argsStart = i
  const args: Record<string, string> = {}
  for (;;) {
    while (i < body.length && /\s/.test(body[i]!)) i++
    if (body.startsWith(FN_CLOSE, i)) return { name, nameDone: true, args, closed: true, end: i + FN_CLOSE.length, rawArgs: body.slice(argsStart, i) }
    if (i >= body.length) break
    if (body.startsWith(PARAM_OPEN, i)) {
      const keyStart = i + PARAM_OPEN.length
      const keyEnd = body.indexOf('>', keyStart)
      if (keyEnd === -1) break // the parameter's name is still arriving
      const key = body.slice(keyStart, keyEnd).trim()
      const valueStart = keyEnd + 1
      const close = parameterClose(body, valueStart)
      if (close !== -1) {
        if (key) args[key] = cleanValue(body.slice(valueStart, close), true)
        i = close + PARAM_CLOSE.length
        continue
      }
      // No `</parameter>`: the value runs to the next tag, or to the end of what has arrived.
      const next = [body.indexOf(PARAM_OPEN, valueStart), body.indexOf(FN_CLOSE, valueStart)].filter((n) => n !== -1)
      const stop = next.length ? Math.min(...next) : body.length
      if (key) args[key] = cleanValue(body.slice(valueStart, stop), stop < body.length || closedBody)
      i = stop
      continue
    }
    // Anything else between parameters is noise: skip to the next tag.
    const next = [body.indexOf(PARAM_OPEN, i), body.indexOf(FN_CLOSE, i)].filter((n) => n !== -1)
    if (!next.length) break
    i = Math.min(...next)
  }
  return { name, nameDone: true, args, closed: false, end: body.length, rawArgs: body.slice(argsStart) }
}

/**
 * JSON as small models write it, made parseable: raw newlines and tabs inside strings escaped,
 * backslashes that start no JSON escape doubled (a regex's `\d`), trailing commas dropped.
 */
export function repairJson(text: string): string {
  let out = ''
  let inString = false
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!
    if (inString) {
      if (c === '\\') {
        const next = text[i + 1]
        if (next !== undefined && '"\\/bfnrtu'.includes(next)) {
          out += c + next
          i++
        } else {
          out += '\\\\'
        }
      } else if (c === '"') {
        inString = false
        out += c
      } else if (c === '\n') out += '\\n'
      else if (c === '\r') out += '\\r'
      else if (c === '\t') out += '\\t'
      else if (c < ' ') out += `\\u${c.charCodeAt(0).toString(16).padStart(4, '0')}`
      else out += c
    } else {
      if (c === '"') inString = true
      out += c
    }
  }
  return out.replace(/,\s*([}\]])/g, '$1')
}

function parseJsonLoose(text: string): unknown {
  try {
    return JSON.parse(text)
  } catch {
    try {
      return JSON.parse(repairJson(text))
    } catch {
      return undefined
    }
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
}

/** A Hermes-style call: `{"name": …, "arguments": {…}}`, arguments possibly a JSON string. */
function readJsonCall(body: string): { ok: boolean; name: string; args: Record<string, unknown> } {
  const text = body
    .trim()
    .replace(/^```(?:json)?\s*/, '')
    .replace(/\s*```$/, '')
  const guessed = /"name"\s*:\s*"([^"]*)"/.exec(text)?.[1] ?? ''
  const value = parseJsonLoose(text)
  if (!isRecord(value)) return { ok: false, name: guessed, args: {} }
  const fn = isRecord(value.function) ? value.function : null
  const name = String(value.name ?? fn?.name ?? value.tool ?? '')
  let args: unknown = value.arguments ?? value.parameters ?? value.args ?? fn?.arguments ?? fn?.parameters ?? {}
  if (typeof args === 'string') args = parseJsonLoose(args) ?? {}
  return { ok: true, name, args: isRecord(args) ? args : {} }
}

function readCall(body: string, raw: string, closed: boolean): ToolSegment {
  // An engine that leaks the start of the stop string leaves `</tool_c` at the end.
  const inner = closed ? body : body.slice(0, body.length - partialSuffix(body, CALL_CLOSE))
  const fnAt = inner.indexOf(FN_OPEN)
  if (fnAt !== -1) {
    const xml = readXml(inner, fnAt, closed)
    let args: Record<string, unknown> = xml.args
    // `<function=read_file>{"path": "a.rs"}</function>`: JSON arguments inside the XML form.
    if (Object.keys(args).length === 0 && xml.rawArgs.trim().startsWith('{')) {
      const json = parseJsonLoose(xml.rawArgs.trim())
      if (isRecord(json)) args = json
    }
    // A closed call with no readable name is still a call the model made: it is answered with
    // the format, rather than dropped as if nothing had been asked.
    if (closed && xml.name === '') return { kind: 'tool', name: '', args, complete: true, raw, error: `The tool call has no function name. ${CALL_FORMAT_HINT}` }
    return { kind: 'tool', name: xml.name, args, complete: xml.nameDone && xml.name !== '' && (closed || xml.closed), raw }
  }
  if (inner.trim().startsWith('{') || inner.trim().startsWith('```')) {
    const json = readJsonCall(inner)
    if (json.ok) {
      return {
        kind: 'tool',
        name: json.name,
        args: json.args,
        complete: closed,
        raw,
        ...(closed && !json.name ? { error: `The tool call has no "name". ${CALL_FORMAT_HINT}` } : {}),
      }
    }
    if (closed) return { kind: 'tool', name: json.name, args: {}, complete: true, raw, error: `The tool call's JSON could not be read. ${CALL_FORMAT_HINT}` }
    return { kind: 'tool', name: json.name, args: {}, complete: false, raw }
  }
  if (closed) return { kind: 'tool', name: '', args: {}, complete: true, raw, error: `The tool call could not be read. ${CALL_FORMAT_HINT}` }
  return { kind: 'tool', name: '', args: {}, complete: false, raw }
}

function pushText(segments: Segment[], text: string) {
  if (!text) return
  const last = segments[segments.length - 1]
  if (last?.kind === 'text') last.text += text
  else segments.push({ kind: 'text', text })
}

/** Text as it should be shown: no tool output the model imagined for itself, no stray closing
 *  tags, no code fence that only wrapped a call. */
function tidy(segments: Segment[]): Segment[] {
  const out: Segment[] = []
  segments.forEach((segment, index) => {
    if (segment.kind === 'tool') {
      out.push(segment)
      return
    }
    let text = segment.text
      .replace(/<tool_response>[\s\S]*?(<\/tool_response>|$)/g, '')
      .replace(/<\/tool_call>|<\/function>/g, '')
    if (segments[index + 1]?.kind === 'tool') text = text.replace(/```[\w-]*\s*$/, '')
    if (segments[index - 1]?.kind === 'tool') text = text.replace(/^\s*```(?!\w)/, '')
    text = text.trim()
    if (text) out.push({ kind: 'text', text })
  })
  return out
}

/**
 * A reply as text and tool calls, in the order written.
 *
 * Streaming-safe: with `final: false`, a tag still arriving at the end (`<tool_c`) is held back
 * rather than shown, and a call still arriving is an incomplete tool segment — never raw XML. With
 * `final: true` an unclosed call is complete only if its `</function>` arrived; one cut off before
 * that stays incomplete, and must not be run (a `write_file` cut mid-content would truncate a file).
 */
export function parseAgentText(input: string, options: { final: boolean }): Segment[] {
  const text = removeThinking(input)
  const segments: Segment[] = []
  let pos = 0
  while (pos <= text.length) {
    const opener = findOpener(text, pos)
    if (!opener) {
      let rest = text.slice(pos)
      const fnPartial = partialSuffix(rest, FN_OPEN)
      const held = Math.max(partialSuffix(rest, CALL_OPEN), fnPartial > 0 && atLineStart(rest, rest.length - fnPartial) ? fnPartial : 0)
      // While streaming even a lone `<` may be a call beginning; in a finished reply only a
      // recognisable start of one is dropped.
      if (held > 0 && (!options.final || held >= 2)) rest = rest.slice(0, rest.length - held)
      pushText(segments, rest)
      break
    }
    pushText(segments, text.slice(pos, opener.index))
    if (!opener.bare) {
      const bodyStart = opener.index + CALL_OPEN.length
      const close = text.indexOf(CALL_CLOSE, bodyStart)
      if (close === -1) {
        segments.push(readCall(text.slice(bodyStart), text.slice(opener.index), false))
        break
      }
      segments.push(readCall(text.slice(bodyStart, close), text.slice(opener.index, close + CALL_CLOSE.length), true))
      pos = close + CALL_CLOSE.length
      continue
    }
    // A <function=…> block without its <tool_call> wrapper.
    const xml = readXml(text, opener.index)
    if (!xml.closed) {
      segments.push(readCall(text.slice(opener.index), text.slice(opener.index), false))
      break
    }
    let end = xml.end
    const stray = /^\s*<\/tool_call>/.exec(text.slice(end))
    if (stray) end += stray[0].length
    segments.push(readCall(text.slice(opener.index, xml.end), text.slice(opener.index, end), true))
    pos = end
  }
  return tidy(segments)
}

/** Whether a finished reply's last `<tool_call>` was left open (the stop sequence cut it). */
export function hasOpenToolCall(content: string): boolean {
  const open = content.lastIndexOf(CALL_OPEN)
  return open !== -1 && content.indexOf(CALL_CLOSE, open) === -1
}

/**
 * The reply with its last `<tool_call>` closed. The loop stops generation at `</tool_call>` and
 * the engine leaves the stop string out, so a step's call arrives open; closing it puts back what
 * the model actually said.
 */
export function closeOpenToolCall(content: string): string {
  if (!hasOpenToolCall(content)) return content
  const trimmed = content.slice(0, content.length - (partialSuffix(content, CALL_CLOSE) >= 2 ? partialSuffix(content, CALL_CLOSE) : 0))
  return `${trimmed}${trimmed.endsWith('\n') ? '' : '\n'}${CALL_CLOSE}`
}

/**
 * The reply cut after its first tool call. One call per step is the protocol (the stop sequence
 * enforces it on engines that honour it); anything after the first call — a second call, or a
 * `<tool_response>` the model imagined — is not something it may act on, and would teach it, as
 * history, to keep doing it.
 */
export function keepFirstCall(content: string): string {
  const first = parseAgentText(content, { final: true }).find((s): s is ToolSegment => s.kind === 'tool')
  if (!first) return content
  // The call was found in the reply with its reasoning taken out; the cut is made in the reply as
  // written, after the reasoning, so a <think> block before the call survives as it was.
  const thoughtEnd = content.lastIndexOf('</think>')
  const at = content.indexOf(first.raw, thoughtEnd === -1 ? 0 : thoughtEnd)
  if (at === -1) return content
  const end = at + first.raw.length
  return content.slice(end).trim() ? content.slice(0, end) : content
}

// --- sending results back ----------------------------------------------------------

/**
 * One step's tool results as the user message that carries them: a `<tool_response>` block per
 * call, in order, and nothing before the first tag or after the last. That exact shape is how the
 * Qwen template tells a tool result from a question the user asked.
 */
export function toolResultsMessage(outputs: string[]): string {
  return outputs.map((output) => `<tool_response>\n${output}\n</tool_response>`).join('\n')
}

/**
 * `text` cut to `max` characters, keeping its head and its tail — the first error of a build and
 * the summary of a test run are at opposite ends — with a note saying what was left out and, when
 * given, how to get it.
 */
export function truncateMiddle(text: string, max: number, hint?: string): string {
  if (text.length <= max) return text
  const headLength = Math.floor(max * 0.6)
  const tailLength = max - headLength
  let head = text.slice(0, headLength)
  const headBreak = head.lastIndexOf('\n')
  if (headBreak > headLength / 2) head = head.slice(0, headBreak)
  let tail = text.slice(text.length - tailLength)
  const tailBreak = tail.indexOf('\n')
  if (tailBreak !== -1 && tailBreak < tailLength / 2) tail = tail.slice(tailBreak + 1)
  const omitted = text.slice(head.length, text.length - tail.length)
  const omittedLines = omitted.split('\n').length - 1
  const what = omittedLines > 0 ? `${omittedLines} lines` : `${omitted.length} characters`
  return `${head}\n... [${what} omitted${hint ? `: ${hint}` : ''}] ...\n${tail}`
}

/** A call's identity for the repetition guard: its name and its arguments, keys in order. */
export function callKey(name: string, args: Record<string, unknown>): string {
  const sorted = Object.keys(args)
    .sort()
    .map((key) => [key, args[key]])
  return `${name}:${JSON.stringify(sorted)}`
}
