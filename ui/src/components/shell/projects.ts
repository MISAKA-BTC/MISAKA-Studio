// Code's sidebar, as data: sessions grouped by the folder they work in.
//
// A project is a folder, not a record of its own — every folder that has a session, plus the
// folders opened recently — so there is nothing to create, rename or keep in step: a session's
// `workspace` is the whole of its membership.
//
// Plain TypeScript with type-only imports, so the unit tests run it directly under Node.

import type { Conversation, GitStatus } from '../../lib/types'

type Session = Pick<Conversation, 'id' | 'updatedAt' | 'starred' | 'workspace'>

export type Project<T extends Session = Conversation> = {
  /** The folder with any trailing separator taken off — what sessions are matched on. `''` for
   *  the sessions that have no folder yet. */
  key: string
  /** The folder as it was recorded, for the runtime and the tooltip; null for no folder. */
  path: string | null
  name: string
  /** Starred first, then the most recently changed. */
  sessions: T[]
  /** When the newest of its sessions changed; 0 for a folder with none. */
  latest: number
  /** Listed among the recent folders — the one kind that may be taken off the list. */
  recent: boolean
}

/** The sessions that have no folder yet: listed last, under their own heading. */
export const NO_FOLDER_KEY = ''

/** A folder without the trailing separator a pasted path may carry; `/` and `C:\` stay as they are. */
export function projectKey(path: string): string {
  const trimmed = path.trim()
  const stripped = trimmed.replace(/[\\/]+$/, '')
  if (!stripped) return trimmed ? trimmed.slice(0, 1) : ''
  return /^[A-Za-z]:$/.test(stripped) ? `${stripped}\\` : stripped
}

/** What a folder is called: its last segment. */
export function folderLabel(path: string): string {
  const key = projectKey(path)
  return key.split(/[\\/]/).filter(Boolean).pop() ?? key
}

function bySessionOrder(a: Session, b: Session): number {
  if (Boolean(a.starred) !== Boolean(b.starred)) return a.starred ? -1 : 1
  return b.updatedAt - a.updatedAt
}

/**
 * Every folder with a session, newest activity first; then the recent folders that have none, in
 * the order they were used; then the sessions with no folder. Within a project, starred sessions
 * come first and the rest newest first.
 */
export function groupProjects<T extends Session>(sessions: readonly T[], recent: readonly string[]): Project<T>[] {
  const byKey = new Map<string, Project<T>>()
  const recentKeys = new Set(recent.map(projectKey))

  for (const session of sessions) {
    const path = session.workspace?.trim() ? session.workspace : null
    const key = path ? projectKey(path) : NO_FOLDER_KEY
    let project = byKey.get(key)
    if (!project) {
      project = { key, path, name: path ? folderLabel(path) : 'No folder', sessions: [], latest: 0, recent: recentKeys.has(key) && key !== NO_FOLDER_KEY }
      byKey.set(key, project)
    }
    project.sessions.push(session)
    project.latest = Math.max(project.latest, session.updatedAt)
  }

  const withSessions = [...byKey.values()].filter((p) => p.key !== NO_FOLDER_KEY)
  for (const project of withSessions) project.sessions.sort(bySessionOrder)
  withSessions.sort((a, b) => b.latest - a.latest)

  const empty: Project<T>[] = []
  for (const path of recent) {
    const key = projectKey(path)
    if (!key || byKey.has(key)) continue
    const project: Project<T> = { key, path, name: folderLabel(path), sessions: [], latest: 0, recent: true }
    byKey.set(key, project)
    empty.push(project)
  }

  const loose = byKey.get(NO_FOLDER_KEY)
  if (loose) loose.sessions.sort(bySessionOrder)
  return [...withSessions, ...empty, ...(loose ? [loose] : [])]
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/**
 * How long ago, in the fewest characters that still read: `now`, `5m`, `3h`, `2d`, `3w`, then the
 * date — `Sep 4` this year, `Sep 2024` before it. A sidebar row has room for four characters, not
 * for "3 hours ago".
 */
export function shortAge(at: number, now: number = Date.now()): string {
  const seconds = Math.max(0, (now - at) / 1000)
  if (seconds < 60) return 'now'
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h`
  if (seconds < 7 * 86_400) return `${Math.floor(seconds / 86_400)}d`
  if (seconds < 35 * 86_400) return `${Math.floor(seconds / (7 * 86_400))}w`
  const date = new Date(at)
  const month = MONTHS[date.getMonth()] ?? ''
  return date.getFullYear() === new Date(now).getFullYear() ? `${month} ${date.getDate()}` : `${month} ${date.getFullYear()}`
}

/**
 * A working tree's uncommitted state in one line — "3 uncommitted changes (+40 −12) · 2 ahead" —
 * or null when it is clean (a project's dot is drawn only when there is something to say).
 */
export function dirtySummary(repo: Pick<GitStatus, 'files' | 'added' | 'removed' | 'ahead' | 'behind'>): string | null {
  const files = repo.files.length
  if (files === 0) return null
  const sync = [repo.ahead ? `${repo.ahead} ahead` : null, repo.behind ? `${repo.behind} behind` : null].filter(Boolean).join(', ')
  return `${files} uncommitted change${files === 1 ? '' : 's'} (+${repo.added} −${repo.removed})${sync ? ` · ${sync}` : ''}`
}

/** The title an untouched session is shown with. The store names every new agent thread the same
 *  way; on Code's side it is a session. */
export function sessionTitle(session: Pick<Conversation, 'title' | 'messages'>): string {
  if (session.messages.length === 0 && (session.title === 'New agent thread' || session.title === 'New chat')) return 'New session'
  return session.title
}
