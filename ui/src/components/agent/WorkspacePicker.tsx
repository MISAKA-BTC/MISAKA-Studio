// The folder picker: where an agent thread works.
//
// Three ways in, for three habits: a recent folder (one click), a path pasted or typed (Enter), and
// a browser over the runtime's `browse` — the window cannot open a native folder dialog itself, and
// the runtime is what will be reading the folder anyway. Choosing asks the runtime to open the
// folder first, so a path that does not exist or cannot be read is refused here, in the dialog,
// rather than at the agent's first step.
//
// A thread's Changes are paths inside its folder, so a thread that already changed files is not
// moved to another folder without saying so: a later Revert would otherwise write one project's
// file into another.

import { useEffect, useMemo, useRef, useState } from 'react'
import { api } from '../../lib/api'
import { useAgent } from '../../lib/agent/run'
import { folderName, rememberWorkspace, tildePath } from '../../lib/agent/workspace'
import type { BrowseView } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { Icon, Spinner } from '../common'
import { Modal } from '../ui'

function crumbs(path: string): { label: string; path: string }[] {
  const windows = /^[A-Za-z]:[\\/]/.test(path)
  const separator = windows ? '\\' : '/'
  const parts = path.split(/[\\/]/).filter(Boolean)
  const out: { label: string; path: string }[] = []
  if (!windows) out.push({ label: '/', path: '/' })
  parts.forEach((part, index) => {
    const joined = parts.slice(0, index + 1).join(separator)
    out.push({ label: part, path: windows ? (index === 0 ? `${joined}${separator}` : joined) : `/${joined}` })
  })
  return out
}

function PickerBody({ conversationId, onClose }: { conversationId: string | null; onClose: () => void }) {
  const conversation = useStudio((s) => (conversationId ? (s.conversations.find((c) => c.id === conversationId) ?? null) : null))
  const recent = useUi((s) => s.recentWorkspaces)
  const removeRecent = useUi((s) => s.removeRecentWorkspace)
  const runHere = useAgent((s) => s.run !== null && s.run.conversationId === conversationId)
  const [input, setInput] = useState(conversation?.workspace ?? '')
  const [view, setView] = useState<BrowseView | null>(null)
  const [browseError, setBrowseError] = useState<string | null>(null)
  const [browsing, setBrowsing] = useState(false)
  const [hidden, setHidden] = useState(false)
  const [opening, setOpening] = useState<string | null>(null)
  const [openError, setOpenError] = useState<string | null>(null)
  const [confirmMove, setConfirmMove] = useState<string | null>(null)
  const inputRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLDivElement>(null)
  const home = view?.home ?? null

  const browse = async (path: string | null, showHidden = hidden) => {
    setBrowsing(true)
    setBrowseError(null)
    try {
      setView(await api.workspace.browse(path, showHidden))
    } catch (error) {
      setBrowseError((error as Error).message)
    } finally {
      setBrowsing(false)
    }
  }

  useEffect(() => {
    // Start where the thread already is, else beside the last folder used, else at home.
    const start = conversation?.workspace ?? recent[0] ?? null
    const parent = start && start !== conversation?.workspace ? start.replace(/[\\/][^\\/]+[\\/]?$/, '') || null : start
    void browse(parent)
    inputRef.current?.focus()
    // Once, when the dialog opens: the dialog is remounted for each thread it is opened for.
  }, [])

  const choose = async (raw: string, confirmed = false) => {
    const path = raw.trim()
    if (!path || opening) return
    if (runHere) {
      setOpenError('The agent is working in this thread. Stop it before moving the thread to another folder.')
      return
    }
    setOpening(path)
    setOpenError(null)
    try {
      const info = await api.workspace.open(path)
      const studio = useStudio.getState()
      const thread = conversationId ? studio.conversations.find((c) => c.id === conversationId) : undefined
      if (thread && (thread.changes?.length ?? 0) > 0 && thread.workspace && thread.workspace !== info.root && !confirmed) {
        setConfirmMove(info.root)
        return
      }
      rememberWorkspace(info)
      if (thread) {
        if ((thread.mode ?? 'chat') !== 'agent') studio.setConversationMode(thread.id, 'agent')
        if (thread.workspace !== info.root) {
          studio.setConversationWorkspace(thread.id, info.root)
          // The old folder's changes cannot be reverted from the new one.
          if (thread.workspace) studio.updateConversation(thread.id, { changes: [] })
        }
      } else {
        studio.newConversation({ mode: 'agent', workspace: info.root })
      }
      useUi.getState().addRecentWorkspace(info.root)
      studio.setView('chat')
      onClose()
      useUi.getState().focusComposer()
    } catch (error) {
      setOpenError((error as Error).message)
    } finally {
      setOpening(null)
    }
  }

  const trail = useMemo(() => (view ? crumbs(view.path) : []), [view])
  const folders = view?.entries.filter((e) => e.is_dir) ?? []

  const focusList = (delta: number) => {
    const buttons = Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>('button[data-entry]') ?? [])
    if (!buttons.length) return
    const index = buttons.indexOf(document.activeElement as HTMLButtonElement)
    const next = index === -1 ? (delta > 0 ? 0 : buttons.length - 1) : Math.min(buttons.length - 1, Math.max(0, index + delta))
    buttons[next]?.focus()
  }

  return (
    <div className="flex max-h-[min(40rem,85vh)] flex-col">
      <div className="flex items-start gap-3 px-4 pb-3 pt-4">
        <div className="rounded-lg bg-arc-500/10 p-2 text-arc-600 dark:text-arc-400">
          <Icon name="folderOpen" className="size-4" />
        </div>
        <div className="min-w-0 flex-1">
          <h2 className="text-sm font-semibold text-ink-900 dark:text-ink-50">Open a folder</h2>
          <p className="mt-0.5 text-xs text-ink-500 dark:text-ink-400">The agent reads, edits and runs commands inside this folder only.</p>
        </div>
        <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
          <Icon name="x" className="size-4" />
        </button>
      </div>

      <form
        className="flex gap-2 px-4"
        onSubmit={(event) => {
          event.preventDefault()
          void choose(input)
        }}
      >
        <div className="relative min-w-0 flex-1">
          <Icon name="folder" className="pointer-events-none absolute left-2.5 top-1/2 size-3.5 -translate-y-1/2 text-ink-400" />
          <input
            ref={inputRef}
            className="input mono pl-8 text-[0.8rem]"
            placeholder="Paste a folder's path, e.g. ~/projects/my-app"
            value={input}
            spellCheck={false}
            autoCapitalize="off"
            autoCorrect="off"
            onChange={(event) => setInput(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'ArrowDown') {
                event.preventDefault()
                focusList(1)
              }
            }}
          />
        </div>
        <button type="submit" className="btn-primary px-3 text-xs" disabled={!input.trim() || opening !== null}>
          {opening === input.trim() ? <Spinner className="size-3.5" /> : 'Open'}
        </button>
      </form>

      {openError && (
        <p className="mx-4 mt-2 flex items-start gap-1.5 rounded-lg bg-red-50 px-2.5 py-1.5 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-300">
          <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
          <span className="min-w-0 break-words">{openError}</span>
        </p>
      )}
      {confirmMove && (
        <div className="mx-4 mt-2 rounded-lg border border-amber-200 bg-amber-50 p-2.5 text-xs text-amber-900 dark:border-amber-900/60 dark:bg-amber-950/30 dark:text-amber-200">
          <p>
            This thread changed {conversation?.changes?.length ?? 0} file{(conversation?.changes?.length ?? 0) === 1 ? '' : 's'} in{' '}
            <span className="mono">{folderName(conversation?.workspace ?? '')}</span>. Moving it to{' '}
            <span className="mono">{folderName(confirmMove)}</span> takes them off its Changes list — the files themselves stay as they are, but can no
            longer be reverted from here.
          </p>
          <div className="mt-2 flex justify-end gap-1.5">
            <button type="button" className="btn-ghost px-2 py-0.5 text-xs" onClick={() => setConfirmMove(null)}>
              Cancel
            </button>
            <button
              type="button"
              className="btn-outline bg-white px-2 py-0.5 text-xs dark:bg-transparent"
              onClick={() => {
                const target = confirmMove
                setConfirmMove(null)
                void choose(target, true)
              }}
            >
              Move the thread
            </button>
          </div>
        </div>
      )}

      <div className="mt-3 min-h-0 flex-1 overflow-y-auto border-t border-ink-100 dark:border-ink-800" ref={listRef}>
        {recent.length > 0 && (
          <div className="px-2 pt-2">
            <div className="menu-label">Recent</div>
            {recent.map((path) => (
              <div key={path} className="group flex items-center rounded-lg hover:bg-ink-100 dark:hover:bg-ink-800/70">
                <button
                  type="button"
                  data-entry
                  className="flex min-w-0 flex-1 items-center gap-2 rounded-lg px-2.5 py-1.5 text-left outline-none focus-visible:bg-ink-100 dark:focus-visible:bg-ink-800"
                  onClick={() => void choose(path)}
                  onKeyDown={(event) => {
                    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                      event.preventDefault()
                      focusList(event.key === 'ArrowDown' ? 1 : -1)
                    }
                  }}
                  title={path}
                >
                  <Icon name="history" className="size-3.5 shrink-0 text-ink-400" />
                  <span className="shrink-0 text-[0.8rem] font-medium text-ink-800 dark:text-ink-100">{folderName(path)}</span>
                  <span className="mono min-w-0 truncate text-[0.7rem] text-ink-400 dark:text-ink-500">{tildePath(path, home)}</span>
                  {conversation?.workspace === path && <span className="badge shrink-0 bg-arc-500/10 text-arc-700 dark:text-arc-300">current</span>}
                  {opening === path && <Spinner className="ml-auto size-3.5 shrink-0" />}
                </button>
                <button
                  type="button"
                  className="icon-btn mr-1 size-6 opacity-0 group-hover:opacity-100 focus-visible:opacity-100"
                  title="Remove from recent folders"
                  aria-label={`Remove ${path} from recent folders`}
                  onClick={() => removeRecent(path)}
                >
                  <Icon name="x" className="size-3" />
                </button>
              </div>
            ))}
          </div>
        )}

        <div className="px-2 pb-2 pt-2">
          <div className="flex items-center gap-1 px-1 pb-1">
            <span className="menu-label px-1.5">Browse</span>
            <div className="ml-auto flex items-center gap-1">
              <label className="flex cursor-pointer items-center gap-1.5 px-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
                <input
                  type="checkbox"
                  className="size-3 accent-arc-600"
                  checked={hidden}
                  onChange={(event) => {
                    setHidden(event.target.checked)
                    void browse(view?.path ?? null, event.target.checked)
                  }}
                />
                Hidden folders
              </label>
              <button type="button" className="icon-btn mono size-6 text-[0.8rem]" title="Home folder" aria-label="Home folder" onClick={() => void browse(null)}>
                ~
              </button>
              <button
                type="button"
                className="icon-btn size-6"
                title="Up one folder"
                aria-label="Up one folder"
                disabled={!view?.parent}
                onClick={() => view?.parent && void browse(view.parent)}
              >
                <Icon name="arrowUp" className="size-3.5" />
              </button>
            </div>
          </div>

          {view && (
            <div className="mono flex flex-wrap items-center gap-0.5 px-2 pb-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
              {trail.map((crumb, index) => (
                <span key={crumb.path} className="flex items-center gap-0.5">
                  {index > 1 && <Icon name="chevron" className="size-3 text-ink-300 dark:text-ink-600" />}
                  <button
                    type="button"
                    className={`rounded px-1 hover:bg-ink-100 hover:text-ink-800 dark:hover:bg-ink-800 dark:hover:text-ink-100 ${index === trail.length - 1 ? 'font-medium text-ink-800 dark:text-ink-100' : ''}`}
                    onClick={() => void browse(crumb.path)}
                  >
                    {crumb.label}
                  </button>
                </span>
              ))}
            </div>
          )}

          {browseError ? (
            <p className="mx-1 flex items-start gap-1.5 rounded-lg bg-red-50 px-2.5 py-1.5 text-xs text-red-700 dark:bg-red-950/40 dark:text-red-300">
              <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
              <span className="min-w-0 break-words">
                {browseError}
                {/404|not found/i.test(browseError) && ' — this runtime does not offer the folder browser; paste a path above instead.'}
              </span>
            </p>
          ) : !view && browsing ? (
            <div className="flex items-center gap-2 px-3 py-3 text-xs text-ink-500">
              <Spinner className="size-3.5" /> Reading folders…
            </div>
          ) : view && folders.length === 0 ? (
            <p className="px-3 py-3 text-xs text-ink-500 dark:text-ink-400">No folders in here.</p>
          ) : (
            <div className={browsing ? 'opacity-60' : ''}>
              {folders.map((entry) => (
                <button
                  key={entry.path}
                  type="button"
                  data-entry
                  className="flex w-full items-center gap-2 rounded-lg px-2.5 py-1 text-left text-[0.8rem] text-ink-700 outline-none hover:bg-ink-100 focus-visible:bg-ink-100 dark:text-ink-200 dark:hover:bg-ink-800/70 dark:focus-visible:bg-ink-800"
                  onClick={() => void browse(entry.path)}
                  onDoubleClick={() => void choose(entry.path)}
                  onKeyDown={(event) => {
                    if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                      event.preventDefault()
                      focusList(event.key === 'ArrowDown' ? 1 : -1)
                    } else if (event.key === 'Enter' && (event.metaKey || event.ctrlKey)) {
                      event.preventDefault()
                      void choose(entry.path)
                    }
                  }}
                  title={`${entry.path} — click to open, double-click to choose`}
                >
                  <Icon name="folder" className="size-3.5 shrink-0 text-arc-600/70 dark:text-arc-400/70" />
                  <span className="min-w-0 flex-1 truncate">{entry.name}</span>
                  {entry.is_git && (
                    <span className="badge shrink-0 gap-1 bg-ink-100 text-ink-500 dark:bg-ink-800 dark:text-ink-400">
                      <Icon name="gitBranch" className="size-3" />
                      git
                    </span>
                  )}
                </button>
              ))}
            </div>
          )}
        </div>
      </div>

      <div className="flex items-center gap-2 border-t border-ink-100 bg-ink-50/60 px-4 py-2.5 dark:border-ink-800 dark:bg-ink-950/40">
        <span className="mono min-w-0 flex-1 truncate text-[0.7rem] text-ink-400 dark:text-ink-500" title={view?.path}>
          {view ? tildePath(view.path, home) : ''}
        </span>
        <button type="button" className="btn-ghost px-3 py-1 text-xs" onClick={onClose}>
          Cancel
        </button>
        <button type="button" className="btn-primary px-3 py-1 text-xs" disabled={!view || opening !== null} onClick={() => view && void choose(view.path)}>
          {opening && view && opening === view.path ? <Spinner className="size-3.5" /> : <Icon name="check" className="size-3.5" />}
          Open {view ? `“${folderName(view.path) || view.path}”` : 'this folder'}
        </button>
      </div>
    </div>
  )
}

/** The folder picker: a global modal, opened through `useUi.openWorkspacePicker`. */
export function WorkspacePicker() {
  const picker = useUi((s) => s.workspacePicker)
  const close = useUi((s) => s.closeWorkspacePicker)
  return (
    <Modal open={picker !== null} onClose={close} label="Open a folder" className="max-w-xl">
      {picker && <PickerBody key={picker.conversationId ?? 'new'} conversationId={picker.conversationId} onClose={close} />}
    </Modal>
  )
}
