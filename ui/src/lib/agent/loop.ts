// The coding agent's loop: ask the model for a step, run the tool call it makes, send the result
// back, and again — until it answers without a call, the person stops it, or a guard does.
//
// Everything the loop touches is passed in (`AgentLoopDeps`): the model, the conversation, the
// workspace, the approval prompt. `run.ts` wires those to the store, `streamReply` and the runtime;
// the tests wire them to a scripted model and a fake folder. Nothing here imports the store, so a
// test can drive a whole run — approvals, denials, stops — without a window or an engine.
//
// One tool call per step, by design. The stop sequence `</tool_call>` ends generation at the first
// call, so a step is "some reasoning, then one call", and the model always sees a call's result
// before deciding the next one. For a small model that is the difference between an agent and a
// guess: a second call written before the first one's result is a call made blind.

import type { ApprovalPolicy, Attachment, ChatMessage, ContextReport, Conversation, ToolCall, TurnStats } from '../types.ts'
import { contentForModel, stripThinking } from '../history.ts'
import {
  callKey,
  closeOpenToolCall,
  hasOpenToolCall,
  keepFirstCall,
  parseAgentText,
  removeThinking,
  toolResultsMessage,
  type ToolSegment,
} from './protocol.ts'
import {
  canonicalToolName,
  compactArgs,
  prepareCall,
  recordChange,
  summarizeCall,
  type ApprovalKind,
  type OutputLimits,
  type ToolOutcome,
  type WorkspaceApi,
} from './tools.ts'

/** One streamed reply, as `streamReply` returns it (`ReplyOutcome`). */
export type ModelReply = {
  content: string
  reasoning?: string
  reasoningMs?: number
  stats?: TurnStats
  context?: ContextReport
  error?: string
  aborted: boolean
  stoppedForRepetition: boolean
  failed?: boolean
}

export type ModelMessage = { role: 'system' | 'user' | 'assistant'; content: string }

/** The person's answer to an approval card. `stop` is the Stop button pressed while it waited. */
export type ApprovalAnswer = 'approve' | 'deny' | 'always' | 'stop'

export type AgentLoopDeps = {
  conversationId: string
  getConversation: () => Conversation | undefined
  updateConversation: (patch: (conversation: Conversation) => Partial<Conversation>) => void
  /** Append a message. For an assistant step this is also where the live overlay is dropped, in the
   *  same update, so the step is never drawn twice. */
  appendMessage: (message: ChatMessage) => void
  /** Stream one reply into the live overlay under `assistantId`, and return it. */
  callModel: (request: { assistantId: string; messages: ModelMessage[]; pinned: string[] }) => Promise<ModelReply>
  /** The system prompt for this step — rebuilt each time, so a policy changed mid-run applies. */
  systemPrompt: (conversation: Conversation) => string
  workspace: { root: string; api: WorkspaceApi; limits: OutputLimits }
  /** Wait for the person to answer the approval card of `toolCallId`. */
  requestApproval: (toolCallId: string) => Promise<ApprovalAnswer>
  /** Kinds approved with "Always allow in this thread". */
  allowed: () => ReadonlySet<ApprovalKind>
  allow: (kind: ApprovalKind) => void
  onStatus: (status: { status: 'running' | 'awaiting_approval'; step: number }) => void
  /** Called with the person's message once it is in the thread — where it is queued for mining. */
  onUserMessage?: (message: ChatMessage) => void
  /** Aborted by Stop. */
  signal: AbortSignal
  newId: () => string
  now: () => number
  maxSteps?: number
}

export type LoopOutcome = 'done' | 'stopped' | 'error' | 'limit' | 'stuck'

export type LoopResult = { outcome: LoopOutcome; steps: number; note?: string }

export const MAX_STEPS = 40
/** The same call this many times in a row is answered with "you are repeating yourself", not run. */
const REPEAT_WARN = 3
/** …and once more after that ends the run. */
const REPEAT_STOP = 4
/** The task as a pinned note is kept whole by the context manager, so it is kept short. */
const TASK_PIN_CHARS = 1500

const STOPPED = 'Not run: the user stopped the agent.'

/** A conversation's title from its first message — the store's rule. */
function deriveTitle(text: string): string {
  const clean = text.trim().replace(/\s+/g, ' ')
  return clean.length > 48 ? `${clean.slice(0, 48)}…` : clean || 'New session'
}

/**
 * The thread as the model is sent it.
 *
 * Not `historyForModel`: its rule that a question asked again replaces its earlier asking would drop
 * tool results that repeat — and an agent that reads the same file twice got the same text twice,
 * both real. Here a person's message goes with its attachments, a step goes as written (reasoning
 * out, its call closed), and tool results as stored. A text reply that failed is left out, as in
 * chat; a step with a call never is, because its result follows it. A step whose results never
 * arrived — the window closed mid-run — is answered as interrupted, so no call is left hanging.
 */
export function agentHistory(messages: ChatMessage[]): ModelMessage[] {
  const out: ModelMessage[] = []
  messages.forEach((message, index) => {
    if (message.role === 'system') return // the agent's own system prompt leads; a template allows one
    if (message.role === 'user') {
      out.push({ role: 'user', content: message.toolResults ? message.content : contentForModel(message) })
      return
    }
    if (message.streaming) return
    const calls = message.toolCalls ?? []
    const content = stripThinking(closeOpenToolCall(message.content)).trim()
    if (calls.length === 0 && (message.error || !content)) return
    out.push({ role: 'assistant', content })
    if (calls.length > 0 && !messages[index + 1]?.toolResults) {
      out.push({ role: 'user', content: toolResultsMessage(calls.map(() => 'Not run: the run was interrupted before this call ran.')) })
    }
  })
  return out
}

/**
 * A step's reply, put back into the shape the model meant.
 *
 * - The stop sequence ends generation at `</tool_call>` and the engine leaves it out, so a reply that
 *   stopped there gets it back. Only a reply that *stopped* — one cut by the token limit ends
 *   mid-call, and closing that would run half a call.
 * - A model thinking in the content can hit the stop sequence inside its reasoning, drafting the
 *   call it was about to make; so can one whose reasoning streams apart. Either way the call is
 *   the one it meant, and is moved to where calls go.
 * - Anything after the first call is dropped (see `keepFirstCall`).
 */
export function normalizeReply(reply: ModelReply): { content: string; reasoning?: string } {
  let content = reply.content
  let reasoning = reply.reasoning
  const stopped = reply.stats?.finishReason === 'stop' && !reply.aborted && !reply.failed && !reply.stoppedForRepetition
  if (stopped) {
    if (reasoning && !removeThinking(content).trim() && hasOpenToolCall(reasoning)) {
      const at = reasoning.lastIndexOf('<tool_call>')
      content = reasoning.slice(at)
      reasoning = reasoning.slice(0, at).trimEnd() || undefined
    }
    const think = content.lastIndexOf('<think>')
    if (think !== -1 && content.indexOf('</think>', think) === -1) {
      const call = content.indexOf('<tool_call>', think)
      if (call !== -1) content = `${content.slice(0, call).trimEnd()}\n</think>\n${content.slice(call)}`
    }
    if (hasOpenToolCall(content)) content = closeOpenToolCall(content)
  }
  content = keepFirstCall(content)
  return { content, ...(reasoning ? { reasoning } : {}) }
}

function patchCall(deps: AgentLoopDeps, messageId: string, callId: string, patch: Partial<ToolCall>) {
  deps.updateConversation((c) => ({
    messages: c.messages.map((m) =>
      m.id !== messageId || !m.toolCalls ? m : { ...m, toolCalls: m.toolCalls.map((t) => (t.id === callId ? { ...t, ...patch } : t)) },
    ),
  }))
}

function patchMessage(deps: AgentLoopDeps, messageId: string, patch: Partial<ChatMessage>) {
  deps.updateConversation((c) => ({ messages: c.messages.map((m) => (m.id === messageId ? { ...m, ...patch } : m)) }))
}

/** Tool calls a closed window left mid-flight: nobody is waiting on them any more. */
function settleStaleCalls(deps: AgentLoopDeps) {
  deps.updateConversation((c) => ({
    messages: c.messages.map((m) =>
      m.toolCalls?.some((t) => t.status === 'pending' || t.status === 'awaiting_approval' || t.status === 'running')
        ? {
            ...m,
            toolCalls: m.toolCalls.map((t) =>
              t.status === 'pending' || t.status === 'awaiting_approval' || t.status === 'running'
                ? { ...t, status: 'error' as const, error: 'Interrupted: the app was closed or reloaded before this finished.' }
                : t,
            ),
          }
        : m,
    ),
  }))
}

function policyOf(conversation: Conversation | undefined): ApprovalPolicy {
  return conversation?.approval ?? 'auto_edit'
}

/** What the model is told about a call that cannot run as written. */
function unrunnable(segment: ToolSegment, reply: ModelReply): string | null {
  if (!segment.complete) {
    const why =
      reply.stats?.finishReason === 'length'
        ? ' The reply reached its token limit. Write a large file in parts: create it with write_file, then add the rest with edit_file.'
        : reply.stoppedForRepetition
          ? ' The reply was stopped because it began repeating itself.'
          : ''
    return `Your ${segment.name || 'tool'} call was cut off before it was complete, so it was not run.${why}`
  }
  if (segment.error) return segment.error
  return null
}

/** Run the agent on one message from the person. */
export async function runAgentLoop(deps: AgentLoopDeps, request: { text: string; attachments?: Attachment[] }): Promise<LoopResult> {
  const maxSteps = deps.maxSteps ?? MAX_STEPS
  const text = request.text.trim()
  settleStaleCalls(deps)

  const userMessage: ChatMessage = {
    id: deps.newId(),
    role: 'user',
    content: text,
    createdAt: deps.now(),
    ...(request.attachments?.length ? { attachments: request.attachments } : {}),
  }
  deps.updateConversation((c) => ({
    messages: [...c.messages, userMessage],
    title: c.messages.length === 0 ? deriveTitle(text || request.attachments?.map((a) => a.name).join(', ') || '') : c.title,
    updatedAt: deps.now(),
  }))
  deps.onUserMessage?.(userMessage)

  const task = text.length > TASK_PIN_CHARS ? `${text.slice(0, TASK_PIN_CHARS)}…` : text
  let lastKey: string | null = null
  let repeats = 0
  let emptySteps = 0

  for (let step = 1; step <= maxSteps; step++) {
    if (deps.signal.aborted) return { outcome: 'stopped', steps: step - 1 }
    const conversation = deps.getConversation()
    if (!conversation) return { outcome: 'stopped', steps: step - 1, note: 'The thread was deleted.' }
    deps.onStatus({ status: 'running', step })

    const assistantId = deps.newId()
    const createdAt = deps.now()
    const reply = await deps.callModel({
      assistantId,
      messages: [{ role: 'system', content: deps.systemPrompt(conversation) }, ...agentHistory(conversation.messages)],
      pinned: [...(conversation.pinned ?? []), ...(task ? [`Task: ${task}`] : [])],
    })
    const { content, reasoning } = normalizeReply(reply)
    const segments = parseAgentText(content, { final: true })
    const toolSegments = segments.filter((s): s is ToolSegment => s.kind === 'tool')
    const hasText = segments.some((s) => s.kind === 'text')
    const calls: ToolCall[] = toolSegments.map((segment) => {
      const name = canonicalToolName(segment.name)
      return { id: deps.newId(), name, args: compactArgs(segment.args), status: 'pending', summary: summarizeCall(name, segment.args) }
    })
    const stepMessage: ChatMessage = {
      id: assistantId,
      role: 'assistant',
      content,
      createdAt,
      ...(reasoning ? { reasoning, ...(reply.reasoningMs !== undefined ? { reasoningMs: reply.reasoningMs } : {}) } : {}),
      ...(reply.stats ? { stats: reply.stats } : {}),
      ...(reply.context ? { context: reply.context } : {}),
      ...(calls.length ? { toolCalls: calls } : {}),
    }

    // Stopped while it streamed: what arrived is kept, and nothing it asked for runs.
    if (reply.aborted || deps.signal.aborted) {
      deps.appendMessage({ ...stepMessage, ...(calls.length ? { toolCalls: calls.map((c) => ({ ...c, status: 'denied' as const, error: 'Stopped before it ran.' })) } : {}) })
      if (calls.length) deps.appendMessage({ id: deps.newId(), role: 'user', content: toolResultsMessage(calls.map(() => STOPPED)), toolResults: true, createdAt: deps.now() })
      return { outcome: 'stopped', steps: step }
    }
    // The request failed, or the engine reported an error: the step ends the run, said on it.
    const looped = reply.stoppedForRepetition && calls.length > 0
    if (reply.error && !looped) {
      deps.appendMessage({
        ...stepMessage,
        error: reply.error,
        ...(calls.length ? { toolCalls: calls.map((c) => ({ ...c, status: 'error' as const, error: 'Not run: the reply failed.' })) } : {}),
      })
      if (calls.length) {
        deps.appendMessage({
          id: deps.newId(),
          role: 'user',
          content: toolResultsMessage(calls.map(() => `Not run: the reply failed (${reply.error}).`)),
          toolResults: true,
          createdAt: deps.now(),
        })
      }
      return { outcome: 'error', steps: step, note: reply.error }
    }

    if (calls.length === 0) {
      if (hasText) {
        deps.appendMessage(stepMessage)
        return { outcome: 'done', steps: step }
      }
      // Nothing at all: said once, plainly, then the run ends if it happens again.
      emptySteps++
      deps.appendMessage(stepMessage)
      if (emptySteps >= 2) {
        const note = 'The model returned nothing usable twice in a row, so the agent stopped.'
        patchMessage(deps, assistantId, { error: note })
        return { outcome: 'stuck', steps: step, note }
      }
      deps.appendMessage({
        id: deps.newId(),
        role: 'user',
        toolResults: true,
        createdAt: deps.now(),
        content:
          reply.stats?.finishReason === 'length'
            ? 'Your last message ran out of room before it said anything. Think less: make one tool call now, or reply with a short summary if the task is done.'
            : 'Your last message was empty. Continue the task: make one tool call, or reply with a short summary if the task is done.',
      })
      continue
    }

    deps.appendMessage(stepMessage)
    const outputs: string[] = []
    let validCalls = 0
    let declined = false
    let stuck = false
    for (const [index, call] of calls.entries()) {
      const segment = toolSegments[index]!
      if (deps.signal.aborted) {
        patchCall(deps, assistantId, call.id, { status: 'denied', error: 'Stopped before it ran.' })
        outputs.push(STOPPED)
        continue
      }
      if (declined) {
        patchCall(deps, assistantId, call.id, { status: 'denied', error: 'Not run: the call before it was declined.' })
        outputs.push('Not run: the user declined the call before this one.')
        continue
      }
      const problem = unrunnable(segment, reply)
      if (problem) {
        patchCall(deps, assistantId, call.id, { status: 'error', error: problem, output: `Error: ${problem}`, finishedAt: deps.now() })
        outputs.push(`Error: ${problem}`)
        continue
      }
      const key = callKey(call.name, segment.args)
      repeats = key === lastKey ? repeats + 1 : 1
      lastKey = key
      if (repeats >= REPEAT_WARN) {
        const warning = `You have made this exact ${call.name} call ${repeats} times in a row, and its result will not change. Use the result you already have, try a different approach, or finish with a summary.`
        patchCall(deps, assistantId, call.id, { status: 'error', error: 'Not run: the same call again.', output: `Error: ${warning}`, finishedAt: deps.now() })
        outputs.push(`Error: ${warning}`)
        if (repeats >= REPEAT_STOP) stuck = true
        continue
      }

      const outcome = await runOneCall(deps, assistantId, call, segment, step)
      outputs.push(outcome.output)
      if (outcome.counted) validCalls++
      if (outcome.declined) declined = true
    }
    deps.appendMessage({ id: deps.newId(), role: 'user', content: toolResultsMessage(outputs), toolResults: true, createdAt: deps.now() })
    deps.updateConversation(() => ({ updatedAt: deps.now() }))

    if (deps.signal.aborted) return { outcome: 'stopped', steps: step }
    if (stuck) {
      const note = 'The agent kept making the same call, so it was stopped. Send a message to steer it.'
      patchMessage(deps, assistantId, { error: note })
      return { outcome: 'stuck', steps: step, note }
    }
    emptySteps = !hasText && validCalls === 0 ? emptySteps + 1 : 0
    if (emptySteps >= 2) {
      const note = 'The model made no usable tool call twice in a row, so the agent stopped.'
      patchMessage(deps, assistantId, { error: note })
      return { outcome: 'stuck', steps: step, note }
    }
  }
  const note = `The agent stopped after ${maxSteps} steps. Send a message to let it carry on.`
  const lastStep = [...(deps.getConversation()?.messages ?? [])].reverse().find((m) => m.role === 'assistant')
  if (lastStep) patchMessage(deps, lastStep.id, { error: note })
  return { outcome: 'limit', steps: maxSteps, note }
}

/** One call: prepared, approved if it must be, run, and recorded — on the card and in Changes. */
async function runOneCall(
  deps: AgentLoopDeps,
  messageId: string,
  call: ToolCall,
  segment: ToolSegment,
  step: number,
): Promise<{ output: string; counted: boolean; declined?: boolean }> {
  const conversation = deps.getConversation()
  const prepared = await prepareCall(
    { name: call.name, args: segment.args },
    { ...deps.workspace, policy: policyOf(conversation), allowed: deps.allowed() },
  )
  if (prepared.kind === 'settled') {
    const outcome = prepared.outcome
    patchCall(deps, messageId, call.id, settledPatch(outcome, deps.now()))
    return { output: outcome.output, counted: outcome.status === 'done' }
  }

  if (prepared.approval === 'ask') {
    patchCall(deps, messageId, call.id, {
      status: 'awaiting_approval',
      summary: prepared.summary,
      args: compactArgs(prepared.args),
      ...(prepared.preview ? { display: prepared.preview } : {}),
    })
    deps.onStatus({ status: 'awaiting_approval', step })
    const answer = await deps.requestApproval(call.id)
    deps.onStatus({ status: 'running', step })
    if (answer === 'stop' || deps.signal.aborted) {
      patchCall(deps, messageId, call.id, { status: 'denied', error: 'Stopped before it ran.', finishedAt: deps.now() })
      return { output: STOPPED, counted: false }
    }
    if (answer === 'deny') {
      const what = prepared.toolKind === 'command' ? 'command' : 'edit'
      patchCall(deps, messageId, call.id, { status: 'denied', finishedAt: deps.now() })
      return {
        output: `The user declined this ${what}, so it was not run. Do not try it again as it was: change your approach, or ask the user what they want.`,
        counted: true,
        declined: true,
      }
    }
    if (answer === 'always' && prepared.toolKind !== 'read') deps.allow(prepared.toolKind)
  }

  patchCall(deps, messageId, call.id, { status: 'running', summary: prepared.summary, args: compactArgs(prepared.args), startedAt: deps.now() })
  const outcome = await prepared.run(deps.signal)
  patchCall(deps, messageId, call.id, { ...settledPatch(outcome, deps.now()), ...(outcome.display ? {} : prepared.preview ? { display: prepared.preview } : {}) })
  if (outcome.change) {
    const change = outcome.change
    deps.updateConversation((c) => ({ changes: recordChange(c.changes, change, deps.now()) }))
  }
  return { output: outcome.output, counted: true }
}

function settledPatch(outcome: ToolOutcome, now: number): Partial<ToolCall> {
  return {
    status: outcome.status,
    summary: outcome.summary,
    output: outcome.output,
    ...(outcome.display ? { display: outcome.display } : {}),
    ...(outcome.error ? { error: outcome.error } : { error: undefined }),
    finishedAt: now,
  }
}
