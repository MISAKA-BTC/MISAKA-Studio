// The one place that keeps the thread in view on the side in view.
//
// Threads are opened from many places — the sidebar, the palette, the status bar's "Agent · step
// 3", a new chat made from Code's side, the folder picker turning an empty chat into a session —
// and none of them should have to know about sides. So instead of each one switching the window,
// this watches the store: whenever the thread in view changes (or changes kind), the window goes
// to that thread's side, and the side remembers it. Switching sides the other way round — from the
// switch — is `switchAppMode`, which picks the thread and lets this follow.
//
// Two cases are not a move to another side and are settled here instead:
// * **A deleted thread.** The store falls back to its first thread, which can be on the other
//   side; the window stays on its side and opens the newest thread left there, or its empty page.
// * **A thread made from a side's empty page.** What was typed there before the thread existed
//   moves into it, so a draft is never stranded on a page that is no longer shown.

import { useEffect } from 'react'
import type { Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi, type AppMode } from '../../store/ui'
import { draftKey, useComposer } from '../chat/composerStore'
import { mostRecent, sideOf } from './sides'

type Snapshot = { activeConversationId: string | null; conversations: Conversation[] }

function carryNewDraft(id: string, side: AppMode) {
  const composer = useComposer.getState()
  const from = draftKey(null, side)
  if (composer.drafts[from] && !composer.drafts[id]) composer.move(from, id)
}

function reconcile(state: Snapshot, previous: Snapshot | null) {
  const ui = useUi.getState()
  const id = state.activeConversationId

  const gone = previous?.activeConversationId
  if (previous && gone && gone !== id && !state.conversations.some((c) => c.id === gone)) {
    const deleted = previous.conversations.find((c) => c.id === gone)
    const side = deleted ? sideOf(deleted) : ui.appMode
    const fallback = mostRecent(state.conversations, side)?.id ?? null
    if (fallback !== id) {
      // Settled by the update this one sets off.
      useStudio.setState({ activeConversationId: fallback })
      return
    }
  }

  const thread = id ? (state.conversations.find((c) => c.id === id) ?? null) : null
  if (!thread) {
    // Nothing in view: the side is on its empty page, and that is what it comes back to.
    if (id === null && ui.lastThread[ui.appMode] !== null) ui.rememberThread(ui.appMode, null)
    return
  }
  const side = sideOf(thread)
  if (side !== ui.appMode) ui.setAppMode(side)
  if (ui.lastThread[side] !== thread.id) ui.rememberThread(side, thread.id)
  if (thread.messages.length === 0) carryNewDraft(thread.id, side)
}

/** Keep the side and the thread in view together. Mounted once, by `App`. */
export function useSideSync(): void {
  useEffect(() => {
    // A window from before the two sides, or one closed mid-switch, can open on a thread of the
    // other side: the thread wins — it is what the person was looking at.
    reconcile(useStudio.getState(), null)
    return useStudio.subscribe((state, previous) => {
      if (state.activeConversationId === previous.activeConversationId && state.conversations === previous.conversations) return
      reconcile(state, previous)
    })
  }, [])
}
