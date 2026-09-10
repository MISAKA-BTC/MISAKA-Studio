// One store for everything the window shows.
//
// The split that matters: **conversations are the only thing cached here.** Models, runtime
// status, downloads and metrics all live in the runtime, and caching them in localStorage would
// mean opening the app to a stale model list from three days ago. They are fetched at startup and
// kept current by SSE.
//
// Conversations used to be the opposite — they existed only in the window. Since ADR-0096
// Decision 12 they are the runtime's (`/api/v1/conversations`: a JSON file per conversation under
// the data directory, in this store's own shape), and localStorage is the cache that lets the
// window open before the runtime answers and keep working when it does not. The runtime is the
// source of truth; the cache is what this window last knew. Every change here goes to the runtime
// too, a little late (300 ms) so a streamed answer is one write and not one per token.

import { create } from 'zustand'
import { persist } from 'zustand/middleware'
import { api, ApiError, streamChat } from '../lib/api'
import type {
  ChatMessage,
  Conversation,
  ConversationImportReport,
  ConversationSummary,
  DownloadProgress,
  MessageMining,
  MiningQueueView,
  ModelView,
  RuntimeSample,
  RuntimeStatus,
  Settings,
  SystemInfo,
} from '../lib/types'

export type View = 'chat' | 'models' | 'network' | 'monitor' | 'components' | 'settings'

/** A message shown to the user about something that just happened. */
export type Toast = { id: string; kind: 'info' | 'error' | 'success'; text: string }

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

  setView: (view: View) => void
  /**
   * A class card the Network tab should scroll to once it has rendered — set by a link from
   * another view (the Components page's artifact rows). The Network tab reads it after its
   * overview arrives, scrolls, and clears it; it is never persisted.
   */
  classFocus: string | null
  focusClass: (name: string | null) => void
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
  editMessage: (messageId: string, content: string) => Promise<void>
  stop: () => void
  isGenerating: () => boolean

  /** Pull what the runtime holds and push what only this window has (ADR-0096 Decision 12). */
  syncConversations: () => Promise<void>
  /** Hand a file's conversations to the runtime, toast its report, and re-sync. */
  importConversations: (body: unknown) => Promise<ConversationImportReport | null>

  /** The runtime's mining queue, as last read; drives the badges under user messages. */
  miningQueue: MiningQueueView | null
  refreshMining: () => Promise<void>
}

/** The in-flight generation. Outside the store: it is not state anyone renders, and it must not
 *  be serialised into localStorage. */
let inFlight: AbortController | null = null

/**
 * Set once the window's cache has been handed to the runtime — Decision 12's "on first run after
 * the change the cache is imported once". Never re-imported: a second import would be a second
 * copy of every conversation, and the runtime cannot tell a duplicate from a twin.
 */
const MIGRATED_KEY = 'misaka-studio.conversations-migrated'

/**
 * Saves waiting to go to the runtime, one timer per conversation. Streaming updates the store per
 * token, and a PUT per token would be a write storm against a JSON file; so a save waits 300 ms
 * for the next change and only the last one goes. A conversation still streaming is skipped at
 * flush time — the end of the stream schedules its own save — so the runtime never holds a
 * placeholder marked `streaming: true`.
 */
const pendingSaves = new Map<string, ReturnType<typeof setTimeout>>()
const SAVE_DEBOUNCE_MS = 300

/**
 * One toast when the runtime's conversation API does not answer. The store keeps working from
 * the cache exactly as it did before the API existed; saying so once, dismissably, is the
 * difference between "offline" and a notification per keystroke.
 */
let conversationApiWarned = false

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
      miningQueue: null,

      setView: (view) => set({ view }),
      classFocus: null,
      focusClass: (name) => set({ classFocus: name }),

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
          // No runtime, no conversation API: the cache is the whole store, as it was before.
          return
        }
        await get().syncConversations()
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
        scheduleSave(get, conversation.id)
        return conversation.id
      },

      selectConversation: (id) => set({ activeConversationId: id }),

      deleteConversation: (id) => {
        // A save still waiting for this id would re-create what was just deleted.
        cancelSave(id)
        set((s) => {
          const conversations = s.conversations.filter((c) => c.id !== id)
          return {
            conversations,
            activeConversationId: s.activeConversationId === id ? (conversations[0]?.id ?? null) : s.activeConversationId,
          }
        })
        // A chat the runtime never held — deleted inside its first 300 ms, or made offline — is
        // refused by name (400), and that is the outcome asked for, not an outage.
        api.deleteConversation(id).catch((error: unknown) => {
          if (!isUnknownConversation(error)) warnConversationApi(get, error)
        })
      },

      renameConversation: (id, title) => {
        // A rename is an update: the timestamp is what tells a merge which copy is newer.
        set((s) => ({ conversations: s.conversations.map((c) => (c.id === id ? { ...c, title, updatedAt: Date.now() } : c)) }))
        scheduleSave(get, id)
      },

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
        scheduleSave(get, conversationId)
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
          // whose job the queue has since trimmed keeps what it last heard. Only a conversation
          // that actually changed becomes a new object — and only those go to the runtime.
          const byId = new Map(queue.jobs.map((j) => [j.id, j]))
          const changed: string[] = []
          set((s) => ({
            conversations: s.conversations.map((c) => {
              let touched = false
              const messages = c.messages.map((m) => {
                if (!m.mining) return m
                const job = byId.get(m.mining.jobId)
                if (!job) return m
                const mining: MessageMining = { jobId: job.id, status: job.status, claimId: job.claim_id, error: job.error, answer: job.answer }
                if (mining.status === m.mining.status && mining.claimId === m.mining.claimId && mining.error === m.mining.error) return m
                touched = true
                return { ...m, mining }
              })
              if (!touched) return c
              changed.push(c.id)
              return { ...c, messages }
            }),
          }))
          for (const id of changed) scheduleSave(get, id)
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
        scheduleSave(get, conversationId)
        await runGeneration(set, get, conversationId)
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
        scheduleSave(get, conversationId)
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

      syncConversations: async () => {
        // Nothing streams across a reload. A message the cache still marks `streaming` is one
        // whose stream died with the last window: left alone it shows a spinner forever and is
        // never saved, because the flush skips a streaming conversation. Only when nothing is
        // in flight now — a sync can also run after an import, mid-answer.
        if (inFlight === null) {
          set((s) => ({
            conversations: s.conversations.map((c) =>
              c.messages.some((m) => m.streaming) ? { ...c, messages: c.messages.map((m) => (m.streaming ? { ...m, streaming: false } : m)) } : c,
            ),
          }))
        }

        let listed: ConversationSummary[]
        try {
          listed = await api.conversations()
        } catch (error) {
          warnConversationApi(get, error)
          return
        }

        // The one-time migration. The runtime listing nothing while this window holds
        // conversations is the first run after the change (or a data directory that was wiped);
        // the cache goes over as a Studio export, and the flag says never again.
        const cached = get().conversations
        if (listed.length === 0 && cached.length > 0 && !readFlag(MIGRATED_KEY)) {
          try {
            const report = await api.importConversations(cacheEnvelope(cached))
            writeFlag(MIGRATED_KEY)
            get().toast('success', `Conversations handed to the runtime: ${describeImport(report)}`)
            listed = await api.conversations()
          } catch (error) {
            warnConversationApi(get, error)
            return
          }
        }

        // Pull what the runtime has that this window lacks or holds stale. Everything is loaded
        // eagerly rather than on select: the cache already keeps every conversation in memory,
        // so this costs nothing new — and a lazily-loaded stub with `messages: []` is a
        // conversation one careless PUT away from being emptied. Warm starts fetch only what
        // changed elsewhere, since a cached copy at least as new as the listing is kept as is.
        const cachedById = new Map(cached.map((c) => [c.id, c]))
        const stale = listed.filter((row) => {
          const mine = cachedById.get(row.id)
          return !mine || mine.updatedAt < row.updatedAt
        })
        const fetched = new Map<string, Conversation>()
        for (let at = 0; at < stale.length; at += 8) {
          const batch = stale.slice(at, at + 8)
          const results = await Promise.allSettled(batch.map((row) => api.conversation(row.id)))
          results.forEach((result, i) => {
            const row = batch[i]
            if (!row) return
            if (result.status === 'fulfilled') fetched.set(row.id, result.value)
            // Listed a moment ago and gone now is a deletion behind this window's back, not an
            // outage; anything else is.
            else if (!isUnknownConversation(result.reason)) warnConversationApi(get, result.reason)
          })
        }

        // Merge against the store as it is NOW, not as it was before the fetches: an answer may
        // have streamed in meanwhile, and a copy updated since is newer than anything fetched.
        const listedById = new Map(listed.map((row) => [row.id, row]))
        set((s) => {
          const current = new Map(s.conversations.map((c) => [c.id, c]))
          const merged: Conversation[] = []
          for (const row of listed) {
            const mine = current.get(row.id)
            const theirs = fetched.get(row.id)
            const pick = mine && (!theirs || mine.updatedAt >= theirs.updatedAt) ? mine : theirs
            if (pick) merged.push(pick)
          }
          // What only this window has — made offline, or outliving a wiped data directory —
          // stays, and is pushed below. (A conversation deleted behind this window's back comes
          // back the same way; losing a person's chat is the worse of the two mistakes.)
          for (const c of s.conversations) if (!listedById.has(c.id)) merged.push(c)
          merged.sort((a, b) => b.createdAt - a.createdAt)
          const active = s.activeConversationId
          return {
            conversations: merged,
            activeConversationId: active && !merged.some((c) => c.id === active) ? (merged[0]?.id ?? null) : active,
          }
        })

        for (const c of get().conversations) {
          const row = listedById.get(c.id)
          if (!row || c.updatedAt > row.updatedAt) scheduleSave(get, c.id)
        }
      },

      importConversations: async (body) => {
        try {
          const report = await api.importConversations(body)
          get().toast('success', `Import: ${describeImport(report)}`)
          await get().syncConversations()
          return report
        } catch (error) {
          get().toast('error', `Import failed: ${(error as Error).message}`)
          return null
        }
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

function readFlag(key: string): boolean {
  try {
    return localStorage.getItem(key) === '1'
  } catch {
    return false
  }
}

function writeFlag(key: string) {
  try {
    localStorage.setItem(key, '1')
  } catch {
    /* a locked-down profile cannot remember the migration; the runtime's listing will not be empty next time anyway */
  }
}

/**
 * The cache as the runtime's import reads it: zustand's persist envelope, the very shape under
 * `misaka-studio.session`. The import also takes the Studio's export, but this one names what
 * every message then carries as its `source` — the window's cache — which is what it is.
 */
function cacheEnvelope(conversations: Conversation[]): { state: { conversations: Conversation[] }; version: number } {
  return { state: { conversations }, version: 1 }
}

/** "no conversation with id …": the runtime's 400 for an id it does not hold. */
function isUnknownConversation(error: unknown): boolean {
  return error instanceof ApiError && (error.status === 400 || error.status === 404) && /no conversation/i.test(error.message)
}

/** "imported 12 · skipped 3 (non-text content_type image)" — the report, in one line. */
export function describeImport(report: ConversationImportReport): string {
  const skipped = report.skipped.reduce((n, s) => n + s.count, 0)
  if (skipped === 0) return `imported ${report.imported}`
  const reasons = report.skipped.map((s) => (report.skipped.length === 1 ? s.reason : `${s.reason} ×${s.count}`)).join(', ')
  return `imported ${report.imported} · skipped ${skipped} (${reasons})`
}

function warnConversationApi(get: Getter, error: unknown) {
  if (conversationApiWarned) return
  conversationApiWarned = true
  const why = error instanceof ApiError && error.status === 404 ? 'this runtime has no conversation API' : (error as Error).message
  get().toast(
    'error',
    `Conversations are kept in this window only — ${why}. They stay cached here, and the next start that reaches the runtime hands them over.`,
  )
}

function scheduleSave(get: Getter, id: string) {
  cancelSave(id)
  pendingSaves.set(
    id,
    setTimeout(() => {
      pendingSaves.delete(id)
      void flushSave(get, id)
    }, SAVE_DEBOUNCE_MS),
  )
}

function cancelSave(id: string) {
  const timer = pendingSaves.get(id)
  if (timer !== undefined) clearTimeout(timer)
  pendingSaves.delete(id)
}

async function flushSave(get: Getter, id: string) {
  const conversation = get().conversations.find((c) => c.id === id)
  if (!conversation) return
  // Half an answer is not a conversation to keep; the end of the stream saves the whole one.
  if (conversation.messages.some((m) => m.streaming)) return
  try {
    await api.putConversation(conversation)
  } catch (error) {
    warnConversationApi(get, error)
  }
}

/**
 * Fold streamed `tool_calls` deltas into whole calls. OpenAI streams a call in pieces keyed by
 * `index` — the id and name first, then the arguments a fragment at a time — and a client that
 * kept every piece as its own call would show one call as twelve. The lane hands back its answer
 * all at once, so the pieces usually arrive together; the fold is what makes both look the same.
 */
type ToolCallDelta = { index?: number; id?: string; type?: string; function?: { name?: string; arguments?: string } }

function mergeToolCalls(existing: unknown[], incoming: unknown[]): unknown[] {
  const calls: ToolCallDelta[] = existing.map((c) => ({ ...(c as ToolCallDelta) }))
  for (const piece of incoming) {
    if (!piece || typeof piece !== 'object') continue
    const delta = piece as ToolCallDelta
    const at = typeof delta.index === 'number' ? calls.findIndex((c) => c.index === delta.index) : -1
    const call = at === -1 ? undefined : calls[at]
    if (!call) {
      calls.push({ ...delta, function: { ...delta.function } })
      continue
    }
    if (delta.id) call.id = delta.id
    if (delta.type) call.type = delta.type
    const name = delta.function?.name ?? call.function?.name
    const args = (call.function?.arguments ?? '') + (delta.function?.arguments ?? '')
    call.function = { ...(name !== undefined ? { name } : {}), arguments: args }
  }
  return calls
}

/**
 * Run one generation into `conversationId`.
 *
 * Shared by send, regenerate and edit because all three are the same operation: take the
 * conversation as it now stands, ask for the next assistant turn, stream it in.
 */
async function runGeneration(set: Setter, get: Getter, conversationId: string) {
  const state = get()
  const conversation = state.conversations.find((c) => c.id === conversationId)
  if (!conversation) return

  const settings = state.settings
  const modelId = state.runtime?.model_id ?? state.models[0]?.id
  if (!modelId) {
    state.toast('error', 'No model is available. Download one from the Models tab.')
    return
  }

  const systemPrompt = settings?.generation.system_prompt?.trim()
  const history = conversation.messages.map((m) => ({ role: m.role, content: m.content }))
  const messages = systemPrompt ? [{ role: 'system', content: systemPrompt }, ...history] : history

  const assistantId = uid()
  const placeholder: ChatMessage = { id: assistantId, role: 'assistant', content: '', streaming: true }
  set((s) => ({
    conversations: s.conversations.map((c) =>
      c.id === conversationId ? { ...c, messages: [...c.messages, placeholder], modelId, updatedAt: Date.now() } : c,
    ),
  }))

  const update = (patch: Partial<ChatMessage>) =>
    set((s) => ({
      conversations: s.conversations.map((c) =>
        c.id === conversationId
          ? { ...c, messages: c.messages.map((m) => (m.id === assistantId ? { ...m, ...patch } : m)), updatedAt: Date.now() }
          : c,
      ),
    }))

  const controller = new AbortController()
  inFlight = controller
  const startedAt = performance.now()
  let firstTokenAt: number | null = null
  let text = ''
  let toolCalls: unknown[] = []

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
      },
      controller.signal,
    )

    for await (const event of generator) {
      if (event.type === 'delta') {
        if (firstTokenAt === null) firstTokenAt = performance.now()
        text += event.text
        update({ content: text })
      } else if (event.type === 'misaka') {
        // The lane's account of the answer, kept on the message beside the answer.
        update({ misaka: event.misaka })
      } else if (event.type === 'tool_calls') {
        toolCalls = mergeToolCalls(toolCalls, event.toolCalls)
        update({ toolCalls })
      } else if (event.type === 'error') {
        update({ error: event.message })
      } else {
        const elapsed = performance.now() - startedAt
        update({
          streaming: false,
          stats: {
            // The runtime's token counts, the window's clock. Neither knows both halves: the
            // engine counts tokens, and only the client knows when the user's request started.
            completionTokens: event.usage.completion_tokens,
            promptTokens: event.usage.prompt_tokens,
            tokensPerSecond: elapsed > 0 ? (event.usage.completion_tokens * 1000) / elapsed : 0,
            timeToFirstTokenMs: firstTokenAt === null ? null : firstTokenAt - startedAt,
            model: modelId,
            finishReason: event.finishReason,
          },
        })
      }
    }
    update({ streaming: false })
  } catch (error) {
    if ((error as Error).name === 'AbortError') {
      // A stopped generation keeps what it produced: the user asked it to stop, not to undo.
      update({ streaming: false, stats: undefined })
    } else {
      update({ streaming: false, error: (error as Error).message })
      get().toast('error', (error as Error).message)
    }
  } finally {
    if (inFlight === controller) inFlight = null
    // The stream is over, so the conversation is whole: this is the write the runtime keeps.
    scheduleSave(get, conversationId)
    void get().refreshRuntime()
  }
}
