// What keeps the Code side's readings current without polling: git status is read again when the
// agent takes a step or finishes, when its tracked edits change, and when the window comes back
// into focus — the moments the working tree is likely to have moved. (A terminal command asks for
// its own refresh as it ends; see `terminalStore.ts`.)
//
// Also the one-time introduction: the first Code session opened in this window's storage opens the
// Session panel, so the summary is found. After that the panel is left as the person leaves it.

import { useEffect, useRef } from 'react'
import { useAgent } from '../../lib/agent/run'
import type { Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'
import { useCodeUi } from './codeStore'
import { scheduleGitRefresh } from './gitStore'

export function useCodeWatchers(conversation: Conversation | null): void {
  const agent = conversation?.mode === 'agent'
  const root = agent ? (conversation?.workspace ?? null) : null
  const changes = agent ? conversation?.changes : undefined
  const rootRef = useRef(root)
  rootRef.current = root

  useEffect(() => {
    if (!agent) return
    const code = useCodeUi.getState()
    if (code.introduced) return
    code.markIntroduced()
    useUi.getState().setRightPanel('session')
  }, [agent, conversation?.id])

  // A step may have edited files or run a command; the end of a run is when someone looks.
  useEffect(
    () =>
      useAgent.subscribe((state, previous) => {
        const before = previous.run
        if (!before) return
        const after = state.run
        const ended = !after || after.conversationId !== before.conversationId
        const stepped = !ended && after.step !== before.step
        if (!ended && !stepped) return
        const thread = useStudio.getState().conversations.find((c) => c.id === before.conversationId)
        if (thread?.workspace) scheduleGitRefresh(thread.workspace, ended ? 300 : 800)
      }),
    [],
  )

  // Back from an editor or a terminal elsewhere: the tree may have changed under the window.
  useEffect(() => {
    const refresh = () => {
      if (rootRef.current) scheduleGitRefresh(rootRef.current, 250)
    }
    const onVisibility = () => {
      if (document.visibilityState === 'visible') refresh()
    }
    window.addEventListener('focus', refresh)
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      window.removeEventListener('focus', refresh)
      document.removeEventListener('visibilitychange', onVisibility)
    }
  }, [])

  // The agent's tracked edits changed — an edit landed, or one was reverted or kept.
  const seen = useRef<{ id: string | undefined; changes: unknown }>({ id: undefined, changes: undefined })
  useEffect(() => {
    const previous = seen.current
    seen.current = { id: conversation?.id, changes }
    if (!root || previous.id !== conversation?.id || previous.changes === changes) return
    scheduleGitRefresh(root, 800)
  }, [conversation?.id, changes, root])
}
