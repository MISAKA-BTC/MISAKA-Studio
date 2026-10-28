import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  MAX_ATTACHMENT_BYTES,
  attachmentFromText,
  baseName,
  decodeUtf8,
  extensionOf,
  isBinaryName,
  isDuplicate,
  languageFor,
  looksBinary,
  readAttachment,
} from '../src/lib/attachments.ts'

const file = (parts: BlobPart[], name: string) => new File(parts, name)

test('the language comes from the extension, or from the whole name for files that have none', () => {
  assert.equal(languageFor('src/main.rs'), 'rust')
  assert.equal(languageFor('App.tsx'), 'tsx')
  assert.equal(languageFor('lib/api.TS'), 'typescript')
  assert.equal(languageFor('Dockerfile'), 'dockerfile')
  assert.equal(languageFor('Makefile'), 'makefile')
  assert.equal(languageFor('notes.txt'), null)
  assert.equal(languageFor('README'), null)
  assert.equal(extensionOf('.env'), '')
  assert.equal(extensionOf('archive.tar.gz'), 'gz')
  assert.equal(baseName('C:\\work\\a.py'), 'a.py')
})

test('binary files are known by name, and by a NUL in their first bytes', () => {
  assert.equal(isBinaryName('photo.PNG'), true)
  assert.equal(isBinaryName('model.gguf'), true)
  assert.equal(isBinaryName('main.rs'), false)
  assert.equal(looksBinary(new Uint8Array([72, 105, 0, 33])), true)
  assert.equal(looksBinary(new TextEncoder().encode('plain text — 日本語')), false)
})

test('a text file becomes an attachment with its name, size, content and language', async () => {
  const result = await readAttachment(file(['fn main() {}\n'], 'main.rs'))
  assert.ok(result.ok)
  assert.equal(result.attachment.name, 'main.rs')
  assert.equal(result.attachment.size, 13)
  assert.equal(result.attachment.content, 'fn main() {}\n')
  assert.equal(result.attachment.language, 'rust')
  assert.equal(result.attachment.truncated, undefined)
})

test('a binary file is refused with a reason, whether its name or its bytes give it away', async () => {
  const byName = await readAttachment(file(['not really an image'], 'shot.png'))
  assert.equal(byName.ok, false)
  assert.match(!byName.ok ? byName.reason : '', /image/)
  const byBytes = await readAttachment(file([new Uint8Array([1, 2, 0, 4])], 'data.txt'))
  assert.equal(byBytes.ok, false)
  assert.match(!byBytes.ok ? byBytes.reason : '', /binary/)
})

test('a file over the limit is cut to it and says so, keeping the original size', async () => {
  const big = 'a'.repeat(MAX_ATTACHMENT_BYTES + 5000)
  const result = await readAttachment(file([big], 'big.log'))
  assert.ok(result.ok)
  assert.equal(result.attachment.content.length, MAX_ATTACHMENT_BYTES)
  assert.equal(result.attachment.truncated, true)
  assert.equal(result.attachment.size, MAX_ATTACHMENT_BYTES + 5000)
})

test('a cut inside a multi-byte character drops the partial character instead of ending in U+FFFD', () => {
  const text = 'あ'.repeat(10) // three bytes each
  const bytes = new TextEncoder().encode(text)
  const { text: cut, truncated } = decodeUtf8(bytes, 7)
  assert.equal(truncated, true)
  assert.equal(cut, 'ああ')
  assert.ok(!cut.includes('\uFFFD'))
  assert.equal(decodeUtf8(new TextEncoder().encode('\uFEFFbom')).text, 'bom')
})

test('workspace files keep their path, and duplicates are recognised', () => {
  const a = attachmentFromText('src/lib/a.ts', 'export {}', { workspacePath: 'src/lib/a.ts' })
  assert.equal(a.name, 'a.ts')
  assert.equal(a.path, 'src/lib/a.ts')
  assert.equal(a.language, 'typescript')
  assert.equal(isDuplicate([a], attachmentFromText('src/lib/a.ts', 'other', { workspacePath: 'src/lib/a.ts' })), true)
  assert.equal(isDuplicate([a], attachmentFromText('a.ts', 'export {}')), false)
  const dropped = attachmentFromText('notes.md', 'hello')
  assert.equal(isDuplicate([dropped], attachmentFromText('notes.md', 'hello')), true)
})
