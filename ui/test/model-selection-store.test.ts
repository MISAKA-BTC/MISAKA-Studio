import assert from 'node:assert/strict'
import { test } from 'node:test'
import { build } from 'esbuild'
import { fileURLToPath } from 'node:url'

// Compile the real browser store and its dependencies, so these tests exercise selection,
// persistence and requests together rather than a second implementation of the state machine.
const storage = new Map<string, string>()
Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
  getItem: (key: string) => storage.get(key) ?? null,
  setItem: (key: string, value: string) => storage.set(key, value),
  removeItem: (key: string) => storage.delete(key),
} })
const compiled = await build({
  stdin: {
    contents: "export { useStudio, chatModelId, chatModelError, streamReply } from './src/store/studio'; export { api } from './src/lib/api';",
    resolveDir: fileURLToPath(new URL('..', import.meta.url)),
  },
  bundle: true, write: false, platform: 'node', format: 'esm', logLevel: 'silent',
})
const { useStudio, chatModelId, chatModelError, streamReply, api } = await import(`data:text/javascript;base64,${Buffer.from(compiled.outputFiles[0]!.text).toString('base64')}`)
const initial = useStudio.getState()
const model = (id: string) => ({ id, path: `/models/${id}.gguf`, fit: { verdict: 'fits' } })
const runtime = (id: string | null) => ({ model_id: id, context_size: 65536, backend: 'llamacpp' })
const conversation = (id: string, modelId: string | null) => ({ id, title: id, modelId, messages: [], createdAt: 0, updatedAt: 0 })

function reset() {
  useStudio.setState({ ...initial, models: [model('a'), model('b'), model('c')], connected: true, runtime: runtime('a'), conversations: [conversation('chat', 'a')], activeConversationId: 'chat' })
  api.runtime = async () => runtime('a')
  api.loadModel = async (id: string) => runtime(id)
  api.unloadModel = async () => runtime(null)
}

test('a failed selection stays selected, sends to that model, and can be retried', async () => {
  reset()
  api.loadModel = async () => { throw new Error('engine unavailable') }
  await useStudio.getState().loadModel('b')
  assert.equal(chatModelId(), 'b')
  assert.equal(useStudio.getState().preferredModelId, 'b')
  assert.equal(chatModelError(useStudio.getState()), 'engine unavailable')
  assert.equal(useStudio.getState().runtime.model_id, 'a')
  let sent: any
  const previousFetch = globalThis.fetch
  globalThis.fetch = async (_url, options) => {
    sent = JSON.parse(options!.body as string)
    return new Response('data: {"choices":[{"delta":{"content":"answer"}}]}\n\ndata: {"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}\n\ndata: [DONE]\n\n', { headers: { 'content-type': 'text/event-stream' } })
  }
  try { await useStudio.getState().send('hello') } finally { globalThis.fetch = previousFetch }
  assert.equal(sent.model, 'b', 'the resident a must never silently answer for the selected b')
  api.loadModel = async (id: string) => runtime(id)
  await useStudio.getState().loadModel('b')
  assert.equal(chatModelError(useStudio.getState()), null)
})

test('new chats inherit the last choice; switching chats restores their individual model', async () => {
  reset()
  useStudio.setState({ conversations: [conversation('old', 'a'), { ...conversation('chat', 'a'), messages: [{ role: 'user', content: 'hello' }] }] })
  await useStudio.getState().loadModel('b')
  useStudio.getState().selectConversation('old')
  assert.equal(chatModelId(), 'a')
  const fresh = useStudio.getState().newConversation()
  assert.equal(chatModelId(), 'b')
  assert.equal(useStudio.getState().conversations.find((c: any) => c.id === fresh).modelId, 'b')
  await useStudio.getState().unloadModel()
  assert.equal(chatModelId(), 'b', 'freeing memory does not deselect the model')
})

test('rapid changes serialize loads and an older failure cannot clear the newest choice', async () => {
  reset()
  let rejectFirst!: (error: Error) => void
  const called: string[] = []
  api.loadModel = (id: string) => {
    called.push(id)
    return id === 'b' ? new Promise((_, reject) => { rejectFirst = reject }) : Promise.resolve(runtime(id))
  }
  const first = useStudio.getState().loadModel('b')
  await Promise.resolve()
  const second = useStudio.getState().loadModel('c')
  assert.equal(chatModelId(), 'c')
  rejectFirst(new Error('late failure'))
  await Promise.all([first, second])
  assert.deepEqual(called, ['b', 'c'])
  assert.equal(chatModelId(), 'c')
  assert.equal(useStudio.getState().runtime.model_id, 'c')
  assert.equal(useStudio.getState().loadingModelId, null)
  assert.equal(chatModelError(useStudio.getState()), null)
})

test('Stop cancels a reply waiting for model preparation without waiting for the engine', async () => {
  reset()
  let finish!: (value: unknown) => void
  api.loadModel = () => new Promise((resolve) => { finish = resolve })
  const loading = useStudio.getState().loadModel('b')
  await Promise.resolve()
  const reply = streamReply({ conversationId: 'chat', assistantId: 'reply', messages: [], modelId: 'b' })
  useStudio.getState().stop()
  assert.equal((await reply).aborted, true)
  finish(runtime('b'))
  await loading
})

test('selection survives persisted session hydration before the runtime is available', async () => {
  reset()
  await useStudio.getState().loadModel('b')
  await new Promise((resolve) => setTimeout(resolve, 550))
  const saved = storage.get('misaka-studio.session')!
  assert.equal(JSON.parse(saved).state.preferredModelId, 'b')
  useStudio.setState({ preferredModelId: null, conversations: [], activeConversationId: null, runtime: null, models: [], connected: false })
  await useStudio.persist.rehydrate()
  assert.equal(chatModelId(), 'b')
  assert.equal(useStudio.getState().preferredModelId, 'b')
})
