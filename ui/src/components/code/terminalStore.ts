// The terminals, one per folder, kept in memory for as long as the window is open.
//
// Per folder rather than per session: two sessions in the same project share one shell history in
// any terminal app, and switching between them should not lose the build that is half-way through.
// So a command keeps running when its pane is closed or another session is opened — its output
// lands here, and the terminal shows it whenever that folder's terminal is looked at again.
//
// Output arrives in bursts of small chunks; it is gathered and written to the store a few times a
// second, so a chatty command redraws one block at a steady rate instead of the window per chunk.

import { create } from 'zustand'
import { ApiError, api, execStream } from '../../lib/api'
import { EMPTY_OUTPUT, appendOutput, parseCd, resolveCd, withCwd, type BlockOutput, type BlockStatus, type OutputStream, type ShellFlavor } from '../../lib/terminal'
import { useCodeUi } from './codeStore'
import { isMissingEndpoint, scheduleGitRefresh } from './gitStore'

export type TerminalBlock = {
  id: string
  command: string
  /** The directory it ran in, relative to the folder. */
  cwd: string
  startedAt: number
  durationMs: number | null
  status: BlockStatus
  exitCode: number | null
  output: BlockOutput
  error: string | null
  /** The runtime has no streaming endpoint for commands yet. */
  unsupported?: boolean
  /** Answered by the terminal itself (`cd`): no shell ran. */
  local?: boolean
}

export type TerminalSession = {
  /** Relative to the folder; `''` is the folder itself. */
  cwd: string
  /** Where `cd -` goes. */
  previous: string | null
  blocks: TerminalBlock[]
  /** The block whose command is running, if any. One at a time per folder. */
  running: string | null
}

export const EMPTY_SESSION: TerminalSession = { cwd: '', previous: null, blocks: [], running: null }

export const useTerminals = create<{ byRoot: Record<string, TerminalSession> }>(() => ({ byRoot: {} }))

/** Blocks kept per folder, and output kept across them; the oldest go first. Each block keeps its
 *  own tail too (`MAX_BLOCK_CHARS`), but a long session of noisy builds would still add up. */
const MAX_BLOCKS = 200
const MAX_FOLDER_CHARS = 2_000_000
/** The longest a command may run — the runtime's ceiling, an hour. A terminal is where a long build
 *  or a dev server runs; the person stops what they no longer want (Ctrl+C, Stop). */
export const TERMINAL_TIMEOUT_SECS = 3600
const FLUSH_MS = 50

const controllers = new Map<string, AbortController>()
/** A `cd` whose directory is being checked, per folder: the block it drew and when it is done. */
const cdChecks = new Map<string, { id: string; done: Promise<void> }>()
const pending = new Map<string, { root: string; chunks: { stream: OutputStream; text: string }[] }>()
let flushTimer: ReturnType<typeof setTimeout> | null = null
let counter = 0

const blockId = () => `term-${Date.now().toString(36)}-${(counter++).toString(36)}`

function update(root: string, fn: (session: TerminalSession) => TerminalSession) {
  useTerminals.setState((s) => {
    const current = s.byRoot[root] ?? EMPTY_SESSION
    const next = fn(current)
    return next === current ? {} : { byRoot: { ...s.byRoot, [root]: next } }
  })
}

function updateBlock(root: string, id: string, fn: (block: TerminalBlock) => TerminalBlock) {
  update(root, (s) => ({ ...s, blocks: s.blocks.map((b) => (b.id === id ? fn(b) : b)) }))
}

/** The blocks with the oldest let go past the folder's limits — never the one still running. */
function trimmed(blocks: TerminalBlock[], running: string | null): TerminalBlock[] {
  let size = blocks.reduce((sum, b) => sum + b.output.size, 0)
  if (blocks.length <= MAX_BLOCKS && size <= MAX_FOLDER_CHARS) return blocks
  const kept = [...blocks]
  while (kept.length > 1 && (kept.length > MAX_BLOCKS || size > MAX_FOLDER_CHARS)) {
    const index = kept.findIndex((b) => b.id !== running)
    if (index === -1 || index === kept.length - 1) break
    size -= kept[index]!.output.size
    kept.splice(index, 1)
  }
  return kept
}

function addBlock(root: string, block: TerminalBlock) {
  update(root, (s) => ({ ...s, blocks: trimmed([...s.blocks, block], s.running) }))
}

function flush() {
  if (flushTimer) {
    clearTimeout(flushTimer)
    flushTimer = null
  }
  if (pending.size === 0) return
  const batches = [...pending.entries()]
  pending.clear()
  useTerminals.setState((s) => {
    const byRoot = { ...s.byRoot }
    for (const [id, { root, chunks }] of batches) {
      const session = byRoot[root]
      if (!session) continue
      const blocks = session.blocks.map((b) => (b.id === id ? { ...b, output: chunks.reduce((out, c) => appendOutput(out, c.stream, c.text), b.output) } : b))
      byRoot[root] = { ...session, blocks: trimmed(blocks, session.running) }
    }
    return { byRoot }
  })
}

function queueOutput(root: string, id: string, stream: OutputStream, text: string) {
  if (!text) return
  const entry = pending.get(id) ?? { root, chunks: [] }
  const last = entry.chunks[entry.chunks.length - 1]
  if (last && last.stream === stream) last.text += text
  else entry.chunks.push({ stream, text })
  pending.set(id, entry)
  flushTimer ??= setTimeout(flush, FLUSH_MS)
}

export type TerminalEnv = { flavor: ShellFlavor; home: string | null }

/** Answer a `cd` here: refuse it with the reason, or move the terminal's directory. */
async function changeDirectory(root: string, command: string, parsed: NonNullable<ReturnType<typeof parseCd>>, env: TerminalEnv) {
  const session = useTerminals.getState().byRoot[root] ?? EMPTY_SESSION
  const id = blockId()
  const block: TerminalBlock = {
    id,
    command,
    cwd: session.cwd,
    startedAt: Date.now(),
    durationMs: null,
    status: 'running',
    exitCode: null,
    output: EMPTY_OUTPUT,
    error: null,
    local: true,
  }
  const refuse = (message: string) => ({ ...block, status: 'done' as const, exitCode: 1, output: appendOutput(EMPTY_OUTPUT, 'stderr', `${message}\n`) })
  if (!parsed.ok) return addBlock(root, refuse(parsed.error))
  const resolved = resolveCd(session.cwd, parsed.target, { root, previous: session.previous, home: env.home, flavor: env.flavor })
  if (!resolved.ok) return addBlock(root, refuse(resolved.error))

  addBlock(root, block)
  update(root, (s) => ({ ...s, running: id }))
  let refusal: string | null = null
  if (resolved.cwd) {
    // The directory must exist and be inside the folder — asked of the runtime, which also refuses a
    // link that leads out of it. A runtime that cannot be asked is not a reason to refuse: the next
    // command will say if the directory is not there.
    try {
      await api.workspace.tree(root, resolved.cwd, 1, 1)
    } catch (error) {
      if (error instanceof ApiError && error.status === 400) refusal = `cd: ${error.message}`
    }
  }
  const refused = refusal
  if (refused) updateBlock(root, id, () => refuse(refused))
  else {
    updateBlock(root, id, (b) => ({ ...b, status: 'done', exitCode: 0 }))
    update(root, (s) => (s.cwd === resolved.cwd ? s : { ...s, previous: s.cwd, cwd: resolved.cwd }))
  }
  update(root, (s) => (s.running === id ? { ...s, running: null } : s))
}

/**
 * Run what was typed at the folder's prompt. `clear` and `cd` are the terminal's own; everything
 * else is a fresh login shell in the folder, its output streamed into a new block.
 */
export async function runCommand(root: string, input: string, env: TerminalEnv, options: { remembered?: boolean } = {}): Promise<void> {
  const command = input.trim()
  if (!command) return
  // A line typed right after a `cd` waits for it, as typing ahead does in any terminal, and then
  // runs in the directory the `cd` went to.
  const checking = cdChecks.get(root)
  if (checking) {
    if (useTerminals.getState().byRoot[root]?.running !== checking.id) return
    useCodeUi.getState().remember(root, input)
    await checking.done
    return runCommand(root, input, env, { remembered: true })
  }
  const session = useTerminals.getState().byRoot[root] ?? EMPTY_SESSION
  if (session.running) return
  if (!options.remembered) useCodeUi.getState().remember(root, input)
  if (command === 'clear' || (env.flavor === 'windows' && command === 'cls')) return clearTerminal(root)
  const cd = parseCd(command)
  if (cd) {
    const done = changeDirectory(root, command, cd, env)
    const id = useTerminals.getState().byRoot[root]?.running
    if (id) cdChecks.set(root, { id, done })
    try {
      await done
    } finally {
      if (cdChecks.get(root)?.done === done) cdChecks.delete(root)
    }
    return
  }

  const id = blockId()
  const started = Date.now()
  const controller = new AbortController()
  controllers.set(root, controller)
  addBlock(root, { id, command, cwd: session.cwd, startedAt: started, durationMs: null, status: 'running', exitCode: null, output: EMPTY_OUTPUT, error: null })
  update(root, (s) => ({ ...s, running: id }))

  let ended = false
  const finish = (patch: Partial<TerminalBlock>) => {
    flush()
    ended = true
    updateBlock(root, id, (b) => ({ ...b, durationMs: Date.now() - started, ...patch }))
  }
  try {
    for await (const event of execStream(root, withCwd(command, session.cwd, env.flavor), controller.signal, TERMINAL_TIMEOUT_SECS)) {
      if (event.type === 'stdout' || event.type === 'stderr') queueOutput(root, id, event.type, event.data)
      else if (event.type === 'exit') finish({ status: event.timed_out ? 'timed_out' : 'done', exitCode: event.exit_code, durationMs: event.duration_ms })
      else if (event.type === 'error') finish({ status: 'error', error: event.message })
    }
    if (!ended) finish(controller.signal.aborted ? { status: 'stopped' } : { status: 'error', error: 'the runtime closed the connection before the command finished' })
  } catch (error) {
    if (ended) {
      /* the outcome is already written; a connection closing after it changes nothing */
    } else if (controller.signal.aborted) finish({ status: 'stopped' })
    else if (isMissingEndpoint(error)) finish({ status: 'error', unsupported: true, error: 'this runtime has no terminal endpoint yet' })
    else finish({ status: 'error', error: (error as Error).message })
  } finally {
    flush()
    if (controllers.get(root) === controller) controllers.delete(root)
    update(root, (s) => (s.running === id ? { ...s, running: null } : s))
    // A command is the likeliest thing to have changed the working tree.
    scheduleGitRefresh(root, 300)
  }
}

/** Stop the folder's running command: the request is aborted, which the runtime takes as the signal
 *  to kill the command's whole process group. */
export function stopCommand(root: string): void {
  controllers.get(root)?.abort()
}

/** Clear the scrollback — all but a command still running. */
export function clearTerminal(root: string): void {
  update(root, (s) => ({ ...s, blocks: s.blocks.filter((b) => b.id === s.running) }))
}

export function useTerminal(root: string | null): TerminalSession {
  return useTerminals((s) => (root ? (s.byRoot[root] ?? EMPTY_SESSION) : EMPTY_SESSION))
}

/** Whether a command is running in the folder — for the header's dot. */
export function useTerminalBusy(root: string | null | undefined): boolean {
  return useTerminals((s) => (root ? Boolean(s.byRoot[root]?.running) : false))
}
