// The strip above a Code session, as in Claude's desktop app: the session's name with its menu,
// the project it works in, and the pane toggles on the right — summary, changes, terminal, files,
// mining.
//
// The toggles carry what is worth a glance without opening anything: how many files differ from the
// last commit, whether a command is still running, whether mining is busy.

import { memo, useEffect, useRef, useState } from 'react'
import { stopAgentRun, useAgent } from '../../lib/agent/run'
import { folderName, tildePath, useWorkspaceInfo } from '../../lib/agent/workspace'
import type { Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi, type RightPanel } from '../../store/ui'
import { conversationMarkdown, downloadMarkdown } from '../chat/exportMarkdown'
import { Icon } from '../common'
import { sessionTitle } from '../shell/projects'
import { IconButton, MenuItem, MenuLabel, MenuSeparator, Popover, PopoverButton } from '../ui'
import { useGitStatus } from './gitStore'
import { PANE_META, PANE_SHORTCUTS, copyText, effectivePanel, togglePane, useHomeDir } from './shared'
import { useTerminalBusy } from './terminalStore'
import { TitleInput } from './TitleInput'

const TOGGLE_LABEL: Partial<Record<RightPanel, string>> = { session: 'Session summary' }

function PaneToggle({ panel, count, dot }: { panel: RightPanel; count?: number; dot?: boolean }) {
  const active = useUi((s) => effectivePanel(s.rightPanel, 'code') === panel)
  const meta = PANE_META[panel]
  return (
    <span className="relative inline-flex">
      <IconButton icon={meta.icon} label={TOGGLE_LABEL[panel] ?? meta.label} active={active} shortcut={PANE_SHORTCUTS[panel]} onClick={() => togglePane(panel, 'code')} />
      {count ? (
        <span className="pointer-events-none absolute -right-0.5 -top-0.5 min-w-3.5 rounded-full bg-ink-500 px-1 text-center text-[0.55rem] font-semibold leading-[0.875rem] text-white tabular-nums dark:bg-ink-400 dark:text-ink-950">
          {count > 99 ? '99+' : count}
        </span>
      ) : dot ? (
        <span className="pointer-events-none absolute right-1 top-1 size-1.5 rounded-full bg-amber-500" />
      ) : null}
    </span>
  )
}

/** The session's name, and the menu behind it: rename, star, mining, export, clear, delete. */
function SessionMenu({ conversation, onRename }: { conversation: Conversation | null; onRename: () => void }) {
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const [confirm, setConfirm] = useState<'clear' | 'delete' | null>(null)
  // Read when acted on, not subscribed to: nothing in the menu needs to redraw while a run streams.
  const studio = useStudio.getState
  const close = () => {
    setOpen(false)
    setConfirm(null)
  }

  if (!conversation) {
    return <h1 className="min-w-0 truncate px-1 text-[0.8rem] font-medium text-ink-800 dark:text-ink-100">New session</h1>
  }
  const empty = conversation.messages.length === 0
  // The sidebar's name for it: an untouched session is "New session", whatever it was made as.
  const title = sessionTitle(conversation)

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={`flex min-w-0 items-center gap-1 rounded-md px-1.5 py-1 text-[0.8rem] font-medium text-ink-800 transition-colors hover:bg-ink-100 dark:text-ink-100 dark:hover:bg-ink-800 ${
          open ? 'bg-ink-100 dark:bg-ink-800' : ''
        }`}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={`${title} — session actions`}
        title={`${title} — double-click to rename`}
        onClick={() => (open ? close() : setOpen(true))}
        onDoubleClick={() => {
          close()
          onRename()
        }}
      >
        <span className="truncate">{title}</span>
        <Icon name="chevronDown" className="size-3.5 shrink-0 text-ink-400" />
      </button>
      <Popover anchor={anchor} open={open} onClose={close} placement="bottom-start">
        <MenuItem
          icon="edit"
          label="Rename"
          onSelect={() => {
            close()
            onRename()
          }}
        />
        <MenuItem
          icon="star"
          label={conversation.starred ? 'Unstar' : 'Star'}
          onSelect={() => {
            studio().starConversation(conversation.id, !conversation.starred)
            close()
          }}
        />
        <MenuItem
          icon="pickaxe"
          label="Mine this session's prompts"
          checked={conversation.mine !== false}
          onSelect={() => {
            studio().setConversationMining(conversation.id, conversation.mine === false)
            close()
          }}
        />
        <MenuSeparator />
        <MenuItem
          icon="download"
          label="Export Markdown"
          disabled={empty}
          onSelect={() => {
            studio().toast('success', `Exported ${downloadMarkdown(conversation)}`)
            close()
          }}
        />
        <MenuSeparator />
        <MenuItem
          icon="trash"
          label={confirm === 'clear' ? 'Click again to clear' : 'Clear messages'}
          hint={confirm === 'clear' ? 'pinned notes and files stay' : undefined}
          danger={confirm === 'clear'}
          disabled={empty}
          onSelect={() => {
            if (confirm !== 'clear') return setConfirm('clear')
            studio().clearConversation(conversation.id)
            close()
          }}
        />
        <MenuItem
          icon="x"
          label={confirm === 'delete' ? 'Click again to delete' : 'Delete session'}
          danger
          onSelect={() => {
            if (confirm !== 'delete') return setConfirm('delete')
            // A reply still streaming into a session that no longer exists would have nowhere to land.
            if (studio().streaming?.conversationId === conversation.id) studio().stop()
            if (useAgent.getState().run?.conversationId === conversation.id) stopAgentRun()
            studio().deleteConversation(conversation.id)
            close()
          }}
        />
      </Popover>
    </>
  )
}

/** The project the session works in — folder and branch — with what can be done with it. */
function ProjectChip({ conversation }: { conversation: Conversation }) {
  const root = conversation.workspace ?? null
  const info = useWorkspaceInfo(root)
  const git = useGitStatus(root)
  const home = useHomeDir()
  const toast = useStudio((s) => s.toast)

  if (!root) {
    return (
      <button
        type="button"
        className="chip shrink-0 text-amber-700 dark:text-amber-300"
        onClick={() => useUi.getState().openWorkspacePicker(conversation.id)}
        title="Choose the folder this session works in"
      >
        <Icon name="folder" className="size-3.5" />
        Choose folder
      </button>
    )
  }
  const known = info?.status === 'ok' ? info.info : null
  const missing = info?.status === 'error'
  const name = known?.name ?? folderName(root)
  const status = git?.status?.is_git ? git.status : null
  const branch = status ? status.branch : (known?.branch ?? null)
  const open = (panel: RightPanel, close: () => void) => {
    close()
    useUi.getState().setRightPanel(panel)
  }

  return (
    <PopoverButton
      className="chip min-w-0 max-w-[18rem] text-ink-500 max-md:hidden dark:text-ink-400"
      title={`${known?.root ?? root}${branch ? ` · ${branch}` : ''}`}
      label={`Project: ${name}${branch ? `, branch ${branch}` : ''}`}
      placement="bottom-start"
      panelClassName="w-72"
      button={
        <>
          <Icon name={missing ? 'warning' : 'folder'} className={`size-3.5 shrink-0 ${missing ? 'text-amber-500' : ''}`} />
          <span className="min-w-0 truncate">{name}</span>
          {branch && (
            <span className="flex min-w-0 shrink items-center gap-1 text-ink-400 dark:text-ink-500">
              <Icon name="gitBranch" className="size-3 shrink-0" />
              <span className="truncate">{branch}</span>
            </span>
          )}
        </>
      }
    >
      {(close) => (
        <>
          <div className="px-2.5 pb-2 pt-1.5">
            <div className="flex items-center gap-1.5 text-[0.8rem] font-medium">
              {name}
              {missing && <span className="text-[0.7rem] font-normal text-amber-600 dark:text-amber-400">not found</span>}
            </div>
            <div className="mono mt-0.5 break-all text-[0.7rem] text-ink-500 dark:text-ink-400">{tildePath(known?.root ?? root, home)}</div>
            {branch && (
              <div className="mt-1 flex items-center gap-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
                <Icon name="gitBranch" className="size-3" />
                <span className="truncate">{branch}</span>
                {status && status.ahead > 0 && <span className="mono">↑{status.ahead}</span>}
                {status && status.behind > 0 && <span className="mono">↓{status.behind}</span>}
                {status && <span className="ml-auto">{status.files.length === 0 ? 'clean' : `${status.files.length} changed`}</span>}
              </div>
            )}
          </div>
          <MenuSeparator />
          <MenuItem
            icon="copy"
            label="Copy path"
            onSelect={() => {
              close()
              void copyText(known?.root ?? root, 'the folder path', toast)
            }}
          />
          <MenuItem icon="folder" label="Show files" onSelect={() => open('files', close)} />
          <MenuItem icon="terminal" label="Open the terminal" onSelect={() => open('terminal', close)} />
          <MenuItem icon="diff" label="Review changes" onSelect={() => open('changes', close)} />
          <MenuSeparator />
          <MenuItem
            icon="folderOpen"
            label="Change folder…"
            onSelect={() => {
              close()
              useUi.getState().openWorkspacePicker(conversation.id)
            }}
          />
        </>
      )}
    </PopoverButton>
  )
}

/** The rest: copying the session out, the folder, help. */
function MoreMenu({ conversation }: { conversation: Conversation | null }) {
  const toast = useStudio((s) => s.toast)
  const root = conversation?.workspace ?? null
  return (
    <PopoverButton className="icon-btn" title="More" label="More session actions" placement="bottom-end" button={<Icon name="more" className="size-4" />}>
      {(close) => (
        <>
          <MenuLabel>Session</MenuLabel>
          <MenuItem
            icon="copy"
            label="Copy as Markdown"
            disabled={!conversation || conversation.messages.length === 0}
            onSelect={() => {
              close()
              if (conversation) void copyText(conversationMarkdown(conversation), 'the session as Markdown', toast)
            }}
          />
          <MenuItem
            icon="folder"
            label="Copy folder path"
            disabled={!root}
            onSelect={() => {
              close()
              if (root) void copyText(root, 'the folder path', toast)
            }}
          />
          <MenuItem
            icon="folderOpen"
            label={root ? 'Change folder…' : 'Choose folder…'}
            disabled={!conversation}
            onSelect={() => {
              close()
              if (conversation) useUi.getState().openWorkspacePicker(conversation.id)
            }}
          />
          <MenuSeparator />
          <MenuItem
            icon="keyboard"
            label="Keyboard shortcuts"
            shortcut="mod+/"
            onSelect={() => {
              close()
              useUi.getState().setShortcutsOpen(true)
            }}
          />
        </>
      )}
    </PopoverButton>
  )
}

/** Memoised: the session redraws for every streamed token, and nothing up here depends on one. */
export const CodeHeader = memo(function CodeHeader({ conversation }: { conversation: Conversation | null }) {
  const [renaming, setRenaming] = useState(false)
  const parametersOpen = useUi((s) => s.parametersOpen)
  const setParametersOpen = useUi((s) => s.setParametersOpen)
  const liveJobs = useStudio((s) => (s.miningQueue ? s.miningQueue.counts.queued + s.miningQueue.counts.running : 0))
  const root = conversation?.workspace ?? null
  const git = useGitStatus(root)
  const terminalBusy = useTerminalBusy(root)
  // What differs from the last commit when git can say; the agent's own edits when it cannot.
  const changes = git?.status?.is_git ? git.status.files.length : (conversation?.changes?.length ?? 0)

  useEffect(() => setRenaming(false), [conversation?.id])

  return (
    <header className="flex h-11 shrink-0 items-center gap-2 border-b border-ink-200 px-3 dark:border-ink-800">
      <div className="flex min-w-0 flex-1 items-center gap-1">
        {conversation?.starred && <Icon name="star" className="size-3.5 shrink-0 text-amber-500" />}
        {renaming && conversation ? (
          <div className="flex min-w-0 flex-1">
            <TitleInput conversation={conversation} onDone={() => setRenaming(false)} label="Session title" />
          </div>
        ) : (
          <>
            <div className="flex min-w-0 max-w-[60%] flex-none">
              <SessionMenu conversation={conversation} onRename={() => setRenaming(true)} />
            </div>
            {conversation && <ProjectChip conversation={conversation} />}
          </>
        )}
      </div>

      <div className="flex shrink-0 items-center gap-0.5" role="toolbar" aria-label="Panes">
        <PaneToggle panel="session" />
        <PaneToggle panel="changes" count={changes} />
        <PaneToggle panel="terminal" dot={terminalBusy} />
        <PaneToggle panel="files" />
        <PaneToggle panel="mining" dot={liveJobs > 0} />
        <span className="mx-1 h-4 w-px shrink-0 bg-ink-200 dark:bg-ink-700" aria-hidden />
        <IconButton icon="sliders" label="Parameters" active={parametersOpen} onClick={() => setParametersOpen(!parametersOpen)} />
        <MoreMenu conversation={conversation} />
      </div>
    </header>
  )
})
