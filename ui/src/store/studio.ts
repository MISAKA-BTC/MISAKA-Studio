// One store for everything the window shows.
//
// Conversations and the preferred model are persisted here. Models, runtime
// status, downloads and metrics all live in the runtime, and caching them in localStorage would
// mean opening the app to a stale model list from three days ago. They are fetched at startup and
// kept current by SSE.
//
// Conversations are the opposite: they exist only in the window. Persisting them locally is what
// makes closing the app safe. (Moving them into the runtime is a later change — the chat history
// is not part of the API this version commits to.)

import { create } from 'zustand'
import { createJSONStorage, persist, type PersistStorage, type StorageValue } from 'zustand/middleware'
import { api, streamChat, type ChatRequest } from '../lib/api'
import { joinContinuation } from '../lib/continuation'
import { splitThinking } from '../lib/reasoning'
import { withReasoningRecovery } from '../lib/reasoningRecovery'
import { defaultModelId } from '../lib/modelSelection'
import { hasRepeatedTail, historyForModel, trimRepeatedTail } from '../lib/history'
import { useUi } from './ui'
import type {
  ApprovalPolicy,
  Attachment,
  ChatMessage,
  Conversation,
  ConversationMode,
  DownloadProgress,
  ImageGeneration,
  ImageModelView,
  ImageRuntimeStatus,
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

export type View = 'chat' | 'models' | 'image' | 'network' | 'monitor' | 'settings'

/** A message shown to the user about something that just happened. */
export type Toast = { id: string; kind: 'info' | 'error' | 'success'; text: string }

/** A reply buffer is rendered live but never becomes history until the request finishes. */
export type StreamingReply = {
  conversationId: string
  assistantId: string
  content: string
  /** A thinking model's reasoning so far, when the engine streams it apart from the answer. */
  reasoning?: string
  /** `performance.now()` when the request was sent, for the live "thinking… 12s" count. */
  startedAt?: number
  /** `performance.now()` when the answer's first token arrived — the end of the thinking. */
  answerStartedAt?: number
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
  /** The default for new chats; a choice, independent of engine residency. */
  preferredModelId: string | null
  modelLoadErrors: Record<string, string>
  toasts: Toast[]

  imageModels: ImageModelView[]
  imageRuntime: ImageRuntimeStatus | null
  loadingImageModelId: string | null
  generatingImage: boolean
  /** Generations run this session — not persisted, the same treatment as `models`/`runtime`. */
  imageGenerations: ImageGeneration[]

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

  refreshImageModels: () => Promise<void>
  refreshImageRuntime: () => Promise<void>
  loadImageModel: (id: string, dtype?: 'fp16' | 'bf16' | 'fp32') => Promise<void>
  unloadImageModel: () => Promise<void>
  generateImage: (request: {
    prompt: string
    negativePrompt?: string
    width?: number
    height?: number
    steps?: number
    guidanceScale?: number
    seed?: number | null
    referenceImages?: string[]
    n?: number
  }) => Promise<void>
  setSample: (sample: RuntimeSample) => void
  setDownload: (progress: DownloadProgress) => void
  setConnected: (connected: boolean) => void

  newConversation: (options?: { mode?: ConversationMode; workspace?: string | null }) => string
  selectConversation: (id: string) => void
  deleteConversation: (id: string) => void
  renameConversation: (id: string, title: string) => void
  /** Any other change to one conversation — the agent's bookkeeping goes through here. */
  updateConversation: (id: string, patch: Partial<Conversation> | ((c: Conversation) => Partial<Conversation>)) => void
  starConversation: (id: string, starred: boolean) => void
  setConversationMode: (id: string, mode: ConversationMode) => void
  setConversationWorkspace: (id: string, workspace: string | null) => void
  setConversationApproval: (id: string, approval: ApprovalPolicy) => void
  /** Whether this conversation's prompts go to the mining queue when background mining is on. */
  setConversationMining: (id: string, mine: boolean) => void
  /** Empty the transcript but keep the conversation, its settings and its pinned notes. */
  clearConversation: (id: string) => void
  send: (text: string, attachments?: Attachment[]) => Promise<void>
  /** Queue a prompt for background mining and tie the job to the message that carries it. A no-op
   *  unless the runtime's queue is in background mode and this conversation has mining on. */
  enqueueMining: (conversationId: string, messageId: string, prompt: string) => Promise<void>
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
let latestModelLoad = 0
let modelLoadQueue: Promise<void> = Promise.resolve()

const uid = () => Math.random().toString(36).slice(2, 10) + Date.now().toString(36)

export { uid }

function emptyConversation(options: { mode?: ConversationMode; workspace?: string | null } = {}): Conversation {
  const now = Date.now()
  const agent = options.mode === 'agent'
  return {
    id: uid(),
    title: agent ? 'New session' : 'New chat',
    createdAt: now,
    updatedAt: now,
    modelId: null,
    messages: [],
    ...(agent ? { mode: 'agent' as const, workspace: options.workspace ?? null, approval: 'auto_edit' as const } : {}),
  }
}

/** A conversation's title, from its first user message. */
function deriveTitle(text: string): string {
  const clean = text.trim().replace(/\s+/g, ' ')
  return clean.length > 48 ? `${clean.slice(0, 48)}…` : clean || 'New chat'
}

type Persisted = Pick<StudioState, 'conversations' | 'activeConversationId' | 'view' | 'preferredModelId'>

/** The persisted session's key in localStorage. */
const SESSION_KEY = 'misaka-studio.session'

/**
 * localStorage, written when what is persisted changed — and then at most twice a second.
 *
 * The persist middleware writes on every change to the store, and a streamed reply changes it on
 * every token. The live overlay is not persisted, so nearly all of those writes wrote the same
 * bytes again — the whole history, which in an agent thread holds file contents, dozens of times a
 * second. A write is skipped when nothing persisted changed, delayed when something did, and
 * flushed when the window is hidden or closed, so a reload loses nothing.
 */
function sessionStorage(): PersistStorage<Persisted> | undefined {
  const json = createJSONStorage<Persisted>(() => localStorage)
  if (!json) return undefined
  let last: Persisted | null = null
  let pending: { name: string; value: StorageValue<Persisted> } | null = null
  let timer: ReturnType<typeof setTimeout> | null = null
  const flush = () => {
    if (timer) clearTimeout(timer)
    timer = null
    if (!pending) return
    const { name, value } = pending
    pending = null
    try {
      void json.setItem(name, value)
    } catch (error) {
      // Over quota, most likely. The window keeps working; what was not saved is what a reload
      // would lose, which is better said in the console than by refusing to stream.
      console.warn('MISAKA Studio could not save the conversations:', error)
    }
  }
  if (typeof window !== 'undefined') {
    window.addEventListener('pagehide', flush)
    document.addEventListener('visibilitychange', () => {
      if (document.visibilityState === 'hidden') flush()
    })
  }
  return {
    getItem: (name) => json.getItem(name),
    removeItem: (name) => json.removeItem(name),
    setItem: (name, value) => {
      const state = value.state
      if (last && last.conversations === state.conversations && last.activeConversationId === state.activeConversationId && last.view === state.view && last.preferredModelId === state.preferredModelId) {
        return
      }
      last = state
      pending = { name, value }
      if (!timer) timer = setTimeout(flush, 500)
    },
  }
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
      preferredModelId: null,
      modelLoadErrors: {},
      toasts: [],
      imageModels: [],
      imageRuntime: null,
      loadingImageModelId: null,
      generatingImage: false,
      imageGenerations: [],
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
          set({ preferredModelId: get().preferredModelId ?? defaultModelId(get()) })
        } catch (error) {
          set({ connected: false })
          get().toast('error', `Cannot reach the MISAKA Runtime: ${(error as Error).message}`)
        }
        // Fetched separately from the core bootstrap: an image backend is opt-in and not
        // configuring one must never be the reason chat fails to open.
        void get().refreshImageModels()
        void get().refreshImageRuntime()
      },

      refreshModels: async () => {
        try {
          set({ models: await api.refreshModels() })
          if (!get().preferredModelId) set({ preferredModelId: defaultModelId(get()) })
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
        if (get().streaming) {
          get().toast('info', '回答が終わるか停止してからモデルを変更してください。')
          return
        }
        const token = ++latestModelLoad
        // Commit the choice before touching the engine. A failed load must never undo it.
        set((s) => ({
          preferredModelId: id,
          loadingModelId: id,
          modelLoadErrors: { ...s.modelLoadErrors, [id]: '' },
          conversations: s.conversations.map((c) => c.id === s.activeConversationId ? { ...c, modelId: id } : c),
        }))
        const load = async () => {
          if (token !== latestModelLoad) return
          try {
            const runtime = await api.loadModel(id, contextSize)
            if (token !== latestModelLoad) return
            set({ runtime })
            if (runtime.offload_note) get().toast('info', runtime.offload_note)
          } catch (error) {
            if (token !== latestModelLoad) return
            set((s) => ({ modelLoadErrors: { ...s.modelLoadErrors, [id]: (error as Error).message } }))
            await get().refreshRuntime()
          } finally {
            if (token === latestModelLoad) set({ loadingModelId: null })
          }
        }
        // Rapid selections cannot let an older load complete after the latest one.
        modelLoadQueue = modelLoadQueue.then(load, load)
        await modelLoadQueue
      },

      unloadModel: async () => {
        if (get().streaming || get().loadingModelId) return
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

      refreshImageModels: async () => {
        try {
          set({ imageModels: await api.imageModels() })
        } catch {
          /* the panel shows its own "no image backend" state; this is not worth a toast */
        }
      },

      refreshImageRuntime: async () => {
        try {
          set({ imageRuntime: await api.imageRuntime() })
        } catch {
          /* same as above */
        }
      },

      loadImageModel: async (id, dtype) => {
        set({ loadingImageModelId: id })
        try {
          const imageRuntime = await api.loadImageModel(id, dtype)
          set({ imageRuntime })
          get().toast('success', `${id} loaded in ${((imageRuntime.load_ms ?? 0) / 1000).toFixed(1)}s`)
        } catch (error) {
          get().toast('error', (error as Error).message)
        } finally {
          set({ loadingImageModelId: null })
        }
      },

      unloadImageModel: async () => {
        try {
          set({ imageRuntime: await api.unloadImageModel() })
        } catch (error) {
          get().toast('error', (error as Error).message)
        }
      },

      generateImage: async (request) => {
        set({ generatingImage: true })
        try {
          const response = await api.generateImage({
            prompt: request.prompt,
            negative_prompt: request.negativePrompt ?? null,
            width: request.width,
            height: request.height,
            steps: request.steps,
            guidance_scale: request.guidanceScale,
            seed: request.seed ?? null,
            reference_images: request.referenceImages ?? [],
            n: request.n,
          })
          const generation: ImageGeneration = {
            id: uid(),
            prompt: request.prompt,
            modelId: get().imageRuntime?.model_id ?? null,
            images: response.images,
            createdAt: Date.now(),
          }
          set((s) => ({ imageGenerations: [generation, ...s.imageGenerations] }))
        } catch (error) {
          get().toast('error', (error as Error).message)
        } finally {
          set({ generatingImage: false })
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

      newConversation: (options) => {
        // An untouched empty conversation is reused rather than stacked: pressing "new chat" three
        // times should not leave three "New chat" rows behind.
        const fresh = get().conversations.find(
          (c) => c.messages.length === 0 && (c.mode ?? 'chat') === (options?.mode ?? 'chat') && !c.starred,
        )
        if (fresh) {
          const patch = { modelId: defaultModelId(get()), ...(options?.mode === 'agent' && options.workspace !== undefined ? { workspace: options.workspace } : {}) }
          set((s) => ({
            conversations: s.conversations.map((c) => (c.id === fresh.id ? { ...c, ...patch, updatedAt: Date.now() } : c)),
            activeConversationId: fresh.id,
          }))
          return fresh.id
        }
        const conversation = emptyConversation(options)
        conversation.modelId = defaultModelId(get())
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
        set((s) => ({ conversations: s.conversations.map((c) => (c.id === id ? { ...c, title: title.trim() || c.title } : c)) })),

      updateConversation: (id, patch) =>
        set((s) => ({
          conversations: s.conversations.map((c) => (c.id === id ? { ...c, ...(typeof patch === 'function' ? patch(c) : patch) } : c)),
        })),

      starConversation: (id, starred) => get().updateConversation(id, { starred }),
      setConversationMode: (id, mode) =>
        get().updateConversation(id, (c) => ({
          mode,
          approval: c.approval ?? 'auto_edit',
          title: c.messages.length === 0 ? (mode === 'agent' ? 'New session' : 'New chat') : c.title,
        })),
      setConversationWorkspace: (id, workspace) => get().updateConversation(id, { workspace, updatedAt: Date.now() }),
      setConversationApproval: (id, approval) => get().updateConversation(id, { approval }),
      setConversationMining: (id, mine) => get().updateConversation(id, { mine }),
      clearConversation: (id) => {
        if (get().streaming?.conversationId === id) get().stop()
        get().updateConversation(id, { messages: [], changes: [], updatedAt: Date.now() })
      },

      enqueueMining: async (conversationId, messageId, prompt) => {
        const queue = get().miningQueue
        const conversation = get().conversations.find((c) => c.id === conversationId)
        if (!queue || queue.mode !== 'background' || !queue.background_available || conversation?.mine === false) return
        try {
          const job = await api.miningEnqueue(prompt, conversationId, messageId)
          const mining: MessageMining = { jobId: job.id, status: job.status }
          set((s) => ({
            conversations: s.conversations.map((c) =>
              c.id === conversationId ? { ...c, messages: c.messages.map((m) => (m.id === messageId ? { ...m, mining } : m)) } : c,
            ),
          }))
        } catch (error) {
          // The chat still answers; only the mining did not queue. Said once, where it can be
          // acted on, rather than inside the message.
          get().toast('error', `not queued for mining: ${(error as Error).message}`)
        }
      },

      send: async (text, attachments) => {
        const trimmed = text.trim()
        if (!trimmed && !attachments?.length) return
        let conversationId = get().activeConversationId
        if (!conversationId || !get().conversations.some((c) => c.id === conversationId)) conversationId = get().newConversation()

        const message: ChatMessage = {
          id: uid(),
          role: 'user',
          content: trimmed,
          createdAt: Date.now(),
          ...(attachments?.length ? { attachments } : {}),
        }
        const title = deriveTitle(trimmed || attachments?.map((a) => a.name).join(', ') || '')
        set((s) => ({
          conversations: s.conversations.map((c) =>
            c.id === conversationId
              ? {
                  ...c,
                  messages: [...c.messages, message],
                  title: c.messages.length === 0 ? title : c.title,
                  updatedAt: Date.now(),
                }
              : c,
          ),
        }))
        // Background mining: the prompt goes to this Studio's mining queue as well, and the chat carries
        // on with whatever engine answers here. Only when the runtime says the queue can be fed
        // without doubling up — with the gateway as the chat engine, the chat IS the mining. What is
        // mined is what the person typed: an attachment's file content stays on this machine.
        if (trimmed) await get().enqueueMining(conversationId, message.id, trimmed)
        await runGeneration(set, get, conversationId)
      },

      refreshMining: async () => {
        try {
          const queue = await api.miningQueue()
          set({ miningQueue: queue })
          // Fold the queue's word back onto the messages that were queued from here. A message
          // whose job the queue has since trimmed keeps what it last heard.
          // Only what changed is replaced: the poll runs every few seconds, and a fresh object for
          // every conversation each time redrew everything that reads them.
          const byId = new Map(queue.jobs.map((j) => [j.id, j]))
          set((s) => {
            let changedAny = false
            const conversations = s.conversations.map((c) => {
              let changed = false
              const messages = c.messages.map((m) => {
                if (!m.mining) return m
                const job = byId.get(m.mining.jobId)
                if (!job) return m
                const mining: MessageMining = { jobId: job.id, status: job.status, claimId: job.claim_id, error: job.error, answer: job.answer }
                if (
                  mining.status === m.mining.status &&
                  mining.claimId === m.mining.claimId &&
                  mining.error === m.mining.error &&
                  mining.answer === m.mining.answer
                )
                  return m
                changed = true
                return { ...m, mining }
              })
              if (!changed) return c
              changedAny = true
              return { ...c, messages }
            })
            return changedAny ? { conversations } : {}
          })
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
        await runGeneration(set, get, conversationId, {
          targetAssistantId: last.id,
          thinking: splitThinking(last.content).answer.trim() ? undefined : false,
        })
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

      // The live reply is the overlay, not a flag on a message: nothing in history streams.
      isGenerating: () => {
        const stream = get().streaming
        return stream !== null && stream.conversationId === get().activeConversationId
      },
    }),
    {
      name: SESSION_KEY,
      storage: sessionStorage(),
      // The conversation history, model choice and current view. Engine residency is transient.
      partialize: (state): Persisted => ({
        conversations: state.conversations,
        activeConversationId: state.activeConversationId,
        view: state.view,
        preferredModelId: state.preferredModelId,
      }),
      // 2: conversations gained optional fields (mode, workspace, attachments, tool calls…).
      // Every one is optional, so a version-1 session loads as it is.
      version: 2,
      migrate: (persisted) => persisted as StudioState,
    },
  ),
)

type Setter = (partial: Partial<StudioState> | ((s: StudioState) => Partial<StudioState>)) => void
type Getter = () => StudioState

/** The same selection drives the picker and the request, even if another model is resident. */
export function chatModelId(state: StudioState = useStudio.getState(), conversationId = state.activeConversationId): string | null {
  return state.conversations.find((c) => c.id === conversationId)?.modelId ?? defaultModelId(state)
}

export function chatModelError(state: StudioState): string | null {
  const id = chatModelId(state)
  if (!id) return null
  if (state.connected && !state.models.some((m) => m.id === id)) return '選択したモデルが見つかりません。再インストールするか、別のモデルを選択してください。'
  const model = state.models.find((m) => m.id === id)
  if (model?.fit.verdict === 'does_not_fit') return model.fit_summary
  return state.modelLoadErrors[id] || null
}

/** Stop also works while a previously requested model load is still pending. */
async function waitForModelLoad(signal: AbortSignal) {
  signal.throwIfAborted()
  let onAbort: () => void = () => {}
  try {
    await Promise.race([
      modelLoadQueue,
      new Promise<never>((_, reject) => {
        onAbort = () => reject(signal.reason)
        signal.addEventListener('abort', onAbort, { once: true })
      }),
    ])
  } finally {
    signal.removeEventListener('abort', onAbort)
  }
}

/** What one streamed reply came to. The caller decides what becomes of it in the conversation. */
export type ReplyOutcome = {
  content: string
  reasoning?: string
  reasoningMs?: number
  stats?: TurnStats
  context?: ContextReport
  /** The lane's or the stream's error, or the note on a reply stopped for repeating itself. */
  error?: string
  /** The person pressed Stop. What arrived before that is in `content`. */
  aborted: boolean
  stoppedForRepetition: boolean
  /** The request itself failed — refused, or the connection broke — rather than the lane
   *  reporting an error inside a reply that arrived. */
  failed?: boolean
}

/**
 * Stream one assistant reply into the live overlay (`streaming`) and return it.
 *
 * Shared by the chat (`runGeneration`, below) and the coding agent (`lib/agent`), because both are
 * the same operation underneath — send these messages, show the reply as it arrives, stop when
 * asked — and two copies of the stop, stats and repetition handling would drift. It never writes
 * the conversation: the overlay is render-only, and the caller commits the reply exactly once.
 */
export async function streamReply(options: {
  conversationId: string
  assistantId: string
  messages: ChatRequest['messages']
  modelId: string
  pinned?: string[]
  /** Text already in the reply this one continues (a cut-off answer). */
  baseContent?: string
  /** Override the configured ceiling — the agent asks for room to write a whole file. */
  maxTokens?: number
  /** Per-request override for recovering a reply cut off before its answer. */
  thinking?: boolean
  /** Stop sequences, e.g. the agent's `</tool_call>`. The engine leaves them out of the text. */
  stop?: string[]
  /** Whether the reply so far is looping, checked after every token. The chat's rule by default;
   *  the agent exempts the inside of a tool call, where a file of repetitive code is not a loop. */
  isLooping?: (text: string) => boolean
}): Promise<ReplyOutcome> {
  const { conversationId, assistantId, messages, modelId } = options
  const get = useStudio.getState
  const set = useStudio.setState
  const settings = get().settings
  const baseContent = options.baseContent ?? ''
  const startedAt = performance.now()
  set({ streaming: { conversationId, assistantId, content: baseContent, startedAt } })

  const updateStream = (patch: Partial<StreamingReply>) =>
    set((s) =>
      s.streaming?.conversationId === conversationId && s.streaming.assistantId === assistantId ? { streaming: { ...s.streaming, ...patch } } : {},
    )

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
  let firstTokenAt: number | null = null
  let firstReasoningAt: number | null = null
  let added = ''
  let text = baseContent
  let reasoning = ''
  let stoppedForRepetition = false
  let context: ContextReport | undefined
  let streamError: string | undefined
  let stats: TurnStats | undefined
  const thinking = options.thinking ?? useUi.getState().thinking

  const outcome = (patch: Partial<ReplyOutcome> = {}): ReplyOutcome => ({
    content: text,
    ...(reasoning ? { reasoning, reasoningMs: firstReasoningAt !== null ? (firstTokenAt ?? performance.now()) - firstReasoningAt : undefined } : {}),
    stats,
    context,
    aborted: false,
    stoppedForRepetition,
    ...patch,
  })

  try {
    // A send made while the selected model is loading waits for that load, rather than racing it.
    await waitForModelLoad(controller.signal)
    controller.signal.throwIfAborted()
    const generator = withReasoningRecovery(
      streamChat,
      {
        model: modelId,
        messages,
        temperature: settings?.generation.temperature,
        top_p: settings?.generation.top_p,
        top_k: settings?.generation.top_k,
        min_p: settings?.generation.min_p,
        repeat_penalty: settings?.generation.repeat_penalty,
        max_tokens: options.maxTokens ?? settings?.generation.max_tokens,
        seed: settings?.generation.seed ?? null,
        ...(options.stop?.length ? { stop: options.stop } : {}),
        // `thinking` is only sent when it is off: a runtime that predates the field ignores it
        // either way, and "on" is every engine's default.
        misaka: { pinned: options.pinned ?? [], ...(thinking ? {} : { thinking: false }) },
      },
      controller.signal,
    )

    for await (const event of generator) {
      if (event.type === 'context') {
        context = event.report
        if (get().modelLoadErrors[modelId]) set((s) => ({ modelLoadErrors: { ...s.modelLoadErrors, [modelId]: '' } }))
      } else if (event.type === 'reasoning') {
        if (firstReasoningAt === null) firstReasoningAt = performance.now()
        reasoning += event.text
        updateStream({ reasoning })
      } else if (event.type === 'delta') {
        if (firstTokenAt === null) {
          firstTokenAt = performance.now()
          updateStream({ answerStartedAt: firstTokenAt })
        }
        added += event.text
        text = baseContent ? joinContinuation(baseContent, added) : added
        updateStream({ content: text })
        if ((options.isLooping ?? hasRepeatedTail)(text)) {
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
      text = trimRepeatedTail(text) ?? text
      updateStream({ content: text })
      return outcome({ error: repetitionNoteFor(context?.backend ?? get().runtime?.backend) })
    }
    return outcome({ error: streamError })
  } catch (error) {
    if (stoppedForRepetition) {
      text = trimRepeatedTail(text) ?? text
      updateStream({ content: text })
      return outcome({ error: repetitionNoteFor(context?.backend ?? get().runtime?.backend) })
    }
    // A stopped generation keeps what it produced: the user asked it to stop, not to undo.
    if ((error as Error).name === 'AbortError') return outcome({ aborted: true })
    return outcome({ error: (error as Error).message, failed: true })
  } finally {
    if (inFlight === controller) inFlight = null
    void get().refreshRuntime()
  }
}

/** Clear the live overlay if it still belongs to this reply. */
export function clearStream(conversationId: string, assistantId: string) {
  useStudio.setState((s) =>
    s.streaming?.conversationId === conversationId && s.streaming.assistantId === assistantId ? { streaming: null } : {},
  )
}

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
  options: { targetAssistantId?: string; prompt?: string; thinking?: boolean } = {},
) {
  const state = get()
  const conversation = state.conversations.find((c) => c.id === conversationId)
  if (!conversation) return

  const modelId = chatModelId(state, conversationId)
  if (!modelId) {
    state.toast('error', 'No model is available. Download one from the Models tab.')
    return
  }

  const systemPrompt = state.settings?.generation.system_prompt?.trim()
  // Not every turn in the window is context: failed, looping and superseded replies stay visible
  // but are not sent back — see `lib/history`.
  const history = historyForModel(conversation.messages)
  const continuation = options.prompt ? [{ role: 'user' as const, content: options.prompt }] : []
  const messages = systemPrompt ? [{ role: 'system' as const, content: systemPrompt }, ...history, ...continuation] : [...history, ...continuation]

  const assistantId = options.targetAssistantId ?? uid()
  get().updateConversation(conversationId, { modelId })
  const existing = conversation.messages.find((m) => m.id === assistantId)

  const result = await streamReply({
    conversationId,
    assistantId,
    messages,
    modelId,
    pinned: conversation.pinned ?? [],
    baseContent: existing ? splitThinking(existing.content).answer : '',
    thinking: options.thinking,
  })

  set((s) => {
    const stream = s.streaming
    // A stale SSE completion must not overwrite a reply started afterwards.
    if (!stream || stream.conversationId !== conversationId || stream.assistantId !== assistantId) return {}
    const answer: ChatMessage = {
      id: assistantId,
      role: 'assistant',
      content: result.content,
      createdAt: existing?.createdAt ?? Date.now(),
      ...(result.reasoning ? { reasoning: (existing?.reasoning ? existing.reasoning + '\n\n' : '') + result.reasoning, reasoningMs: result.reasoningMs } : existing?.reasoning ? { reasoning: existing.reasoning, reasoningMs: existing.reasoningMs } : {}),
      ...(result.error ? { error: result.error } : {}),
      ...(result.stats ? { stats: result.stats } : {}),
      ...(result.context ? { context: result.context } : {}),
    }
    return {
      streaming: null,
      conversations: s.conversations.map((c) => {
        if (c.id !== conversationId) return c
        const present = c.messages.some((m) => m.id === assistantId)
        return {
          ...c,
          messages: present ? c.messages.map((m) => (m.id === assistantId ? answer : m)) : [...c.messages, answer],
          // A reply never chooses the model for the next request.
          modelId: c.modelId ?? modelId,
          updatedAt: Date.now(),
        }
      }),
    }
  })
  // A request that failed outright — a dropped connection, a refused request — is said once more
  // where it cannot be missed. The lane's own error inside a reply stays on the reply.
  if (result.failed && result.error) get().toast('error', result.error)
}
