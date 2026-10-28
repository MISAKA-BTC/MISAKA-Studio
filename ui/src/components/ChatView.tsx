// The chat surface.
//
// Three behaviours here are worth more than they look:
//
// * **Auto-scroll that stops when you scroll up.** A view that yanks itself back to the bottom
//   while someone is reading the middle of a long answer is unusable; one that never follows the
//   stream is equally so. So it follows only while the user is already at the bottom.
// * **Editing a message re-runs from that point.** The turns after it answered a question that no
//   longer exists, so they go. The alternative — leaving them — produces a transcript the model
//   never actually produced.
// * **Stop keeps what was generated.** The user asked it to stop, not to undo.
//
// And one about layout: there is one composer, and it moves. An empty thread has it in the middle,
// under the question of what to ask; a thread with history has it at the bottom. The draft lives in
// `chat/composerStore`, per thread, so nothing typed is lost when it moves or when someone looks at
// another thread and comes back.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState, type DragEvent } from 'react'
import { startAgentRun, stopAgentRun, useAgent } from '../lib/agent/run'
import type { ChatMessage, Conversation } from '../lib/types'
import { useStudio, type StreamingReply } from '../store/studio'
import { useUi } from '../store/ui'
import { Icon } from './common'
import { useCodeWatchers } from './code/watchers'
import { ChatHeader } from './chat/ChatHeader'
import { Composer } from './chat/Composer'
import { attachFiles, draftKey } from './chat/composerStore'
import { EmptyHero } from './chat/EmptyHero'
import { Message, type LiveStream, type TranscriptActions } from './chat/Message'
import { ParametersDrawer } from './chat/ParametersDrawer'
import { RightPanel } from './chat/RightPanel'

const NO_MESSAGES: ChatMessage[] = []
/** Closer to the bottom than this counts as "at the bottom": following the stream. */
const FOLLOW_SLACK_PX = 80

/**
 * Whether another thread holds the engine. There is one live overlay and one request in flight; a
 * second request would take the overlay from the first, whose answer would then never be committed.
 */
function busyElsewhere(conversationId: string): boolean {
  const streaming = useStudio.getState().streaming
  const run = useAgent.getState().run
  return (streaming !== null && streaming.conversationId !== conversationId) || (run !== null && run.conversationId !== conversationId)
}

/** The index of the last message the person typed (not a tool result), or -1. */
function lastTypedIndex(messages: ChatMessage[]): number {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]
    if (m && m.role === 'user' && !m.toolResults) return i
  }
  return -1
}

function liveOf(stream: StreamingReply): LiveStream {
  return { reasoning: stream.reasoning, startedAt: stream.startedAt, answerStartedAt: stream.answerStartedAt }
}

export function ChatView() {
  const conversation = useStudio((s) => s.conversations.find((c) => c.id === s.activeConversationId) ?? null)
  const streaming = useStudio((s) => s.streaming)
  const agentRun = useAgent((s) => s.run)

  const messages = conversation?.messages ?? NO_MESSAGES
  const activeStream = conversation && streaming?.conversationId === conversation.id ? streaming : null
  // The pending reply is a render-only overlay. It joins the transcript exactly once, when the
  // request ends, and therefore can never leak an incomplete answer into the next model request.
  const visibleMessages = activeStream
    ? messages.some((m) => m.id === activeStream.assistantId)
      ? messages.map((m) => (m.id === activeStream.assistantId ? { ...m, content: activeStream.content, streaming: true } : m))
      : [...messages, { id: activeStream.assistantId, role: 'assistant' as const, content: activeStream.content, streaming: true }]
    : messages
  // A tool result is history the agent sends back to the model, not something the person wrote.
  const rows = visibleMessages.filter((m) => m.role !== 'system' && !(m.role === 'user' && m.toolResults))
  const agent = conversation?.mode === 'agent'
  const generating = activeStream !== null || (conversation !== null && agentRun?.conversationId === conversation.id)
  const empty = rows.length === 0

  // A Code session's git status and panes follow what its agent and the window do.
  useCodeWatchers(conversation)

  const [editingId, setEditingId] = useState<string | null>(null)
  const scrollRef = useRef<HTMLDivElement>(null)
  const followRef = useRef(true)
  const jumpUntil = useRef(0)
  const [atBottom, setAtBottom] = useState(true)

  // The latest of what the transcript's actions read — so the actions themselves never change and
  // a memoised message is not redrawn for every token of the reply below it.
  const latest = useRef({ conversation, generating })
  latest.current = { conversation, generating }

  // A thread opens at its end, following.
  useLayoutEffect(() => {
    followRef.current = true
    setAtBottom(true)
    setEditingId(null)
  }, [conversation?.id])

  // Follow the stream only while the user is at the bottom.
  useLayoutEffect(() => {
    const element = scrollRef.current
    if (!element || !followRef.current) return
    element.scrollTop = element.scrollHeight
  }, [conversation?.id, messages, activeStream?.content, activeStream?.reasoning])

  // Things that change height without changing the messages — a composer growing, a reply's final
  // render with its math, a reasoning block folding — keep a following view at the bottom too.
  useEffect(() => {
    const element = scrollRef.current
    if (!element) return
    const observer = new ResizeObserver(() => {
      if (followRef.current) element.scrollTop = element.scrollHeight
    })
    observer.observe(element)
    if (element.firstElementChild) observer.observe(element.firstElementChild)
    return () => observer.disconnect()
  }, [conversation?.id, empty])

  const onScroll = () => {
    const element = scrollRef.current
    if (!element) return
    const near = element.scrollHeight - element.scrollTop - element.clientHeight < FOLLOW_SLACK_PX
    setAtBottom(near)
    // A smooth jump to the end passes through positions that are not the end; it still follows.
    if (performance.now() < jumpUntil.current) {
      followRef.current = true
      return
    }
    followRef.current = near
  }

  const jumpToBottom = () => {
    const element = scrollRef.current
    if (!element) return
    followRef.current = true
    jumpUntil.current = performance.now() + 800
    element.scrollTo({ top: element.scrollHeight, behavior: 'smooth' })
  }

  const follow = useCallback(() => {
    followRef.current = true
  }, [])

  const actions = useMemo<TranscriptActions>(() => {
    const toast = (text: string) => useStudio.getState().toast('info', text)
    const elsewhere = 'Another thread is still answering. Stop it there, or wait, then try again.'

    /** Agent threads re-run the agent: the message and everything after it go, and it runs again. */
    const rerunAgent = async (thread: Conversation, index: number, text: string) => {
      const original = thread.messages[index]
      if (!original) return
      if (useAgent.getState().run?.conversationId === thread.id) stopAgentRun()
      useStudio.getState().updateConversation(thread.id, (c) => ({ messages: c.messages.slice(0, index), updatedAt: Date.now() }))
      // The same words again (regenerate) were mined when they were first sent: the job carries
      // over rather than the prompt being queued a second time. Edited words are a new prompt.
      const same = text.trim() === original.content.trim() && original.mining
      await startAgentRun(thread.id, text, original.attachments, same ? { mining: original.mining } : {})
    }

    return {
      startEdit: (id) => setEditingId(id),
      cancelEdit: () => {
        setEditingId(null)
        useUi.getState().focusComposer()
      },
      saveEdit: (id, content) => {
        const thread = latest.current.conversation
        if (!thread) return
        if (busyElsewhere(thread.id)) return toast(elsewhere)
        setEditingId(null)
        useUi.getState().focusComposer()
        followRef.current = true
        const studio = useStudio.getState()
        // Stop keeps what was generated — but an edit replaces it, so the reply in flight stops first.
        if (studio.streaming?.conversationId === thread.id) studio.stop()
        if (thread.mode === 'agent') {
          void rerunAgent(thread, thread.messages.findIndex((m) => m.id === id), content.trim())
          return
        }
        void studio.editMessage(id, content)
      },
      regenerate: () => {
        const { conversation: thread, generating: busy } = latest.current
        if (!thread || busy) return
        if (busyElsewhere(thread.id)) return toast(elsewhere)
        followRef.current = true
        useUi.getState().focusComposer()
        if (thread.mode === 'agent') {
          const index = lastTypedIndex(thread.messages)
          const original = thread.messages[index]
          if (original) void rerunAgent(thread, index, original.content)
          return
        }
        void useStudio.getState().regenerate()
      },
      continueReply: () => {
        const { conversation: thread, generating: busy } = latest.current
        if (!thread || busy || thread.mode === 'agent') return
        if (busyElsewhere(thread.id)) return toast(elsewhere)
        followRef.current = true
        useUi.getState().focusComposer()
        void useStudio.getState().continueGeneration()
      },
    }
  }, [])

  // ↑ in an empty composer: edit the last thing the person wrote.
  const editLast = useCallback(() => {
    const { conversation: thread, generating: busy } = latest.current
    if (!thread || busy) return
    const last = thread.messages[lastTypedIndex(thread.messages)]
    if (!last) return
    setEditingId(last.id)
    requestAnimationFrame(() => document.getElementById(`message-${last.id}`)?.scrollIntoView({ block: 'nearest' }))
  }, [])

  // --- files dropped anywhere on the chat go to the composer --------------------------------------

  const dragDepth = useRef(0)
  const [dragging, setDragging] = useState(false)
  const carriesFiles = (event: DragEvent) => Array.from(event.dataTransfer.types).includes('Files')
  const dropHandlers = {
    onDragEnter: (event: DragEvent) => {
      if (!carriesFiles(event)) return
      event.preventDefault()
      dragDepth.current += 1
      setDragging(true)
    },
    onDragOver: (event: DragEvent) => {
      if (!carriesFiles(event)) return
      event.preventDefault()
      event.dataTransfer.dropEffect = 'copy'
    },
    onDragLeave: (event: DragEvent) => {
      if (!carriesFiles(event)) return
      dragDepth.current = Math.max(0, dragDepth.current - 1)
      if (dragDepth.current === 0) setDragging(false)
    },
    onDrop: (event: DragEvent) => {
      if (!carriesFiles(event)) return
      event.preventDefault()
      dragDepth.current = 0
      setDragging(false)
      const files = Array.from(event.dataTransfer.files)
      if (files.length) void attachFiles(draftKey(conversation?.id), files)
      useUi.getState().focusComposer()
    },
  }

  return (
    <div className="relative flex h-full min-w-0 bg-ink-50 dark:bg-ink-950">
      <div className="relative flex min-w-0 flex-1 flex-col" {...dropHandlers}>
        <ChatHeader conversation={conversation} generating={generating} />
        <ParametersDrawer />

        <div className="relative min-h-0 flex-1">
          <div ref={scrollRef} onScroll={onScroll} className="h-full overflow-y-auto">
            {empty ? (
              <EmptyHero conversation={conversation}>
                <Composer conversation={conversation} variant="hero" onSend={follow} />
              </EmptyHero>
            ) : (
              <div className="mx-auto w-full max-w-[760px] px-4 pb-6 pt-6">
                {rows.map((message, index) => {
                  const previous = rows[index - 1]
                  const reply = message.role !== 'user'
                  const continuing = reply && previous !== undefined && previous.role !== 'user'
                  return (
                    // Every message reserves its action row below it, and that row is most of the gap.
                    <div key={message.id} className={index === 0 ? '' : continuing ? 'mt-1' : 'mt-3'}>
                      <Message
                        message={message}
                        agent={agent}
                        last={index === rows.length - 1}
                        generating={generating}
                        editing={editingId === message.id}
                        live={activeStream && message.id === activeStream.assistantId ? liveOf(activeStream) : null}
                        showAvatar={reply && !continuing}
                        actions={actions}
                      />
                    </div>
                  )
                })}
              </div>
            )}
          </div>

          {!empty && (
            <div className="pointer-events-none absolute inset-x-0 bottom-0 h-4 bg-linear-to-t from-ink-50 dark:from-ink-950" aria-hidden />
          )}
          {!empty && !atBottom && (
            <button
              type="button"
              className="absolute bottom-3 left-1/2 z-10 inline-flex size-8 -translate-x-1/2 items-center justify-center rounded-full border border-ink-200 bg-white text-ink-600 shadow-md shadow-ink-950/10 transition-colors hover:text-ink-900 dark:border-ink-700 dark:bg-ink-850 dark:text-ink-300 dark:shadow-black/40 dark:hover:text-ink-50"
              onClick={jumpToBottom}
              aria-label="Jump to the latest message"
              title="Jump to the latest message"
            >
              <Icon name="arrowDown" className="size-4" />
            </button>
          )}
        </div>

        {!empty && (
          <div className="shrink-0 px-4 pb-4 pt-1">
            <div className="mx-auto w-full max-w-[760px]">
              <Composer conversation={conversation} variant="dock" onSend={follow} onEditLast={editLast} />
            </div>
          </div>
        )}

        {dragging && (
          <div className="pointer-events-none absolute inset-2 z-40 flex items-center justify-center rounded-2xl border-2 border-dashed border-arc-500/60 bg-arc-500/5">
            <div className="flex flex-col items-center gap-1.5 rounded-xl bg-white px-6 py-4 text-center shadow-lg dark:bg-ink-900">
              <Icon name="upload" className="size-6 text-arc-600 dark:text-arc-400" />
              <span className="text-sm font-medium">Drop to attach</span>
              <span className="text-xs text-ink-500 dark:text-ink-400">Text files, up to 200 KB each — never mined</span>
            </div>
          </div>
        )}
      </div>

      <RightPanel conversation={conversation} />
    </div>
  )
}
