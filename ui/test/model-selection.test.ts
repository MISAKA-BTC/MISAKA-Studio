import assert from 'node:assert/strict'
import { test } from 'node:test'
import { defaultModelId } from '../src/lib/modelSelection.ts'
import type { ModelView } from '../src/lib/types.ts'

const local = { id: 'chat', path: '/models/chat.gguf', fit: { verdict: 'fits' } } as ModelView
const mining = { id: 'mining', path: '/models/mining.palwart', fit: { verdict: 'fits' } } as ModelView

test('a fresh chat gets a local model even when the stale startup setting names a mining artifact', () => {
  assert.equal(defaultModelId({ preferredModelId: null, models: [mining, local], settings: { load_on_start: 'mining' } }), 'chat')
})

test('explicit preferences survive missing files, offline runtimes and unloaded engines', () => {
  assert.equal(defaultModelId({ preferredModelId: 'chosen', models: [local], runtime: null }), 'chosen')
  assert.equal(defaultModelId({ preferredModelId: 'chosen', models: [], runtime: { model_id: 'other' } }), 'chosen')
})

test('the first default respects a resident model or configured local model and avoids models that do not fit', () => {
  assert.equal(defaultModelId({ preferredModelId: null, models: [local, mining], runtime: { model_id: 'mining' } }), 'mining')
  const other = { ...local, id: 'other' }
  assert.equal(defaultModelId({ preferredModelId: null, models: [local, other], settings: { load_on_start: 'other' } }), 'other')
  const large = { ...local, id: 'large', fit: { verdict: 'does_not_fit' } } as ModelView
  assert.equal(defaultModelId({ preferredModelId: null, models: [large, local] }), 'chat')
  assert.equal(defaultModelId({ preferredModelId: null, models: [] }), null)
})
