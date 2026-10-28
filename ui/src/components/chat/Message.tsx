// One turn of the transcript.
//
// The person's messages are quiet bubbles on the right; the model's replies are prose across the
// column with no bubble, because they are what is read. Everything that is *about* a reply rather
// than part of it — how long it thought, how fast it ran, what it was given, whether it was cut —
// sits around it, small, in the order it is needed: reasoning above, notices and actions below.

import { memo, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react'
import misakaPi from '../../assets/misaka-pi.png'
import { duration } from '../../lib/format'
import { splitThinking } from '../../lib/reasoning'
import type { ChatMessage } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { AgentMessageBody } from '../agent/AgentMessage'
import { CopyButton, Icon, type IconName } from '../common'
import { ContextLine, pinTextFromMessage } from '../ContextPanel'
import { Markdown } from '../Markdown'
import { AttachmentChip } from './AttachmentChip'
import { MiningBadge } from './MiningBadge'
import { ReasoningBlock } from './ReasoningBlock'
import { Waiting } from './Waiting'

/** What a message can ask the transcript to do. One stable object, so messages can be memoised. */
export type TranscriptActions = {
  startEdit: (id: string) => void
  cancelEdit: () => void
  saveEdit: (id: string, content: string) => void
  regenerate: (hint?: boolean) => void
  continueReply: () => void
}

/** The live overlay's timing and reasoning, for the reply streaming now. */
export type LiveStream = { reasoning?: string; startedAt?: number; answerStartedAt?: number }

type MessageProps = {
  message: ChatMessage
  /** An agent thread: replies are agent steps, drawn by the agent's own body. */
  agent: boolean
  /** The thread's last turn — regenerate and continue live only there. */
  last: boolean
  /** Something is generating in this thread right now. */
  generating: boolean
  editing: boolean
  live: LiveStream | null
  /** The first reply after the person's message: the avatar is drawn once per answer. */
  showAvatar: boolean
  actions: TranscriptActions
}

export const Message = memo(function Message(props: MessageProps) {
  return props.message.role === 'user' ? <UserMessage {...props} /> : <AssistantMessage {...props} />
})

function ActionButton({ icon, label, onClick, children }: { icon: IconName; label: string; onClick: () => void; children?: ReactNode }) {
  return (
    <button type="button" className={`icon-btn ${children ? 'w-auto gap-1 px-1.5 text-[0.7rem]' : ''}`} aria-label={label} title={label} onClick={onClick}>
      <Icon name={icon} className="size-3.5" />
      {children}
    </button>
  )
}

/** Hover actions under a message. The last reply's stay visible: it is the one acted on. */
function ActionRow({ align, visible, children }: { align: 'start' | 'end'; visible: boolean; children: ReactNode }) {
  return (
    <div
      className={`mt-1 flex h-7 items-center gap-0.5 text-ink-500 transition-opacity dark:text-ink-400 ${align === 'end' ? 'justify-end' : ''} ${
        visible ? 'opacity-100' : 'opacity-0 focus-within:opacity-100 group-hover:opacity-100'
      }`}
    >
      {children}
    </div>
  )
}

function PinButton({ text }: { text: string }) {
  const addPin = useStudio((s) => s.addPin)
  const toast = useStudio((s) => s.toast)
  return (
    <ActionButton
      icon="pin"
      label="Pin as a note: kept in every request ahead of the history"
      onClick={() => {
        addPin(pinTextFromMessage(text))
        toast('success', 'Pinned. Edit or remove it in the Context panel.')
      }}
    />
  )
}

function UserMessage({ message, editing, actions }: MessageProps) {
  return (
    <div id={`message-${message.id}`} className="group flex flex-col items-end">
      {message.attachments && message.attachments.length > 0 && (
        <div className="mb-1.5 flex max-w-[85%] flex-wrap justify-end gap-1.5">
          {message.attachments.map((attachment) => (
            <AttachmentChip key={attachment.id} attachment={attachment} />
          ))}
        </div>
      )}
      {editing ? (
        <EditBox initial={message.content} onCancel={actions.cancelEdit} onSave={(content) => actions.saveEdit(message.id, content)} />
      ) : (
        message.content && (
          <div className="max-w-[85%] rounded-2xl bg-ink-100 px-4 py-2.5 text-ink-900 dark:bg-ink-800 dark:text-ink-50">
            <p className="whitespace-pre-wrap break-words text-[0.94rem] leading-relaxed">{message.content}</p>
          </div>
        )
      )}
      {!editing && message.mining && <MiningBadge mining={message.mining} />}
      {!editing && (
        <ActionRow align="end" visible={false}>
          {message.content && <CopyButton text={message.content} label="Copy message" className="icon-btn" />}
          <ActionButton icon="edit" label="Edit and re-run from here" onClick={() => actions.startEdit(message.id)} />
          {message.content && <PinButton text={message.content} />}
        </ActionRow>
      )}
    </div>
  )
}

/** Edit a message in place. Saving re-runs from it: the turns after it answered another question. */
function EditBox({ initial, onCancel, onSave }: { initial: string; onCancel: () => void; onSave: (content: string) => void }) {
  const [draft, setDraft] = useState(initial)
  const field = useRef<HTMLTextAreaElement>(null)
  useEffect(() => {
    const element = field.current
    if (!element) return
    element.focus()
    element.setSelectionRange(element.value.length, element.value.length)
  }, [])
  useLayoutEffect(() => {
    const element = field.current
    if (!element) return
    element.style.height = 'auto'
    element.style.height = `${Math.min(element.scrollHeight, 400)}px`
  }, [draft])

  return (
    <div className="w-full max-w-[85%] rounded-2xl border border-arc-500/60 bg-white p-2 ring-4 ring-arc-500/10 dark:bg-ink-900">
      <textarea
        ref={field}
        rows={1}
        className="block w-full resize-none bg-transparent px-2 py-1 text-[0.94rem] leading-relaxed outline-none"
        value={draft}
        aria-label="Edit message"
        onChange={(event) => setDraft(event.target.value)}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.keyCode === 229) return
          if (event.key === 'Escape') {
            // Claimed here, so the window does not also read it as "stop the reply".
            event.preventDefault()
            event.stopPropagation()
            onCancel()
          } else if (event.key === 'Enter' && !event.shiftKey) {
            event.preventDefault()
            if (draft.trim()) onSave(draft)
          }
        }}
      />
      <div className="mt-1 flex items-center justify-between gap-2 px-1">
        <span className="text-[0.7rem] text-ink-400 dark:text-ink-500">Re-runs from here — the replies after it are replaced.</span>
        <div className="flex shrink-0 gap-1.5">
          <button type="button" className="btn-ghost px-2.5 py-1 text-xs" onClick={onCancel}>
            Cancel
          </button>
          <button type="button" className="btn-primary px-2.5 py-1 text-xs" onClick={() => onSave(draft)} disabled={!draft.trim()}>
            Save and re-run
          </button>
        </div>
      </div>
    </div>
  )
}

function AssistantMessage({ message, agent, last, generating, live, showAvatar, actions }: MessageProps) {
  const streaming = message.streaming === true
  const inline = splitThinking(message.content)
  const answer = inline.answer
  // Reasoning streamed apart from the answer wins; a `<think>` block in the content is the fallback.
  const separate = live?.reasoning ?? message.reasoning ?? null
  const reasoning = separate ?? inline.thinking
  const thinkingNow = streaming && (separate ? live?.answerStartedAt === undefined && !answer : inline.open)
  const thoughtMs =
    message.reasoningMs ??
    (live?.answerStartedAt !== undefined && live.startedAt !== undefined ? live.answerStartedAt - live.startedAt : null)
  const toolCalls = agent && (message.toolCalls?.length ?? 0) > 0
  const cut = !streaming && message.stats?.finishReason === 'length'
  const canAct = last && !generating

  return (
    <div id={`message-${message.id}`} className="group flex gap-3">
      {showAvatar ? (
        <img
          src={misakaPi}
          alt="MISAKA"
          className="mt-0.5 size-7 shrink-0 rounded-lg bg-white object-contain p-0.5 ring-1 ring-ink-200 dark:bg-ink-100 dark:ring-ink-700"
        />
      ) : (
        <div className="w-7 shrink-0" aria-hidden />
      )}

      <div className="min-w-0 flex-1 pt-0.5">
        {reasoning !== null && (thinkingNow || reasoning.trim()) && (
          <ReasoningBlock text={reasoning} live={thinkingNow} startedAt={live?.startedAt} durationMs={thoughtMs} />
        )}

        {agent
          ? (answer || toolCalls) && (
              <AgentMessageBody message={answer === message.content ? message : { ...message, content: answer }} streaming={streaming} />
            )
          : answer && <Markdown streaming={streaming}>{answer}</Markdown>}

        {streaming && !answer && !toolCalls && reasoning === null && <Waiting />}

        {/* A reply that ran out of ceiling used to end mid-sentence and say nothing about it — the
            reason was in a hover title. The lane decodes to its ceiling and the class's context is
            fixed on chain, so this is the one number a person can actually act on. */}
        {cut && message.stats && (
          <div className="mt-3 rounded-lg bg-amber-50 p-2.5 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
            <p className="flex items-start gap-2">
              <Icon name="stop" className="mt-0.5 size-3.5 shrink-0" />
              <span>
                {answer.trim()
                  ? '生成上限に達したため、回答が途中で止まりました。続きを生成するか、Parameters で生成上限を増やしてください。'
                  : '回答本文を生成する前に、思考で生成上限に達しました。思考をオフにして回答を再生成できます。'}
              </span>
            </p>
            {canAct && !agent && (
              <button type="button" className="btn-outline mt-2 px-2 py-1 text-xs" onClick={actions.continueReply}>
                {answer.trim() ? '続きを生成' : '思考をオフにして回答を生成'}
              </button>
            )}
          </div>
        )}

        {!streaming && message.context && <ContextLine report={message.context} />}

        {message.error && (
          <div className="mt-3 flex items-start gap-2 rounded-lg border border-red-200 bg-red-50 px-3 py-2 text-xs text-red-700 dark:border-red-900/60 dark:bg-red-950/30 dark:text-red-300">
            <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
            <p className="min-w-0 flex-1 whitespace-pre-wrap break-words">{message.error}</p>
            {canAct && (
              <button
                type="button"
                className="-my-0.5 shrink-0 rounded-md border border-red-300 px-2 py-0.5 font-medium hover:bg-red-100 dark:border-red-800 dark:hover:bg-red-950/60"
                onClick={() => actions.regenerate(message.context?.backend === 'gateway' || message.context?.backend === 'misaka')}
              >
                {message.context?.backend === 'gateway' || message.context?.backend === 'misaka' ? '要点を絞って再生成' : 'Retry'}
              </button>
            )}
          </div>
        )}

        {!streaming && (
          <ActionRow align="start" visible={last}>
            {answer && <CopyButton text={answer} label="Copy reply" className="icon-btn" />}
            {canAct && <ActionButton icon="refresh" label="Regenerate" onClick={() => actions.regenerate(message.context?.backend === 'gateway' || message.context?.backend === 'misaka')} />}
            {canAct && cut && !agent && (
              <ActionButton icon="arrowRight" label="Continue the cut-off reply" onClick={actions.continueReply}>
                {answer.trim() ? 'Continue' : '回答を生成'}
              </ActionButton>
            )}
            {answer && <PinButton text={answer} />}
            {message.stats && (
              <span
                className="mono ml-1.5 truncate text-[0.7rem] text-ink-400 dark:text-ink-500"
                title={`${message.stats.completionTokens} tokens · ${message.stats.promptTokens} prompt tokens · ${message.stats.finishReason} · ${message.stats.model}`}
              >
                {message.stats.tokensPerSecond.toFixed(1)} tok/s
                {message.stats.timeToFirstTokenMs !== null && ` · first token ${duration(message.stats.timeToFirstTokenMs)}`}
                {` · ${message.stats.completionTokens} tokens`}
              </span>
            )}
          </ActionRow>
        )}
      </div>
    </div>
  )
}
