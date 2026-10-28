import assert from 'node:assert/strict'
import { test } from 'node:test'
import { dirtySummary, folderLabel, groupProjects, NO_FOLDER_KEY, projectKey, sessionTitle, shortAge } from '../src/components/shell/projects.ts'
import type { Conversation } from '../src/lib/types.ts'

const session = (id: string, updatedAt: number, workspace: string | null | undefined, extra: Partial<Conversation> = {}): Conversation => ({
  id,
  title: id,
  createdAt: 0,
  updatedAt,
  modelId: null,
  messages: [],
  mode: 'agent',
  workspace,
  ...extra,
})

test('a folder is matched without its trailing separator', () => {
  assert.equal(projectKey('/work/alpha/'), '/work/alpha')
  assert.equal(projectKey('/work/alpha'), '/work/alpha')
  assert.equal(projectKey('C:\\code\\app\\'), 'C:\\code\\app')
  assert.equal(projectKey('/'), '/', 'the root stays the root')
  assert.equal(projectKey('C:\\'), 'C:\\')
  assert.equal(folderLabel('/Users/me/MISAKA-Studio/'), 'MISAKA-Studio')
  assert.equal(folderLabel('C:\\code\\app'), 'app')
})

test('sessions are grouped by folder, the folder with the newest activity first', () => {
  const projects = groupProjects(
    [session('a1', 10, '/work/alpha'), session('b1', 50, '/work/beta'), session('a2', 30, '/work/alpha/'), session('b2', 20, '/work/beta')],
    [],
  )
  assert.deepEqual(
    projects.map((p) => [p.name, p.sessions.map((s) => s.id)]),
    [
      ['beta', ['b1', 'b2']],
      ['alpha', ['a2', 'a1']],
    ],
  )
  assert.equal(projects[1]?.path, '/work/alpha', 'the folder as recorded, for the runtime')
  assert.equal(projects[0]?.latest, 50)
})

test('starred sessions float to the top of their project', () => {
  const [project] = groupProjects([session('old', 1, '/p', { starred: true }), session('new', 9, '/p'), session('mid', 5, '/p')], [])
  assert.deepEqual(project?.sessions.map((s) => s.id), ['old', 'new', 'mid'])
})

test('recent folders without sessions follow, in the order they were used, and only once', () => {
  const projects = groupProjects([session('a1', 10, '/work/alpha')], ['/work/gamma', '/work/alpha/', '/work/delta', '/work/gamma'])
  assert.deepEqual(
    projects.map((p) => [p.name, p.sessions.length, p.recent]),
    [
      ['alpha', 1, true],
      ['gamma', 0, true],
      ['delta', 0, true],
    ],
  )
})

test('sessions without a folder are listed last, under their own heading', () => {
  const projects = groupProjects([session('loose', 99, null), session('blank', 98, '  '), session('a1', 1, '/work/alpha')], ['/work/zeta'])
  assert.deepEqual(
    projects.map((p) => p.key),
    ['/work/alpha', '/work/zeta', NO_FOLDER_KEY],
  )
  const last = projects.at(-1)
  assert.equal(last?.name, 'No folder')
  assert.equal(last?.path, null)
  assert.equal(last?.recent, false)
  assert.deepEqual(last?.sessions.map((s) => s.id), ['loose', 'blank'])
})

test('nothing to show is an empty list', () => {
  assert.deepEqual(groupProjects([], []), [])
})

test('ages are short enough for a sidebar row', () => {
  const now = new Date(2026, 9, 27, 12, 0, 0).getTime()
  const ago = (ms: number) => shortAge(now - ms, now)
  assert.equal(ago(5_000), 'now')
  assert.equal(ago(-5_000), 'now', 'a clock a little ahead is not "in the future"')
  assert.equal(ago(5 * 60_000), '5m')
  assert.equal(ago(3 * 3_600_000), '3h')
  assert.equal(ago(2 * 86_400_000), '2d')
  assert.equal(ago(15 * 86_400_000), '2w')
  assert.equal(shortAge(new Date(2026, 8, 4, 9).getTime(), now), 'Sep 4')
  assert.equal(shortAge(new Date(2024, 8, 4, 9).getTime(), now), 'Sep 2024')
})

test('a dirty tree is summed up in a line, a clean one not at all', () => {
  const file = { path: 'a.ts', orig_path: null, index: '.', worktree: 'M', status: 'modified' as const, added: 3, removed: 1, binary: false }
  assert.equal(dirtySummary({ files: [], added: 0, removed: 0, ahead: 2, behind: 0 }), null, 'ahead but clean: nothing to mark')
  assert.equal(dirtySummary({ files: [file], added: 3, removed: 1, ahead: 0, behind: 0 }), '1 uncommitted change (+3 −1)')
  assert.equal(dirtySummary({ files: [file, file], added: 40, removed: 12, ahead: 2, behind: 1 }), '2 uncommitted changes (+40 −12) · 2 ahead, 1 behind')
})

test('an untouched session is called a session', () => {
  assert.equal(sessionTitle({ title: 'New agent thread', messages: [] }), 'New session')
  assert.equal(sessionTitle({ title: 'Fix the parser', messages: [] }), 'Fix the parser')
  assert.equal(sessionTitle({ title: 'New agent thread', messages: [{ id: '1', role: 'user', content: 'New agent thread' }] }), 'New agent thread')
})
