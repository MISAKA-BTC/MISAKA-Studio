// An empty thread: the composer in the middle, and an honest line about what will happen.
//
// The explanations are the ones that were reported missing, kept word for word in substance: an
// app with nothing installed says where models come from; a class artifact on disk is named as what
// it is (not a chat model) rather than left to read as "nothing was installed"; no model loaded is
// not an error, because the first message loads one.
//
// Code's side asks what to build, and says where: the project the session works in (or will be
// made in) is a chip at the top, the other recent projects are one click from a session of their
// own, and the two things that stop the agent before it starts — the mining gateway as the engine,
// a window too small for tool output — are said here, before the first task is typed rather than
// after it is refused.

import type { ReactNode } from 'react'
import logo from '../../assets/misaka-logo.png'
import { AGENT_GOOD_CONTEXT, contextTooSmall } from '../../lib/agent/run'
import { useWorkspaceInfo } from '../../lib/agent/workspace'
import type { Conversation } from '../../lib/types'
import { chatModelId, useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { Icon, Spinner, type IconName } from '../common'
import { useClassStatuses } from '../MiningCatalog'
import { goTo, newSessionIn, openFolder } from '../shell/actions'
import { folderLabel, projectKey } from '../shell/projects'
import { useNewSessionProject } from '../shell/useSessionProject'
import { useChatMiningState } from './mining'

const CHAT_SUGGESTIONS: { icon: IconName; label: string; text: string }[] = [
  { icon: 'sparkles', label: 'Explain', text: 'Explain, step by step, how ' },
  { icon: 'globe', label: 'Translate', text: 'Translate into natural English:\n\n' },
  { icon: 'code', label: 'Write code', text: 'Write a function that ' },
  { icon: 'fileText', label: 'Summarize', text: 'Summarize this in five bullet points:\n\n' },
]

const AGENT_SUGGESTIONS: { icon: IconName; label: string; text: string }[] = [
  { icon: 'fileSearch', label: 'Explain this codebase', text: 'Explain this codebase: what it does, how it is laid out, and where to start reading.' },
  { icon: 'wrench', label: 'Find and fix a bug in…', text: 'Find and fix the bug in ' },
  { icon: 'circleCheck', label: 'Write tests for…', text: 'Write tests for ' },
  { icon: 'diff', label: 'Review my uncommitted changes', text: 'Review my uncommitted changes: look at the diff, then point out bugs, risks and anything left unfinished.' },
]

/** How many other recent projects the Code page offers as one-click sessions. */
const QUICK_PICKS = 4

function MiningLine({ conversation, code }: { conversation: Conversation | null; code: boolean }) {
  const state = useChatMiningState(conversation)
  const setRightPanel = useUi((s) => s.setRightPanel)
  const setConversationMining = useStudio((s) => s.setConversationMining)
  const base = 'inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs transition-colors'

  // On Code's side the gateway cannot run the agent at all; the note above says so and what to do.
  if (code && state.kind === 'inline') return null
  if (state.kind === 'setup')
    return (
      <button type="button" className={`${base} text-ink-500 hover:bg-ink-100 hover:text-ink-800 dark:text-ink-400 dark:hover:bg-ink-800 dark:hover:text-ink-100`} onClick={() => goTo('network')}>
        <Icon name="pickaxe" className="size-3.5" />
        Set up prompt mining
        <Icon name="arrowRight" className="size-3" />
      </button>
    )
  if (state.kind === 'off-here')
    return (
      <span className="inline-flex items-center gap-1.5 text-xs text-ink-500 dark:text-ink-400">
        <Icon name="pickaxe" className="size-3.5" />
        Mining is off for this {code ? 'session' : 'thread'}.
        {conversation && (
          <button type="button" className="font-medium text-arc-700 hover:underline dark:text-arc-400" onClick={() => setConversationMining(conversation.id, true)}>
            Turn it on
          </button>
        )}
      </span>
    )
  return (
    <button
      type="button"
      className={`${base} text-emerald-700 hover:bg-emerald-50 dark:text-emerald-400 dark:hover:bg-emerald-950/40`}
      onClick={() => setRightPanel('mining')}
      title="Open the mining panel"
    >
      <Icon name="pickaxe" className="size-3.5" />
      {state.kind === 'inline'
        ? 'Inline: answers here are the mining work'
        : code
          ? 'What you type here is mined in the background — never files or tool output'
          : 'Prompts here are mined in the background'}
    </button>
  )
}

/** The folder the session works in — or, with no session yet, the one it will be made in. */
function ProjectChip({ conversation, root }: { conversation: Conversation | null; root: string | null }) {
  const entry = useWorkspaceInfo(root)
  const open = () => useUi.getState().openWorkspacePicker(conversation?.id ?? null)
  if (!root) {
    return (
      <button type="button" className="btn-outline mt-3 text-xs" onClick={open}>
        <Icon name="folderOpen" className="size-3.5" />
        Choose a folder to work in
      </button>
    )
  }
  const info = entry?.status === 'ok' ? entry.info : null
  const missing = entry?.status === 'error'
  const where = info?.root ?? root
  return (
    <button
      type="button"
      className="mt-2.5 inline-flex h-7 max-w-full items-center gap-1.5 rounded-lg border border-ink-200 bg-white px-2.5 text-[0.8rem] text-ink-700 shadow-sm shadow-ink-950/[0.03] transition-colors hover:bg-ink-50 hover:text-ink-900 dark:border-ink-700 dark:bg-ink-900 dark:text-ink-200 dark:hover:bg-ink-800 dark:hover:text-ink-50"
      onClick={open}
      title={`${where}${info?.branch ? ` · ${info.branch}` : ''}${missing ? `\n${entry.error}` : ''}\n${conversation ? 'Move this session to another folder' : 'Start the session in another folder'}`}
    >
      <Icon name={missing ? 'warning' : 'folder'} className={`size-3.5 shrink-0 ${missing ? 'text-amber-500' : 'text-ink-400 dark:text-ink-500'}`} />
      <span className="min-w-0 truncate font-medium">{info?.name ?? folderLabel(root)}</span>
      {info?.branch && (
        <span className="flex min-w-0 items-center gap-1 text-ink-400 dark:text-ink-500">
          <span aria-hidden>·</span>
          <Icon name="gitBranch" className="size-3 shrink-0" />
          <span className="min-w-0 truncate">{info.branch}</span>
        </span>
      )}
      {missing && <span className="shrink-0 text-xs text-amber-600 dark:text-amber-400">not found</span>}
      <Icon name="chevronDown" className="size-3 shrink-0 opacity-50" />
    </button>
  )
}

/** What stops the agent before it starts, said before the first task rather than after it. */
function CodeNotes() {
  const backend = useStudio((s) => s.runtime?.model_id === chatModelId(s) ? s.runtime?.backend ?? null : null)
  const contextSize = useStudio((s) => s.runtime?.model_id === chatModelId(s) ? s.runtime?.context_size ?? null : null)
  const loading = useStudio((s) => s.loadingModelId)
  const modelId = useStudio((s) => chatModelId(s))
  const note = 'mt-3 flex max-w-lg items-start gap-2 rounded-lg border px-3 py-2 text-left text-xs leading-relaxed'
  const amber = 'border-amber-200 bg-amber-50 text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200'
  const link = 'font-medium text-amber-800 underline decoration-amber-400/60 underline-offset-2 hover:decoration-amber-600 dark:text-amber-300'

  if (backend === 'gateway') {
    return (
      <div className={`${note} ${amber}`} role="note">
        <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
        <span>
          The agent cannot run on the mining gateway: its 512-token class cannot hold even one tool result. Load a local model (llama.cpp) to work with,
          and turn on background mining — what you type here is still mined while the agent works on the local engine.{' '}
          <button type="button" className={link} onClick={() => goTo('settings')}>
            Backend settings
          </button>
          {' · '}
          <button type="button" className={link} onClick={() => goTo('network')}>
            Mining setup
          </button>
        </span>
      </div>
    )
  }
  if (modelId && contextTooSmall(contextSize)) {
    return (
      <div className={`${note} ${amber}`} role="note">
        <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
        <span className="min-w-0 flex-1">
          <span className="mono">{modelId}</span> is loaded with a {(contextSize ?? 0).toLocaleString('en-US')}-token window. A few file reads fill it, and
          then the agent loses the start of its task — it works best with 32K.
        </span>
        <button
          type="button"
          className="btn-outline shrink-0 border-amber-300 bg-white px-2 py-0.5 text-xs dark:border-amber-800 dark:bg-transparent"
          disabled={loading !== null}
          onClick={() => void useStudio.getState().loadModel(modelId, AGENT_GOOD_CONTEXT)}
        >
          {loading ? <Spinner className="size-3" /> : <Icon name="refresh" className="size-3" />}
          Reload at 32K
        </button>
      </div>
    )
  }
  return null
}

/** The other recent projects, each one click from a session of its own, and any folder at all. */
function QuickProjects({ current }: { current: string | null }) {
  const recent = useUi((s) => s.recentWorkspaces)
  const currentKey = current ? projectKey(current) : null
  const picks = recent.filter((path) => projectKey(path) !== currentKey).slice(0, QUICK_PICKS)
  return (
    <div className="mt-5 flex flex-wrap items-center justify-center gap-1.5">
      {picks.length > 0 && <span className="mr-0.5 text-[0.7rem] text-ink-400 dark:text-ink-500">{current ? 'Or start in' : 'Start in'}</span>}
      {picks.map((path) => (
        <button key={path} type="button" className="chip-outline" title={`New session in ${path}`} onClick={() => newSessionIn(path)}>
          <Icon name="folder" className="size-3.5 text-ink-400" />
          {folderLabel(path)}
        </button>
      ))}
      <button type="button" className="chip-outline" title="Open a folder and start a session in it" onClick={openFolder}>
        <Icon name="folderOpen" className="size-3.5 text-ink-400" />
        Open folder…
      </button>
    </div>
  )
}

export function EmptyHero({ conversation, children }: { conversation: Conversation | null; children: ReactNode }) {
  const models = useStudio((s) => s.models)
  const modelId = useStudio(chatModelId)
  const setComposerDraft = useUi((s) => s.setComposerDraft)
  const side = useUi((s) => s.appMode)
  const pending = useNewSessionProject()
  // Only consulted here, to tell "nothing installed" apart from "something installed that this
  // window cannot load".
  const { classes } = useClassStatuses()
  const installedClass = (classes ?? []).find((cls) => cls.readiness.state === 'artifact_present')
  // A thread says what it is; with none, the side in view does.
  const code = conversation ? conversation.mode === 'agent' : side === 'code'
  const project = code ? (conversation ? (conversation.workspace ?? null) : pending) : null
  const suggestions = code ? AGENT_SUGGESTIONS : CHAT_SUGGESTIONS

  return (
    <div className="flex min-h-full flex-col items-center justify-center px-4 py-10">
      <div className="w-full max-w-[760px]">
        <div className="mb-6 flex flex-col items-center text-center">
          <img src={logo} alt="" className="h-8 w-auto" />
          <h1 className="mt-4 text-xl font-semibold tracking-tight text-ink-900 dark:text-ink-50">{code ? 'What should we build?' : 'Ask MISAKA anything'}</h1>

          {code && <ProjectChip conversation={conversation} root={project} />}

          <div className="mt-3 max-w-lg text-[0.8rem] leading-relaxed text-ink-500 dark:text-ink-400">
            {models.length === 0 ? (
              <>
                No models are installed yet. Open <strong>Models → Discover</strong> to find one on Hugging Face — the list shows what fits
                this machine before you download anything.
                {/* Otherwise this reads as "nothing was installed" to someone who just watched
                    1.7 GiB arrive. A class artifact is a real thing on disk; it is simply not a
                    thing this window can load, and saying which is the difference between a
                    confusing screen and an informative one. */}
                {installedClass && (
                  <>
                    {' '}
                    The <span className="mono">{installedClass.spec.name}</span> class artifact <em>is</em> installed — but it is not a
                    chat model. It is what the <strong>node</strong> executes to produce blocks; this window drives the inference engine,
                    which loads GGUF.
                  </>
                )}
                <div className="mt-3">
                  <button type="button" className="btn-primary text-xs" onClick={() => goTo('models')}>
                    <Icon name="download" className="size-3.5" />
                    Get a model
                  </button>
                </div>
              </>
            ) : modelId ? (
              code ? (
                <>
                  The agent uses <span className="mono text-ink-700 dark:text-ink-300">{modelId}</span>. It works only inside
                  the project folder, and every edit it makes can be reverted from Changes.
                </>
              ) : (
                <>
                  <span className="mono text-ink-700 dark:text-ink-300">{modelId}</span> を選択中。そのままメッセージを送信できます。
                </>
              )
            ) : (
              <>使用するモデルを入力欄から選択してください。</>
            )}
          </div>

          {code && models.length > 0 && <CodeNotes />}
        </div>

        {children}

        <div className="mt-3 flex flex-wrap justify-center gap-1.5">
          {suggestions.map((suggestion) => (
            <button key={suggestion.label} type="button" className="chip-outline" onClick={() => setComposerDraft(suggestion.text)}>
              <Icon name={suggestion.icon} className="size-3.5 text-ink-400" />
              {suggestion.label}
            </button>
          ))}
        </div>

        {code && <QuickProjects current={project} />}

        <div className="mt-5 flex justify-center">
          <MiningLine conversation={conversation} code={code} />
        </div>
      </div>
    </div>
  )
}
