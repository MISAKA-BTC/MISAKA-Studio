import { test } from 'node:test'
import assert from 'node:assert/strict'
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { prepareDesktop } from './desktop.mjs'

for (const platform of ['darwin', 'win32', 'linux']) {
  for (const mode of ['build', 'dev']) {
    test(`${platform} ${mode} replaces a stale staged runtime after building the UI`, t => {
      const root = mkdtempSync(join(tmpdir(), 'studio build '))
      t.after(() => rmSync(root, { recursive: true, force: true }))
      const name = platform === 'win32' ? 'misaka-studiod.exe' : 'misaka-studiod'
      const staged = join(root, 'desktop', 'src-tauri', 'resources', name)
      mkdirSync(join(root, 'desktop', 'src-tauri', 'resources'), { recursive: true })
      writeFileSync(staged, 'old runtime')
      let uiBuilt = false
      const execute = (command, args, cwd) => {
        assert.equal(cwd, root)
        if (command === 'npm' && args.includes('build')) uiBuilt = true
        if (command === 'cargo') {
          assert.ok(uiBuilt, 'the runtime must embed the new UI')
          assert.equal(args.includes('--release'), mode === 'build')
          const target = join(root, 'target', mode === 'build' ? 'release' : 'debug')
          mkdirSync(target, { recursive: true })
          writeFileSync(join(target, name), 'new runtime with new UI')
        }
      }
      prepareDesktop(mode, { root, platform, execute })
      assert.equal(readFileSync(staged, 'utf8'), 'new runtime with new UI')
    })
  }
}

test('a failed build aborts packaging instead of silently using a stale resource', t => {
  const root = mkdtempSync(join(tmpdir(), 'studio failure '))
  t.after(() => rmSync(root, { recursive: true, force: true }))
  const resources = join(root, 'desktop', 'src-tauri', 'resources')
  mkdirSync(resources, { recursive: true })
  writeFileSync(join(resources, 'misaka-studiod'), 'old runtime')
  assert.throws(() => prepareDesktop('build', {
    root,
    execute(command) { if (command === 'cargo') throw new Error('compile failed') },
  }), /compile failed/)
  assert.equal(readFileSync(join(resources, 'misaka-studiod'), 'utf8'), 'old runtime')
})
