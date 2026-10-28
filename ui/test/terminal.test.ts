import assert from 'node:assert/strict'
import { test } from 'node:test'
import { MAX_ATTACHMENT_BYTES } from '../src/lib/attachments.ts'
import {
  EMPTY_OUTPUT,
  appendOutput,
  applyCarriageReturns,
  droppedNote,
  formatDuration,
  historyStep,
  normalizeRelative,
  outcomeText,
  outputText,
  parseCd,
  promptLabel,
  pushHistory,
  resolveCd,
  shellFlavor,
  shellQuote,
  stripAnsi,
  terminalAttachment,
  transcript,
  withCwd,
  type BlockOutput,
  type BlockSummary,
} from '../src/lib/terminal.ts'

const ROOT = '/Users/me/work/project'

// --- quoting -----------------------------------------------------------------------------------

test('an argument is single-quoted, and a quote inside it closes, escapes and reopens', () => {
  assert.equal(shellQuote('src'), `'src'`)
  assert.equal(shellQuote('my dir'), `'my dir'`)
  assert.equal(shellQuote(`it's`), `'it'\\''s'`)
  assert.equal(shellQuote('$HOME `x` "y" \\z'), `'$HOME \`x\` "y" \\z'`, 'nothing is special inside single quotes')
  assert.equal(shellQuote('C:\\work dir', 'windows'), '"C:\\work dir"')
})

test('a command runs in the directory with a cd prefix; the folder itself needs none', () => {
  assert.equal(withCwd('ls -la', ''), 'ls -la')
  assert.equal(withCwd('ls', 'src/lib'), `cd 'src/lib' && ls`)
  assert.equal(withCwd('make', `it's here`), `cd 'it'\\''s here' && make`)
  assert.equal(withCwd('ls', '-weird'), `cd './-weird' && ls`, 'a leading dash is not read as an option')
  assert.equal(withCwd('dir', 'src/lib', 'windows'), 'cd /d "src\\lib" && dir')
  assert.equal(shellFlavor('Windows 11 (26100)'), 'windows')
  assert.equal(shellFlavor('Darwin 25.6.0'), 'posix')
  assert.equal(shellFlavor(null), 'posix')
})

test('the prompt names the folder and the directory within it', () => {
  assert.equal(promptLabel('project', ''), 'project')
  assert.equal(promptLabel('project', 'src/lib'), 'project/src/lib')
})

// --- cd ----------------------------------------------------------------------------------------

test('a lone cd is recognised with its argument; anything else is left to the shell', () => {
  assert.deepEqual(parseCd('cd'), { ok: true, target: null })
  assert.deepEqual(parseCd('  cd   src  '), { ok: true, target: 'src' })
  assert.deepEqual(parseCd(`cd 'my dir'`), { ok: true, target: 'my dir' })
  assert.deepEqual(parseCd('cd "a \\"b\\""'), { ok: true, target: 'a "b"' })
  assert.deepEqual(parseCd('cd my\\ dir'), { ok: true, target: 'my dir' })
  assert.deepEqual(parseCd(`cd ''`), { ok: true, target: '' })
  assert.deepEqual(parseCd('cd -'), { ok: true, target: '-' })
  assert.deepEqual(parseCd('cd -P -- -dash'), { ok: true, target: '-dash' })
  assert.deepEqual(parseCd('cd src # into the sources'), { ok: true, target: 'src' })
  assert.equal(parseCd('ls'), null)
  assert.equal(parseCd('cdk deploy'), null, 'a command that only starts with "cd"')
  assert.equal(parseCd('cd src && make'), null, 'a compound line runs in the shell as typed')
  assert.equal(parseCd('cd src; ls'), null)
  assert.equal(parseCd('cd src | cat'), null)
})

test('what cd cannot do here is said, not guessed', () => {
  assert.equal(parseCd('cd a b')?.ok, false)
  assert.equal(parseCd('cd $HOME')?.ok, false)
  assert.equal(parseCd('cd "$HOME/x"')?.ok, false)
  assert.equal(parseCd('cd src/*')?.ok, false)
  assert.equal(parseCd(`cd 'unclosed`)?.ok, false)
  assert.equal(parseCd('cd -x')?.ok, false)
  assert.deepEqual(parseCd(`cd '$HOME'`), { ok: true, target: '$HOME' }, 'quoted, it is a name like any other')
})

test('relative paths resolve against the directory, with . and ..', () => {
  const at = (cwd: string, target: string | null) => resolveCd(cwd, target, { root: ROOT })
  assert.deepEqual(at('', 'src'), { ok: true, cwd: 'src' })
  assert.deepEqual(at('src', 'lib/agent'), { ok: true, cwd: 'src/lib/agent' })
  assert.deepEqual(at('src/lib', '..'), { ok: true, cwd: 'src' })
  assert.deepEqual(at('src/lib', '../..'), { ok: true, cwd: '' })
  assert.deepEqual(at('src', './lib//./x/'), { ok: true, cwd: 'src/lib/x' })
  assert.deepEqual(at('src/lib', null), { ok: true, cwd: '' }, 'a bare cd goes back to the folder')
  assert.deepEqual(at('src/lib', ''), { ok: true, cwd: '' })
})

test('leaving the folder is refused, however it is spelled', () => {
  const refused = (cwd: string, target: string, extra: { home?: string } = {}) => {
    const result = resolveCd(cwd, target, { root: ROOT, ...extra })
    assert.equal(result.ok, false, `${cwd} → ${target}`)
    if (!result.ok) assert.match(result.error, /outside project/)
  }
  refused('', '..')
  refused('src', '../..')
  refused('src', '../../project-other')
  refused('', '/')
  refused('', '/Users/me/work')
  refused('', '/Users/me/work/project-other', {})
  refused('', '/Users/me/work/project/../other')
  refused('', '~', { home: '/Users/me' })
  refused('', '~')
})

test('an absolute path or ~ path inside the folder is turned into a relative one', () => {
  const at = (target: string) => resolveCd('src', target, { root: ROOT, home: '/Users/me' })
  assert.deepEqual(at(ROOT), { ok: true, cwd: '' })
  assert.deepEqual(at(`${ROOT}/`), { ok: true, cwd: '' })
  assert.deepEqual(at(`${ROOT}/ui/src`), { ok: true, cwd: 'ui/src' })
  assert.deepEqual(at(`${ROOT}/ui/../docs`), { ok: true, cwd: 'docs' })
  assert.deepEqual(at('~/work/project/ui'), { ok: true, cwd: 'ui' })
  assert.equal(at('~other/x').ok, false)
  assert.deepEqual(resolveCd('', 'C:\\Work\\Project\\src', { root: 'C:\\work\\project', flavor: 'windows' }), { ok: true, cwd: 'src' })
  assert.deepEqual(resolveCd('src', '..\\docs', { root: 'C:\\work\\project', flavor: 'windows' }), { ok: true, cwd: 'docs' })
})

test('cd - goes back to the previous directory, when there is one', () => {
  assert.deepEqual(resolveCd('src', '-', { root: ROOT, previous: 'docs' }), { ok: true, cwd: 'docs' })
  assert.deepEqual(resolveCd('src', '-', { root: ROOT, previous: '' }), { ok: true, cwd: '' })
  assert.equal(resolveCd('src', '-', { root: ROOT }).ok, false)
})

test('normalising a relative path keeps POSIX backslashes as characters', () => {
  assert.equal(normalizeRelative('a/./b/../c'), 'a/c')
  assert.equal(normalizeRelative('..'), null)
  assert.equal(normalizeRelative('a\\b'), 'a\\b')
  assert.equal(normalizeRelative('a\\b', 'windows'), 'a/b')
})

// --- history -----------------------------------------------------------------------------------

test('history keeps one copy of each command, newest last, and at most its limit', () => {
  let history: string[] = []
  history = pushHistory(history, 'ls')
  history = pushHistory(history, 'git status')
  history = pushHistory(history, 'ls')
  assert.deepEqual(history, ['git status', 'ls'])
  history = pushHistory(history, '   ')
  assert.deepEqual(history, ['git status', 'ls'], 'an empty line is not remembered')
  history = pushHistory(history, ' export TOKEN=secret')
  assert.deepEqual(history, ['git status', 'ls'], 'a line typed with a leading space is not remembered')
  history = pushHistory(history, 'npm test  ')
  assert.deepEqual(history, ['git status', 'ls', 'npm test'])
  let long: string[] = []
  for (let i = 0; i < 120; i++) long = pushHistory(long, `echo ${i}`)
  assert.equal(long.length, 100)
  assert.equal(long[0], 'echo 20')
  assert.equal(long[99], 'echo 119')
  assert.deepEqual(pushHistory(['a', 'b', 'c'], 'd', 3), ['b', 'c', 'd'])
})

test('↑ walks back to the oldest command and ↓ returns to the line being typed', () => {
  assert.equal(historyStep(0, null, 'up'), null)
  assert.equal(historyStep(3, null, 'up'), 2)
  assert.equal(historyStep(3, 2, 'up'), 1)
  assert.equal(historyStep(3, 0, 'up'), 0, 'stays at the oldest')
  assert.equal(historyStep(3, 0, 'down'), 1)
  assert.equal(historyStep(3, 2, 'down'), null, 'past the newest is the typed line')
  assert.equal(historyStep(3, null, 'down'), null)
})

// --- output ------------------------------------------------------------------------------------

function fill(chunks: [stream: 'stdout' | 'stderr', text: string][], max?: number): BlockOutput {
  return chunks.reduce((output, [stream, text]) => appendOutput(output, stream, text, max), EMPTY_OUTPUT)
}

test('output keeps one segment per run of a stream', () => {
  const output = fill([
    ['stdout', 'a'],
    ['stdout', 'b\n'],
    ['stderr', 'warn\n'],
    ['stdout', 'c\n'],
  ])
  assert.deepEqual(output.segments, [
    { stream: 'stdout', text: 'ab\n' },
    { stream: 'stderr', text: 'warn\n' },
    { stream: 'stdout', text: 'c\n' },
  ])
  assert.equal(output.size, 10)
  assert.equal(output.dropped, 0)
  assert.equal(droppedNote(output), null)
  assert.equal(appendOutput(output, 'stdout', ''), output, 'nothing added is the same object')
})

test('past the cap the start goes and the tail is kept, cut at a line break', () => {
  const lines = Array.from({ length: 100 }, (_, i) => `line ${String(i).padStart(3, '0')}\n`) // 9 chars each
  let output = EMPTY_OUTPUT
  for (const line of lines) output = appendOutput(output, 'stdout', line, 100)
  assert.ok(output.size <= 100)
  const text = outputText(output)
  assert.ok(text.endsWith('line 099\n'), 'the newest line is kept')
  assert.ok(text.startsWith('line '), 'the kept text starts with a whole line')
  assert.equal(output.size + output.dropped, 900, 'every character is either kept or counted as dropped')
  assert.equal(output.droppedLines, 100 - text.split('\n').filter(Boolean).length)
  assert.match(droppedNote(output) ?? '', /earlier lines not kept/)
})

test('a cap that drops a whole segment drops it, and stderr stays where it was in the tail', () => {
  const output = fill(
    [
      ['stdout', 'x'.repeat(50)],
      ['stderr', 'error: it broke\n'],
      ['stdout', 'y'.repeat(30)],
    ],
    60,
  )
  assert.equal(output.size, 60)
  assert.equal(output.dropped, 36)
  assert.deepEqual(
    output.segments.map((s) => s.stream),
    ['stdout', 'stderr', 'stdout'],
  )
  assert.equal(output.segments[1]?.text, 'error: it broke\n')
  const single = fill([['stdout', 'z'.repeat(500)]], 100)
  assert.equal(single.size, 100)
  assert.equal(single.dropped, 400)
  assert.equal(single.droppedLines, 0)
  assert.match(droppedNote(single) ?? '', /400 earlier characters/)
})

test('escapes are removed and a redrawn line shows only its last drawing', () => {
  assert.equal(stripAnsi('\x1b[31mred\x1b[0m and \x1b[1;32mgreen\x1b[m'), 'red and green')
  assert.equal(stripAnsi('\x1b]0;title\x07text'), 'text')
  assert.equal(applyCarriageReturns('10%\r50%\r100%\ndone\n'), '100%\ndone\n')
  assert.equal(applyCarriageReturns('windows line\r\nnext\r\n'), 'windows line\nnext\n')
  assert.equal(applyCarriageReturns('no returns'), 'no returns')
})

// --- transcripts ---------------------------------------------------------------------------------

const block = (extra: Partial<BlockSummary> = {}): BlockSummary => ({
  command: 'npm test',
  prompt: 'project/ui',
  output: fill([
    ['stdout', 'ok 1\n'],
    ['stderr', 'not ok 2\n'],
  ]),
  status: 'done',
  exitCode: 1,
  durationMs: 2300,
  ...extra,
})

test('durations read the way a person says them', () => {
  assert.equal(formatDuration(42), '42ms')
  assert.equal(formatDuration(420), '0.4s')
  assert.equal(formatDuration(2300), '2.3s')
  assert.equal(formatDuration(12_400), '12s')
  assert.equal(formatDuration(185_000), '3m 05s')
  assert.equal(formatDuration(3_725_000), '1h 02m')
  assert.equal(formatDuration(-5), '0ms')
})

test('the outcome says how the command ended', () => {
  assert.equal(outcomeText(block()), 'exit 1 · 2.3s')
  assert.equal(outcomeText(block({ status: 'stopped', durationMs: 1000 })), 'stopped · 1.0s')
  assert.equal(outcomeText(block({ status: 'timed_out', durationMs: 900_000 })), 'timed out · 15m 00s')
  assert.equal(outcomeText(block({ status: 'error', error: 'the runtime is unreachable', durationMs: null })), 'not run: the runtime is unreachable')
  assert.equal(outcomeText(block({ status: 'running', durationMs: null })), 'running')
})

test('a transcript is the prompt and command, the output, and the outcome', () => {
  assert.equal(transcript(block()), 'project/ui $ npm test\nok 1\nnot ok 2\n[exit 1 · 2.3s]')
  const silent = transcript(block({ output: EMPTY_OUTPUT, exitCode: 0 }))
  assert.equal(silent, 'project/ui $ npm test\n[exit 0 · 2.3s]')
})

test('a block becomes a text attachment named for its command', () => {
  const attachment = terminalAttachment(block())
  assert.match(attachment.name, /^terminal output · npm test$/)
  assert.equal(attachment.content, transcript(block()))
  assert.equal(attachment.language, 'console')
  assert.equal(attachment.truncated, undefined)
  assert.equal(attachment.size, new TextEncoder().encode(attachment.content).length)
  assert.ok(terminalAttachment(block({ command: 'x'.repeat(80) })).name.endsWith('…'))
})

test('an attachment past the limit keeps the end of the output, where the verdict is', () => {
  const huge = appendOutput(EMPTY_OUTPUT, 'stdout', `${'日本語のログ\n'.repeat(30_000)}FINAL LINE\n`, 1_000_000)
  const attachment = terminalAttachment(block({ output: huge }))
  assert.equal(attachment.truncated, true)
  assert.ok(new TextEncoder().encode(attachment.content).length <= MAX_ATTACHMENT_BYTES)
  assert.ok(attachment.content.startsWith('project/ui $ npm test\n[… the start of the output is not attached]'))
  assert.ok(attachment.content.includes('FINAL LINE\n[exit 1 · 2.3s]'))
  assert.ok(!attachment.content.includes('\uFFFD'))
  const dropped = terminalAttachment(block({ output: { ...block().output, dropped: 10, droppedLines: 2 } }))
  assert.equal(dropped.truncated, true, 'output the terminal already let go of makes it a cut attachment')
})
