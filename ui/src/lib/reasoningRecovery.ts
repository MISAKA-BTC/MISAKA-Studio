import type { ChatRequest, ChatStreamEvent } from './api'

type Stream = (request: ChatRequest, signal: AbortSignal) => AsyncIterable<ChatStreamEvent>

/** Older engines may ignore the reasoning budget. Retry a reasoning-only cut once, with
 * thinking off and the original question intact. Never retry a user stop, error, or partial answer. */
export async function* withReasoningRecovery(
  stream: Stream,
  request: ChatRequest,
  signal: AbortSignal,
): AsyncGenerator<ChatStreamEvent> {
  let previousUsage: Extract<ChatStreamEvent, { type: 'done' }>['usage'] | undefined
  for (let attempt = 0; attempt < 2; attempt++) {
    signal.throwIfAborted()
    let reasoning = false
    let answer = false
    let failed = false
    let done: Extract<ChatStreamEvent, { type: 'done' }> | undefined
    for await (const event of stream(request, signal)) {
      signal.throwIfAborted()
      if (event.type === 'reasoning' && event.text.trim()) reasoning = true
      if (event.type === 'delta' && event.text.trim()) answer = true
      if (event.type === 'error') failed = true
      if (event.type === 'done') done = event
      else yield event
    }
    signal.throwIfAborted()
    if (attempt === 0 && !failed && reasoning && !answer && done?.finishReason === 'length' && request.misaka?.thinking !== false) {
      previousUsage = done.usage
      request = { ...request, misaka: { pinned: [], ...request.misaka, thinking: false } }
      continue
    }
    if (done) {
      yield {
        ...done,
        usage: previousUsage ? {
          prompt_tokens: previousUsage.prompt_tokens + done.usage.prompt_tokens,
          completion_tokens: previousUsage.completion_tokens + done.usage.completion_tokens,
          total_tokens: previousUsage.total_tokens + done.usage.total_tokens,
        } : done.usage,
      }
    }
    if (previousUsage && !answer && !failed) {
      yield { type: 'error', message: '思考をオフにして再試行しましたが、回答本文が生成されませんでした。生成上限を増やすか、別のモデルで再生成してください。' }
    }
    return
  }
}
