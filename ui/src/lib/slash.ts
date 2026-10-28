// The composer's inline triggers: `/` commands at the start of a message, `@` file mentions.
//
// Only a known command is a command. `/usr/bin is a directory` names no command, so it is sent as
// the message it is; a menu that swallowed every leading slash would make that sentence unsendable.
// Commands are one line — a multi-line message that happens to start with `/` is a message.
//
// Plain TypeScript with no imports, so the unit tests run it directly under Node.

export type SlashName = 'new' | 'chat' | 'code' | 'agent' | 'model' | 'clear' | 'rename' | 'export' | 'mine' | 'think' | 'params' | 'help'

export type SlashCommand = {
  name: SlashName
  /** What follows the name, as the menu shows it. */
  args?: string
  description: string
  /** Runs only with an argument: choosing it from the menu waits for one. */
  needsArg?: boolean
  /** Fixed choices for the argument. Optional ones: the command runs without, as a toggle. */
  choices?: string[]
}

export const SLASH_COMMANDS: SlashCommand[] = [
  { name: 'new', description: 'New chat or session on this side' },
  { name: 'chat', description: 'Switch to Chat' },
  { name: 'code', description: 'Switch to Code' },
  // Kept from before the two sides existed, when the agent was a mode of a chat thread.
  { name: 'agent', description: 'Switch to Code (same as /code)' },
  { name: 'model', args: '<name>', description: 'Load an installed model', needsArg: true },
  { name: 'clear', description: 'Clear this thread — its pinned notes stay' },
  { name: 'rename', args: '<title>', description: 'Rename this thread', needsArg: true },
  { name: 'export', description: 'Download this thread as Markdown' },
  { name: 'mine', args: 'on|off', description: "Mine this thread's prompts", choices: ['on', 'off'] },
  { name: 'think', args: 'on|off', description: 'Let reasoning models think before answering', choices: ['on', 'off'] },
  { name: 'params', description: 'Show the sampling parameters' },
  { name: 'help', description: 'Keyboard shortcuts' },
]

/**
 * The slash token being typed, or null when the text is not one.
 *
 * `/mo` → `{ name: 'mo', arg: null }` (still naming the command); `/model qw` → `{ name: 'model',
 * arg: 'qw' }`; `/model ` → `{ name: 'model', arg: '' }` (the argument has begun, empty).
 */
export function slashInput(text: string): { name: string; arg: string | null } | null {
  if (!text.startsWith('/') || text.includes('\n')) return null
  const body = text.slice(1)
  const space = body.search(/\s/)
  if (space === -1) return { name: body, arg: null }
  return { name: body.slice(0, space), arg: body.slice(space + 1) }
}

export function findCommand(name: string): SlashCommand | null {
  const lower = name.toLowerCase()
  return SLASH_COMMANDS.find((c) => c.name === lower) ?? null
}

/** Commands for the menu while the name is typed: prefix matches first, then any other match. */
export function matchCommands(query: string): SlashCommand[] {
  const q = query.toLowerCase()
  if (!q) return SLASH_COMMANDS
  const scored = SLASH_COMMANDS.map((command) => ({ command, score: fuzzyScore(q, command.name) })).filter(
    (entry): entry is { command: SlashCommand; score: number } => entry.score !== null,
  )
  return scored.sort((a, b) => b.score - a.score).map((entry) => entry.command)
}

/** A complete command and its argument, or null when `text` names no command. */
export function parseSlash(text: string): { command: SlashCommand; arg: string } | null {
  const input = slashInput(text.trim())
  if (!input) return null
  const command = findCommand(input.name)
  return command ? { command, arg: (input.arg ?? '').trim() } : null
}

/** `on`/`off` and their spellings; null when the argument is empty or says neither. */
export function parseToggle(arg: string): boolean | null {
  const value = arg.trim().toLowerCase()
  if (['on', 'true', 'yes', '1', 'enable', 'enabled'].includes(value)) return true
  if (['off', 'false', 'no', '0', 'disable', 'disabled'].includes(value)) return false
  return null
}

/** Lower case, with the separators model names are full of (`-`, `_`, `.`, `/`, `:`) taken out. */
function compact(text: string): string {
  return text.toLowerCase().replace(/[\s\-_./:]+/g, '')
}

/**
 * How well `query` matches `text`, or null for no match. Higher is better.
 *
 * Every word of the query appearing somewhere is a strong match (`qwen 9b` finds
 * `Huihui-Qwen3.5-9B-abliterated-Q4_K_M`); the query's letters appearing in order is a weak one
 * (`qw9` finds it too). Shorter candidates and matches at the start rank first.
 */
export function fuzzyScore(query: string, text: string): number | null {
  const q = query.trim().toLowerCase()
  const t = text.toLowerCase()
  if (!q) return 0
  if (t === q) return 100_000
  const words = q.split(/\s+/).filter(Boolean)
  if (words.every((word) => t.includes(word) || compact(t).includes(compact(word)))) {
    const startsWith = t.startsWith(words[0] ?? '') ? 5_000 : 0
    const wordStart = words.some((word) => new RegExp(`(^|[\\s\\-_./:])${escapeRegExp(word)}`).test(t)) ? 1_000 : 0
    return 10_000 + startsWith + wordStart - t.length
  }
  const needle = compact(q)
  const hay = compact(t)
  let gaps = 0
  let at = -1
  for (const ch of needle) {
    const next = hay.indexOf(ch, at + 1)
    if (next === -1) return null
    if (at >= 0) gaps += next - at - 1
    at = next
  }
  return 1_000 - gaps * 10 - hay.length
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

/** `candidates` that match `query`, best first. An empty query keeps them all, in order. */
export function fuzzyFilter<T>(query: string, candidates: T[], key: (candidate: T) => string): T[] {
  if (!query.trim()) return candidates
  return candidates
    .map((candidate) => ({ candidate, score: fuzzyScore(query, key(candidate)) }))
    .filter((entry): entry is { candidate: T; score: number } => entry.score !== null)
    .sort((a, b) => b.score - a.score)
    .map((entry) => entry.candidate)
}

/** The installed model `/model <query>` means: the best fuzzy match, or null. */
export function pickModel(query: string, ids: string[]): string | null {
  if (!query.trim()) return null
  return fuzzyFilter(query, ids, (id) => id)[0] ?? null
}

/**
 * The `@` mention the caret is in, or null.
 *
 * A mention starts at an `@` that begins the text or follows whitespace — `me@example.com` is an
 * address, not a mention — and runs to the caret without whitespace.
 */
export function mentionAt(text: string, caret: number): { start: number; query: string } | null {
  for (let i = caret - 1; i >= 0; i--) {
    const ch = text[i] ?? ''
    if (/\s/.test(ch)) return null
    if (ch === '@') {
      const before = i === 0 ? '' : (text[i - 1] ?? '')
      if (before !== '' && !/\s/.test(before)) return null
      return { start: i, query: text.slice(i + 1, caret) }
    }
  }
  return null
}

/** The text with the mention at `start…caret` replaced by `@path `, and where the caret goes. */
export function insertMention(text: string, start: number, caret: number, path: string): { text: string; caret: number } {
  const inserted = `@${path} `
  const after = text.slice(caret).replace(/^\s/, '')
  return { text: text.slice(0, start) + inserted + after, caret: start + inserted.length }
}
