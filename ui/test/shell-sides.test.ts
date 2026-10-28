import assert from 'node:assert/strict'
import { test } from 'node:test'
import { mostRecent, projectForNewSession, restoreTarget, sideOf, threadsOf } from '../src/components/shell/sides.ts'
import type { Conversation } from '../src/lib/types.ts'

const thread = (id: string, updatedAt: number, extra: Partial<Conversation> = {}): Conversation => ({
  id,
  title: id,
  createdAt: 0,
  updatedAt,
  modelId: null,
  messages: [],
  ...extra,
})

const chatA = thread('chat-a', 10)
const chatB = thread('chat-b', 30, { mode: 'chat' })
const codeA = thread('code-a', 20, { mode: 'agent', workspace: '/work/alpha' })
const codeB = thread('code-b', 40, { mode: 'agent', workspace: '/work/beta' })
const all = [chatA, chatB, codeA, codeB]

test('an agent thread is on the Code side, every other thread on the Chat side', () => {
  assert.equal(sideOf(codeA), 'code')
  assert.equal(sideOf(chatA), 'chat', 'no mode at all is a chat')
  assert.equal(sideOf(chatB), 'chat')
  assert.deepEqual(threadsOf(all, 'code').map((t) => t.id), ['code-a', 'code-b'])
})

test('the most recent thread is picked within the side only', () => {
  assert.equal(mostRecent(all, 'chat')?.id, 'chat-b')
  assert.equal(mostRecent(all, 'code')?.id, 'code-b')
  assert.equal(mostRecent(all, 'code', 'code-b')?.id, 'code-a', 'the thread being deleted is not its own fallback')
  assert.equal(mostRecent([chatA], 'code'), null)
})

test('switching sides comes back to the thread the side had open', () => {
  assert.equal(restoreTarget(all, 'code-a', 'code'), 'code-a', 'the remembered one, though another is newer')
  assert.equal(restoreTarget(all, 'chat-a', 'chat'), 'chat-a')
})

test('a remembered thread that is gone or changed sides falls back to the newest on the side', () => {
  assert.equal(restoreTarget(all, 'deleted', 'code'), 'code-b')
  assert.equal(restoreTarget(all, 'chat-a', 'code'), 'code-b', 'a chat is never restored on the Code side')
  assert.equal(restoreTarget(all, null, 'chat'), 'chat-b')
})

test('a side with no threads opens on nothing — its empty page', () => {
  assert.equal(restoreTarget([chatA, chatB], 'code-a', 'code'), null)
  assert.equal(restoreTarget([], null, 'chat'), null)
})

test('a new session goes where the work is: the session in view, the last session, the last folder', () => {
  const recent = ['/work/gamma', '/work/delta']
  assert.equal(projectForNewSession({ active: codeA, last: codeB, recent }), '/work/alpha')
  assert.equal(projectForNewSession({ active: chatA, last: codeB, recent }), '/work/beta', 'a chat in view has no folder to offer')
  assert.equal(projectForNewSession({ active: null, last: null, recent }), '/work/gamma')
  assert.equal(projectForNewSession({ active: thread('loose', 1, { mode: 'agent', workspace: null }), last: null, recent }), '/work/gamma')
  assert.equal(projectForNewSession({ active: null, last: null, recent: [] }), null, 'nothing to go on: ask for a folder')
})
