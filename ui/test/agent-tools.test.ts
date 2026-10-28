import assert from 'node:assert/strict'
import { test } from 'node:test'
import { applyEdit, planEdit, stripLineNumbers } from '../src/lib/agent/edit.ts'
import { fakeWorkspace } from '../src/lib/agent/fakeWorkspace.ts'
import {
  approvalFor,
  canonicalToolName,
  coerceArgs,
  formatExec,
  numberLines,
  outputLimits,
  prepareCall,
  recordChange,
  TRACK_LIMIT_BYTES,
  type ApprovalKind,
  type PreparedCall,
  type ToolContext,
} from '../src/lib/agent/tools.ts'
import { toolSpec } from '../src/lib/agent/protocol.ts'
import type { ApprovalPolicy } from '../src/lib/types.ts'

const MAIN = 'fn main() {\n    println!("hello");\n}\n'

function context(api: ReturnType<typeof fakeWorkspace>, policy: ApprovalPolicy = 'auto_edit', allowed: ApprovalKind[] = []): ToolContext {
  return { root: '/fake', api, policy, allowed: new Set(allowed), limits: outputLimits(16384) }
}

async function runIt(prepared: PreparedCall) {
  if (prepared.kind === 'settled') return prepared.outcome
  return prepared.run(new AbortController().signal)
}

test('the policy table: reads never ask; edits ask only under "ask"; commands run alone only under "full auto"', () => {
  const table = (['read_only', 'ask', 'auto_edit', 'full_auto'] as const).map((policy) => [
    approvalFor('read', policy),
    approvalFor('edit', policy),
    approvalFor('command', policy),
  ])
  assert.deepEqual(table, [
    ['auto', 'refuse', 'refuse'],
    ['auto', 'ask', 'ask'],
    ['auto', 'auto', 'ask'],
    ['auto', 'auto', 'auto'],
  ])
  assert.equal(approvalFor('edit', 'ask', new Set(['edit'])), 'auto', '"always allow" edits')
  assert.equal(approvalFor('command', 'auto_edit', new Set(['command'])), 'auto', '"always allow" commands')
  assert.equal(approvalFor('command', 'read_only', new Set(['command'])), 'refuse', 'never past read only')
})

test('arguments are coerced to what the tool declares, and required ones are checked', () => {
  const read = toolSpec('read_file')!
  assert.deepEqual(coerceArgs(read, { path: 'a.rs', start_line: '12', end_line: ' 20 ' }), { ok: true, args: { path: 'a.rs', start_line: 12, end_line: 20 } })
  const missing = coerceArgs(read, { start_line: 3 })
  assert.equal(missing.ok, false)
  assert.match((missing as { error: string }).error, /needs the parameter "path"/)
  assert.equal(coerceArgs(read, { path: 'a', start_line: 'ten' }).ok, false)
  const edit = toolSpec('edit_file')!
  assert.deepEqual(coerceArgs(edit, { path: 'a', old_string: 'x', new_string: '', replace_all: 'TRUE' }), {
    ok: true,
    args: { path: 'a', old_string: 'x', new_string: '', replace_all: true },
  })
  const write = toolSpec('write_file')!
  assert.deepEqual(coerceArgs(write, { path: 'p.json', content: { a: 1 } }), { ok: true, args: { path: 'p.json', content: '{\n  "a": 1\n}' } })
})

test('tool names small models reach for are understood', () => {
  assert.equal(canonicalToolName('functions.read_file'), 'read_file')
  assert.equal(canonicalToolName('readFile'), 'read_file')
  assert.equal(canonicalToolName('bash'), 'run_command')
  assert.equal(canonicalToolName('str_replace'), 'edit_file')
  assert.equal(canonicalToolName('teleport'), 'teleport')
})

test('read_file: numbered lines under a header, and how to read on', async () => {
  const body = Array.from({ length: 30 }, (_, i) => `line ${i + 1}`).join('\n') + '\n'
  const api = fakeWorkspace({ 'src/a.txt': body })
  const ctx = { ...context(api), limits: { ...outputLimits(16384), read: 80 } }
  const outcome = await runIt(await prepareCall({ name: 'read_file', args: { path: './src/a.txt' } }, ctx))
  assert.equal(outcome.status, 'done')
  const [header, first] = outcome.output.split('\n')
  assert.match(header!, /^src\/a\.txt \(lines 1-\d+ of 30\)$/)
  assert.equal(first, '  1\tline 1')
  assert.match(outcome.output, /To read on, call read_file with start_line \d+\.\]$/)
  assert.equal(outcome.display?.kind, 'read')
  const whole = await runIt(await prepareCall({ name: 'read_file', args: { path: 'src/a.txt' } }, context(api)))
  assert.ok(whole.output.startsWith('src/a.txt (30 lines)\n'))
  assert.match(whole.summary, /Read src\/a\.txt · 30 lines/)
})

test('read_file of a missing file is an error the model can act on', async () => {
  const api = fakeWorkspace({})
  const outcome = await runIt(await prepareCall({ name: 'read_file', args: { path: 'nope.rs' } }, context(api)))
  assert.equal(outcome.status, 'error')
  assert.equal(outcome.output, 'Error: nope.rs does not exist')
})

test('an edit that must be approved shows its diff first and touches nothing until it runs', async () => {
  const api = fakeWorkspace({ 'src/main.rs': MAIN })
  const prepared = await prepareCall(
    { name: 'edit_file', args: { path: 'src/main.rs', old_string: '    println!("hello");', new_string: '    println!("hi");' } },
    context(api, 'ask'),
  )
  assert.equal(prepared.kind, 'run')
  if (prepared.kind !== 'run') return
  assert.equal(prepared.approval, 'ask')
  assert.equal(prepared.preview?.kind, 'diff')
  if (prepared.preview?.kind === 'diff') {
    assert.match(prepared.preview.patch, /-    println!\("hello"\);\n\+    println!\("hi"\);/)
    assert.deepEqual([prepared.preview.added, prepared.preview.removed], [1, 1])
  }
  assert.ok(!api.calls.some((c) => c.method === 'edit' || c.method === 'write'), 'nothing written while it waits')
  assert.equal(api.files.get('src/main.rs'), MAIN)
  const outcome = await prepared.run(new AbortController().signal)
  assert.equal(outcome.status, 'done')
  assert.equal(api.files.get('src/main.rs'), 'fn main() {\n    println!("hi");\n}\n')
  assert.match(outcome.output, /^Edited src\/main\.rs: 1 replacement \(\+1 -1\)\.\nThe changed part now reads:\n/)
  assert.match(outcome.output, / {2}2\t {4}println!\("hi"\);/)
  assert.deepEqual(outcome.change, { path: 'src/main.rs', previous: MAIN, content: 'fn main() {\n    println!("hi");\n}\n', created: false })
})

test('an edit that cannot apply is answered at once, without asking anyone', async () => {
  const api = fakeWorkspace({ 'src/main.rs': MAIN })
  const missing = await prepareCall({ name: 'edit_file', args: { path: 'src/main.rs', old_string: 'println!("bye")', new_string: 'x' } }, context(api, 'ask'))
  assert.equal(missing.kind, 'settled')
  assert.match((await runIt(missing)).output, /^Error: old_string was not found in src\/main\.rs\./)
  const twice = fakeWorkspace({ 'a.txt': 'x\nx\n' })
  const ambiguous = await runIt(await prepareCall({ name: 'edit_file', args: { path: 'a.txt', old_string: 'x', new_string: 'y' } }, context(twice, 'ask')))
  assert.match(ambiguous.output, /old_string matches 2 places in a\.txt/)
  const absent = await runIt(await prepareCall({ name: 'edit_file', args: { path: 'b.txt', old_string: 'x', new_string: 'y' } }, context(twice)))
  assert.equal(absent.output, 'Error: b.txt does not exist. To create it, use write_file.')
  assert.ok(!twice.calls.some((c) => c.method === 'edit'))
})

test('line numbers copied from read_file are taken off old_string and new_string', async () => {
  const api = fakeWorkspace({ 'src/main.rs': MAIN })
  const outcome = await runIt(
    await prepareCall({ name: 'edit_file', args: { path: 'src/main.rs', old_string: '  2\t    println!("hello");', new_string: '  2\t    println!("hey");' } }, context(api)),
  )
  assert.equal(outcome.status, 'done')
  assert.equal(api.files.get('src/main.rs'), 'fn main() {\n    println!("hey");\n}\n')
  assert.match(outcome.output, /line-number prefixes/)
})

test('write_file creates a file, and the change records it as created', async () => {
  const api = fakeWorkspace({})
  const outcome = await runIt(await prepareCall({ name: 'write_file', args: { path: 'src/new.ts', content: 'export {}\n' } }, context(api)))
  assert.equal(outcome.output, 'Created src/new.ts (1 line).')
  assert.equal(outcome.summary, 'Create src/new.ts')
  assert.deepEqual(outcome.change, { path: 'src/new.ts', previous: null, content: 'export {}\n', created: true })
  assert.equal(outcome.display?.kind === 'diff' && outcome.display.created, true)
})

test('write_file under "ask" previews the whole new file, and an identical write changes nothing', async () => {
  const api = fakeWorkspace({ 'a.txt': 'same\n' })
  const create = await prepareCall({ name: 'write_file', args: { path: 'b.txt', content: 'one\ntwo\n' } }, context(api, 'ask'))
  assert.equal(create.kind === 'run' && create.summary, 'Create b.txt')
  assert.equal(create.kind === 'run' && create.preview?.kind === 'diff' && create.preview.added, 2)
  const same = await prepareCall({ name: 'write_file', args: { path: 'a.txt', content: 'same\n' } }, context(api, 'ask'))
  assert.equal(same.kind, 'settled')
  assert.match((await runIt(same)).output, /already has exactly this content/)
})

test('read only refuses every change and every command, and says how to get them', async () => {
  const api = fakeWorkspace({ 'a.txt': 'x\n' })
  for (const call of [
    { name: 'write_file', args: { path: 'a.txt', content: 'y' } },
    { name: 'edit_file', args: { path: 'a.txt', old_string: 'x', new_string: 'y' } },
    { name: 'run_command', args: { command: 'rm a.txt' } },
  ]) {
    const prepared = await prepareCall(call, context(api, 'read_only'))
    assert.equal(prepared.kind, 'settled', call.name)
    assert.match((await runIt(prepared)).output, /approval policy is "Read only"/)
  }
  assert.equal(api.calls.length, 0)
})

test('an unknown tool is answered with the tools there are', async () => {
  const outcome = await runIt(await prepareCall({ name: 'teleport', args: {} }, context(fakeWorkspace())))
  assert.match(outcome.output, /no tool called "teleport". The tools are: list_files, read_file, search, edit_file, write_file, run_command\./)
  const readOnly = await runIt(await prepareCall({ name: 'teleport', args: {} }, context(fakeWorkspace(), 'read_only')))
  assert.match(readOnly.output, /The tools are: list_files, read_file, search\.$/)
})

test('search: one match per line, and a clear answer when there is none', async () => {
  const api = fakeWorkspace({ 'src/a.ts': 'const foo = 1\nfoo()\n', 'src/b.rs': 'fn foo() {}\n', 'README.md': 'nothing\n' })
  const outcome = await runIt(await prepareCall({ name: 'search', args: { pattern: 'foo', glob: '*.ts' } }, context(api)))
  assert.equal(outcome.output, '2 matches in 1 file:\nsrc/a.ts:1: const foo = 1\nsrc/a.ts:2: foo()')
  const none = await runIt(await prepareCall({ name: 'search', args: { pattern: 'zzz' } }, context(api)))
  assert.equal(none.output, 'No matches for "zzz" (3 files searched).')
})

test('list_files: an indented tree relative to the folder listed', async () => {
  const api = fakeWorkspace({ 'src/a.ts': '', 'src/lib/b.ts': '', 'package.json': '{}' })
  const outcome = await runIt(await prepareCall({ name: 'list_files', args: {} }, context(api)))
  assert.equal(outcome.output, './ (4 entries)\nsrc/\n  lib/\n  a.ts\npackage.json')
  const sub = await runIt(await prepareCall({ name: 'list_files', args: { path: 'src', depth: '3' } }, context(api)))
  assert.equal(sub.output, 'src/ (3 entries)\nlib/\n  b.ts\na.ts')
})

test('run_command: exit code and both streams for the model, and a stop is said as one', async () => {
  const api = fakeWorkspace({})
  api.onExec((command) => ({ command, exit_code: 1, stdout: 'ran 3 tests\n', stderr: 'test b failed\n', timed_out: false, duration_ms: 1234, truncated: false }))
  const prepared = await prepareCall({ name: 'run_command', args: { command: 'npm test', timeout_secs: '30' } }, context(api))
  assert.equal(prepared.kind === 'run' && prepared.approval, 'ask', 'auto edit still asks before a command')
  const outcome = await runIt(prepared)
  assert.equal(outcome.output, 'Exit code 1 (1.2 s).\nstdout:\nran 3 tests\nstderr:\ntest b failed')
  assert.deepEqual(api.calls.at(-1), { method: 'exec', args: ['npm test', 30] })
  assert.equal(outcome.display?.kind === 'command' && outcome.display.exitCode, 1)

  api.onExec(() => new Promise(() => {}))
  const hanging = await prepareCall({ name: 'run_command', args: { command: 'sleep 100' } }, context(api, 'full_auto'))
  assert.equal(hanging.kind === 'run' && hanging.approval, 'auto')
  const controller = new AbortController()
  const pending = hanging.kind === 'run' ? hanging.run(controller.signal) : null
  controller.abort()
  const stopped = await pending!
  assert.equal(stopped.status, 'error')
  assert.match(stopped.output, /Stopped by the user/)
})

test('a long command output keeps its head and tail within the cap', () => {
  const stdout = Array.from({ length: 2000 }, (_, i) => `out ${i}`).join('\n')
  const text = formatExec({ command: 'x', exit_code: 0, stdout, stderr: 'warning: w\n', timed_out: false, duration_ms: 10, truncated: false }, 2000)
  assert.ok(text.length < 2300, String(text.length))
  assert.ok(text.includes('out 0\n') && text.includes('out 1999'))
  assert.ok(text.includes('stderr:\nwarning: w'))
  assert.match(formatExec({ command: 'x', exit_code: null, stdout: '', stderr: '', timed_out: true, duration_ms: 120_000, truncated: false }, 2000), /^Timed out after 120\.0 s/)
})

test('numbered lines: right-aligned numbers, a tab, carriage returns dropped', () => {
  assert.equal(numberLines('a\r\nb\n', 9), '  9\ta\n 10\tb')
})

test('the Changes list keeps the first original, drops a file back where it started, and will not keep huge files', () => {
  let list = recordChange(undefined, { path: 'a.rs', previous: 'v1', content: 'v2', created: false }, 1)
  list = recordChange(list, { path: 'a.rs', previous: 'v2', content: 'v3', created: false }, 2)
  assert.deepEqual(list, [{ path: 'a.rs', original: 'v1', current: 'v3', updatedAt: 2 }])
  list = recordChange(list, { path: 'a.rs', previous: 'v3', content: 'v1', created: false }, 3)
  assert.deepEqual(list, [], 'edited back to the original: no change left')
  list = recordChange(list, { path: 'new.rs', previous: null, content: 'x', created: true }, 4)
  assert.deepEqual(list, [{ path: 'new.rs', original: null, current: 'x', updatedAt: 4 }])
  const huge = 'x'.repeat(TRACK_LIMIT_BYTES + 1)
  const big = recordChange([], { path: 'big.txt', previous: huge, content: 'small', created: false }, 5)
  assert.deepEqual(big, [{ path: 'big.txt', original: null, current: null, untracked: true, updatedAt: 5 }])
  const unknown = recordChange([], { path: 'huge.bin', previous: null, content: 'y', created: false }, 6)
  assert.equal(unknown[0]!.untracked, true, 'the runtime returned no original (over 2 MiB)')
  assert.deepEqual(recordChange([], { path: 'same', previous: 'a', content: 'a', created: false }, 7), [])
})

test('the edit rules mirror the runtime: line endings, line-end whitespace, never indentation', () => {
  const crlf = 'one\r\ntwo\r\nthree\r\n'
  const converted = applyEdit(crlf, 'one\ntwo\n', 'uno\ndos\n', false, 'f')
  assert.deepEqual(converted, { ok: true, content: 'uno\r\ndos\r\nthree\r\n', replacements: 1 })
  const trailing = applyEdit('let a = 1;   \nlet b = 2;\n', 'let a = 1;\nlet b = 2;', 'let c = 3;', false, 'f')
  assert.deepEqual(trailing, { ok: true, content: 'let c = 3;\n', replacements: 1 })
  const indented = applyEdit('def f():\n    return 1\n', 'def f():\n  return 1', 'x', false, 'f.py')
  assert.equal(indented.ok, false)
  assert.match((indented as { error: string }).error, /except for indentation starts at line 1/)
  const dollars = applyEdit('a\na\n', 'a', '$&$1', true, 'f')
  assert.deepEqual(dollars, { ok: true, content: '$&$1\n$&$1\n', replacements: 2 }, 'a replacement is literal text')
  assert.equal(applyEdit('aaa', 'aa', 'b', false, 'f').ok, false, 'overlapping matches are two matches')
  assert.match((applyEdit('x', '', 'y', false, 'f') as { error: string }).error, /old_string is empty/)
  assert.match((applyEdit('x', 'x', 'x', false, 'f') as { error: string }).error, /identical/)
})

test('line numbers are stripped only when every line carries one', () => {
  assert.equal(stripLineNumbers('  1\tfoo\n  2\tbar'), 'foo\nbar')
  assert.equal(stripLineNumbers('  1\tfoo\nbar'), null)
  assert.equal(stripLineNumbers('plain'), null)
  const plan = planEdit('a\nb\n', ' 12\ta', ' 12\tz', false, 'f')
  assert.equal(plan.ok && plan.strippedLineNumbers, true)
  assert.equal(plan.ok && plan.content, 'z\nb\n')
})
