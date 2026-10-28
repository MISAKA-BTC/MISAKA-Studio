import assert from 'node:assert/strict'
import { test } from 'node:test'
import { withReasoningRecovery } from '../src/lib/reasoningRecovery.ts'
import type { ChatRequest, ChatStreamEvent } from '../src/lib/api.ts'

const thought: ChatStreamEvent = { type: 'reasoning', text: 'Thinking…' }
const answer: ChatStreamEvent = { type: 'delta', text: '確認できません。' }
const done = (finishReason: string): ChatStreamEvent => ({ type: 'done', finishReason, usage: { prompt_tokens: 10, completion_tokens: 20, total_tokens: 30 } })
const request: ChatRequest = { model: 'qwen', messages: [{ role: 'user', content: '質問' }], max_tokens: 2048, misaka: { pinned: ['note'] } }

async function run(replies: ChatStreamEvent[][], input = request, controller = new AbortController()) {
  const requests: ChatRequest[] = []
  const events: ChatStreamEvent[] = []
  const stream = async function* (req: ChatRequest) {
    requests.push(req)
    yield* replies[requests.length - 1] ?? []
  }
  for await (const event of withReasoningRecovery(stream, input, controller.signal)) events.push(event)
  return { requests, events }
}

test('reasoning-only length finish retries once without thinking and preserves the question and pinned notes', async () => {
  const { requests, events } = await run([[thought, done('length')], [answer, done('stop')]])
  assert.equal(requests.length, 2)
  assert.deepEqual(requests[1], { ...request, misaka: { pinned: ['note'], thinking: false } })
  assert.deepEqual(events, [thought, answer, { type: 'done', finishReason: 'stop', usage: { prompt_tokens: 20, completion_tokens: 40, total_tokens: 60 } }])
})

test('a failed recovery is surfaced and never loops into a third request', async () => {
  const { requests, events } = await run([[thought, done('length')], [thought, done('length')]])
  assert.equal(requests.length, 2)
  assert.equal(events.at(-1)?.type, 'error')
})

test('partial answers, normal stops, errors, and thinking already off never trigger a retry', async () => {
  for (const reply of [[thought, answer, done('length')], [thought, done('stop')], [thought, { type: 'error', message: 'offline' } as ChatStreamEvent, done('length')], [done('length')]]) {
    assert.equal((await run([reply])).requests.length, 1)
  }
  assert.equal((await run([[thought, done('length')]], { ...request, misaka: { pinned: [], thinking: false } })).requests.length, 1)
})

test('Stop between reasoning and recovery aborts without sending a second request', async () => {
  const controller = new AbortController()
  let calls = 0
  const stream = async function* () {
    calls++
    yield thought
    yield done('length')
    controller.abort()
  }
  await assert.rejects(async () => {
    for await (const _event of withReasoningRecovery(stream, request, controller.signal)) { /* drain */ }
  }, { name: 'AbortError' })
  assert.equal(calls, 1)
})
