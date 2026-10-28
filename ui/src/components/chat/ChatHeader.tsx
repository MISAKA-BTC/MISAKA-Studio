// The strip above the thread: its name, what is answering it, and the panels that explain it.
//
// A Code session gets its own strip (`code/CodeHeader.tsx`) — the session's menu, its project and
// the pane toggles — so this one stays the Chat side's, as it was.
//
// No sidebar toggle here: with the sidebar shut the window keeps an icon rail, and a second way to
// open it in the header would be a second place to look for one thing.

import { memo, useEffect, useRef, useState } from 'react'
import { stopAgentRun, useAgent } from '../../lib/agent/run'
import type { Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi, type RightPanel } from '../../store/ui'
import { CodeHeader } from '../code/CodeHeader'
import { effectivePanel, togglePane, useSide } from '../code/shared'
import { TitleInput } from '../code/TitleInput'
import { Icon } from '../common'
import { IconButton, MenuItem, MenuSeparator, Popover } from '../ui'
import { SHORTCUTS } from '../shell/shortcuts'
import { conversationMarkdown, downloadMarkdown } from './exportMarkdown'
import { ModelStatusPill } from './ModelPicker'

function ThreadTitle({ conversation, renaming, setRenaming }: { conversation: Conversation | null; renaming: boolean; setRenaming: (on: boolean) => void }) {
  if (renaming && conversation) return <TitleInput conversation={conversation} onDone={() => setRenaming(false)} />
  return (
    <h1
      className="min-w-0 cursor-default truncate rounded-md px-1 py-0.5 text-[0.8rem] font-medium text-ink-800 dark:text-ink-100"
      title={conversation ? `${conversation.title} — double-click to rename` : undefined}
      onDoubleClick={() => conversation && setRenaming(true)}
    >
      {conversation?.title ?? 'New chat'}
    </h1>
  )
}

/** A panel toggle with a count or a dot on it, for a panel that has something to say. Pressed when
 *  the panel shows its tab — including a Code pane the Chat side shows as its neighbour. */
function PanelToggle({
  icon,
  label,
  panel,
  shortcut,
  count,
  dot,
}: {
  icon: 'pickaxe' | 'info'
  label: string
  panel: RightPanel
  shortcut?: string
  count?: number
  dot?: boolean
}) {
  const active = useUi((s) => effectivePanel(s.rightPanel, 'chat') === panel)
  return (
    <span className="relative inline-flex">
      <IconButton icon={icon} label={label} active={active} shortcut={shortcut} onClick={() => togglePane(panel, 'chat')} />
      {count ? (
        <span className="pointer-events-none absolute -right-0.5 -top-0.5 min-w-3.5 rounded-full bg-ink-500 px-1 text-center text-[0.55rem] font-semibold leading-[0.875rem] text-white dark:bg-ink-400 dark:text-ink-950">
          {count}
        </span>
      ) : dot ? (
        <span className="pointer-events-none absolute right-1 top-1 size-1.5 rounded-full bg-amber-500" />
      ) : null}
    </span>
  )
}

function MoreMenu({ conversation, onRename }: { conversation: Conversation | null; onRename: () => void }) {
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const [confirm, setConfirm] = useState<'clear' | 'delete' | null>(null)
  // Read when acted on, not subscribed to: a menu that re-rendered on every streamed token would be
  // doing work for nothing.
  const studio = useStudio.getState
  const close = () => {
    setOpen(false)
    setConfirm(null)
  }
  const empty = !conversation || conversation.messages.length === 0

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={`icon-btn ${open ? 'bg-ink-100 text-ink-900 dark:bg-ink-800 dark:text-ink-50' : ''}`}
        aria-label="Thread actions"
        aria-expanded={open}
        title="Thread actions"
        onClick={() => (open ? close() : setOpen(true))}
      >
        <Icon name="more" className="size-4" />
      </button>
      <Popover anchor={anchor} open={open} onClose={close} placement="bottom-end">
        <MenuItem
          icon="star"
          label={conversation?.starred ? 'Unstar' : 'Star'}
          disabled={!conversation}
          onSelect={() => {
            if (conversation) studio().starConversation(conversation.id, !conversation.starred)
            close()
          }}
        />
        <MenuItem
          icon="edit"
          label="Rename"
          disabled={!conversation}
          onSelect={() => {
            close()
            onRename()
          }}
        />
        <MenuItem
          icon="pickaxe"
          label="Mine this thread's prompts"
          checked={conversation?.mine !== false}
          disabled={!conversation}
          onSelect={() => {
            if (conversation) studio().setConversationMining(conversation.id, conversation.mine === false)
            close()
          }}
        />
        <MenuSeparator />
        <MenuItem
          icon="download"
          label="Export Markdown"
          disabled={empty}
          onSelect={() => {
            if (conversation) studio().toast('success', `Exported ${downloadMarkdown(conversation)}`)
            close()
          }}
        />
        <MenuItem
          icon="copy"
          label="Copy as Markdown"
          disabled={empty}
          onSelect={async () => {
            close()
            if (!conversation) return
            try {
              await navigator.clipboard.writeText(conversationMarkdown(conversation))
              studio().toast('success', 'Copied the thread as Markdown')
            } catch {
              studio().toast('error', 'The clipboard is not available here.')
            }
          }}
        />
        <MenuSeparator />
        <MenuItem
          icon="trash"
          label={confirm === 'clear' ? 'Click again to clear' : 'Clear messages'}
          hint={confirm === 'clear' ? 'pinned notes stay' : undefined}
          danger={confirm === 'clear'}
          disabled={empty}
          onSelect={() => {
            if (confirm !== 'clear') return setConfirm('clear')
            if (conversation) studio().clearConversation(conversation.id)
            close()
          }}
        />
        <MenuItem
          icon="x"
          label={confirm === 'delete' ? 'Click again to delete' : 'Delete thread'}
          danger
          disabled={!conversation}
          onSelect={() => {
            if (confirm !== 'delete') return setConfirm('delete')
            if (conversation) {
              // A reply still streaming into a thread that no longer exists would have nowhere to land.
              if (studio().streaming?.conversationId === conversation.id) studio().stop()
              if (useAgent.getState().run?.conversationId === conversation.id) stopAgentRun()
              studio().deleteConversation(conversation.id)
            }
            close()
          }}
        />
      </Popover>
    </>
  )
}

/** The rate of the reply streaming now, or of the last one — the engine's own count. */
function LiveRate({ generating }: { generating: boolean }) {
  const rate = useStudio((s) => s.sample?.generation.last_tokens_per_second ?? 0)
  if (!rate) return null
  return (
    <span
      className={`mono hidden shrink-0 text-[0.7rem] tabular-nums sm:inline ${generating ? 'text-ink-600 dark:text-ink-300' : 'text-ink-400 dark:text-ink-500'}`}
      title={generating ? 'Tokens per second, as the engine reports it' : 'Tokens per second of the last reply'}
    >
      {rate.toFixed(1)} tok/s
    </span>
  )
}

/** Memoised: the chat redraws for every streamed token, and nothing up here depends on one. */
export const ChatHeader = memo(function ChatHeader({ conversation, generating }: { conversation: Conversation | null; generating: boolean }) {
  const side = useSide(conversation)
  return side === 'code' ? <CodeHeader conversation={conversation} /> : <ChatSideHeader conversation={conversation} generating={generating} />
})

function ChatSideHeader({ conversation, generating }: { conversation: Conversation | null; generating: boolean }) {
  const [renaming, setRenaming] = useState(false)
  const parametersOpen = useUi((s) => s.parametersOpen)
  const setParametersOpen = useUi((s) => s.setParametersOpen)
  const liveJobs = useStudio((s) => (s.miningQueue ? s.miningQueue.counts.queued + s.miningQueue.counts.running : 0))

  useEffect(() => setRenaming(false), [conversation?.id])

  return (
    <header className="flex h-11 shrink-0 items-center gap-2 border-b border-ink-200 px-3 dark:border-ink-800">
      <div className="flex min-w-0 flex-1 items-center gap-1.5">
        {conversation?.starred && <Icon name="star" className="size-3.5 shrink-0 text-amber-500" />}
        {/* The title keeps its width up to its cap; the model pill is what gives way first. */}
        <div className={`flex min-w-0 ${renaming ? 'flex-1' : 'max-w-[45%] flex-none'}`}>
          <ThreadTitle conversation={conversation} renaming={renaming} setRenaming={setRenaming} />
        </div>
        {!renaming && (
          <>
            <span className="h-4 w-px shrink-0 bg-ink-200 dark:bg-ink-700" aria-hidden />
            <div className="flex min-w-0 shrink">
              <ModelStatusPill />
            </div>
          </>
        )}
      </div>

      <LiveRate generating={generating} />

      <div className="flex shrink-0 items-center gap-0.5">
        <IconButton icon="sliders" label="Parameters" active={parametersOpen} onClick={() => setParametersOpen(!parametersOpen)} />
        <PanelToggle icon="pickaxe" label="Mining" panel="mining" shortcut={SHORTCUTS.miningPanel} dot={liveJobs > 0} />
        <PanelToggle icon="info" label="Context and pinned notes" panel="context" count={conversation?.pinned?.length ?? 0} />
        <MoreMenu conversation={conversation} onRename={() => setRenaming(true)} />
      </div>
    </header>
  )
}
