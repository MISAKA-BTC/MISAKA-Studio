// The bar along the bottom of the window: what the runtime is doing, at a glance.
//
// Left, the engine: connected or not, which backend, which model, how big a window, where the
// weights are — and on Code's side, which project and branch the session works in. Right, what is
// happening now: a reply being generated, the agent's step, and prompt mining — the one part of the
// Studio that is easy to wonder about and hard to see, which is why it lives here, in every view,
// rather than only in the Network tab.

import { useEffect, useRef, useState } from 'react'
import { useAgent } from '../../lib/agent/run'
import { useWorkspaceInfo } from '../../lib/agent/workspace'
import { bytes, tokens } from '../../lib/format'
import type { Offload } from '../../lib/types'
import { chatModelId, useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { Icon, Spinner, Toggle } from '../common'
import { Kbd, MenuItem, MenuLabel, MenuSeparator, Popover, StatusDot } from '../ui'
import { goTo, openSidePanel, openThread, setMiningMode, setPromptStack } from './actions'
import { useGitStatus } from '../code/gitStore'
import { MINING_MODE_LABEL, refreshMiningState, useMiningSummary, type MiningSummary } from './mining'
import { dirtySummary, folderLabel } from './projects'
import { SHORTCUTS } from './shortcuts'

/** `29/29 on MTL0`, `CPU`, or `GPU: engine decides` — with a `?` when nothing verified it.
 *  The same reading as the model bar's: what the engine did, not what it was asked. */
function offloadLabel(offload: Offload, blockCount: number | null): string {
  const unverified = offload.evidence === 'unverified' ? '?' : ''
  if (offload.layers === null) return 'GPU: engine decides'
  if (offload.layers === 0) return 'CPU'
  const total = offload.total_layers ?? (blockCount ? blockCount + 1 : null)
  const device = offload.device?.split(' (')[0] ?? 'GPU'
  return total ? `${Math.min(offload.layers, total)}/${total} on ${device}${unverified}` : `${offload.layers} layers on ${device}${unverified}`
}

function offloadTitle(offload: Offload): string {
  const where = offload.device ? `Weights on ${offload.device}` : 'Weights in system memory'
  const evidence = { engine_log: "from the engine's own load log", device_list: "from the engine's device list", unverified: 'unverified — the engine is too old to say' }[
    offload.evidence
  ]
  const sizes =
    offload.accelerator_bytes !== null && offload.cpu_bytes !== null
      ? ` · ${bytes(offload.accelerator_bytes)} on the accelerator, ${bytes(offload.cpu_bytes)} on the CPU`
      : ''
  return `${where} (${evidence})${sizes}`
}

// Display is spelt out per segment rather than folded in here: `hidden md:inline-flex` next to a
// shared `inline-flex` is two display utilities, and the stylesheet's order — not the breakpoint —
// would decide which one wins.
const box = 'h-full min-w-0 shrink-0 items-center gap-1.5 px-2'
const hover =
  'transition-colors hover:bg-ink-100 hover:text-ink-800 focus-visible:bg-ink-100 focus-visible:outline-none dark:hover:bg-ink-800 dark:hover:text-ink-100 dark:focus-visible:bg-ink-800'
const segment = `inline-flex ${box} ${hover}`
const plain = `inline-flex ${box}`

export function StatusBar() {
  return (
    <footer
      className="flex h-6 shrink-0 items-stretch justify-between overflow-hidden border-t border-ink-200 bg-white text-[0.7rem] leading-none text-ink-500 select-none dark:border-ink-800 dark:bg-ink-900 dark:text-ink-400"
      aria-label="Status bar"
    >
      {/* The left side gives way first — the model's name truncates — so what is happening now,
          on the right, is never pushed out of the window. */}
      <div className="flex min-w-0 flex-1 items-stretch overflow-hidden">
        <RuntimeSegment />
      </div>
      <div className="flex shrink-0 items-stretch">
        <GenerationSegment />
        <AgentSegment />
        <MiningSegment />
      </div>
    </footer>
  )
}

function RuntimeSegment() {
  const connected = useStudio((s) => s.connected)
  const runtime = useStudio((s) => s.runtime)
  const models = useStudio((s) => s.models)
  const loadingModelId = useStudio((s) => s.loadingModelId)
  const selectedId = useStudio(chatModelId)
  const openPalette = useUi((s) => s.openPalette)

  const loaded = models.find((m) => m.id === runtime?.model_id) ?? null
  // The same light as the sidebar's: it answers "is the runtime there", and the model's own name
  // beside it answers "is anything loaded".
  const tone = connected ? 'green' : 'red'
  const status = !connected ? 'Runtime unreachable' : runtime?.model_id ? 'Model loaded' : 'Runtime ready — no model loaded'

  return (
    <>
      <button type="button" className={segment} title={`${status}${runtime ? ` · backend ${runtime.backend}` : ''}`} onClick={() => goTo('models')}>
        <StatusDot tone={tone} />
        <span className="hidden sm:inline">{connected ? (runtime?.backend ?? 'runtime') : 'offline'}</span>
      </button>
      <ProjectSegment />
      {loadingModelId ? (
        <span className={`inline-flex h-full min-w-0 shrink items-center gap-1.5 px-2`} title={`Loading ${loadingModelId}`}>
          <Spinner className="size-3 shrink-0 text-arc-600 dark:text-arc-400" />
          <span className="max-w-[16rem] truncate">Loading {loadingModelId}…</span>
        </span>
      ) : (
        connected && (
          <button
            type="button"
            className={`inline-flex h-full min-w-0 max-w-[18rem] shrink items-center gap-1.5 px-2 ${hover}`}
            title={selectedId ? `${selectedId} — 選択中` : 'モデルを追加'}
            onClick={() => openPalette('load model ')}
          >
            <Icon name="cube" className="size-3 shrink-0" />
            <span className="truncate">{selectedId ?? 'モデルを追加'}</span>
          </button>
        )
      )}
      {runtime?.model_id === selectedId && runtime?.context_size && (
        <span className={`hidden md:inline-flex ${box}`} title="Context window">
          {tokens(runtime.context_size)} ctx
        </span>
      )}
      {runtime?.model_id === selectedId && runtime?.offload && (
        <span
          className={`hidden lg:inline-flex ${box} ${runtime.offload_note ? 'text-amber-700 dark:text-amber-400' : ''}`}
          title={runtime.offload_note ?? offloadTitle(runtime.offload)}
        >
          {runtime.offload_note && <Icon name="warning" className="size-3 shrink-0" />}
          {offloadLabel(runtime.offload, loaded?.block_count ?? null)}
        </span>
      )}
    </>
  )
}

/** On Code's side: the folder the session in view works in, and its branch. Read as a string, so
 *  the bar does not redraw while messages stream into the session. */
function ProjectSegment() {
  const side = useUi((s) => s.appMode)
  const root = useStudio((s) => {
    const thread = s.conversations.find((c) => c.id === s.activeConversationId)
    return thread?.mode === 'agent' ? (thread.workspace ?? null) : null
  })
  if (side !== 'code' || !root) return null
  return <ProjectSegmentFor root={root} />
}

function ProjectSegmentFor({ root }: { root: string }) {
  // The folder from the cache the sidebar and the composer share; the branch and whether the tree
  // is dirty from the git reading the Code panes share — the session's own header asks for the
  // same folder, so this costs no request of its own.
  const entry = useWorkspaceInfo(root)
  const git = useGitStatus(root)
  const info = entry?.status === 'ok' ? entry.info : null
  const repo = git?.status?.is_git ? git.status : null
  const branch = repo?.branch ?? repo?.head ?? info?.branch ?? null
  const dirty = repo ? dirtySummary(repo) : null
  const missing = entry?.status === 'error'
  return (
    <button
      type="button"
      className={`hidden h-full min-w-0 shrink items-center gap-1.5 px-2 sm:inline-flex ${hover} ${missing ? 'text-amber-700 dark:text-amber-400' : ''}`}
      title={[`${info?.root ?? root}${branch ? ` · ${branch}` : ''}`, dirty, missing ? entry.error : null, 'Open the session summary'].filter(Boolean).join('\n')}
      onClick={() => openSidePanel('session')}
    >
      <Icon name={missing ? 'warning' : 'folder'} className="size-3 shrink-0" />
      <span className="max-w-[10rem] truncate">{info?.name ?? folderLabel(root)}</span>
      {branch && (
        <>
          <Icon name="gitBranch" className="size-3 shrink-0" />
          <span className="max-w-[12rem] truncate">{branch}</span>
          {dirty && <span className="size-1.5 shrink-0 rounded-full bg-amber-500" role="img" aria-label={dirty} />}
        </>
      )}
    </button>
  )
}

/** "Generating… 12s" while a reply streams, the last speed after. */
function GenerationSegment() {
  const startedAt = useStudio((s) => s.streaming?.startedAt ?? null)
  const streaming = useStudio((s) => s.streaming !== null)
  const thinking = useStudio((s) => Boolean(s.streaming?.reasoning) && s.streaming?.answerStartedAt === undefined)
  const conversationId = useStudio((s) => s.streaming?.conversationId ?? null)
  const tokensPerSecond = useStudio((s) => s.sample?.generation.last_tokens_per_second ?? 0)
  const [now, setNow] = useState(() => performance.now())

  useEffect(() => {
    if (!streaming) return
    setNow(performance.now())
    const timer = setInterval(() => setNow(performance.now()), 1000)
    return () => clearInterval(timer)
  }, [streaming])

  if (streaming) {
    const seconds = startedAt !== null ? Math.max(0, Math.floor((now - startedAt) / 1000)) : null
    return (
      <button
        type="button"
        className={`${segment} text-arc-700 dark:text-arc-300`}
        title="A reply is being generated — Esc stops it"
        onClick={() => conversationId && openThread(conversationId)}
      >
        <Spinner className="size-3" />
        <span className="tabular-nums">
          {thinking ? 'Thinking…' : 'Generating…'}
          {seconds !== null ? ` ${seconds}s` : ''}
        </span>
      </button>
    )
  }
  if (tokensPerSecond <= 0) return null
  return (
    <span className={`${plain} mono tabular-nums`} title="Speed of the last reply">
      {tokensPerSecond.toFixed(1)} tok/s
    </span>
  )
}

function AgentSegment() {
  const run = useAgent((s) => s.run)
  if (!run) return null
  const waiting = run.status === 'awaiting_approval'
  return (
    <button
      type="button"
      className={`${segment} text-amber-700 dark:text-amber-400`}
      title={waiting ? 'The agent is waiting for your approval' : 'The coding agent is working'}
      onClick={() => openThread(run.conversationId)}
    >
      {waiting ? <StatusDot tone="amber" pulse /> : <Icon name="bot" className="size-3.5" />}
      <span>{waiting ? 'Waiting for approval' : `Agent · step ${run.step}`}</span>
    </button>
  )
}

function miningTitle(summary: MiningSummary): string {
  if (summary.unknown) return 'Prompt mining: not read yet'
  if (summary.mode === 'inline') return 'Inline mining: the chat engine is the mining gateway, so every answer is the mined work'
  if (summary.mode === 'background') return 'Background mining: the chat answers here, and every prompt is queued and mined behind it'
  return summary.blocker ? `Not mining: ${summary.blocker}` : 'Not mining: this chat is answered by an engine that is not the gateway'
}

/** ⛏ and the mode, the queue's counts, and a popover with the rest. */
function MiningSegment() {
  const summary = useMiningSummary()
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const pending = summary.queued + summary.running

  const tone = summary.busy ? 'amber' : summary.mode !== 'off' ? 'green' : 'gray'
  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={`${segment} ${open ? 'bg-ink-100 text-ink-800 dark:bg-ink-800 dark:text-ink-100' : ''} ${
          summary.mode !== 'off' ? 'text-emerald-700 dark:text-emerald-400' : ''
        }`}
        aria-expanded={open}
        aria-haspopup="dialog"
        title={miningTitle(summary)}
        onClick={() => {
          if (!open) void refreshMiningState()
          setOpen((v) => !v)
        }}
      >
        <StatusDot tone={tone} pulse={summary.busy} />
        <Icon name="pickaxe" className="size-3.5" />
        <span>{summary.unknown ? 'Mining' : MINING_MODE_LABEL[summary.mode]}</span>
        {pending > 0 && (
          <span className="tabular-nums text-amber-700 dark:text-amber-400" title={`${summary.running} being mined, ${summary.queued} queued`}>
            · {summary.running > 0 ? `${summary.running} mining` : ''}
            {summary.running > 0 && summary.queued > 0 ? ', ' : ''}
            {summary.queued > 0 ? `${summary.queued} queued` : ''}
          </span>
        )}
        {summary.committed > 0 && (
          <span className="hidden tabular-nums sm:inline" title="Prompts committed by the lane — committed, not yet paid">
            · {summary.committed} committed
          </span>
        )}
      </button>
      <Popover anchor={anchor} open={open} onClose={() => setOpen(false)} placement="top-end" className="w-80">
        <MiningPopover summary={summary} close={() => setOpen(false)} />
      </Popover>
    </>
  )
}

function MiningPopover({ summary, close }: { summary: MiningSummary; close: () => void }) {
  const conversation = useStudio((s) => s.conversations.find((c) => c.id === s.activeConversationId) ?? null)
  const setConversationMining = useStudio((s) => s.setConversationMining)
  const generating = useStudio((s) => s.streaming !== null)
  const acceptUnverified = useStudio((s) => s.settings?.node.free_prompt?.accept_unverified ?? false)
  const [busy, setBusy] = useState(false)

  const act = async (fn: () => Promise<unknown>) => {
    setBusy(true)
    try {
      await fn()
    } finally {
      setBusy(false)
    }
  }

  const modeLine =
    summary.mode === 'inline'
      ? 'The chat engine is the mining gateway: each answer you get is the mined work.'
      : summary.mode === 'background'
        ? 'The chat answers here; each prompt you send is also queued and mined on the gateway, at its own pace.'
        : summary.setting === 'background'
          ? 'Background mining is on, but not in effect yet — so this chat is not mined.'
          : 'This chat is not mined: it is answered by an engine that is not the mining gateway.'

  return (
    <div className="p-1 text-[0.8rem]">
      <div className="flex items-center gap-2 px-2.5 pb-1 pt-1.5">
        <Icon name="pickaxe" className="size-4 text-ink-500 dark:text-ink-400" />
        <span className="font-semibold">Prompt mining</span>
        <span
          className={`badge ml-auto ${
            summary.mode === 'off'
              ? 'bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300'
              : 'bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300'
          }`}
        >
          {MINING_MODE_LABEL[summary.mode]}
        </span>
      </div>
      <p className="px-2.5 pb-2 text-[0.75rem] leading-relaxed text-ink-500 dark:text-ink-400">
        {modeLine}
        {summary.mode === 'off' && summary.blocker && (
          <span className="mt-1 flex items-start gap-1 text-amber-700 dark:text-amber-400">
            <Icon name="warning" className="mt-px size-3.5 shrink-0" />
            <span>{summary.blocker}</span>
          </span>
        )}
      </p>

      <div className="mx-2.5 mb-2 grid grid-cols-4 gap-1 rounded-lg bg-ink-50 p-1.5 text-center dark:bg-ink-900/70">
        <Count label="queued" value={summary.queued} tone={summary.queued > 0 ? 'amber' : undefined} />
        <Count label="mining" value={summary.running} tone={summary.running > 0 ? 'amber' : undefined} />
        <Count label="committed" value={summary.committed} tone={summary.committed > 0 ? 'green' : undefined} />
        <Count label="not mined" value={summary.notMined} tone={summary.notMined > 0 ? 'red' : undefined} />
      </div>

      <div className="flex items-center gap-2 px-2.5 pb-2 text-[0.75rem] text-ink-600 dark:text-ink-300">
        <StatusDot tone={summary.stackRunning ? 'green' : summary.stackPartial ? 'amber' : 'gray'} />
        <span className="min-w-0 flex-1 truncate">
          Prompt stack{' '}
          {summary.stackRunning
            ? `running · ${summary.carriers} carrier${summary.carriers === 1 ? '' : 's'}`
            : summary.stackPartial
              ? 'partly running — it mines nothing like this'
              : summary.stackReady
                ? 'stopped'
                : 'not set up'}
        </span>
        {(summary.stackRunning || summary.stackPartial) && (
          <button type="button" className="btn-ghost px-1.5 py-0.5 text-[0.7rem]" disabled={busy} onClick={() => void act(() => setPromptStack(false))}>
            Stop
          </button>
        )}
        {!summary.stackRunning && !summary.stackPartial && summary.stackReady && (
          <button
            type="button"
            className="btn-ghost px-1.5 py-0.5 text-[0.7rem]"
            disabled={busy || !acceptUnverified}
            title={acceptUnverified ? 'Start the gateway and the rail' : 'Accept signing on a public node first (Network → Prompt mining — no node)'}
            onClick={() => void act(() => setPromptStack(true))}
          >
            Start
          </button>
        )}
      </div>

      {conversation && (
        <div className="mx-2.5 mb-2 rounded-lg border border-ink-200 px-2.5 py-2 dark:border-ink-700">
          <Toggle
            label="Mine prompts from this thread"
            hint={
              conversation.mode === 'agent'
                ? 'Only what you type is mined — never file contents or tool output.'
                : 'Applies when background mining is in effect.'
            }
            checked={conversation.mine !== false}
            onChange={(on) => setConversationMining(conversation.id, on)}
          />
        </div>
      )}

      <p className="px-2.5 pb-1.5 text-[0.7rem] leading-relaxed text-ink-400 dark:text-ink-500">
        Committed is not paid: a claim pays only once it is final on the chain.
      </p>

      <MenuSeparator />
      <MenuLabel>Mode setting</MenuLabel>
      <MenuItem
        label="Inline"
        hint="the chat is the mining"
        checked={summary.setting === 'inline'}
        disabled={busy || generating || summary.setting === null}
        onSelect={() => summary.setting !== 'inline' && void act(() => setMiningMode('inline'))}
      />
      <MenuItem
        label="Background"
        hint={!summary.backgroundAvailable && summary.setting !== 'background' ? 'not available' : 'queued behind the chat'}
        checked={summary.setting === 'background'}
        disabled={busy || generating || summary.setting === null || (!summary.backgroundAvailable && summary.setting !== 'background')}
        onSelect={() => summary.setting !== 'background' && void act(() => setMiningMode('background'))}
      />
      {generating && <p className="px-2.5 pb-1 text-[0.7rem] text-ink-400 dark:text-ink-500">The mode can change once the reply finishes.</p>}
      <MenuSeparator />
      <MenuItem
        icon="panelRight"
        label="Open mining panel"
        shortcut={SHORTCUTS.miningPanel}
        onSelect={() => {
          close()
          openSidePanel('mining')
        }}
      />
      <MenuItem
        icon="globe"
        label="Mining setup"
        hint="Network"
        onSelect={() => {
          close()
          goTo('network')
        }}
      />
      <div className="flex items-center gap-1.5 px-2.5 pb-1 pt-1.5 text-[0.7rem] text-ink-400 dark:text-ink-500">
        More in the palette: <Kbd shortcut={SHORTCUTS.palette} /> then “mine”
      </div>
    </div>
  )
}

function Count({ label, value, tone }: { label: string; value: number; tone?: 'amber' | 'green' | 'red' }) {
  const color =
    tone === 'amber'
      ? 'text-amber-700 dark:text-amber-400'
      : tone === 'green'
        ? 'text-emerald-700 dark:text-emerald-400'
        : tone === 'red'
          ? 'text-red-600 dark:text-red-400'
          : 'text-ink-700 dark:text-ink-200'
  return (
    <div className="min-w-0">
      <div className={`text-sm font-semibold tabular-nums ${color}`}>{value}</div>
      <div className="truncate text-[0.65rem] text-ink-400 dark:text-ink-500">{label}</div>
    </div>
  )
}
