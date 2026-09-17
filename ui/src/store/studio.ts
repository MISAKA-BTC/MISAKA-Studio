// One store for everything the window shows.
//
// The split that matters: **conversations are the only thing persisted here.** Models, runtime
// status, downloads and metrics all live in the runtime, and caching them in localStorage would
// mean opening the app to a stale model list from three days ago. They are fetched at startup and
// kept current by SSE.
//
// Conversations are the opposite: they exist only in the window. Persisting them locally is what
// makes closing the app safe. (Moving them into the runtime is a later change — the chat history
// is not part of the API this version commits to.)

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import { api, streamChat } from '../lib/api'
import { joinContinuation } from '../lib/continuation'
import { hasRepeatedTail, historyForModel, trimRepeatedTail } from '../lib/history'
import type {
  ChatMessage,
  Conversation,
  DownloadProgress,
  MessageMining,
  MiningQueueView,
  ModelView,
  RuntimeSample,
  RuntimeStatus,
  Settings,
  SystemInfo,
  TurnStats,
  ContextReport,
} from '../lib/types'

export type View = 'chat' | 'models' | 'network' | 'monitor' | 'settings'

/** A message shown to the user about something that just happened. */
export type Toast = { id: string; kind: 'info' | 'error' | 'success'; text: string }

/** A reply buffer is rendered live but never becomes history until the request finishes. */
export type StreamingReply = {
  conversationId: string
  assistantId: string
  content: string
}

type StudioState = {
  view: View
  system: SystemInfo | null
  settings: Settings | null
  models: ModelView[]
  runtime: RuntimeStatus | null
  downloads: DownloadProgress[]
  sample: RuntimeSample | null
  connected: boolean
  loadingModelId: string | null
  toasts: Toast[]

  conversations: Conversation[]
  activeConversationId: string | null
  streaming: StreamingReply | null

  setView: (view: View) => void
  toast: (kind: Toast['kind'], text: string) => void
  dismissToast: (id: string) => void

  bootstrap: () => Promise<void>
  refreshModels: () => Promise<void>
  refreshRuntime: () => Promise<void>
  refreshDownloads: () => Promise<void>
  loadModel: (id: string, contextSize?: number) => Promise<void>
  unloadModel: () => Promise<void>
  deleteModel: (id: string) => Promise<void>
  hashModel: (id: string) => Promise<void>
  saveSettings: (settings: Settings) => Promise<void>
  setSample: (sample: RuntimeSample) => void
  setDownload: (progress: DownloadProgress) => void
  setConnected: (connected: boolean) => void

  newConversation: () => string
  selectConversation: (id: string) => void
  deleteConversation: (id: string) => void
  renameConversation: (id: string, title: string) => void
  send: (text: string) => Promise<void>
  regenerate: () => Promise<void>
  continueGeneration: () => Promise<void>
  editMessage: (messageId: string, content: string) => Promise<void>
  /** Pinned notes on the active conversation: standing facts the context manager keeps. */
  addPin: (text: string) => void
  updatePin: (index: number, text: string) => void
  removePin: (index: number) => void
  stop: () => void
  isGenerating: () => boolean

  /** The runtime's mining queue, as last read; drives the badges under user messages. */
  miningQueue: MiningQueueView | null
  refreshMining: () => Promise<void>
}

/** The in-flight generation. Outside the store: it is not state anyone renders, and it must not
 *  be serialised into localStorage. */
let inFlight: AbortController | null = null

const uid = () => Math.random().toString(36).slice(2, 10) + Date.now().toString(36)

function emptyConversation(): Conversation {
  const now = Date.now()
  return { id: uid(), title: 'New chat', createdAt: now, updatedAt: now, modelId: null, messages: [] }
}

/** A conversation's title, from its first user message. */
function deriveTitle(text: string): string {
  const clean = text.trim().replace(/\s+/g, ' ')
  return clean.length > 48 ? `${clean.slice(0, 48)}…` : clean || 'New chat'
}

export const useStudio = create<StudioState>()(
  persist(
    (set, get) => ({
      view: 'chat',
      system: null,
      settings: null,
      models: [],
      runtime: null,
      downloads: [],
      sample: null,
      connected: false,
      loadingModelId: null,
      toasts: [],
      conversations: [],
      activeConversationId: null,
      streaming: null,
      miningQueue: null,

      setView: (view) => set({ view }),

      toast: (kind, text) => {
        const toast: Toast = { id: uid(), kind, text }
        set((s) => ({ toasts: [...s.toasts, toast] }))
        // Errors stay until dismissed; the rest clear themselves. An error that vanishes before
        // it is read is an error that gets reported as "it just doesn't work".
        if (kind !== 'error') setTimeout(() => get().dismissToast(toast.id), 4000)
      },
      dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),

      bootstrap: async () => {
        try {
          const [system, settings, models, runtime, downloads] = await Promise.all([
            api.system(),
            api.settings(),
            api.models(),
            api.runtime(),
            api.downloads(),
          ])
          set({ system, settings, models, runtime, downloads, connected: true })
        } catch (error) {
          set({ connected: false })
          get().toast('error', `Cannot reach the MISAKA Runtime: ${(error as Error).message}`)
        }
      },

      refreshModels: async () => {
        try {
          set({ models: await api.refreshModels() })
        } catch (error) {
          get().toast('error', (error as Error).message)
        }
      },

      refreshRuntime: async () => {
        try {
          set({ runtime: await api.runtime(), connected: true })
        } catch {
          set({ connected: false })
        }
      },

      refreshDownloads: async () => {
        try {
          set({ downloads: await api.downloads() })
        } catch {
          /* the downloads panel is not worth a toast */
        }
      },

      loadModel: async (id, contextSize) => {
        set({ loadingModelId: id })
        try {
          const runtime = await api.loadModel(id, contextSize)
          set({ runtime })
          get().toast('success', `${id} loaded in ${((runtime.load_ms ?? 0) / 1000).toFixed(1)}s`)
          // The one thing a person cannot see from the chat: that the model is on the CPU when
          // the setting says otherwise. The note names the cause and where to fix it.
          if (runtime.offload_note) get().toast('info', runtime.offload_note)
        } catch (error) {
          get().toast('error', (error as Error).message)
        } finally {
          set({ loadingModelId: null })
        }
      },

      unloadModel: async () => {
        try {
          set({ runtime: await api.unloadModel() })
        } catch (error) {
          get().toast('error', (error as Error).message)
        }
      },

      deleteModel: async (id) => {
        try {
          await api.deleteModel(id)
          await get().refreshModels()
          await get().refreshRuntime()
          get().toast('success', `${id} deleted`)
        } catch (error) {
          get().toast('error', (error as Error).message)
        }
      },

      hashModel: async (id) => {
        try {
          const model = await api.hashModel(id)
          set((s) => ({ models: s.models.map((m) => (m.id === model.id ? model : m)) }))
          await get().refreshRuntime()
          get().toast('success', `${id} hashed — model identity available`)
        } catch (error) {
          get().toast('error', (error as Error).message)
        }
      },

      saveSettings: async (settings) => {
        try {
          const saved = await api.saveSettings(settings)
          set({ settings: saved })
          await Promise.all([get().refreshModels(), get().refreshRuntime()])
          get().toast('success', 'Settings saved')
        } catch (error) {
          get().toast('error', (error as Error).message)
        }
      },

      setSample: (sample) => set({ sample, connected: true }),
      setConnected: (connected) => set({ connected }),

      setDownload: (progress) => {
        set((s) => {
          const downloads = s.downloads.some((d) => d.id === progress.id)
            ? s.downloads.map((d) => (d.id === progress.id ? progress : d))
            : [...s.downloads, progress]
          return { downloads }
        })
        if (progress.status === 'completed') {
          get().toast('success', `${progress.model_id} downloaded`)
          void get().refreshModels()
        }
        if (progress.status === 'failed') get().toast('error', progress.error ?? `${progress.file} failed`)
      },

      newConversation: () => {
        const conversation = emptyConversation()
        set((s) => ({ conversations: [conversation, ...s.conversations], activeConversationId: conversation.id }))
        return conversation.id
      },

      selectConversation: (id) => set({ activeConversationId: id }),

      addPin: (text) => {
        const pin = text.trim()
        const id = get().activeConversationId
        if (!pin || !id) return
        set((s) => ({
          conversations: s.conversations.map((c) =>
            c.id === id && !(c.pinned ?? []).includes(pin) ? { ...c, pinned: [...(c.pinned ?? []), pin], updatedAt: Date.now() } : c,
          ),
        }))
      },

      updatePin: (index, text) => {
        const id = get().activeConversationId
        if (!id) return
        set((s) => ({
          conversations: s.conversations.map((c) =>
            c.id === id ? { ...c, pinned: (c.pinned ?? []).map((p, i) => (i === index ? text : p)).filter((p) => p.trim()) } : c,
          ),
        }))
      },

      removePin: (index) => {
        const id = get().activeConversationId
        if (!id) return
        set((s) => ({ conversations: s.conversations.map((c) => (c.id === id ? { ...c, pinned: (c.pinned ?? []).filter((_, i) => i !== index) } : c)) }))
      },

      deleteConversation: (id) =>
        set((s) => {
          const conversations = s.conversations.filter((c) => c.id !== id)
          return {
            conversations,
            activeConversationId: s.activeConversationId === id ? (conversations[0]?.id ?? null) : s.activeConversationId,
          }
        }),

      renameConversation: (id, title) =>
        set((s) => ({ conversations: s.conversations.map((c) => (c.id === id ? { ...c, title } : c)) })),

      send: async (text) => {
        const trimmed = text.trim()
        if (!trimmed) return
        let conversationId = get().activeConversationId
        if (!conversationId || !get().conversations.some((c) => c.id === conversationId)) conversationId = get().newConversation()

        const message: ChatMessage = { id: uid(), role: 'user', content: trimmed }
        set((s) => ({
          conversations: s.conversations.map((c) =>
            c.id === conversationId
              ? {
                  ...c,
                  messages: [...c.messages, message],
                  title: c.messages.length === 0 ? deriveTitle(trimmed) : c.title,
                  updatedAt: Date.now(),
                }
              : c,
          ),
        }))
        // Background mining: the prompt goes to the slot's queue as well, and the chat carries
        // on with whatever engine answers here. Only when the runtime says the queue can be fed
        // without doubling up — with the gateway as the chat engine, the chat IS the mining.
        const queue = get().miningQueue
        if (queue && queue.mode === 'background' && queue.background_available) {
          try {
            const job = await api.miningEnqueue(trimmed, conversationId, message.id)
            const mining: MessageMining = { jobId: job.id, status: job.status }
            set((s) => ({
              conversations: s.conversations.map((c) =>
                c.id === conversationId ? { ...c, messages: c.messages.map((m) => (m.id === message.id ? { ...m, mining } : m)) } : c,
              ),
            }))
          } catch (error) {
            // The chat still answers; only the mining did not queue. Said once, where it can be
            // acted on, rather than inside the message.
            get().toast('error', `not queued for mining: ${(error as Error).message}`)
          }
        }
        await runGeneration(set, get, conversationId)
      },

      refreshMining: async () => {
        try {
          const queue = await api.miningQueue()
          set({ miningQueue: queue })
          // Fold the queue's word back onto the messages that were queued from here. A message
          // whose job the queue has since trimmed keeps what it last heard.
          const byId = new Map(queue.jobs.map((j) => [j.id, j]))
          set((s) => ({
            conversations: s.conversations.map((c) => ({
              ...c,
              messages: c.messages.map((m) => {
                if (!m.mining) return m
                const job = byId.get(m.mining.jobId)
                if (!job) return m
                const mining: MessageMining = { jobId: job.id, status: job.status, claimId: job.claim_id, error: job.error, answer: job.answer }
                return mining.status === m.mining.status && mining.claimId === m.mining.claimId && mining.error === m.mining.error
                  ? m
                  : { ...m, mining }
              }),
            })),
          }))
        } catch {
          // The runtime is the source of truth; when it is unreachable the badges simply keep
          // their last state, the same way the connection dot already says so.
        }
      },

      regenerate: async () => {
        const conversationId = get().activeConversationId
        if (!conversationId) return
        const conversation = get().conversations.find((c) => c.id === conversationId)
        if (!conversation) return
        // Drop trailing assistant turns, then generate again from the same user message. Editing
        // history in place would leave two answers to one question with no way to tell which the
        // model actually produced.
        const messages = [...conversation.messages]
        while (messages.length > 0 && messages[messages.length - 1]?.role === 'assistant') messages.pop()
        if (messages.length === 0) return
        set((s) => ({ conversations: s.conversations.map((c) => (c.id === conversationId ? { ...c, messages } : c)) }))
        await runGeneration(set, get, conversationId)
      },

      continueGeneration: async () => {
        const conversationId = get().activeConversationId
        if (!conversationId) return
        const conversation = get().conversations.find((c) => c.id === conversationId)
        const last = conversation?.messages.at(-1)
        if (!conversation || !last || last.role !== 'assistant' || last.streaming || last.stats?.finishReason !== 'length') return
        // No instruction is added here. The conversation already ends with the cut-off reply, and
        // the runtime decides how that reaches the engine: as an open assistant turn the engine
        // continues (llama.cpp), or as the question plus the reply's end in one instruction that
        // fits a small class's window. Adding "please continue" as a user turn was the bug: on a
        // 512-token class the trim kept that line and dropped the question, and the model asked
        // what it was meant to continue — glued onto the answer.
        await runGeneration(set, get, conversationId, { targetAssistantId: last.id })
      },

      editMessage: async (messageId, content) => {
        const conversationId = get().activeConversationId
        if (!conversationId) return
        const conversation = get().conversations.find((c) => c.id === conversationId)
        if (!conversation) return
        const index = conversation.messages.findIndex((m) => m.id === messageId)
        if (index === -1) return
        // Everything after an edited message answered a question that no longer exists.
        const messages = conversation.messages.slice(0, index + 1).map((m) => (m.id === messageId ? { ...m, content } : m))
        set((s) => ({
          conversations: s.conversations.map((c) => (c.id === conversationId ? { ...c, messages, updatedAt: Date.now() } : c)),
        }))
        await runGeneration(set, get, conversationId)
      },

      stop: () => {
        inFlight?.abort()
        inFlight = null
      },

      isGenerating: () => {
        const id = get().activeConversationId
        const conversation = get().conversations.find((c) => c.id === id)
        return conversation?.messages.some((m) => m.streaming) ?? false
      },
    }),
    {
      name: 'misaka-studio.session',
      // Only the conversation history and the current view. Everything else is the runtime's.
      partialize: (state) => ({
        conversations: state.conversations,
        activeConversationId: state.activeConversationId,
        view: state.view,
      }),
      version: 1,
    },
  ),
)

type Setter = (partial: Partial<StudioState> | ((s: StudioState) => Partial<StudioState>)) => void
type Getter = () => StudioState

/**
 * Run one generation into `conversationId`.
 *
 * Shared by send, regenerate and edit because all three are the same operation: take the
 * conversation as it now stands, ask for the next assistant turn, stream it in.
 */
async function runGeneration(
  set: Setter,
  get: Getter,
  conversationId: string,
  options: { targetAssistantId?: string; prompt?: string } = {},
) {
  const state = get()
  const conversation = state.conversations.find((c) => c.id === conversationId)
  if (!conversation) return

  const settings = state.settings
  // The loaded model; else the one the runtime loads at startup, which may still be loading — asking
  // for the first model in the list instead started a second engine in a race with it, and a chat
  // meant for the 512-token class was answered by a 32K GGUF with the whole history behind it.
  const startupModel = settings?.load_on_start && state.models.some((m) => m.id === settings.load_on_start) ? settings.load_on_start : null
  const modelId = state.runtime?.model_id ?? startupModel ?? state.models[0]?.id
  if (!modelId) {
    state.toast('error', 'No model is available. Download one from the Models tab.')
    return
  }

  const systemPrompt = settings?.generation.system_prompt?.trim()
  // Not every turn in the window is context: failed, looping and superseded replies stay visible
  // but are not sent back — see `lib/history`.
  const history = historyForModel(conversation.messages)
  const continuation = options.prompt ? [{ role: 'user' as const, content: options.prompt }] : []
  const messages = systemPrompt ? [{ role: 'system' as const, content: systemPrompt }, ...history, ...continuation] : [...history, ...continuation]

  const assistantId = options.targetAssistantId ?? uid()
  const existing = conversation.messages.find((m) => m.id === assistantId)
  const baseContent = existing?.content ?? ''
  set({ streaming: { conversationId, assistantId, content: baseContent } })

  const updateStream = (content: string) =>
    set((s) =>
      s.streaming?.conversationId === conversationId && s.streaming.assistantId === assistantId
        ? { streaming: { ...s.streaming, content } }
        : {},
    )

  const commit = (patch: { error?: string; stats?: TurnStats; context?: ContextReport } = {}) =>
    set((s) => {
      const stream = s.streaming
      // A stale SSE completion must not overwrite a reply started afterwards.
      if (!stream || stream.conversationId !== conversationId || stream.assistantId !== assistantId) return {}
      const answer: ChatMessage = { id: assistantId, role: 'assistant', content: stream.content, ...patch }
      return {
        streaming: null,
        conversations: s.conversations.map((c) => {
          if (c.id !== conversationId) return c
          const present = c.messages.some((m) => m.id === assistantId)
          return {
            ...c,
            messages: present ? c.messages.map((m) => (m.id === assistantId ? answer : m)) : [...c.messages, answer],
            modelId,
            updatedAt: Date.now(),
          }
        }),
      }
    })

  // "Regenerate" only helps where sampling can differ. The mining lane and the integer runtime
  // decode greedily by construction — the same prompt gives the same answer, and telling someone
  // to try again there sends them round in a circle they can see for themselves.
  // Which engine answered is the context report's to say: the window's idea of the runtime can be a
  // load behind.
  const repetitionNoteFor = (lane: string | undefined) =>
    lane === 'gateway' || lane === 'misaka'
      ? '同じ文章の反復を検出したため、この表示を停止しました。この経路（マイニング用の整数ランタイム）は決定論的で、同じ質問には同じ答えが返ります。質問を言い換えるか、Settings → Backend で別のエンジンを選んでください。'
      : '同じ文章の反復を検出したため、この表示を停止しました。再生成すると別の回答を試せます。'

  const controller = new AbortController()
  inFlight = controller
  const startedAt = performance.now()
  let firstTokenAt: number | null = null
  let added = ''
  let text = baseContent
  let stoppedForRepetition = false
  let context: ContextReport | undefined
  let streamError: string | undefined
  let stats: TurnStats | undefined

  try {
    const generator = streamChat(
      {
        model: modelId,
        messages,
        temperature: settings?.generation.temperature,
        top_p: settings?.generation.top_p,
        top_k: settings?.generation.top_k,
        min_p: settings?.generation.min_p,
        repeat_penalty: settings?.generation.repeat_penalty,
        max_tokens: settings?.generation.max_tokens,
        seed: settings?.generation.seed ?? null,
        misaka: { pinned: conversation.pinned ?? [] },
      },
      controller.signal,
    )

    for await (const event of generator) {
      if (event.type === 'context') {
        context = event.report
      } else if (event.type === 'delta') {
        if (firstTokenAt === null) firstTokenAt = performance.now()
        added += event.text
        text = baseContent ? joinContinuation(baseContent, added) : added
        updateStream(text)
        if (hasRepeatedTail(text)) {
          stoppedForRepetition = true
          controller.abort()
          break
        }
      } else if (event.type === 'error') {
        streamError = event.message
      } else {
        const elapsed = performance.now() - startedAt
        // The runtime's token counts, the window's clock. Neither knows both halves: the engine
        // counts tokens, and only the client knows when the user's request started.
        stats = {
          completionTokens: event.usage.completion_tokens,
          promptTokens: event.usage.prompt_tokens,
          tokensPerSecond: elapsed > 0 ? (event.usage.completion_tokens * 1000) / elapsed : 0,
          timeToFirstTokenMs: firstTokenAt === null ? null : firstTokenAt - startedAt,
          model: modelId,
          finishReason: event.finishReason,
        }
      }
    }
    if (stoppedForRepetition) {
      // The reply is kept as far as it got, without the copy of itself it was stopped in.
      updateStream(trimRepeatedTail(text) ?? text)
      commit({ error: repetitionNoteFor(context?.backend ?? get().runtime?.backend), context })
    } else {
      commit({ error: streamError, stats, context })
    }
  } catch (error) {
    if (stoppedForRepetition) {
      updateStream(trimRepeatedTail(text) ?? text)
      commit({ error: repetitionNoteFor(context?.backend ?? get().runtime?.backend), context })
    } else if ((error as Error).name === 'AbortError') {
      // A stopped generation keeps what it produced: the user asked it to stop, not to undo.
      commit({ context })
    } else {
      commit({ error: (error as Error).message })
      get().toast('error', (error as Error).message)
    }
  } finally {
    if (inFlight === controller) inFlight = null
    void get().refreshRuntime()
  }
}
