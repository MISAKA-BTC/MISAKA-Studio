// The composer's toolbar part for an agent thread: which folder, how much the agent may do on its
// own, and — while it runs — where it is.
//
// The approval policy is one click away and says what each level means in a line, because it is
// the setting that decides whether a model may run `rm` without asking. A window too small for the
// agent gets a chip that says so and reloads the model at 32K.

import { AGENT_GOOD_CONTEXT, contextTooSmall, useAgent } from '../../lib/agent/run'
import { folderName, useWorkspaceInfo } from '../../lib/agent/workspace'
import type { ApprovalPolicy, Conversation } from '../../lib/types'
import { chatModelId, useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { Icon, Spinner, type IconName } from '../common'
import { PopoverButton, StatusDot } from '../ui'

export const POLICIES: { value: ApprovalPolicy; label: string; icon: IconName; description: string }[] = [
  { value: 'read_only', label: 'Read only', icon: 'lock', description: 'Reads and searches. Never edits a file or runs a command.' },
  { value: 'ask', label: 'Ask', icon: 'eye', description: 'Every edit and every command waits for your approval.' },
  { value: 'auto_edit', label: 'Auto edit', icon: 'bolt', description: 'Edits apply at once — revert any in Changes. Commands ask first.' },
  { value: 'full_auto', label: 'Full auto', icon: 'sparkles', description: 'Edits and commands run without asking.' },
]

function FolderChip({ conversation }: { conversation: Conversation }) {
  const root = conversation.workspace ?? null
  const entry = useWorkspaceInfo(root)
  const open = () => useUi.getState().openWorkspacePicker(conversation.id)
  if (!root) {
    return (
      <button type="button" className="chip text-amber-700 dark:text-amber-300" onClick={open} title="Choose the folder the agent works in">
        <Icon name="folder" className="size-3.5" />
        Choose folder
      </button>
    )
  }
  const info = entry?.status === 'ok' ? entry.info : null
  const missing = entry?.status === 'error'
  return (
    <button
      type="button"
      className="chip min-w-0 max-w-[13rem]"
      onClick={open}
      title={missing ? `${root}\n${entry.error}` : `${info?.root ?? root}${info?.branch ? ` · ${info.branch}` : ''}\nClick to change the folder`}
    >
      <Icon name={missing ? 'warning' : 'folder'} className={`size-3.5 shrink-0 ${missing ? 'text-amber-500' : ''}`} />
      <span className="min-w-0 truncate">{info?.name ?? folderName(root)}</span>
      {info?.branch && (
        <span className="flex min-w-0 shrink items-center gap-0.5 text-ink-400 dark:text-ink-500">
          <Icon name="gitBranch" className="size-3 shrink-0" />
          <span className="truncate">{info.branch}</span>
        </span>
      )}
      {missing && <span className="shrink-0 text-amber-600 dark:text-amber-400">not found</span>}
    </button>
  )
}

function PolicyPicker({ conversation }: { conversation: Conversation }) {
  const policy = conversation.approval ?? 'auto_edit'
  const current = POLICIES.find((p) => p.value === policy) ?? POLICIES[2]!
  const allowed = useAgent((s) => s.allowed[conversation.id] ?? null)
  return (
    <PopoverButton
      className="chip shrink-0"
      title={`Approval: ${current.label} — ${current.description}`}
      placement="top-start"
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
          {allowed && allowed.length > 0 && (
            <>
              <div className="menu-sep" />
              <div className="flex items-center gap-2 px-2.5 py-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
                <Icon name="shield" className="size-3.5 shrink-0" />
                <span className="min-w-0 flex-1">
                  Also always allowed in this thread: {allowed.map((kind) => (kind === 'edit' ? 'edits' : 'commands')).join(' and ')}.
                </span>
                <button
                  type="button"
                  className="shrink-0 font-medium text-arc-600 hover:underline dark:text-arc-400"
                  onClick={() => useAgent.setState((s) => ({ allowed: { ...s.allowed, [conversation.id]: [] } }))}
                >
                  Reset
                </button>
              </div>
            </>
          )}
        </>
      )}
    </PopoverButton>
  )
}

/** A window too small for the agent, and the fix: the model again, at 32K. */
function ContextChip() {
  const contextSize = useStudio((s) => s.runtime?.model_id === chatModelId(s) ? s.runtime?.context_size ?? null : null)
  const backend = useStudio((s) => s.runtime?.model_id === chatModelId(s) ? s.runtime?.backend ?? null : null)
  const loading = useStudio((s) => s.loadingModelId)
  const modelId = useStudio((s) => chatModelId(s))
  if (!contextTooSmall(contextSize) || backend === 'gateway' || !modelId) return null
  return (
    <PopoverButton
      className="chip text-amber-700 dark:text-amber-300"
      title="The model's window is small for the agent"
      placement="top-start"
      panelClassName="w-72 p-3"
      button={
        <>
          {loading ? <Spinner className="size-3" /> : <Icon name="warning" className="size-3.5" />}
          {Math.round((contextSize ?? 0) / 1024)}K window
        </>
      }
    >
      {(close) => (
        <div className="text-xs text-ink-600 dark:text-ink-300">
          <p>
            <span className="mono">{modelId}</span> is loaded with a {(contextSize ?? 0).toLocaleString('en-US')}-token window. A few file reads fill it,
            and then the agent loses the start of its task.
          </p>
          <button
            type="button"
            className="btn-primary mt-2.5 w-full py-1 text-xs"
            disabled={loading !== null}
            onClick={() => {
              close()
              void useStudio.getState().loadModel(modelId, AGENT_GOOD_CONTEXT)
            }}
          >
            {loading ? <Spinner className="size-3.5" /> : <Icon name="refresh" className="size-3.5" />}
            Reload at 32K
          </button>
          <p className="mt-1.5 text-[0.7rem] text-ink-400">A larger window takes more memory; the Models tab shows what fits.</p>
        </div>
      )}
    </PopoverButton>
  )
}

function RunStatus({ conversationId }: { conversationId: string }) {
  const run = useAgent((s) => (s.run?.conversationId === conversationId ? s.run : null))
  if (!run) return null
  const waiting = run.status === 'awaiting_approval'
  // Short, because the toolbar is shared: the card in the transcript and the status bar say the rest.
  return (
    <span
      className={`flex min-w-0 items-center gap-1.5 px-1.5 text-[0.7rem] font-medium ${waiting ? 'text-amber-700 dark:text-amber-300' : 'text-ink-500 dark:text-ink-400'}`}
      role="status"
      title={waiting ? `Step ${run.step} · waiting for your approval` : `The agent is on step ${run.step}`}
    >
      {waiting ? <StatusDot tone="amber" pulse /> : <Spinner className="size-3 shrink-0" />}
      <span className="truncate">Step {run.step}</span>
    </span>
  )
}

/** The composer toolbar part for agent threads: the folder, the approval policy, the run's state. */
export function AgentControls({ conversation }: { conversation: Conversation }) {
  return (
    // Clipped rather than painted over the next control when the toolbar runs out of room; the
    // folder's name is what gives way first.
    <div className="flex min-w-0 items-center gap-0.5 overflow-hidden">
      <FolderChip conversation={conversation} />
      <PolicyPicker conversation={conversation} />
      <ContextChip />
      <RunStatus conversationId={conversation.id} />
    </div>
  )
}
