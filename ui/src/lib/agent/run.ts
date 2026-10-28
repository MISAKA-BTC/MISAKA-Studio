// The coding agent, wired to the window: the store, the engine and the runtime's workspace.
//
// `loop.ts` is the agent; this file gives it hands. It checks that a run can start — an agent
// thread with a folder, a model, an engine whose window can hold tool output — builds the system
// prompt from the folder's details, streams every step through `streamReply` (the chat's own path,
// so Stop, the stats and the context manager behave exactly as they do in a chat), and keeps the
// run's state where the composer, the status bar and the approval cards read it.
//
// **One run at a time**, because the engine holds one model and serves one request. A stopped run
// still unwinds for a moment — its stream closing, a read finishing — and a run started in that
// moment (Retry stops and restarts in one go) must not have the old run's last writes land in its
// thread. So every run has a token, and a run's writes are dropped once a newer one has started.

import { create } from 'zustand'
import { api } from '../api'
import { hasRepeatedTail } from '../history'
import type { Attachment, MessageMining, WorkspaceInfo } from '../types'
import { chatModelId, streamReply, uid, useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { runAgentLoop, type ApprovalAnswer, type LoopResult } from './loop'
import { buildSystemPrompt, hasOpenToolCall } from './protocol'
import { outputLimits, type ApprovalKind } from './tools'
import { openWorkspace } from './workspace'

export type AgentRunStatus = 'running' | 'awaiting_approval'

export type AgentState = {
  /** The run in progress, if any. One at a time: the engine holds one model and one request. */
  run: { conversationId: string; status: AgentRunStatus; step: number } | null
  /** The tool call whose approval card is waiting for an answer. */
  pendingApproval: string | null
  /** Kinds approved with "Always allow in this thread", per thread — for this session only: a
   *  standing permission to edit or run commands should not outlive the window that granted it. */
  allowed: Record<string, ApprovalKind[]>
}

export const useAgent = create<AgentState>(() => ({ run: null, pendingApproval: null, allowed: {} }))

/** Below this window the agent works, but a few file reads fill it; 32K is comfortable. */
export const AGENT_MIN_CONTEXT = 16_384
export const AGENT_GOOD_CONTEXT = 32_768

let active: { token: number; conversationId: string; controller: AbortController } | null = null
let latestToken = 0
let approval: { token: number; toolCallId: string; resolve: (answer: ApprovalAnswer) => void } | null = null
const contextWarned = new Set<string>()

const GATEWAY_EXPLAINED =
  'The agent cannot run on the mining gateway: its 512-token class cannot hold even one tool result. Load a local model to chat with (llama.cpp), and turn on background mining — the prompts you type are still mined while the agent works on the local engine.'

function localDate(): string {
  const now = new Date()
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`
}

/** A run that cannot start gives the person's words — and their attachments — back to the
 *  composer rather than losing them. */
function refuse(kind: 'info' | 'error', text: string, draft: string, attachments?: Attachment[]) {
  useStudio.getState().toast(kind, text)
  if (draft.trim() || attachments?.length) useUi.getState().setComposerDraft(draft, attachments)
}

/** Whether the window is too small for the agent to do much: a question for the warning chip too. */
export function contextTooSmall(contextSize: number | null | undefined): boolean {
  return contextSize !== null && contextSize !== undefined && contextSize > 0 && contextSize < AGENT_MIN_CONTEXT
}

function report(result: LoopResult) {
  const toast = useStudio.getState().toast
  if (result.outcome === 'error' && result.note) toast('error', `The agent stopped: ${result.note}`)
  else if ((result.outcome === 'limit' || result.outcome === 'stuck') && result.note) toast('info', result.note)
}

/**
 * Start the coding agent on `text` in a conversation whose `mode` is `agent`.
 *
 * `options.mining` is for running the same prompt again (regenerate): it was mined when it was
 * first sent, so its job carries over to the new message instead of the prompt being queued twice.
 */
export async function startAgentRun(
  conversationId: string,
  text: string,
  attachments?: Attachment[],
  options: { mining?: MessageMining } = {},
): Promise<void> {
  const back = (kind: 'info' | 'error', note: string) => refuse(kind, note, text, attachments)
  const studio = useStudio.getState()
  const conversation = studio.conversations.find((c) => c.id === conversationId)
  if (!conversation) return
  if (!text.trim() && !attachments?.length) return
  if (active && !active.controller.signal.aborted) {
    return back(
      'info',
      active.conversationId === conversationId
        ? 'The agent is still working in this thread. Wait for it, or stop it first.'
        : 'The agent is working in another thread. Stop it there first.',
    )
  }
  if (studio.streaming && studio.streaming.conversationId !== conversationId) {
    return back('info', 'Another thread is still answering. Stop it there, or wait, then send again.')
  }
  if (conversation.mode !== 'agent' || !conversation.workspace) {
    useUi.getState().openWorkspacePicker(conversationId)
    return back('info', 'Choose the folder the agent works in, then send again.')
  }
  const modelId = chatModelId(studio, conversationId)
  if (!modelId) return back('error', 'No model is available. Download one from the Models tab.')
  if (studio.runtime?.model_id === modelId && studio.runtime.backend === 'gateway') return back('error', GATEWAY_EXPLAINED)
  const window = studio.runtime?.model_id === modelId ? studio.runtime.context_size : null
  if (contextTooSmall(window) && !contextWarned.has(`${modelId}:${window}`)) {
    contextWarned.add(`${modelId}:${window}`)
    studio.toast(
      'info',
      `${modelId} is loaded with a ${window?.toLocaleString('en-US')}-token window, which a few file reads fill. The agent works best with 32K: reload the model at 32K from the window chip beside the folder, or from the model picker.`,
    )
  }

  // The folder as the runtime sees it now — it may have moved or gone since the thread was made —
  // and its branch, for the system prompt.
  let info: WorkspaceInfo
  try {
    info = await openWorkspace(conversation.workspace, true)
  } catch (error) {
    return back('error', `The folder ${conversation.workspace} cannot be opened: ${(error as Error).message}`)
  }
  if (active && !active.controller.signal.aborted) return back('info', 'The agent is already working. Stop it first.')

  const token = ++latestToken
  const controller = new AbortController()
  active = { token, conversationId, controller }
  const live = () => latestToken === token
  const running = () => live() && !controller.signal.aborted
  controller.signal.addEventListener('abort', () => {
    const now = useStudio.getState()
    if (now.streaming?.conversationId === conversationId) now.stop()
  })
  useAgent.setState({ run: { conversationId, status: 'running', step: 1 }, pendingApproval: null })

  try {
    const result = await runAgentLoop(
      {
        conversationId,
        getConversation: () => useStudio.getState().conversations.find((c) => c.id === conversationId),
        updateConversation: (patch) => {
          if (live()) useStudio.getState().updateConversation(conversationId, patch)
        },
        appendMessage: (message) => {
          if (!live()) return
          useStudio.setState((s) => ({
            conversations: s.conversations.map((c) =>
              c.id === conversationId
                ? { ...c, messages: [...c.messages, message], updatedAt: Date.now(), modelId: c.modelId ?? modelId }
                : c,
            ),
            // The step joins the transcript in the same update that drops its live overlay, so it
            // is never drawn twice, not even for a frame.
            ...(s.streaming?.conversationId === conversationId && s.streaming.assistantId === message.id ? { streaming: null } : {}),
          }))
        },
        callModel: async ({ assistantId, messages, pinned }) => {
          const now = useStudio.getState()
          return streamReply({
            conversationId,
            assistantId,
            messages,
            modelId: chatModelId(now, conversationId) ?? modelId,
            pinned,
            // Room for a whole file in one write_file; the context manager keeps the answer's share.
            maxTokens: Math.max(now.settings?.generation.max_tokens ?? 0, 4096),
            stop: ['</tool_call>'],
            // A file being written is not a loop, however repetitive its code: the chat's check
            // runs only outside an open call.
            isLooping: (text) => !hasOpenToolCall(text) && hasRepeatedTail(text),
          })
        },
        systemPrompt: (thread) =>
          buildSystemPrompt({
            root: info.root,
            name: info.name,
            branch: info.branch,
            isGit: info.is_git,
            os: useStudio.getState().system?.hardware.os ?? null,
            date: localDate(),
            policy: thread.approval ?? 'auto_edit',
            userInstructions: useStudio.getState().settings?.generation.system_prompt ?? null,
          }),
        workspace: { root: info.root, api: api.workspace, limits: outputLimits(window) },
        requestApproval: (toolCallId) =>
          new Promise<ApprovalAnswer>((resolve) => {
            if (!running()) return resolve('stop')
            approval = { token, toolCallId, resolve }
            useAgent.setState({ pendingApproval: toolCallId })
          }),
        allowed: () => new Set(useAgent.getState().allowed[conversationId] ?? []),
        allow: (kind) =>
          useAgent.setState((s) => ({ allowed: { ...s.allowed, [conversationId]: [...new Set([...(s.allowed[conversationId] ?? []), kind])] } })),
        onStatus: ({ status, step }) => {
          if (running()) useAgent.setState({ run: { conversationId, status, step } })
        },
        // Mined: what the person typed, and only that — never an attachment, never tool output.
        onUserMessage: (message) => {
          if (!live()) return
          if (options.mining) {
            const mining = options.mining
            useStudio.getState().updateConversation(conversationId, (c) => ({
              messages: c.messages.map((m) => (m.id === message.id ? { ...m, mining } : m)),
            }))
            return
          }
          if (message.content) void useStudio.getState().enqueueMining(conversationId, message.id, message.content)
        },
        signal: controller.signal,
        newId: uid,
        now: Date.now,
      },
      { text, attachments },
    )
    if (live()) report(result)
  } catch (error) {
    if (live()) useStudio.getState().toast('error', `The agent stopped: ${(error as Error).message}`)
  } finally {
    if (live()) {
      active = null
      approval = null
      useAgent.setState({ run: null, pendingApproval: null })
      // A step that never reached the transcript (an exception mid-stream) leaves no overlay behind.
      useStudio.setState((s) => (s.streaming?.conversationId === conversationId ? { streaming: null } : {}))
    }
  }
}

/**
 * Stop the run: the stream in flight is aborted, a waiting approval is answered "stop" (its call
 * shows as denied), and a running command's request is aborted — which the runtime takes as the
 * signal to kill the command's process group. The run then unwinds by itself, within moments; the
 * composer is free again at once.
 */
export function stopAgentRun(): void {
  if (!active) return
  active.controller.abort()
  if (approval && approval.token === active.token) {
    const waiting = approval
    approval = null
    waiting.resolve('stop')
  }
  useAgent.setState({ run: null, pendingApproval: null })
}

/** Answer a tool call that is waiting for approval. `always` approves this kind for the rest of the thread. */
export function resolveApproval(toolCallId: string, decision: 'approve' | 'deny' | 'always'): void {
  if (!approval || approval.toolCallId !== toolCallId) return
  const waiting = approval
  approval = null
  useAgent.setState({ pendingApproval: null })
  waiting.resolve(decision)
}

/** Whether `toolCallId` is the call a run is waiting on now — an approval card for a call no run
 *  waits for (the window was reloaded) must not offer buttons that do nothing. */
export function isAwaiting(toolCallId: string): boolean {
  return approval?.toolCallId === toolCallId
}
