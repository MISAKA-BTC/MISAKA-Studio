// Where a new Code session would start, live — for the places that say so before it is made: the
// sidebar's "New session", the empty page's project chip, the composer's file mentions.

import { useStudio } from '../../store/studio'
import { useUi } from '../../store/ui'

/** A session's folder, read as a string so the selector stays stable while messages stream in. */
function sessionFolder(id: string | null) {
  return (s: ReturnType<typeof useStudio.getState>): string | null => {
    if (!id) return null
    const thread = s.conversations.find((c) => c.id === id)
    return thread?.mode === 'agent' ? (thread.workspace ?? null) : null
  }
}

/**
 * The folder `newSession` would use now: the session in view's, else the one Code had open last,
 * else the folder used last — `sides.projectForNewSession`, subscribed to. Null when a new session
 * has to ask for its folder.
 */
export function useNewSessionProject(): string | null {
  const activeId = useStudio((s) => s.activeConversationId)
  const lastId = useUi((s) => s.lastThread.code)
  const active = useStudio(sessionFolder(activeId))
  const last = useStudio(sessionFolder(lastId))
  const recent = useUi((s) => s.recentWorkspaces[0] ?? null)
  return active ?? last ?? recent
}
