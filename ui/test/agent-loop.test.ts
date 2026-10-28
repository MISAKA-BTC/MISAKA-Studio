import assert from 'node:assert/strict'
import { test } from 'node:test'
import { agentHistory, normalizeReply, runAgentLoop, type AgentLoopDeps, type ApprovalAnswer, type ModelMessage, type ModelReply } from '../src/lib/agent/loop.ts'
import { fakeWorkspace } from '../src/lib/agent/fakeWorkspace.ts'
import { buildSystemPrompt } from '../src/lib/agent/protocol.ts'
import { outputLimits, type ApprovalKind } from '../src/lib/agent/tools.ts'
import type { ApprovalPolicy, ChatMessage, Conversation } from '../src/lib/types.ts'

const MAIN = 'fn main() {\n    println!("hello");\n}\n'

/** A reply as llama.cpp returns it under `stop: ['</tool_call>']`: the stop string left out. */
function stopped(content: string, extra: Partial<ModelReply> = {}): ModelReply {
  return {
    content,
    aborted: false,
    stoppedForRepetition: false,
    stats: { finishReason: 'stop', completionTokens: 20, promptTokens: 300, tokensPerSecond: 30, timeToFirstTokenMs: 40, model: 'qwen' },
    ...extra,
  }
}

const call = (name: string, params: Record<string, string>, before = '') =>
  `${before}<tool_call>\n<function=${name}>\n${Object.entries(params)
    .map(([key, value]) => `<parameter=${key}>\n${value}\n</parameter>\n`)
    .join('')}</function>\n`

type Script = ModelReply | ((request: { messages: ModelMessage[]; pinned: string[] }) => ModelReply | Promise<ModelReply>)

function harness(options: { files?: Record<string, string>; policy?: ApprovalPolicy; replies: Script[]; approve?: (toolCallId: string, h: Harness) => ApprovalAnswer | Promise<ApprovalAnswer>; maxSteps?: number; messages?: ChatMessage[] }) {
  const api = fakeWorkspace(options.files ?? {})
  const controller = new AbortController()
  const allowed = new Set<ApprovalKind>()
  const h = {
    conversation: {
      id: 'c1',
      title: 'New agent thread',
      createdAt: 0,
      updatedAt: 0,
      modelId: null,
      messages: options.messages ?? [],
      mode: 'agent',
      workspace: '/fake',
      approval: options.policy ?? 'auto_edit',
    } as Conversation,
    requests: [] as { messages: ModelMessage[]; pinned: string[] }[],
    statuses: [] as { status: string; step: number }[],
    mined: [] as ChatMessage[],
    asked: [] as string[],
    api,
    controller,
    allowed,
    deps: null as unknown as AgentLoopDeps,
  }
  type Harness = typeof h
  let n = 0
  const replies = [...options.replies]
  h.deps = {
    conversationId: 'c1',
    getConversation: () => h.conversation,
    updateConversation: (patch) => {
      h.conversation = { ...h.conversation, ...patch(h.conversation) }
    },
    appendMessage: (message) => {
      h.conversation = { ...h.conversation, messages: [...h.conversation.messages, message] }
    },
    callModel: async (request) => {
      h.requests.push({ messages: request.messages, pinned: request.pinned })
      const next = replies.shift()
      assert.ok(next, `the model was asked for step ${h.requests.length}, but the script has ended`)
      return typeof next === 'function' ? next(request) : next
    },
    systemPrompt: (c) => buildSystemPrompt({ root: '/fake', date: '2026-10-27', policy: c.approval ?? 'auto_edit' }),
    workspace: { root: '/fake', api, limits: outputLimits(16384) },
    requestApproval: async (toolCallId) => {
      h.asked.push(toolCallId)
      return options.approve ? options.approve(toolCallId, h) : 'approve'
    },
    allowed: () => allowed,
    allow: (kind) => allowed.add(kind),
    onStatus: (status) => h.statuses.push(status),
    onUserMessage: (message) => h.mined.push(message),
    signal: controller.signal,
    newId: () => `id${++n}`,
    now: () => 1000 + n,
    maxSteps: options.maxSteps,
  }
  return h
}

const toolCalls = (c: Conversation) => c.messages.flatMap((m) => m.toolCalls ?? [])
const results = (c: Conversation) => c.messages.filter((m) => m.toolResults).map((m) => m.content)

test('a tool call, its result, then an answer: the whole round trip', async () => {
  const h = harness({
    files: { 'src/main.rs': MAIN },
    replies: [stopped(call('read_file', { path: 'src/main.rs' }, 'Let me read it.\n')), stopped('`main` prints "hello".')],
  })
  const result = await runAgentLoop(h.deps, { text: 'what does main do?' })
  assert.deepEqual(result, { outcome: 'done', steps: 2 })

  const [user, step, toolResult, answer] = h.conversation.messages
  assert.equal(h.conversation.messages.length, 4)
  assert.equal(user!.role, 'user')
  assert.equal(user!.content, 'what does main do?')
  assert.equal(h.conversation.title, 'what does main do?')
  assert.ok(step!.content.endsWith('</function>\n</tool_call>'), 'the stop string is put back')
  assert.equal(step!.toolCalls!.length, 1)
  assert.equal(step!.toolCalls![0]!.status, 'done')
  assert.equal(step!.toolCalls![0]!.summary, 'Read src/main.rs · 3 lines')
  assert.equal(toolResult!.toolResults, true)
  assert.ok(toolResult!.content.startsWith('<tool_response>\nsrc/main.rs (3 lines)\n  1\tfn main() {\n'))
  assert.ok(toolResult!.content.endsWith('</tool_response>'))
  assert.equal(answer!.content, '`main` prints "hello".')

  // The second request is the first plus the step and its result — as the model will read them.
  const second = h.requests[1]!.messages
  assert.equal(second[0]!.role, 'system')
  assert.deepEqual(
    second.slice(1).map((m) => m.role),
    ['user', 'assistant', 'user'],
  )
  assert.equal(second[2]!.content, step!.content)
  assert.equal(second[3]!.content, toolResult!.content)
  assert.deepEqual(h.requests[0]!.pinned, ['Task: what does main do?'], 'the task is pinned, so a trimmed run keeps it')
  assert.deepEqual(h.mined.map((m) => m.content), ['what does main do?'], 'only what was typed is mined')
  assert.deepEqual(h.statuses, [
    { status: 'running', step: 1 },
    { status: 'running', step: 2 },
  ])
})

test('attachments go to the model with the message, but only the typed text is mined or pinned', async () => {
  const h = harness({ replies: [stopped('Seen it.')] })
  await runAgentLoop(h.deps, { text: 'look at this', attachments: [{ id: 'a', name: 'secret.env', size: 7, content: 'KEY=123' }] })
  assert.match(h.requests[0]!.messages[1]!.content, /look at this\n\nAttached file `secret.env`/)
  assert.equal(h.mined[0]!.content, 'look at this')
  assert.deepEqual(h.requests[0]!.pinned, ['Task: look at this'])
})

test('an edit under "ask" waits, shows its diff, and a denial leaves the file alone and is told to the model', async () => {
  let seenWhileWaiting: Conversation | null = null
  const h = harness({
    policy: 'ask',
    files: { 'src/main.rs': MAIN },
    replies: [
      stopped(call('edit_file', { path: 'src/main.rs', old_string: '    println!("hello");', new_string: '    println!("bye");' })),
      stopped('Understood, I left it as it is.'),
    ],
    approve: (_id, harnessNow) => {
      seenWhileWaiting = harnessNow.conversation
      return 'deny'
    },
  })
  const result = await runAgentLoop(h.deps, { text: 'say bye' })
  assert.equal(result.outcome, 'done')
  const waiting = toolCalls(seenWhileWaiting!)[0]!
  assert.equal(waiting.status, 'awaiting_approval')
  assert.equal(waiting.display?.kind, 'diff')
  assert.ok(h.statuses.some((s) => s.status === 'awaiting_approval'))
  assert.equal(h.api.files.get('src/main.rs'), MAIN, 'nothing was written')
  assert.ok(!h.api.calls.some((c) => c.method === 'edit'))
  assert.equal(toolCalls(h.conversation)[0]!.status, 'denied')
  assert.match(results(h.conversation)[0]!, /The user declined this edit, so it was not run\./)
  assert.equal(h.conversation.changes, undefined)
})

test('"Always allow" approves that kind for the rest of the thread', async () => {
  const h = harness({
    policy: 'ask',
    files: { 'a.txt': 'one\n' },
    replies: [
      stopped(call('edit_file', { path: 'a.txt', old_string: 'one', new_string: 'two' })),
      stopped(call('edit_file', { path: 'a.txt', old_string: 'two', new_string: 'three' })),
      stopped('Done.'),
    ],
    approve: () => 'always',
  })
  await runAgentLoop(h.deps, { text: 'count up' })
  assert.equal(h.asked.length, 1, 'asked once')
  assert.deepEqual([...h.allowed], ['edit'])
  assert.equal(h.api.files.get('a.txt'), 'three\n')
  assert.deepEqual(h.conversation.changes?.map((c) => [c.path, c.original, c.current]), [['a.txt', 'one\n', 'three\n']])
})

test('Stop while an approval waits: the call is not run and the run ends', async () => {
  const h = harness({
    policy: 'auto_edit',
    replies: [stopped(call('run_command', { command: 'rm -rf build' }))],
    approve: (_id, harnessNow) => {
      harnessNow.controller.abort()
      return 'stop'
    },
  })
  const result = await runAgentLoop(h.deps, { text: 'clean up' })
  assert.equal(result.outcome, 'stopped')
  assert.ok(!h.api.calls.some((c) => c.method === 'exec'))
  assert.equal(toolCalls(h.conversation)[0]!.status, 'denied')
  assert.deepEqual(results(h.conversation), ['<tool_response>\nNot run: the user stopped the agent.\n</tool_response>'])
})

test('Stop while the reply streams: what arrived is kept, and the call in it never runs', async () => {
  const h = harness({
    replies: [{ content: call('write_file', { path: 'x.txt', content: 'partial' }), aborted: true, stoppedForRepetition: false }],
  })
  const result = await runAgentLoop(h.deps, { text: 'write x' })
  assert.equal(result.outcome, 'stopped')
  assert.equal(h.api.files.size, 0)
  const step = h.conversation.messages[1]!
  assert.equal(step.role, 'assistant')
  assert.equal(step.toolCalls![0]!.status, 'denied')
})

test('the same call three times in a row is not run the third time, and a fourth ends the run', async () => {
  const read = () => stopped(call('read_file', { path: 'src/main.rs' }))
  const h = harness({ files: { 'src/main.rs': MAIN }, replies: [read(), read(), read(), read()] })
  const result = await runAgentLoop(h.deps, { text: 'read main' })
  assert.equal(result.outcome, 'stuck')
  assert.equal(h.api.calls.filter((c) => c.method === 'read').length, 2, 'run twice, then refused')
  assert.match(results(h.conversation)[2]!, /You have made this exact read_file call 3 times in a row/)
  assert.ok(h.conversation.messages.at(-2)!.error?.includes('kept making the same call'))
})

test('an empty reply is answered once; a second in a row ends the run', async () => {
  const h = harness({ replies: [stopped(''), stopped('   ')] })
  const result = await runAgentLoop(h.deps, { text: 'hello?' })
  assert.equal(result.outcome, 'stuck')
  assert.match(results(h.conversation)[0]!, /^Your last message was empty\./)
})

test('a run stops at the step limit and says so', async () => {
  const h = harness({
    files: { 'a.txt': 'a\n', 'b.txt': 'b\n', 'c.txt': 'c\n' },
    replies: ['a.txt', 'b.txt', 'c.txt'].map((path) => stopped(call('read_file', { path }))),
    maxSteps: 3,
  })
  const result = await runAgentLoop(h.deps, { text: 'read everything' })
  assert.equal(result.outcome, 'limit')
  assert.equal(result.steps, 3)
  assert.match(result.note!, /stopped after 3 steps/)
})

test('a write cut off by the token limit is never run, and the model is told to write in parts', async () => {
  const cut = '<tool_call>\n<function=write_file>\n<parameter=path>\nbig.txt\n</parameter>\n<parameter=content>\nline 1\nline 2'
  const h = harness({
    replies: [stopped(cut, { stats: { finishReason: 'length', completionTokens: 4096, promptTokens: 300, tokensPerSecond: 30, timeToFirstTokenMs: 40, model: 'q' } }), stopped('I will split it.')],
  })
  await runAgentLoop(h.deps, { text: 'write big' })
  assert.equal(h.api.files.size, 0)
  assert.equal(toolCalls(h.conversation)[0]!.status, 'error')
  assert.match(results(h.conversation)[0]!, /cut off before it was complete, so it was not run\. The reply reached its token limit\. Write a large file in parts/)
})

test('read only: an edit is refused and explained, and the prompt never offered it', async () => {
  const h = harness({
    policy: 'read_only',
    files: { 'a.txt': 'x\n' },
    replies: [stopped(call('write_file', { path: 'a.txt', content: 'y' })), stopped('I can only suggest the change.')],
  })
  await runAgentLoop(h.deps, { text: 'change a' })
  assert.ok(!h.requests[0]!.messages[0]!.content.includes('write_file'))
  assert.equal(h.api.files.get('a.txt'), 'x\n')
  assert.match(results(h.conversation)[0]!, /approval policy is "Read only"/)
})

test('a policy changed mid-run applies from the next call', async () => {
  const h = harness({
    policy: 'auto_edit',
    files: { 'a.txt': 'x\n' },
    replies: [
      stopped(call('read_file', { path: 'a.txt' })),
      () => stopped(call('write_file', { path: 'a.txt', content: 'y\n' })),
      stopped('ok'),
    ],
  })
  const deps = h.deps
  const callModel = deps.callModel
  deps.callModel = async (request) => {
    // Switched while the first step streams: the second step's prompt and its call both follow it.
    if (h.requests.length === 0) h.conversation = { ...h.conversation, approval: 'read_only' }
    return callModel(request)
  }
  await runAgentLoop(deps, { text: 'go' })
  assert.ok(h.requests[0]!.messages[0]!.content.includes('write_file'))
  assert.ok(!h.requests[1]!.messages[0]!.content.includes('write_file'))
  assert.equal(h.api.files.get('a.txt'), 'x\n')
  assert.match(results(h.conversation)[1]!, /approval policy is "Read only"/)
})

test('a failed request ends the run with the error on the step', async () => {
  const h = harness({ replies: [{ content: '', aborted: false, stoppedForRepetition: false, failed: true, error: 'connection reset' }] })
  const result = await runAgentLoop(h.deps, { text: 'hi' })
  assert.deepEqual(result, { outcome: 'error', steps: 1, note: 'connection reset' })
  assert.equal(h.conversation.messages[1]!.error, 'connection reset')
})

test('an unknown tool is answered with the real ones, and the run goes on', async () => {
  const h = harness({ replies: [stopped(call('open_browser', { url: 'x' })), stopped('Sorry — done without it.')] })
  const result = await runAgentLoop(h.deps, { text: 'go' })
  assert.equal(result.outcome, 'done')
  assert.match(results(h.conversation)[0]!, /no tool called "open_browser"/)
})

test('auto edit applies an edit at once and lists it in Changes with its original', async () => {
  const h = harness({
    files: { 'src/main.rs': MAIN },
    replies: [stopped(call('edit_file', { path: 'src/main.rs', old_string: 'hello', new_string: 'world' })), stopped('Changed.')],
  })
  await runAgentLoop(h.deps, { text: 'greet the world' })
  assert.equal(h.asked.length, 0)
  assert.deepEqual(h.conversation.changes?.map((c) => ({ path: c.path, original: c.original, current: c.current })), [
    { path: 'src/main.rs', original: MAIN, current: MAIN.replace('hello', 'world') },
  ])
  const display = toolCalls(h.conversation)[0]!.display
  assert.equal(display?.kind, 'diff')
})

test('a call the model drafted inside its reasoning, cut there by the stop sequence, is the call it meant', async () => {
  const reasoning = 'I should read the file first.\n<tool_call>\n<function=read_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n</function>\n'
  const normalized = normalizeReply(stopped('', { reasoning }))
  assert.equal(normalized.reasoning, 'I should read the file first.')
  assert.ok(normalized.content.startsWith('<tool_call>\n<function=read_file>'))
  assert.ok(normalized.content.endsWith('</tool_call>'))
  const inline = normalizeReply(stopped('<think>Reading first.\n<tool_call>\n<function=read_file>\n<parameter=path>\na\n</parameter>\n</function>\n'))
  assert.ok(inline.content.includes('</think>\n<tool_call>'))
  // Cut by the token limit, a call is NOT closed: it may be missing its end.
  const cut = normalizeReply(stopped(call('read_file', { path: 'a' }), { stats: { finishReason: 'length', completionTokens: 1, promptTokens: 1, tokensPerSecond: 1, timeToFirstTokenMs: 1, model: 'q' } }))
  assert.ok(!cut.content.endsWith('</tool_call>'))
})

test('calls left mid-flight by a closed window are settled when the next run starts', async () => {
  const stale: ChatMessage = {
    id: 's',
    role: 'assistant',
    content: call('run_command', { command: 'make' }),
    toolCalls: [{ id: 't', name: 'run_command', args: { command: 'make' }, status: 'awaiting_approval' }],
  }
  const h = harness({ messages: [{ id: 'u', role: 'user', content: 'build' }, stale], replies: [stopped('Ready when you are.')] })
  await runAgentLoop(h.deps, { text: 'try again' })
  const settled = h.conversation.messages.find((m) => m.id === 's')!.toolCalls![0]!
  assert.equal(settled.status, 'error')
  assert.match(settled.error!, /Interrupted/)
  // …and the model is told that call never ran, instead of seeing it unanswered.
  const sent = h.requests[0]!.messages
  assert.equal(sent[3]!.content, '<tool_response>\nNot run: the run was interrupted before this call ran.\n</tool_response>')
})

test('agent history: repeated tool results both stay, reasoning goes, failed text replies go', () => {
  const step = (id: string): ChatMessage => ({
    id,
    role: 'assistant',
    content: `<think>hmm</think>\n${call('read_file', { path: 'a' })}`,
    toolCalls: [{ id: `${id}t`, name: 'read_file', args: { path: 'a' }, status: 'done' }],
  })
  const result: ChatMessage = { id: 'r', role: 'user', content: '<tool_response>\nsame\n</tool_response>', toolResults: true }
  const history = agentHistory([
    { id: 'u', role: 'user', content: 'read a twice' },
    step('1'),
    { ...result, id: 'r1' },
    step('2'),
    { ...result, id: 'r2' },
    { id: 'f', role: 'assistant', content: 'half an ans', error: 'stream broke' },
    { id: 'sys', role: 'system', content: 'ignored' },
  ])
  assert.deepEqual(
    history.map((m) => m.role),
    ['user', 'assistant', 'user', 'assistant', 'user'],
  )
  assert.equal(history[1]!.content, `${call('read_file', { path: 'a' })}</tool_call>`)
  assert.equal(history[2]!.content, history[4]!.content)
})
