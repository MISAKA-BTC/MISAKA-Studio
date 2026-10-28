// Prompt mining, as the chat states it: one reading of the runtime's queue, shared by the composer's
// chip, its popover and the Mining panel, so the three can never disagree about what is happening.
//
// The states are the runtime's, not the window's guess:
//
// * **Inline** — the chat engine IS the mining gateway (`runtime.backend === 'gateway'`): the
//   answer is the mined work, one execution, and nothing is queued.
// * **Background** — the chat answers on a local engine and each prompt is also queued, to be
//   mined on the gateway at its own pace. Only when the queue says it can be honoured
//   (`background_available`); otherwise one prompt would be mined twice.
// * **Off here** — background mining is on, but this thread opted out (`conversation.mine`).
// * **Set up** — none of the above: nothing this chat sends is mined, and the reason is the
//   queue's own (`background_blocker`) or the plain fact that the engine is not the gateway.

import { useState } from 'react'
import { api } from '../../lib/api'
import type { ChatMessage, Conversation, MiningMode, MiningQueueView } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { CopyButton, Icon, Spinner, Toggle } from '../common'
import { Segmented } from '../ui'
import { miningTone } from './MiningBadge'

export type ChatMiningState =
  | { kind: 'inline' }
  | { kind: 'background'; live: number }
  | { kind: 'off-here'; live: number }
  | { kind: 'setup'; reason: string }

export function chatMiningState(input: {
  queue: MiningQueueView | null
  backend: string | undefined
  connected: boolean
  /** The thread's own switch: absent means on. */
  mine: boolean
}): ChatMiningState {
  const { queue, backend, connected, mine } = input
  if (!connected) return { kind: 'setup', reason: 'The runtime is unreachable, so nothing is mined until it is back.' }
  const live = queue ? queue.counts.queued + queue.counts.running : 0
  // Background is checked first, as the status bar does: the runtime only reports it available
  // when an engine other than the gateway answers, so the two are never both true of one prompt —
  // and when the window's copies disagree for a moment, both surfaces settle on the same word.
  if (queue?.mode === 'background' && queue.background_available) return mine ? { kind: 'background', live } : { kind: 'off-here', live }
  if (backend === 'gateway') return { kind: 'inline' }
  if (!queue) return { kind: 'setup', reason: 'The mining queue has not answered yet.' }
  // Why not is said once, under the mode switch, in the queue's own words (`background_blocker`).
  if (queue.mode === 'background') {
    return { kind: 'setup', reason: 'Background mining is chosen but not in effect yet — until it is, nothing typed here is queued.' }
  }
  const engine = backend ? `the ${backend} engine` : 'a local engine'
  return {
    kind: 'setup',
    reason:
      `Inline mining mines only answers that come from the mining gateway, and this chat runs on ${engine}.` +
      (queue.background_available ? ' Switch to Background to mine its prompts behind the chat.' : ''),
  }
}

export function useChatMiningState(conversation: Conversation | null): ChatMiningState {
  const queue = useStudio((s) => s.miningQueue)
  const backend = useStudio((s) => s.runtime?.backend)
  const connected = useStudio((s) => s.connected)
  return chatMiningState({ queue, backend, connected, mine: conversation?.mine !== false })
}

/** Inline ⇄ Background, through the runtime — which reloads the model into the engine the mode picks. */
export function MiningModeSwitch({ queue }: { queue: MiningQueueView }) {
  const refreshMining = useStudio((s) => s.refreshMining)
  const refreshRuntime = useStudio((s) => s.refreshRuntime)
  const toast = useStudio((s) => s.toast)
  const [busy, setBusy] = useState(false)

  const change = async (mode: MiningMode) => {
    if (mode === queue.mode || busy) return
    if (useStudio.getState().streaming) {
      toast('info', 'Wait for the reply to finish: switching the mining mode changes the engine that is answering it.')
      return
    }
    setBusy(true)
    try {
      await api.miningMode(mode)
      // The engine can change with the mode (the gateway answers inline, a local engine in the
      // background), so the runtime is read again along with the queue — and the settings, whose
      // window copy would otherwise put the old mode back on the next save of anything else.
      const [settings] = await Promise.all([api.settings().catch(() => null), refreshMining(), refreshRuntime()])
      if (settings) useStudio.setState({ settings })
      toast(
        'success',
        mode === 'background'
          ? 'Background mining: the chat answers locally and every prompt is queued for the slot'
          : 'Inline mining: the chat waits for the lane, and its answer is the mined answer',
      )
    } catch (error) {
      toast('error', (error as Error).message)
    } finally {
      setBusy(false)
    }
  }

  return (
    <div>
      <div className="flex items-center gap-2">
        <span className="text-[0.7rem] font-medium text-ink-600 dark:text-ink-300">Mode</span>
        <Segmented<MiningMode>
          value={queue.mode}
          onChange={(mode) => void change(mode)}
          options={[
            { value: 'inline', label: 'Inline', title: 'The chat engine is the mining gateway: each answer is the mined work' },
            { value: 'background', label: 'Background', title: 'The chat answers locally; each prompt is queued and mined behind it' },
          ]}
        />
        {busy && <Spinner className="size-3.5 text-ink-400" />}
      </div>
      {!queue.background_available && queue.background_blocker && (
        <p className="mt-1.5 flex items-start gap-1 text-[0.7rem] leading-relaxed text-amber-700 dark:text-amber-400">
          <Icon name="warning" className="mt-0.5 size-3 shrink-0" />
          <span>Background is not available: {queue.background_blocker}.</span>
        </p>
      )}
    </div>
  )
}

function Counts({ queue }: { queue: MiningQueueView }) {
  const { counts } = queue
  const notMined = counts.refused + counts.failed
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
      <span className={counts.running > 0 ? 'text-amber-700 dark:text-amber-400' : ''}>{counts.running} mining</span>
      <span>{counts.queued} queued</span>
      <span className={counts.committed > 0 ? 'text-emerald-700 dark:text-emerald-400' : ''}>{counts.committed} committed</span>
      {notMined > 0 && <span className="text-red-700 dark:text-red-400">{notMined} not mined</span>}
    </div>
  )
}

const HEADLINE: Record<ChatMiningState['kind'], string> = {
  inline: 'Inline mining — answers are the mining work',
  background: 'Mining in the background',
  'off-here': 'Not mining this thread',
  setup: 'Nothing here is mined yet',
}

/**
 * What happens to what is typed here, in a few honest lines, with the controls that change it.
 * The composer chip's popover and the head of the Mining panel.
 */
export function MiningOverview({ conversation, state, onNavigate }: { conversation: Conversation | null; state: ChatMiningState; onNavigate?: () => void }) {
  const queue = useStudio((s) => s.miningQueue)
  const setConversationMining = useStudio((s) => s.setConversationMining)
  const setView = useStudio((s) => s.setView)
  const setRightPanel = useUi((s) => s.setRightPanel)
  const rightPanel = useUi((s) => s.rightPanel)
  const live = state.kind === 'inline' || state.kind === 'background'

  return (
    <div className="space-y-3">
      <div>
        <div className={`flex items-center gap-1.5 text-xs font-semibold ${live ? 'text-emerald-700 dark:text-emerald-400' : 'text-ink-700 dark:text-ink-200'}`}>
          <Icon name="pickaxe" className="size-3.5" />
          {HEADLINE[state.kind]}
        </div>
        <p className="mt-1 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
          {state.kind === 'inline' && (
            <>
              This chat runs on the mining gateway, so each answer is one mined execution — the answer and the claim behind it are
              the same run, and it arrives whole when the job ends. Everything the model is sent for it is the job.
            </>
          )}
          {state.kind === 'background' && (
            <>
              The chat answers on this machine&apos;s engine; each prompt you send is also queued and mined on the gateway at its own
              pace. The badge under your message says what became of it. Only what you type is mined — never attachments, files or
              tool output.
            </>
          )}
          {state.kind === 'off-here' && (
            <>Background mining is on, but this thread&apos;s prompts are not queued. Other threads keep mining.</>
          )}
          {state.kind === 'setup' && state.reason}
        </p>
      </div>

      {conversation && state.kind !== 'inline' && (
        <Toggle
          label="Mine this thread's prompts"
          checked={conversation.mine !== false}
          onChange={(on) => setConversationMining(conversation.id, on)}
          hint={state.kind === 'setup' ? 'Takes effect once background mining is.' : undefined}
        />
      )}
      {conversation && state.kind === 'inline' && conversation.mine === false && (
        <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">
          This thread is set not to mine, which applies to background mining; inline, the answer itself is the mining.
        </p>
      )}

      {queue && (
        <div className="space-y-2 rounded-lg bg-ink-50 p-2 dark:bg-ink-900/60">
          <MiningModeSwitch queue={queue} />
          <Counts queue={queue} />
        </div>
      )}

      {conversation && <ThreadJobs messages={conversation.messages} limit={3} />}

      <div className="flex flex-wrap gap-1.5">
        {rightPanel !== 'mining' && (
          <button
            type="button"
            className="btn-outline px-2 py-1 text-xs"
            onClick={() => {
              setRightPanel('mining')
              onNavigate?.()
            }}
          >
            <Icon name="panelRight" className="size-3.5" />
            Open mining panel
          </button>
        )}
        <button
          type="button"
          className="btn-ghost px-2 py-1 text-xs"
          onClick={() => {
            setView('network')
            onNavigate?.()
          }}
        >
          <Icon name="settings" className="size-3.5" />
          Mining setup
        </button>
      </div>
    </div>
  )
}

const STATUS_WORD = { queued: 'queued', running: 'mining…', committed: 'committed', refused: 'refused', failed: 'failed' } as const

/** The newest of this thread's queued prompts, one line each. */
function ThreadJobs({ messages, limit }: { messages: ChatMessage[]; limit: number }) {
  const mined = messages.filter((m) => m.role === 'user' && m.mining).slice(-limit).reverse()
  if (mined.length === 0) return null
  return (
    <div>
      <div className="mb-1 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">This thread</div>
      <ul className="space-y-1">
        {mined.map((m) => (
          <li key={m.id} className="flex items-center gap-2 text-[0.7rem]">
            <span className={`badge shrink-0 ${miningTone(m.mining!.status)}`}>{STATUS_WORD[m.mining!.status]}</span>
            <span className="min-w-0 flex-1 truncate text-ink-600 dark:text-ink-300" title={m.content}>
              {m.content}
            </span>
          </li>
        ))}
      </ul>
    </div>
  )
}

/** One of this thread's mined prompts in full: status, claim, the mined answer, the lane's words. */
export function MinedPrompt({ message }: { message: ChatMessage }) {
  const [open, setOpen] = useState(false)
  const mining = message.mining
  if (!mining) return null
  return (
    <li className="rounded-lg border border-ink-200 p-2 dark:border-ink-800">
      <div className="flex items-start gap-2">
        <span className={`badge shrink-0 ${miningTone(mining.status)}`}>{STATUS_WORD[mining.status]}</span>
        <p className="line-clamp-2 min-w-0 flex-1 break-words text-xs text-ink-700 dark:text-ink-200" title={message.content}>
          {message.content}
        </p>
      </div>
      {mining.claimId && (
        <div className="mt-1 flex items-center gap-1 text-[0.65rem] text-ink-500 dark:text-ink-400">
          <span className="mono min-w-0 flex-1 truncate" title={mining.claimId}>
            claim {mining.claimId}
          </span>
          <CopyButton text={mining.claimId} label="Copy claim id" className="icon-btn size-6" />
        </div>
      )}
      {mining.error && <p className="mt-1 whitespace-pre-wrap text-[0.65rem] text-red-700 dark:text-red-300">{mining.error}</p>}
      {mining.answer && (
        <div className="mt-1">
          <button
            type="button"
            className="flex items-center gap-1 text-[0.65rem] text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-200"
            onClick={() => setOpen((v) => !v)}
            aria-expanded={open}
          >
            <Icon name="chevron" className={`size-3 transition-transform ${open ? 'rotate-90' : ''}`} />
            Mined answer — what the chain holds
          </button>
          {open && <p className="mt-1 whitespace-pre-wrap break-words rounded-md bg-ink-50 p-2 text-[0.7rem] text-ink-700 dark:bg-ink-900/60 dark:text-ink-300">{mining.answer}</p>}
        </div>
      )}
    </li>
  )
}
