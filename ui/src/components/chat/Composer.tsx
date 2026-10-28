// The composer: what is typed, what is attached, and everything that decides where it goes.
//
// Everything a person changes *while* writing lives inside the card — the files, the model, the
// agent's folder and leash, whether the prompt is mined — so none of it is a trip to another tab.
// What kind of thread it writes to is not one of them: that is the side of the window (Chat or
// Code), chosen with the switch at the top of the sidebar, and the composer follows it. The keys are
// the ones every chat app has taught people: Enter sends, Shift+Enter is a new line, ↑ in an empty
// box edits the last message, `/` opens commands, `@` mentions a file in a Code session.
//
// Escape is not handled here. An open menu closes on it through `useLayer`, before this sees the
// key; with nothing open it reaches the window's handler, which stops a running reply — and a
// second stop here would be a stop of whatever starts next.

import { memo, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react'
import { api } from '../../lib/api'
import { attachmentFromText, baseName, MAX_ATTACHMENT_BYTES } from '../../lib/attachments'
import { startAgentRun, stopAgentRun, useAgent } from '../../lib/agent/run'
import { bytes } from '../../lib/format'
import { matches } from '../../lib/keys'
import {
  findCommand,
  fuzzyFilter,
  insertMention,
  matchCommands,
  mentionAt,
  parseSlash,
  parseToggle,
  pickModel,
  slashInput,
  type SlashCommand,
} from '../../lib/slash'
import type { Conversation, ConversationMode } from '../../lib/types'
import { chatModelId, useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { AgentControls } from '../agent/AgentControls'
import { Icon } from '../common'
import { newThreadHere, openFolder, projectForNewSession, switchAppMode } from '../shell/actions'
import { useNewSessionProject } from '../shell/useSessionProject'
import { IconButton, Kbd, Popover } from '../ui'
import { AttachmentChip } from './AttachmentChip'
import { attachFiles, draftKey, useComposer, useDraft } from './composerStore'
import { downloadMarkdown } from './exportMarkdown'
import { MiningChip } from './MiningChip'
import { ModelPicker, ModelSelectionNotice } from './ModelPicker'

/** The tallest the box grows before it scrolls: about fourteen lines. */
const MAX_HEIGHT_PX = 14 * 24 + 4
/** The last `composerDraft` loaded. Module-wide, so a composer that remounts does not reload it.
 *  A run that cannot start (no model, the folder gone) gives its words and files back through it. */
let loadedDraftTick = 0

type MenuEntry = {
  key: string
  label: ReactNode
  detail?: ReactNode
  icon?: 'slash' | 'file' | 'cube' | 'check' | 'edit'
  disabled?: boolean
  choose: () => void
}

/** `true` once `value` has held for `ms` — so a connection blip at startup draws no warning. */
function useSettled(value: boolean, ms: number): boolean {
  const [settled, setSettled] = useState(false)
  useEffect(() => {
    if (!value) {
      setSettled(false)
      return
    }
    const timer = setTimeout(() => setSettled(true), ms)
    return () => clearTimeout(timer)
  }, [value, ms])
  return settled
}

export const Composer = memo(function Composer({
  conversation,
  variant,
  onSend,
  onEditLast,
}: {
  conversation: Conversation | null
  variant: 'hero' | 'dock'
  /** Just before a message goes out — the transcript follows it to the bottom. */
  onSend?: () => void
  /** ↑ in an empty composer: edit the last thing the person wrote. */
  onEditLast?: () => void
}) {
  // With no thread, the side decides what a send makes — a chat or a session — and which page's
  // draft this is.
  const side = useUi((s) => s.appMode)
  const key = draftKey(conversation?.id, side)
  const draft = useDraft(key)
  const setText = useComposer((s) => s.setText)
  const removeAttachment = useComposer((s) => s.removeAttachment)
  const pendingProject = useNewSessionProject()

  const models = useStudio((s) => s.models)
  const connected = useStudio((s) => s.connected)
  // Which thread is streaming, not what: the composer has no business redrawing for every token.
  const streamingIn = useStudio((s) => s.streaming?.conversationId ?? null)
  const agentRunIn = useAgent((s) => s.run?.conversationId ?? null)
  const thinking = useUi((s) => s.thinking)
  const focusTick = useUi((s) => s.composerFocusTick)
  const composerDraft = useUi((s) => s.composerDraft)

  const textarea = useRef<HTMLTextAreaElement>(null)
  const card = useRef<HTMLDivElement>(null)
  const fileInput = useRef<HTMLInputElement>(null)
  const menuList = useRef<HTMLDivElement>(null)
  const [caret, setCaret] = useState(0)
  const [focused, setFocused] = useState(false)
  const [menuIndex, setMenuIndex] = useState(0)
  /** The text a menu was dismissed at: it stays shut until the text changes. */
  const [dismissedAt, setDismissedAt] = useState<string | null>(null)
  const [mentionResults, setMentionResults] = useState<{ query: string; paths: string[]; error: string | null } | null>(null)

  const mode: ConversationMode = conversation ? (conversation.mode ?? 'chat') : side === 'code' ? 'agent' : 'chat'
  // Before the session exists, `@` searches the folder it will be made in.
  const workspace = mode === 'agent' ? (conversation ? (conversation.workspace ?? null) : pendingProject) : null
  const busyHere = conversation !== null && (streamingIn === conversation.id || agentRunIn === conversation.id)
  const busyElsewhere = !busyHere && (streamingIn !== null || agentRunIn !== null)
  const offline = useSettled(!connected, 1200)
  const isCommand = parseSlash(draft.text) !== null
  const hasContent = draft.text.trim().length > 0 || draft.attachments.length > 0
  const canSend = isCommand || (hasContent && connected && !busyElsewhere)

  // --- focus and drafts from elsewhere ---------------------------------------------------------

  useEffect(() => {
    const element = textarea.current
    if (!element) return
    element.focus({ preventScroll: true })
    const end = element.value.length
    element.setSelectionRange(end, end)
  }, [focusTick, conversation?.id])

  useEffect(() => {
    if (!composerDraft || composerDraft.tick === loadedDraftTick) return
    loadedDraftTick = composerDraft.tick
    useComposer.getState().setText(key, composerDraft.text)
    if (composerDraft.attachments?.length) useComposer.getState().addAttachments(key, composerDraft.attachments)
    requestAnimationFrame(() => {
      const element = textarea.current
      if (!element) return
      element.focus({ preventScroll: true })
      element.setSelectionRange(composerDraft.text.length, composerDraft.text.length)
      setCaret(composerDraft.text.length)
    })
  }, [composerDraft, key])

  // One line to about fourteen, then it scrolls.
  useLayoutEffect(() => {
    const element = textarea.current
    if (!element) return
    element.style.height = 'auto'
    element.style.height = `${Math.min(element.scrollHeight, MAX_HEIGHT_PX)}px`
    element.style.overflowY = element.scrollHeight > MAX_HEIGHT_PX ? 'auto' : 'hidden'
  }, [draft.text, variant])

  // --- commands ----------------------------------------------------------------------------------

  /** A thread of this side's kind, made now — for a command that needs one before anything is sent. */
  const createHere = (): string => {
    const studio = useStudio.getState()
    if (mode !== 'agent') return studio.newConversation({ mode: 'chat' })
    const project = projectForNewSession()
    if (project) useUi.getState().addRecentWorkspace(project)
    return studio.newConversation({ mode: 'agent', workspace: project })
  }

  const runCommand = (command: SlashCommand, arg: string) => {
    const studio = useStudio.getState()
    const ui = useUi.getState()
    setText(key, '')
    setDismissedAt(null)
    const thread = () => conversation?.id ?? createHere()
    const noun = mode === 'agent' ? 'session' : 'thread'
    switch (command.name) {
      case 'new':
        newThreadHere()
        break
      // The kind of thread is the window's side now: these two switch sides, and the side brings
      // back the thread it had open.
      case 'code':
      case 'agent':
        switchAppMode('code')
        break
      case 'chat':
        switchAppMode('chat')
        break
      case 'model': {
        if (!arg) {
          studio.toast('info', 'Name the model: /model <part of its name>.')
          break
        }
        const id = pickModel(arg, studio.models.map((m) => m.id))
        if (!id) studio.toast('error', `No installed model matches “${arg}”.`)
        else void studio.loadModel(id)
        break
      }
      case 'clear':
        if (!conversation || conversation.messages.length === 0) studio.toast('info', `There is nothing in this ${noun} to clear.`)
        else {
          studio.clearConversation(conversation.id)
          studio.toast('success', 'Cleared. The pinned notes stay.')
        }
        break
      case 'rename':
        if (!arg) studio.toast('info', 'Give the title: /rename <title>.')
        else studio.renameConversation(thread(), arg)
        break
      case 'export':
        if (!conversation || conversation.messages.length === 0) studio.toast('info', `There is nothing in this ${noun} to export yet.`)
        else studio.toast('success', `Exported ${downloadMarkdown(conversation)}`)
        break
      case 'mine': {
        const on = arg ? parseToggle(arg) : !(conversation?.mine !== false)
        if (on === null) {
          studio.toast('info', 'Use /mine on or /mine off.')
          break
        }
        studio.setConversationMining(thread(), on)
        studio.toast('info', on ? `This ${noun}'s prompts are mined when background mining is on.` : `This ${noun}'s prompts are not mined.`)
        break
      }
      case 'think': {
        const on = arg ? parseToggle(arg) : !thinking
        if (on === null) {
          studio.toast('info', 'Use /think on or /think off.')
          break
        }
        ui.setThinking(on)
        studio.toast('info', on ? 'Thinking on: reasoning models think before they answer.' : 'Thinking off: requests ask reasoning models to answer without thinking.')
        break
      }
      case 'params':
        ui.setParametersOpen(!ui.parametersOpen)
        break
      case 'help':
        ui.setShortcutsOpen(true)
        break
    }
    ui.focusComposer()
  }

  // --- sending -------------------------------------------------------------------------------------

  const submit = async () => {
    const text = draft.text
    const command = parseSlash(text)
    if (command) {
      runCommand(command.command, command.arg)
      return
    }
    if (!hasContent || busyHere || busyElsewhere || !connected) return
    const studio = useStudio.getState()
    const attachments = draft.attachments
    // Sent with the mouse, the focus is on a button that is about to become Stop; the next message
    // is typed here.
    textarea.current?.focus({ preventScroll: true })

    if (mode === 'agent') {
      let id = conversation?.id ?? null
      if (!id) {
        // Code's empty page: the session is made where "New session" would make it. With no folder
        // to go on, the picker asks for one and the draft waits on this page — the session the
        // picker makes takes it over (`sideSync`), ready to send.
        const project = projectForNewSession()
        if (!project) {
          openFolder()
          return
        }
        id = studio.newConversation({ mode: 'agent', workspace: project })
        useUi.getState().addRecentWorkspace(project)
        useComposer.getState().move(key, id)
      }
      const thread = useStudio.getState().conversations.find((c) => c.id === id)
      if (!thread?.workspace) {
        // No folder, no agent. The draft stays where it is, to be sent once a folder is chosen.
        useUi.getState().openWorkspacePicker(id)
        return
      }
      useComposer.getState().clear(draftKey(id))
      onSend?.()
      await startAgentRun(id, text.trim(), attachments.length ? attachments : undefined)
      return
    }

    useComposer.getState().clear(key)
    onSend?.()
    await studio.send(text, attachments.length ? attachments : undefined)
  }

  const stopHere = () => {
    if (agentRunIn !== null && agentRunIn === conversation?.id) stopAgentRun()
    else useStudio.getState().stop()
    textarea.current?.focus({ preventScroll: true })
  }

  // --- menus: `/` commands and `@` files ----------------------------------------------------------

  const slash = slashInput(draft.text)
  const mention = !slash && workspace ? mentionAt(draft.text, caret) : null
  const mentionQuery = mention?.query ?? null

  useEffect(() => {
    if (mentionQuery === null || !workspace) {
      setMentionResults(null)
      return
    }
    let current = true
    // Debounced: a request per keystroke would outrun the walk of a large tree.
    const timer = setTimeout(async () => {
      try {
        const { paths } = await api.workspace.files(workspace, mentionQuery, 50)
        if (current) setMentionResults({ query: mentionQuery, paths, error: null })
      } catch {
        // The runtime may predate the workspace endpoints. Quietly: the menu says it, nothing else.
        if (current) setMentionResults({ query: mentionQuery, paths: [], error: 'File search is not available from this runtime.' })
      }
    }, 120)
    return () => {
      current = false
      clearTimeout(timer)
    }
  }, [mentionQuery, workspace])

  const chooseMention = async (path: string) => {
    if (!mention || !workspace) return
    const next = insertMention(draft.text, mention.start, caret, path)
    setText(key, next.text)
    setCaret(next.caret)
    requestAnimationFrame(() => {
      textarea.current?.focus()
      textarea.current?.setSelectionRange(next.caret, next.caret)
    })
    try {
      const file = await api.workspace.read(workspace, path, { max_bytes: MAX_ATTACHMENT_BYTES })
      if (file.binary) {
        useStudio.getState().toast('info', `${baseName(path)} is a binary file — it is mentioned, but its content is not attached.`)
        return
      }
      useComposer
        .getState()
        .addAttachments(key, [attachmentFromText(path, file.content, { size: file.size, truncated: file.truncated, workspacePath: path })])
    } catch (error) {
      useStudio.getState().toast('info', `${path} could not be read: ${(error as Error).message}`)
    }
  }

  let entries: MenuEntry[] = []
  let menuTitle = ''
  if (slash) {
    if (slash.arg === null) {
      menuTitle = 'Commands'
      entries = matchCommands(slash.name).map((command) => ({
        key: command.name,
        icon: 'slash',
        label: (
          <>
            <span className="mono text-ink-800 dark:text-ink-100">/{command.name}</span>
            {command.args && <span className="mono ml-1 text-ink-400 dark:text-ink-500">{command.args}</span>}
          </>
        ),
        detail: command.description,
        choose: () => {
          if (command.needsArg || command.choices) {
            setText(key, `/${command.name} `)
            setMenuIndex(0)
          } else runCommand(command, '')
        },
      }))
    } else {
      const command = findCommand(slash.name)
      const arg = slash.arg.trim()
      if (command?.name === 'model') {
        menuTitle = 'Load a model'
        const loadedId = chatModelId()
        entries = fuzzyFilter(arg, models, (m) => m.id)
          .slice(0, 8)
          .map((model) => ({
            key: model.id,
            icon: model.id === loadedId ? 'check' : 'cube',
            label: <span className="truncate">{model.id}</span>,
            detail: `${bytes(model.size_bytes)}${model.id === loadedId ? ' · selected' : ''}`,
            disabled: model.fit.verdict === 'does_not_fit',
            choose: () => runCommand(command, model.id),
          }))
      } else if (command?.choices) {
        menuTitle = `/${command.name}`
        entries = command.choices
          .filter((choice) => choice.startsWith(arg.toLowerCase()))
          .map((choice) => ({
            key: choice,
            icon: 'slash',
            label: <span className="mono">/{command.name} {choice}</span>,
            detail: command.description,
            choose: () => runCommand(command, choice),
          }))
      } else if (command?.name === 'rename') {
        menuTitle = 'Rename this thread'
        entries = [
          arg
            ? { key: 'rename', icon: 'edit', label: <span className="truncate">Rename to “{arg}”</span>, choose: () => runCommand(command, arg) }
            : { key: 'hint', icon: 'edit', label: <span className="text-ink-500 dark:text-ink-400">Type the new title, then Enter</span>, disabled: true, choose: () => {} },
        ]
      } else if (command) {
        menuTitle = 'Commands'
        entries = [{ key: command.name, icon: 'slash', label: <span className="mono">/{command.name}</span>, detail: command.description, choose: () => runCommand(command, '') }]
      }
    }
  } else if (mention) {
    menuTitle = workspace ? `Files in ${baseName(workspace)}` : 'Files'
    // The last answer stays up while the next is fetched, so the list does not blink per keystroke.
    const results = mentionResults
    if (results?.error) entries = [{ key: 'error', label: <span className="text-ink-500 dark:text-ink-400">{results.error}</span>, disabled: true, choose: () => {} }]
    else if (results && results.paths.length === 0)
      entries = [{ key: 'none', label: <span className="text-ink-500 dark:text-ink-400">No file matches “{mention.query}”</span>, disabled: true, choose: () => {} }]
    else
      entries = (results?.paths ?? []).map((path) => {
        const slashAt = path.lastIndexOf('/')
        return {
          key: path,
          icon: 'file',
          label: (
            <span className="min-w-0 truncate">
              <span className="text-ink-800 dark:text-ink-100">{path.slice(slashAt + 1)}</span>
              {slashAt > 0 && <span className="ml-1.5 text-ink-400 dark:text-ink-500">{path.slice(0, slashAt)}</span>}
            </span>
          ),
          choose: () => void chooseMention(path),
        }
      })
  }

  const menuOpen = focused && entries.length > 0 && dismissedAt !== draft.text
  const menuIdentity = slash ? `${slash.name}|${slash.arg ?? ''}` : mention ? `@${mention.query}` : ''
  useEffect(() => setMenuIndex(0), [menuIdentity])
  useEffect(() => {
    menuList.current?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'nearest' })
  }, [menuIndex])

  // --- keys --------------------------------------------------------------------------------------

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    // A key inside an input method's composition belongs to the composition — Enter there
    // commits the candidate, it must not send half a sentence.
    if (event.nativeEvent.isComposing || event.keyCode === 229) return

    if (menuOpen) {
      if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
        event.preventDefault()
        const step = event.key === 'ArrowDown' ? 1 : -1
        setMenuIndex((i) => (i + step + entries.length) % entries.length)
        return
      }
      if ((event.key === 'Enter' && !event.shiftKey) || event.key === 'Tab') {
        const entry = entries[Math.min(menuIndex, entries.length - 1)]
        // Tab never leaves the box while a menu is open: it completes, or it does nothing.
        if (event.key === 'Tab') event.preventDefault()
        if (entry && !entry.disabled) {
          event.preventDefault()
          entry.choose()
          return
        }
        if (event.key === 'Tab') return
      }
    }

    if (event.key === 'Enter' && (matches(event.nativeEvent, 'mod+enter') || !event.shiftKey)) {
      event.preventDefault()
      if (canSend) void submit()
      return
    }
    if (event.key === 'ArrowUp' && !event.shiftKey && !event.altKey && !event.metaKey && !event.ctrlKey && draft.text === '' && draft.attachments.length === 0) {
      if (onEditLast) {
        event.preventDefault()
        onEditLast()
      }
    }
  }

  // --- the card ------------------------------------------------------------------------------------

  const placeholder = busyHere
    ? mode === 'agent'
      ? 'The agent is working — Esc stops it'
      : 'Generating — Esc stops it'
    : mode === 'agent'
      ? workspace
        ? 'Describe a task — @ to mention a file, / for commands'
        : 'Describe a task — you will choose its folder first'
      : 'Ask anything — / for commands'

  return (
    <div className="w-full">
      <div
        ref={card}
        className={`@container rounded-2xl border bg-white shadow-sm transition-[border-color,box-shadow] dark:bg-ink-900 ${
          focused
            ? 'border-arc-500/70 ring-4 ring-arc-500/10 dark:border-arc-500/60'
            : 'border-ink-200 shadow-ink-950/[0.03] dark:border-ink-700'
        }`}
      >
        {draft.attachments.length > 0 && (
          <div className="flex flex-wrap gap-1.5 px-3 pt-3">
            {draft.attachments.map((attachment) => (
              <AttachmentChip key={attachment.id} attachment={attachment} onRemove={() => removeAttachment(key, attachment.id)} />
            ))}
          </div>
        )}

        <textarea
          ref={textarea}
          rows={1}
          value={draft.text}
          placeholder={placeholder}
          aria-label={mode === 'agent' ? 'Task for the agent' : 'Message'}
          spellCheck
          className={`block w-full resize-none bg-transparent px-4 pt-3 text-[0.94rem] leading-6 text-ink-900 outline-none placeholder:text-ink-400 dark:text-ink-50 dark:placeholder:text-ink-500 ${
            variant === 'hero' ? 'min-h-[4.5rem]' : ''
          }`}
          onChange={(event) => {
            setText(key, event.target.value)
            setCaret(event.target.selectionStart)
          }}
          onSelect={(event) => setCaret(event.currentTarget.selectionStart)}
          onFocus={() => setFocused(true)}
          onBlur={() => setFocused(false)}
          onKeyDown={onKeyDown}
          onPaste={(event) => {
            const files = Array.from(event.clipboardData.files)
            if (files.length === 0) return
            event.preventDefault()
            void attachFiles(key, files)
          }}
        />

        {/* The toolbar is laid out against the card's own width (a container query), not the
            window's: the same card sits in the middle of an empty thread and beside a side panel.
            Labels give way first, then the model's name; nothing wraps. */}
        <div className="flex items-center gap-1 whitespace-nowrap px-2 pb-2 pt-1.5">
          {/* Both sides may give way, the right much sooner: the model's name truncates first (its
              chip stops at its own minimum), and only then the folder's name on the left. The right
              side may shrink below its contents — a model id has no spaces, so its natural minimum is
              the whole name — and clips rather than overflowing: unclipped, its chips used to spill
              leftwards across the agent's controls on a narrow card. */}
          <div className="flex min-w-0 items-center gap-1">
            <IconButton icon="paperclip" label="Attach text files" onClick={() => fileInput.current?.click()} />
            <input
              ref={fileInput}
              type="file"
              multiple
              hidden
              onChange={(event) => {
                const files = Array.from(event.target.files ?? [])
                event.target.value = ''
                if (files.length) void attachFiles(key, files)
                textarea.current?.focus()
              }}
            />
            {conversation && mode === 'agent' && (
              <div className="flex min-w-0 items-center gap-1">
                <AgentControls conversation={conversation} />
              </div>
            )}
          </div>

          <div className="-my-0.5 ml-auto flex min-w-0 shrink-[8] items-center justify-end gap-1 overflow-hidden py-0.5 pr-0.5">
            {!thinking && (
              <button
                type="button"
                className="chip shrink-0 text-ink-500 dark:text-ink-400"
                onClick={() => useUi.getState().setThinking(true)}
                aria-label="Thinking is off. Turn it on"
                title="Thinking is off: each request sends thinking: false. Click to let reasoning models think again."
              >
                <Icon name="brain" className="size-3.5" />
                <span className="hidden line-through decoration-ink-400 @2xl:inline">Thinking</span>
              </button>
            )}
            <MiningChip conversation={conversation} compact={mode === 'agent'} />
            <ModelPicker />
            {busyHere ? (
              <button
                type="button"
                className="ml-1 inline-flex size-8 shrink-0 items-center justify-center rounded-full bg-ink-900 text-white transition-colors hover:bg-ink-700 dark:bg-ink-100 dark:text-ink-900 dark:hover:bg-white"
                onClick={stopHere}
                aria-label={mode === 'agent' ? 'Stop the agent' : 'Stop generating'}
                title="Stop (Esc)"
              >
                <span className="size-2.5 rounded-[3px] bg-current" />
              </button>
            ) : (
              <button
                type="button"
                className="ml-1 inline-flex size-8 shrink-0 items-center justify-center rounded-full bg-arc-600 text-white transition-colors hover:bg-arc-500 disabled:bg-ink-200 disabled:text-ink-400 dark:disabled:bg-ink-800 dark:disabled:text-ink-500"
                onClick={() => void submit()}
                disabled={!canSend}
                aria-label={isCommand ? 'Run command' : 'Send'}
                title={isCommand ? 'Run the command (Enter)' : 'Send (Enter)'}
              >
                <Icon name="arrowUp" className="size-4" />
              </button>
            )}
          </div>
        </div>
      </div>

      <ModelSelectionNotice />
      {(offline || busyElsewhere) && (
        <p className="mt-1.5 flex items-center justify-center gap-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400" role="status">
          <Icon name="warning" className="size-3.5 text-amber-500" />
          {offline
            ? 'The runtime is unreachable — sending is paused until it is back.'
            : 'Another thread is still answering. Stop it there, or wait, to send here.'}
        </p>
      )}

      <Popover anchor={card} open={menuOpen} onClose={() => setDismissedAt(draft.text)} placement="top-start" className="w-[min(30rem,calc(100vw-2rem))] p-0">
        {/* Clicks inside the menu must not take the focus from the box: the box is where the keys go. */}
        <div onMouseDown={(event) => event.preventDefault()}>
          <div className="flex items-center justify-between px-3 pb-1 pt-2">
            <span className="menu-label p-0">{menuTitle}</span>
            <span className="flex items-center gap-1 text-[0.65rem] text-ink-400 dark:text-ink-500">
              <Kbd shortcut="arrowup" />
              <Kbd shortcut="arrowdown" />
              <span className="mx-0.5">to move</span>
              <Kbd shortcut="enter" />
              <span>to choose</span>
            </span>
          </div>
          <div ref={menuList} className="max-h-72 overflow-y-auto p-1 pt-0" role="listbox" aria-label={menuTitle}>
            {entries.map((entry, index) => (
              <button
                key={entry.key}
                type="button"
                role="option"
                aria-selected={index === menuIndex}
                className="menu-item"
                data-active={index === menuIndex && !entry.disabled}
                disabled={entry.disabled}
                onMouseEnter={() => setMenuIndex(index)}
                onClick={() => entry.choose()}
              >
                {entry.icon && <Icon name={entry.icon} className="size-3.5 shrink-0 text-ink-400" />}
                <span className="flex min-w-0 flex-1 items-center">{entry.label}</span>
                {entry.detail && <span className="ml-3 shrink-0 truncate text-[0.7rem] text-ink-400 dark:text-ink-500">{entry.detail}</span>}
              </button>
            ))}
          </div>
        </div>
      </Popover>
    </div>
  )
})
