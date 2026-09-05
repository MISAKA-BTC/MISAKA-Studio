// Navigation and conversation history.
//
// The connection dot is not decoration: this window talks to a separate process, and when that
// process is not there every other part of the UI is showing stale data. Saying so in one place,
// permanently, beats a toast that has already faded by the time someone looks up.

import { useEffect, useState } from 'react'
import logo from '../assets/misaka-logo.png'
import { api } from '../lib/api'
import { relativeTime } from '../lib/format'
import type { Effort, MiningState, PoolStatus } from '../lib/types'
import { useStudio, type View } from '../store/studio'
import { Icon, type IconName } from './common'

/**
 * **The mining light, where it is always visible.**
 *
 * The Network tab answers this properly; this exists because the question is asked from the Chat
 * tab, by someone who just watched a model reply and reasonably wondered whether that was mining.
 * It is not, and a dot that is only truthful when you go looking for it is not much of an answer.
 *
 * Polled slowly on purpose: it is a light, not a dashboard, and every read walks the node's log.
 */
function MiningLight() {
  const [mining, setMining] = useState<MiningState | null>(null)
  const [effort, setEffort] = useState<Effort | null>(null)
  const [pool, setPool] = useState<PoolStatus | null>(null)

  useEffect(() => {
    let live = true
    const read = async () => {
      try {
        const overview = await api.network()
        if (live) {
          setMining(overview.node.mining)
          setEffort(overview.node.effort)
        }
      } catch {
        if (live) setMining(null)
      }
      // Someone mining through a pool slot has no node of their own, so `node.mining` is
      // truthfully "not mining" — and the light went dark for exactly the people it was added
      // for. The slot's lane is asked separately and shown the same way.
      try {
        const slot = await api.pool()
        if (live) setPool(slot)
      } catch {
        if (live) setPool(null)
      }
    }
    void read()
    const timer = setInterval(() => void read(), 15_000)
    return () => {
      live = false
      clearInterval(timer)
    }
  }, [])

  if (!mining || mining.state === 'not_mining') {
    if (!pool || !pool.joined) return null
    const lane = pool.fp
    const promptMining = lane !== null && lane.mode === 'fp' && lane.gateway_running && lane.submitter_running
    if (!promptMining && pool.phase !== 'producing') return null
    return (
      <div
        className={`mx-2 mb-2 flex items-center gap-2 rounded-lg px-3 py-2 text-[0.7rem] ${
          promptMining
            ? 'bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300'
            : 'bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300'
        }`}
        title={
          promptMining
            ? `Every chat in the Chat tab is mined on pool slot ${pool.slot_id}: the answer and the claim behind it are one execution under that slot's bond.`
            : `Pool slot ${pool.slot_id} is producing on its own draws; turn on prompt mining in the Network tab to make your chats the work.`
        }
      >
        {promptMining ? (
          <span className="relative flex size-2 shrink-0">
            <span className="absolute inline-flex size-full animate-ping rounded-full bg-emerald-400 opacity-75" />
            <span className="relative inline-flex size-2 rounded-full bg-emerald-500" />
          </span>
        ) : (
          <span className="size-2 shrink-0 rounded-full bg-amber-500" />
        )}
        <span className="min-w-0 flex-1 truncate">
          {promptMining
            ? `Mining on · ${pool.slot_id} · ${lane.claims_submitted} claim${lane.claims_submitted === 1 ? '' : 's'} · ${pool.blocks_won} block${pool.blocks_won === 1 ? '' : 's'}`
            : `Pool slot producing · ${pool.slot_id} · ${pool.blocks_won} block${pool.blocks_won === 1 ? '' : 's'}`}
        </span>
      </div>
    )
  }

  const producing = mining.state === 'producing'
  return (
    <div
      className={`mx-2 mb-2 flex items-center gap-2 rounded-lg px-3 py-2 text-[0.7rem] ${
        producing
          ? 'bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300'
          : 'bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300'
      }`}
      title={mining.state === 'starting' && mining.holding ? mining.holding : undefined}
    >
      {producing ? (
        <span className="relative flex size-2 shrink-0">
          <span className="absolute inline-flex size-full animate-ping rounded-full bg-emerald-400 opacity-75" />
          <span className="relative inline-flex size-2 rounded-full bg-emerald-500" />
        </span>
      ) : (
        <span className="size-2 shrink-0 rounded-full bg-amber-500" />
      )}
      <span className="min-w-0 flex-1 truncate">
        {producing
          ? `Mining · ${mining.blocks} block${mining.blocks === 1 ? '' : 's'}`
          : effort && effort.draws > 0
            ? `Mining · ${effort.draws.toLocaleString()} draw${effort.draws === 1 ? '' : 's'}`
            : 'Producer running · nothing won yet'}
      </span>
    </div>
  )
}

const NAV: { view: View; label: string; icon: IconName }[] = [
  { view: 'chat', label: 'Chat', icon: 'chat' },
  { view: 'models', label: 'Models', icon: 'cube' },
  { view: 'network', label: 'Network', icon: 'globe' },
  { view: 'monitor', label: 'Monitor', icon: 'gauge' },
  { view: 'settings', label: 'Settings', icon: 'settings' },
]

export function Sidebar() {
  const view = useStudio((s) => s.view)
  const setView = useStudio((s) => s.setView)
  const conversations = useStudio((s) => s.conversations)
  const activeId = useStudio((s) => s.activeConversationId)
  const newConversation = useStudio((s) => s.newConversation)
  const selectConversation = useStudio((s) => s.selectConversation)
  const deleteConversation = useStudio((s) => s.deleteConversation)
  const connected = useStudio((s) => s.connected)
  const runtime = useStudio((s) => s.runtime)
  const downloads = useStudio((s) => s.downloads)

  const active = downloads.filter((d) => d.status === 'downloading' || d.status === 'verifying').length

  return (
    <aside className="flex h-full w-64 shrink-0 flex-col border-r border-ink-200 bg-white dark:border-ink-800 dark:bg-ink-900">
      <div className="flex items-center gap-2.5 px-4 py-4">
        {/* The project's own mark, not a letter in a box. It carries its own colour in both
            themes, so it is drawn at its native aspect rather than cropped into a square. */}
        <img src={logo} alt="" className="h-7 w-auto shrink-0" />
        <div className="min-w-0">
          <div className="truncate text-sm font-semibold">MISAKA Studio</div>
          <div className="flex items-center gap-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
            <span className={`size-1.5 rounded-full ${connected ? 'bg-emerald-500' : 'bg-red-500'}`} />
            {connected ? (runtime?.model_id ? 'model loaded' : 'runtime ready') : 'runtime unreachable'}
          </div>
        </div>
      </div>

      <nav className="px-2">
        {NAV.map((item) => (
          <button
            key={item.view}
            type="button"
            onClick={() => setView(item.view)}
            className={`mb-0.5 flex w-full items-center gap-2.5 rounded-lg px-3 py-2 text-sm transition-colors ${
              view === item.view
                ? 'bg-arc-600/12 font-medium text-arc-700 dark:text-arc-300'
                : 'text-ink-600 hover:bg-ink-100 dark:text-ink-300 dark:hover:bg-ink-800'
            }`}
          >
            <Icon name={item.icon} />
            {item.label}
            {item.view === 'models' && active > 0 && (
              <span className="ml-auto badge bg-arc-600 text-white">{active}</span>
            )}
          </button>
        ))}
      </nav>

      <div className="mt-3">
        <MiningLight />
      </div>

      <div className="mt-1 flex items-center justify-between px-4 pb-1">
        <span className="text-[0.7rem] font-semibold uppercase tracking-wide text-ink-500 dark:text-ink-400">Conversations</span>
        <button
          type="button"
          className="btn-ghost px-1.5 py-1"
          title="New chat"
          onClick={() => {
            newConversation()
            setView('chat')
          }}
        >
          <Icon name="plus" className="size-3.5" />
        </button>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-2 pb-3">
        {conversations.length === 0 && <p className="px-3 py-2 text-xs text-ink-500 dark:text-ink-400">Nothing yet.</p>}
        {conversations.map((conversation) => (
          <div
            key={conversation.id}
            className={`group mb-0.5 flex items-center gap-1 rounded-lg px-3 py-2 text-sm transition-colors ${
              conversation.id === activeId ? 'bg-ink-100 dark:bg-ink-800' : 'hover:bg-ink-100 dark:hover:bg-ink-800'
            }`}
          >
            <button
              type="button"
              className="min-w-0 flex-1 text-left"
              onClick={() => {
                selectConversation(conversation.id)
                setView('chat')
              }}
            >
              <div className="truncate">{conversation.title}</div>
              <div className="text-[0.7rem] text-ink-500 dark:text-ink-400">{relativeTime(conversation.updatedAt / 1000)}</div>
            </button>
            <button
              type="button"
              className="btn-ghost px-1 py-1 opacity-0 transition-opacity group-hover:opacity-100"
              title="Delete conversation"
              onClick={() => deleteConversation(conversation.id)}
            >
              <Icon name="trash" className="size-3.5" />
            </button>
          </div>
        ))}
      </div>
    </aside>
  )
}
