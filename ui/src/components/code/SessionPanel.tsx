// The Session panel: a Code session at a glance, the way Claude's desktop app summarises one —
// where it runs, how full the model's window is, what changed, what was mined, what it cost.
//
// Every line is a summary of a pane that says more (the Changes tab, the Mining tab, the context
// line under a step), so each card is short and links to the long form. The cards fold, and stay
// folded, for the person who only ever looks at one of them.

import { useMemo, type ReactNode } from 'react'
import { AGENT_GOOD_CONTEXT, contextTooSmall } from '../../lib/agent/run'
import { folderName, tildePath, useWorkspaceInfo } from '../../lib/agent/workspace'
import { diffStats } from '../../lib/diff'
import { relativeTime, tokens } from '../../lib/format'
import type { ChatMessage, ContextReport, Conversation } from '../../lib/types'
import { chatModelId, useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { POLICIES } from '../agent/AgentControls'
import { DiffStat } from '../agent/DiffView'
import { useChatMiningState, type ChatMiningState } from '../chat/mining'
import { CopyButton, Icon, Spinner, type IconName } from '../common'
import { PinnedNotesEditor } from '../ContextPanel'
import { PopoverButton, StatusDot } from '../ui'
import { useCodeUi } from './codeStore'
import { useGitStatus, type GitEntry } from './gitStore'
import { useHomeDir } from './shared'

// --- the frame -------------------------------------------------------------------------------------

function Card({ id, title, aside, children }: { id: string; title: string; aside?: ReactNode; children: ReactNode }) {
  const collapsed = useCodeUi((s) => s.collapsed.includes(id))
  const toggle = useCodeUi((s) => s.toggleCard)
  return (
    <section className="rounded-xl border border-ink-200 bg-white dark:border-ink-800 dark:bg-ink-900" aria-label={title}>
      <div className="flex h-9 items-center gap-2 pl-3 pr-2.5">
        <button
          type="button"
          className="-ml-1 flex items-center gap-1 rounded-md px-1 py-0.5 text-xs font-medium text-ink-700 hover:bg-ink-100 hover:text-ink-900 dark:text-ink-200 dark:hover:bg-ink-800 dark:hover:text-ink-50"
          aria-expanded={!collapsed}
          onClick={() => toggle(id)}
        >
          {title}
          <Icon name="chevronDown" className={`size-3 text-ink-400 transition-transform ${collapsed ? '-rotate-90' : ''}`} />
        </button>
        {aside && <div className="ml-auto flex min-w-0 items-center gap-1.5">{aside}</div>}
      </div>
      {!collapsed && <div className="space-y-1.5 px-3 pb-3">{children}</div>}
    </section>
  )
}

function Row({ icon, children, action, title }: { icon: IconName; children: ReactNode; action?: ReactNode; title?: string }) {
  return (
    <div className="flex min-h-7 items-center gap-2.5 text-[0.8rem] text-ink-700 dark:text-ink-200" title={title}>
      <Icon name={icon} className="size-3.5 shrink-0 text-ink-400 dark:text-ink-500" />
      <div className="min-w-0 flex-1">{children}</div>
      {action && <div className="flex shrink-0 items-center gap-1">{action}</div>}
    </div>
  )
}

function SmallButton({ children, onClick, disabled, title }: { children: ReactNode; onClick: () => void; disabled?: boolean; title?: string }) {
  return (
    <button type="button" className="btn-outline h-6 gap-1 px-2 py-0 text-[0.7rem]" onClick={onClick} disabled={disabled} title={title}>
      {children}
    </button>
  )
}

const quiet = 'text-[0.7rem] text-ink-500 dark:text-ink-400'

// --- environment ----------------------------------------------------------------------------------

function BranchRow({ root, git, fallbackBranch }: { root: string; git: GitEntry | null; fallbackBranch: string | null }) {
  const status = git?.status ?? null
  if (git?.unsupported || (!status && git?.error)) {
    return (
      <Row icon="gitBranch" title={git?.unsupported ? 'Ahead/behind and changed files need the updated runtime' : (git?.error ?? undefined)}>
        <span className="truncate">{fallbackBranch ?? <span className={quiet}>Branch unknown</span>}</span>
      </Row>
    )
  }
  if (!status) {
    return (
      <Row icon="gitBranch">
        <span className={`inline-flex items-center gap-1.5 ${quiet}`}>
          <Spinner className="size-3" />
          Reading git…
        </span>
      </Row>
    )
  }
  if (!status.is_git) {
    return (
      <Row icon="gitBranch" title={`${root} is not inside a git repository`}>
        <span className={quiet}>Not a git repository</span>
      </Row>
    )
  }
  const dirty = status.files.length
  return (
    <Row icon="gitBranch" title={status.upstream ? `Tracking ${status.upstream}` : 'No upstream branch'}>
      <div className="flex min-w-0 items-center gap-2">
        <span className="min-w-0 truncate font-medium">{status.branch ?? (status.head ? `detached at ${status.head}` : 'no commits yet')}</span>
        {(status.ahead > 0 || status.behind > 0) && (
          <span className="mono shrink-0 text-[0.7rem] text-ink-500 dark:text-ink-400" title={`${status.ahead} ahead, ${status.behind} behind ${status.upstream ?? 'upstream'}`}>
            {status.ahead > 0 && `↑${status.ahead}`}
            {status.ahead > 0 && status.behind > 0 && ' '}
            {status.behind > 0 && `↓${status.behind}`}
          </span>
        )}
        <span className={`ml-auto shrink-0 ${quiet}`}>{dirty === 0 ? 'clean' : `${dirty} changed`}</span>
      </div>
    </Row>
  )
}

function ApprovalPicker({ conversation }: { conversation: Conversation }) {
  const policy = conversation.approval ?? 'auto_edit'
  const current = POLICIES.find((p) => p.value === policy) ?? POLICIES[2]!
  return (
    <PopoverButton
      className="chip h-6 whitespace-nowrap px-1.5"
      title={`Approval: ${current.label} — ${current.description}`}
      label={`Approval policy: ${current.label}`}
      placement="bottom-end"
      panelClassName="w-80"
      button={
        <>
          <Icon name={current.icon} className="size-3.5" />
          <span>{current.label}</span>
          <Icon name="chevronDown" className="size-3 opacity-60" />
        </>
      }
    >
      {(close) => (
        <>
          <div className="menu-label">What the agent may do without asking</div>
          {POLICIES.map((option) => (
            <button
              key={option.value}
              type="button"
              className="menu-item items-start"
              data-active={option.value === policy}
              onClick={() => {
                useStudio.getState().setConversationApproval(conversation.id, option.value)
                close()
              }}
            >
              <Icon name={option.icon} className="mt-0.5 size-4 shrink-0 opacity-80" />
              <span className="min-w-0 flex-1">
                <span className="block font-medium">{option.label}</span>
                <span className="block text-[0.7rem] leading-snug text-ink-500 dark:text-ink-400">{option.description}</span>
              </span>
              <Icon name="check" className={`mt-0.5 size-3.5 shrink-0 text-arc-600 dark:text-arc-400 ${option.value === policy ? '' : 'invisible'}`} />
            </button>
          ))}
        </>
      )}
    </PopoverButton>
  )
}

function EnvironmentCard({ conversation }: { conversation: Conversation }) {
  const server = useStudio((s) => (s.settings ? `${s.settings.server.host}:${s.settings.server.port}` : null))
  const connected = useStudio((s) => s.connected)
  const modelId = useStudio((s) => chatModelId(s, conversation.id))
  const windowSize = useStudio((s) => s.runtime?.model_id === chatModelId(s, conversation.id) ? s.runtime?.context_size ?? null : null)
  const backend = useStudio((s) => s.runtime?.backend ?? null)
  const root = conversation.workspace ?? null
  const info = useWorkspaceInfo(root)
  const git = useGitStatus(root)
  const home = useHomeDir()
  const known = info?.status === 'ok' ? info.info : null

  return (
    <Card
      id="environment"
      title="Environment"
      aside={
        <span className={`flex items-center gap-1.5 ${quiet}`} title={connected ? 'The runtime is answering' : 'The runtime is not answering'}>
          <StatusDot tone={connected ? 'green' : 'red'} />
          {connected ? 'Local' : 'Offline'}
        </span>
      }
    >
      <Row icon="monitor" action={server && <span className="mono text-[0.7rem] text-ink-400 dark:text-ink-500">{server}</span>}>
        This computer
      </Row>
      <Row
        icon={info?.status === 'error' ? 'warning' : 'folder'}
        action={
          root ? (
            <CopyButton text={known?.root ?? root} label="Copy the folder's path" className="icon-btn size-6" />
          ) : (
            <SmallButton onClick={() => useUi.getState().openWorkspacePicker(conversation.id)}>Choose…</SmallButton>
          )
        }
      >
        {root ? (
          <div className="min-w-0">
            <div className="truncate font-medium" title={info?.status === 'error' ? info.error : undefined}>
              {known?.name ?? folderName(root)}
              {info?.status === 'error' && <span className="ml-1.5 text-[0.7rem] font-normal text-amber-600 dark:text-amber-400">not found</span>}
            </div>
            <div className="mono truncate text-[0.7rem] text-ink-400 dark:text-ink-500" title={known?.root ?? root}>
              {tildePath(known?.root ?? root, home)}
            </div>
          </div>
        ) : (
          <span className="text-amber-700 dark:text-amber-300">No folder chosen</span>
        )}
      </Row>
      {root && <BranchRow root={root} git={git} fallbackBranch={known?.branch ?? null} />}
      <Row icon="shield" action={<ApprovalPicker conversation={conversation} />}>
        Approval
      </Row>
      <Row icon="cpu" title={backend ? `Engine: ${backend}` : undefined}>
        {modelId ? (
          <div className="flex min-w-0 items-center gap-2">
            <span className="mono min-w-0 truncate text-[0.75rem]">{modelId}</span>
            {windowSize ? <span className={`ml-auto shrink-0 ${quiet}`}>{tokens(windowSize)} window</span> : null}
          </div>
        ) : (
          <span className={quiet}>モデルを追加してください</span>
        )}
      </Row>
    </Card>
  )
}

// --- context ----------------------------------------------------------------------------------------

function lastReport(messages: ChatMessage[]): ContextReport | null {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]
    if (m?.role === 'assistant' && m.context) return m.context
  }
  return null
}

/** How full the window was: the prompt plus the room kept for the answer, as a share of it. */
function contextUse(report: ContextReport): { percent: number; tone: 'calm' | 'warn' | 'full' } {
  const used = report.window > 0 ? (report.prompt_tokens + report.answer_reserve) / report.window : 0
  const percent = Math.max(0, Math.min(100, Math.round(used * 100)))
  return { percent, tone: percent >= 90 ? 'full' : percent >= 70 ? 'warn' : 'calm' }
}

const BAR_TONE = { calm: 'bg-arc-500', warn: 'bg-amber-500', full: 'bg-red-500' } as const
const TEXT_TONE = {
  calm: 'text-ink-500 dark:text-ink-400',
  warn: 'text-amber-700 dark:text-amber-400',
  full: 'text-red-700 dark:text-red-400',
} as const

function ContextBar({ report }: { report: ContextReport }) {
  const { tone } = contextUse(report)
  const share = (n: number) => (report.window > 0 ? `${Math.min(100, (n / report.window) * 100)}%` : '0%')
  return (
    <div className="flex h-1.5 w-full overflow-hidden rounded-full bg-ink-100 dark:bg-ink-800" aria-hidden>
      <div className={`h-full ${BAR_TONE[tone]} transition-[width] duration-500`} style={{ width: share(report.prompt_tokens) }} />
      {/* The answer's room, drawn lighter: reserved, not yet used. */}
      <div className={`h-full ${BAR_TONE[tone]} opacity-35 transition-[width] duration-500`} style={{ width: share(report.answer_reserve) }} />
    </div>
  )
}

function ContextCard({ conversation }: { conversation: Conversation }) {
  const report = useMemo(() => lastReport(conversation.messages), [conversation.messages])
  const runtimeWindow = useStudio((s) => s.runtime?.context_size ?? null)
  const backend = useStudio((s) => s.runtime?.backend ?? null)
  const loading = useStudio((s) => s.loadingModelId)
  const modelId = useStudio((s) => chatModelId(s))
  const pins = conversation.pinned?.length ?? 0
  const use = report ? contextUse(report) : null
  const windowSize = runtimeWindow ?? report?.window ?? null
  const small = contextTooSmall(windowSize) && backend !== 'gateway'

  const rows: [string, ReactNode][] = []
  if (report) {
    rows.push(['Pinned notes', report.pinned_omitted > 0 ? <span className="text-amber-700 dark:text-amber-400">{report.pinned_included} sent, {report.pinned_omitted} did not fit</span> : `${report.pinned_included} sent`])
    if (report.continuation) rows.push(['Shape', 'continuing a cut-off reply'])
    else if (!report.managed) rows.push(['History', 'all of it, as it is'])
    else {
      rows.push(['Recent', `${report.recent_messages} message${report.recent_messages === 1 ? '' : 's'} whole`])
      rows.push(['Older', report.older_messages === 0 ? 'none' : report.memory ? `${report.memory.messages_covered} in memory` : `${report.older_messages} not sent`])
      if (report.memory) rows.push(['Memory', `${report.memory.source === 'summary' ? 'a summary' : 'an extract'}, ${report.memory.tokens.toLocaleString('en-US')} tokens`])
    }
  }

  return (
    <Card
      id="context"
      title="Context"
      aside={
        use && report ? (
          <span className="flex items-center gap-2" title={`${use.percent}% of the ${tokens(report.window)}-token window`}>
            <span className={`text-[0.7rem] tabular-nums ${TEXT_TONE[use.tone]}`}>{use.percent}%</span>
            <span className="flex h-1.5 w-10 overflow-hidden rounded-full bg-ink-100 dark:bg-ink-800" aria-hidden>
              <span className={`h-full ${BAR_TONE[use.tone]}`} style={{ width: `${use.percent}%` }} />
            </span>
          </span>
        ) : null
      }
    >
      {report && use ? (
        <>
          <div className="pt-0.5">
            <ContextBar report={report} />
            <p className={`mt-1.5 ${quiet}`}>
              <span className="tabular-nums">{report.prompt_tokens.toLocaleString('en-US')}</span> prompt +{' '}
              <span className="tabular-nums">{report.answer_reserve.toLocaleString('en-US')}</span> for the answer, of {tokens(report.window)}
              <span className="opacity-70"> · {report.counter.kind === 'tokenizer' ? 'exact' : 'estimate'}</span>
            </p>
          </div>
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5 text-[0.7rem]">
            {rows.map(([label, value]) => (
              <div key={label} className="contents">
                <dt className="text-ink-400 dark:text-ink-500">{label}</dt>
                <dd className="min-w-0 truncate text-ink-700 dark:text-ink-300">{value}</dd>
              </div>
            ))}
          </dl>
        </>
      ) : (
        <p className={quiet}>
          No step yet. The bar fills after the agent&apos;s first step{windowSize ? ` — the window is ${tokens(windowSize)} tokens` : ''}.
        </p>
      )}

      {small && (
        <div className="flex items-start gap-1.5 rounded-lg bg-amber-50 px-2 py-1.5 text-[0.7rem] text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <Icon name="warning" className="mt-0.5 size-3 shrink-0" />
          <span className="min-w-0 flex-1">A {tokens(windowSize)} window is small for the agent: a few file reads fill it, and it loses the start of its task.</span>
          {modelId && (
            <button
              type="button"
              className="shrink-0 font-medium underline-offset-2 hover:underline disabled:opacity-50"
              disabled={loading !== null}
              onClick={() => void useStudio.getState().loadModel(modelId, AGENT_GOOD_CONTEXT)}
            >
              {loading ? 'Loading…' : 'Reload at 32K'}
            </button>
          )}
        </div>
      )}

      <details className="group text-[0.7rem]">
        <summary className="flex cursor-pointer list-none items-center gap-1 text-ink-500 hover:text-ink-800 dark:text-ink-400 dark:hover:text-ink-200 [&::-webkit-details-marker]:hidden">
          <Icon name="chevron" className="size-3 transition-transform group-open:rotate-90" />
          Pinned notes{pins > 0 ? ` · ${pins}` : ''}
        </summary>
        <div className="mt-1.5">
          <PinnedNotesEditor />
        </div>
      </details>
    </Card>
  )
}

// --- changes -----------------------------------------------------------------------------------------

function ChangesCard({ conversation }: { conversation: Conversation }) {
  const root = conversation.workspace ?? null
  const git = useGitStatus(root)
  const changes = conversation.changes
  const agent = useMemo(() => {
    let added = 0
    let removed = 0
    for (const change of changes ?? []) {
      if (change.untracked) continue
      const stats = diffStats(change.original, change.current)
      added += stats.added
      removed += stats.removed
    }
    return { count: changes?.length ?? 0, added, removed }
  }, [changes])
  const status = git?.status?.is_git ? git.status : null
  const review = () => useUi.getState().setRightPanel('changes')

  let tree: ReactNode
  if (!root) tree = <span className={quiet}>No folder chosen</span>
  else if (git?.unsupported) tree = <span className={quiet}>Working-tree changes need the updated runtime</span>
  else if (git?.status && !git.status.is_git) tree = <span className={quiet}>Not a git repository</span>
  else if (!status) tree = <span className={quiet}>{git?.error ? 'git status could not be read' : 'Reading git…'}</span>
  else if (status.files.length === 0) tree = <span className={quiet}>Working tree clean</span>
  else
    tree = (
      <span className="flex min-w-0 items-center gap-2">
        <span className="truncate">
          {status.files.length} file{status.files.length === 1 ? '' : 's'} changed
        </span>
        <DiffStat added={status.added} removed={status.removed} />
      </span>
    )

  return (
    <Card
      id="changes"
      title="Changes"
      aside={status && status.files.length > 0 ? <DiffStat added={status.added} removed={status.removed} /> : agent.count > 0 ? <DiffStat added={agent.added} removed={agent.removed} /> : null}
    >
      <Row icon="diff" action={status && status.files.length > 0 ? <SmallButton onClick={review}>Review</SmallButton> : null}>
        {tree}
      </Row>
      <Row
        icon="bot"
        action={
          agent.count > 0 ? (
            <SmallButton
              title="Open the agent's edits in the Changes tab, where each can be reverted"
              onClick={() => {
                review()
                useCodeUi.getState().showAgentEdits()
              }}
            >
              <Icon name="undo" className="size-3" />
              Revert all…
            </SmallButton>
          ) : null
        }
      >
        {agent.count > 0 ? (
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate">
              {agent.count} file{agent.count === 1 ? '' : 's'} edited by the agent
            </span>
            <DiffStat added={agent.added} removed={agent.removed} />
          </span>
        ) : (
          <span className={quiet}>No agent edits to review</span>
        )}
      </Row>
    </Card>
  )
}

// --- mining ------------------------------------------------------------------------------------------

const MINING_LINE: Record<ChatMiningState['kind'], string> = {
  inline: 'Inline — answers are the mining work',
  background: 'Mining in the background',
  'off-here': 'Off for this session',
  setup: 'Not mining yet',
}

function MiningCard({ conversation }: { conversation: Conversation }) {
  const state = useChatMiningState(conversation)
  const mode = useStudio((s) => s.miningQueue?.mode ?? null)
  const live = useStudio((s) => (s.miningQueue ? s.miningQueue.counts.queued + s.miningQueue.counts.running : 0))
  const { mined, committed } = useMemo(() => {
    const prompts = conversation.messages.filter((m) => m.role === 'user' && m.mining)
    return { mined: prompts.length, committed: prompts.filter((m) => m.mining?.status === 'committed').length }
  }, [conversation.messages])
  const on = state.kind === 'inline' || state.kind === 'background'

  return (
    <Card
      id="mining"
      title="Mining"
      aside={
        <span className={`flex items-center gap-1 text-[0.7rem] ${on ? 'text-emerald-700 dark:text-emerald-400' : 'text-ink-400 dark:text-ink-500'}`}>
          <Icon name="pickaxe" className="size-3" />
          {mined}
        </span>
      }
    >
      <Row icon="pickaxe" action={<SmallButton onClick={() => useUi.getState().setRightPanel('mining')}>Open</SmallButton>}>
        <span className={on ? 'text-emerald-700 dark:text-emerald-400' : ''}>{MINING_LINE[state.kind]}</span>
      </Row>
      <p className={`pl-6 ${quiet}`}>
        {mined === 0
          ? 'No prompt from this session has been mined yet.'
          : `${mined} prompt${mined === 1 ? '' : 's'} mined in this session${committed > 0 ? ` · ${committed} committed` : ''}.`}
        {mode && ` Queue: ${mode}${live > 0 ? `, ${live} in progress` : ''}.`} Only what you type is mined — never files or tool output.
      </p>
    </Card>
  )
}

// --- the session -------------------------------------------------------------------------------------

function SessionCard({ conversation }: { conversation: Conversation }) {
  const totals = useMemo(() => {
    let steps = 0
    let prompt = 0
    let generated = 0
    for (const m of conversation.messages) {
      if (m.role !== 'assistant') continue
      if (m.toolCalls?.length) steps++
      if (m.stats) {
        prompt += m.stats.promptTokens
        generated += m.stats.completionTokens
      }
    }
    return { steps, prompt, generated }
  }, [conversation.messages])
  const created = new Date(conversation.createdAt)

  return (
    <Card id="session" title="Session">
      <Row icon="clock" title={created.toLocaleString()} action={<span className={quiet}>{created.toLocaleDateString()}</span>}>
        Created {relativeTime(conversation.createdAt / 1000)}
      </Row>
      <Row icon="wrench" action={<span className="text-[0.75rem] tabular-nums">{totals.steps}</span>}>
        Steps with tools
      </Row>
      <Row
        icon="activity"
        title={`${totals.prompt.toLocaleString('en-US')} prompt tokens read, ${totals.generated.toLocaleString('en-US')} generated`}
        action={<span className="text-[0.75rem] tabular-nums">{(totals.prompt + totals.generated).toLocaleString('en-US')}</span>}
      >
        Tokens used
        {totals.prompt + totals.generated > 0 && (
          <span className={`ml-1.5 ${quiet}`}>
            {tokensShort(totals.prompt)} in · {tokensShort(totals.generated)} out
          </span>
        )}
      </Row>
    </Card>
  )
}

function tokensShort(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 10_000) return `${Math.round(n / 1000)}K`
  if (n >= 1000) return `${(n / 1000).toFixed(1)}K`
  return String(n)
}

// --- the panel ---------------------------------------------------------------------------------------

export function SessionPanel({ conversation }: { conversation: Conversation | null }) {
  if (!conversation) {
    return (
      <div className="flex flex-col items-center gap-3 px-6 py-14 text-center">
        <div className="rounded-xl bg-ink-100 p-3 text-ink-400 dark:bg-ink-800 dark:text-ink-500">
          <Icon name="panelRight" className="size-5" />
        </div>
        <p className="max-w-64 text-xs leading-relaxed text-ink-500 dark:text-ink-400">
          No session is open. Start one in a project folder, and its environment, context and changes are summed up here.
        </p>
        <button type="button" className="btn-outline px-2.5 py-1 text-xs" onClick={() => useUi.getState().openWorkspacePicker(null)}>
          <Icon name="folderOpen" className="size-3.5" />
          Open a folder…
        </button>
      </div>
    )
  }
  return (
    <div className="space-y-2.5 p-3">
      <EnvironmentCard conversation={conversation} />
      <ContextCard conversation={conversation} />
      <ChangesCard conversation={conversation} />
      <MiningCard conversation={conversation} />
      <SessionCard conversation={conversation} />
    </div>
  )
}
