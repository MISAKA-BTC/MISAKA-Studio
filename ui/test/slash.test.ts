import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  fuzzyFilter,
  insertMention,
  matchCommands,
  mentionAt,
  parseSlash,
  parseToggle,
  pickModel,
  slashInput,
  SLASH_COMMANDS,
} from '../src/lib/slash.ts'

test('the slash token is read while it is typed: the name first, then the argument', () => {
  assert.deepEqual(slashInput('/'), { name: '', arg: null })
  assert.deepEqual(slashInput('/mo'), { name: 'mo', arg: null })
  assert.deepEqual(slashInput('/model '), { name: 'model', arg: '' })
  assert.deepEqual(slashInput('/model qwen 9b'), { name: 'model', arg: 'qwen 9b' })
  assert.equal(slashInput('hello /model'), null)
  assert.equal(slashInput('/model\nsecond line'), null, 'a multi-line message is a message')
})

test('the menu lists every command for a bare slash, and the matching ones as the name is typed', () => {
  assert.equal(matchCommands('').length, SLASH_COMMANDS.length)
  assert.equal(matchCommands('mo')[0]?.name, 'model')
  assert.equal(matchCommands('ex')[0]?.name, 'export')
  assert.equal(matchCommands('re')[0]?.name, 'rename')
  assert.deepEqual(matchCommands('zzz'), [])
})

test('only a known command is a command: anything else is sent as the message it is', () => {
  assert.deepEqual(
    (({ command, arg }) => ({ name: command.name, arg }))(parseSlash('/rename  Release notes ')!),
    { name: 'rename', arg: 'Release notes' },
  )
  assert.equal(parseSlash('/NEW')?.command.name, 'new')
  assert.equal(parseSlash('/usr/bin is a directory'), null)
  assert.equal(parseSlash('/news today'), null)
  assert.equal(parseSlash('no slash'), null)
})

test('on and off, in the spellings people use', () => {
  assert.equal(parseToggle('on'), true)
  assert.equal(parseToggle(' OFF '), false)
  assert.equal(parseToggle('yes'), true)
  assert.equal(parseToggle(''), null)
  assert.equal(parseToggle('maybe'), null)
})

test('a model is found from any part of its name', () => {
  const ids = ['Huihui-Qwen3.5-9B-abliterated-Q4_K_M', 'gemma-3-4b-it-Q8_0', 'Llama-3.2-1B-Instruct-Q4_K_M']
  assert.equal(pickModel('qwen', ids), ids[0])
  assert.equal(pickModel('qwen 9b', ids), ids[0])
  assert.equal(pickModel('gemma', ids), ids[1])
  assert.equal(pickModel('llama1b', ids), ids[2])
  assert.equal(pickModel('LLAMA-3.2', ids), ids[2])
  assert.equal(pickModel('mistral', ids), null)
  assert.equal(pickModel('', ids), null)
  assert.deepEqual(fuzzyFilter('', ids, (id) => id), ids)
})

test('a mention starts at an @ that begins a word and runs to the caret', () => {
  assert.deepEqual(mentionAt('@', 1), { start: 0, query: '' })
  assert.deepEqual(mentionAt('look at @src/ma', 15), { start: 8, query: 'src/ma' })
  assert.equal(mentionAt('mail me@example.com', 19), null, 'an address is not a mention')
  assert.equal(mentionAt('@src/main.rs and then', 21), null, 'the caret has left the mention')
  assert.equal(mentionAt('no mention here', 4), null)
})

test('choosing a file replaces the mention with its path and puts the caret after it', () => {
  assert.deepEqual(insertMention('fix @ma please', 4, 7, 'src/main.rs'), { text: 'fix @src/main.rs please', caret: 17 })
  assert.deepEqual(insertMention('@', 0, 1, 'a.ts'), { text: '@a.ts ', caret: 6 })
})
