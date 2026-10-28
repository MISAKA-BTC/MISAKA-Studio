import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  TOOLS,
  buildSystemPrompt,
  callKey,
  closeOpenToolCall,
  hasOpenToolCall,
  keepFirstCall,
  parseAgentText,
  removeThinking,
  repairJson,
  toolResultsMessage,
  toolsFor,
  truncateMiddle,
  type Segment,
  type ToolSegment,
} from '../src/lib/agent/protocol.ts'

const tools = (segments: Segment[]) => segments.filter((s): s is ToolSegment => s.kind === 'tool')
const texts = (segments: Segment[]) => segments.filter((s) => s.kind === 'text').map((s) => (s as { text: string }).text)

const EDIT_CALL =
  '<tool_call>\n<function=edit_file>\n<parameter=path>\nsrc/main.rs\n</parameter>\n<parameter=old_string>\nfn main() {\n    println!("hi");\n}\n</parameter>\n<parameter=new_string>\nfn main() {}\n</parameter>\n</function>\n</tool_call>'

test('the XML-parameter form: the name, and every value verbatim but for one newline at each end', () => {
  const segments = parseAgentText(`I will replace the body.\n${EDIT_CALL}`, { final: true })
  assert.deepEqual(texts(segments), ['I will replace the body.'])
  const [call] = tools(segments)
  assert.equal(call!.name, 'edit_file')
  assert.equal(call!.complete, true)
  assert.deepEqual(call!.args, { path: 'src/main.rs', old_string: 'fn main() {\n    println!("hi");\n}', new_string: 'fn main() {}' })
  assert.equal(call!.raw, EDIT_CALL)
})

test('a value that starts with a blank line keeps it: only the format’s own newline goes', () => {
  const [call] = tools(parseAgentText('<tool_call>\n<function=write_file>\n<parameter=path>\na.md\n</parameter>\n<parameter=content>\n\n# Title\n\n</parameter>\n</function>\n</tool_call>', { final: true }))
  assert.equal(call!.args.content, '\n# Title\n')
})

test('inline values (no newlines around them) are read as they are', () => {
  const [call] = tools(parseAgentText('<tool_call><function=read_file><parameter=path>a.rs</parameter><parameter=start_line>10</parameter></function></tool_call>', { final: true }))
  assert.deepEqual(call!.args, { path: 'a.rs', start_line: '10' })
})

test('a reply the stop sequence left open is complete once its </function> arrived', () => {
  const content = 'Reading it.\n<tool_call>\n<function=read_file>\n<parameter=path>\nsrc/lib.rs\n</parameter>\n</function>\n'
  assert.equal(hasOpenToolCall(content), true)
  const [call] = tools(parseAgentText(content, { final: true }))
  assert.equal(call!.complete, true)
  assert.deepEqual(call!.args, { path: 'src/lib.rs' })
  const closed = closeOpenToolCall(content)
  assert.equal(closed, `${content}</tool_call>`)
  assert.equal(hasOpenToolCall(closed), false)
  assert.equal(closeOpenToolCall(closed), closed, 'closing twice changes nothing')
})

test('a call cut off inside a value is incomplete, even in a finished reply — it must never run', () => {
  const content = '<tool_call>\n<function=write_file>\n<parameter=path>\na.rs\n</parameter>\n<parameter=content>\nline 1\nline'
  const [call] = tools(parseAgentText(content, { final: true }))
  assert.equal(call!.complete, false)
  assert.equal(call!.name, 'write_file')
  assert.equal(call!.args.content, 'line 1\nline')
})

test('closing a stopped call reads an unterminated last value up to the close (the model forgot </parameter>)', () => {
  const closed = closeOpenToolCall('<tool_call>\n<function=run_command>\n<parameter=command>\nls -la\n')
  const [call] = tools(parseAgentText(closed, { final: true }))
  assert.equal(call!.complete, true)
  assert.deepEqual(call!.args, { command: 'ls -la' })
})

test('streaming: a tag still arriving is held back, and a call still arriving is a tool segment, never XML', () => {
  assert.deepEqual(parseAgentText('Let me look.\n<tool_c', { final: false }), [{ kind: 'text', text: 'Let me look.' }])
  assert.deepEqual(parseAgentText('Let me look.\n<', { final: false }), [{ kind: 'text', text: 'Let me look.' }])
  const opening = parseAgentText('Let me look.\n<tool_call>\n<function=read', { final: false })
  assert.equal(opening.length, 2)
  assert.deepEqual({ ...opening[1], raw: undefined }, { kind: 'tool', name: '', args: {}, complete: false, raw: undefined })
  const named = tools(parseAgentText('<tool_call>\n<function=edit_file>\n<parameter=path>\nsrc/a.ts\n</parameter>\n<parameter=old_str', { final: false }))
  assert.equal(named[0]!.name, 'edit_file')
  assert.equal(named[0]!.complete, false)
  assert.deepEqual(named[0]!.args, { path: 'src/a.ts' })
  for (let cut = 1; cut < EDIT_CALL.length; cut++) {
    const segments = parseAgentText(`Text first.\n${EDIT_CALL.slice(0, cut)}`, { final: false })
    for (const text of texts(segments)) assert.ok(!text.includes('<'), `cut at ${cut}: ${JSON.stringify(text)}`)
  }
})

test('a lone "<" ending a finished reply is text; a recognisable start of a call is not', () => {
  assert.deepEqual(texts(parseAgentText('x <', { final: true })), ['x <'])
  assert.deepEqual(texts(parseAgentText('x <tool_ca', { final: true })), ['x'])
  assert.deepEqual(texts(parseAgentText('a < b and <tag>', { final: false })), ['a < b and <tag>'])
})

test('Hermes JSON calls, with the mistakes small models make repaired', () => {
  const raw = '<tool_call>\n{"name": "write_file", "arguments": {"path": "a.py", "content": "def f():\n    return 1\n"}}\n</tool_call>'
  const [call] = tools(parseAgentText(raw, { final: true }))
  assert.equal(call!.name, 'write_file')
  assert.equal(call!.complete, true)
  assert.deepEqual(call!.args, { path: 'a.py', content: 'def f():\n    return 1\n' })
  const [regex] = tools(parseAgentText('<tool_call>{"name": "search", "arguments": {"pattern": "fn \\w+\\(", "regex": true,}}</tool_call>', { final: true }))
  assert.deepEqual(regex!.args, { pattern: 'fn \\w+\\(', regex: true })
  const [stringArgs] = tools(parseAgentText('<tool_call>{"name": "read_file", "arguments": "{\\"path\\": \\"b.rs\\"}"}</tool_call>', { final: true }))
  assert.deepEqual(stringArgs!.args, { path: 'b.rs' })
  const [fenced] = tools(parseAgentText('<tool_call>\n```json\n{"name": "list_files", "arguments": {}}\n```\n</tool_call>', { final: true }))
  assert.equal(fenced!.name, 'list_files')
})

test('a JSON call still streaming shows its name as soon as it has one', () => {
  const [call] = tools(parseAgentText('<tool_call>\n{"name": "read_file", "arguments": {"pa', { final: false }))
  assert.equal(call!.name, 'read_file')
  assert.equal(call!.complete, false)
})

test('JSON arguments inside the XML form are read', () => {
  const [call] = tools(parseAgentText('<tool_call>\n<function=read_file>\n{"path": "a.rs", "start_line": 3}\n</function>\n</tool_call>', { final: true }))
  assert.deepEqual(call!.args, { path: 'a.rs', start_line: 3 })
})

test('reasoning is not text, and a call drafted inside it is not a call', () => {
  const content = '<think>maybe <tool_call>\n<function=run_command>\n<parameter=command>\nrm -rf /\n</parameter>\n</function>\n</tool_call></think>\nAll done.'
  const segments = parseAgentText(content, { final: true })
  assert.deepEqual(segments, [{ kind: 'text', text: 'All done.' }])
  assert.equal(removeThinking('half a thought</think>answer'), 'answer', 'a reply whose <think> was opened by the template')
  assert.equal(removeThinking('<think>still going'), '')
})

test('a <function=…> block without its <tool_call> wrapper is still a call', () => {
  const segments = parseAgentText('Checking.\n<function=read_file>\n<parameter=path>\nREADME.md\n</parameter>\n</function>\n</tool_call>', { final: true })
  assert.deepEqual(texts(segments), ['Checking.'])
  assert.deepEqual(tools(segments)[0]!.args, { path: 'README.md' })
  assert.equal(tools(segments)[0]!.complete, true)
  // …but only at the start of a line: prose that mentions the tag is prose.
  assert.equal(tools(parseAgentText('Use <function=x> to call.', { final: true })).length, 0)
})

test('a code fence that only wrapped a call is not shown', () => {
  const segments = parseAgentText(`Here:\n\`\`\`xml\n${EDIT_CALL}\n\`\`\``, { final: true })
  assert.deepEqual(texts(segments), ['Here:'])
  assert.equal(tools(segments).length, 1)
})

test('a value that mentions </parameter> is not cut short there', () => {
  const content = '<tool_call>\n<function=write_file>\n<parameter=path>\nnote.md\n</parameter>\n<parameter=content>\nClose it with </parameter> then go on.\n</parameter>\n</function>\n</tool_call>'
  const [call] = tools(parseAgentText(content, { final: true }))
  assert.equal(call!.args.content, 'Close it with </parameter> then go on.')
})

test('a closed call that cannot be read is answered with the format, not dropped', () => {
  const [call] = tools(parseAgentText('<tool_call>\nread_file path=a.rs\n</tool_call>', { final: true }))
  assert.equal(call!.complete, true)
  assert.match(call!.error!, /could not be read/)
  assert.match(call!.error!, /<function=read_file>/)
})

test('only the first call is kept, and a tool response the model imagined goes with the rest', () => {
  const imagined = `${EDIT_CALL}\n<tool_response>\nEdited.\n</tool_response>\n<tool_call>\n<function=run_command>\n<parameter=command>\nnpm test\n</parameter>\n</function>\n</tool_call>`
  assert.equal(keepFirstCall(imagined), EDIT_CALL)
  assert.equal(keepFirstCall(`Intro.\n${EDIT_CALL}`), `Intro.\n${EDIT_CALL}`, 'nothing after the call: unchanged')
  assert.equal(keepFirstCall('No call here.'), 'No call here.')
  const withThought = `<think>plan</think>\n${EDIT_CALL}\nextra words`
  assert.equal(keepFirstCall(withThought), `<think>plan</think>\n${EDIT_CALL}`)
})

test('the system prompt mirrors the template, and states the workspace, date and policy', () => {
  const prompt = buildSystemPrompt({ root: '/Users/me/proj', name: 'proj', branch: 'main', os: 'macos', date: '2026-10-27', policy: 'auto_edit' })
  assert.ok(prompt.startsWith('# Tools\n\nYou have access to the following functions:\n\n<tools>\n{"type": "function", "function": {"name": "list_files", '))
  assert.ok(prompt.includes('If you choose to call a function ONLY reply in the following format with NO suffix:'))
  assert.ok(prompt.includes('- You may provide optional reasoning for your function call in natural language BEFORE the function call, but NOT after'))
  for (const tool of TOOLS) assert.ok(prompt.includes(`"name": "${tool.name}"`), tool.name)
  assert.ok(prompt.includes('`proj` (/Users/me/proj), on git branch `main`'))
  assert.ok(prompt.includes('The OS is macOS. Today is 2026-10-27.'))
  assert.ok(prompt.includes('Approval policy: auto edit.'))
  assert.ok(prompt.includes('- Make one tool call per message, then wait for its result.'))
  assert.ok(/^[\x00-\x7F]*$/.test(prompt), 'ASCII only: the runtime switches its own headings to Japanese on any other character')
})

test('read only offers no tool that changes anything — not even by name', () => {
  const prompt = buildSystemPrompt({ root: '/p', date: '2026-10-27', policy: 'read_only' })
  for (const name of ['write_file', 'edit_file', 'run_command']) assert.ok(!prompt.includes(name), name)
  assert.deepEqual(toolsFor('read_only').map((t) => t.name), ['list_files', 'read_file', 'search'])
  assert.equal(toolsFor('ask').length, 6)
})

test('the user’s own system prompt is kept as standing instructions', () => {
  const prompt = buildSystemPrompt({ root: '/p', date: '2026-10-27', policy: 'ask', userInstructions: '  Answer in Japanese.  ' })
  assert.ok(prompt.endsWith("# The user's standing instructions\nAnswer in Japanese."))
  assert.ok(!buildSystemPrompt({ root: '/p', date: '2026-10-27', policy: 'ask', userInstructions: '  ' }).includes('standing instructions'))
})

test('tool results are one message of <tool_response> blocks, nothing outside them', () => {
  const message = toolResultsMessage(['first', 'second\nline'])
  assert.equal(message, '<tool_response>\nfirst\n</tool_response>\n<tool_response>\nsecond\nline\n</tool_response>')
  assert.ok(message.trim().startsWith('<tool_response>') && message.trim().endsWith('</tool_response>'))
})

test('a long output keeps its head and tail and says what was left out', () => {
  const text = Array.from({ length: 1000 }, (_, i) => `line ${i}`).join('\n')
  const cut = truncateMiddle(text, 2000, 'read on with start_line')
  assert.ok(cut.length < 2200)
  assert.ok(cut.startsWith('line 0\n'))
  assert.ok(cut.endsWith('line 999'))
  assert.match(cut, /\.\.\. \[\d+ lines omitted: read on with start_line\] \.\.\./)
  assert.equal(truncateMiddle('short', 100), 'short')
})

test('JSON repair escapes raw control characters and stray backslashes inside strings only', () => {
  assert.deepEqual(JSON.parse(repairJson('{"a": "x\ny\tz", "b": "\\d", "c": [1, 2,],}')), { a: 'x\ny\tz', b: '\\d', c: [1, 2] })
})

test('a call’s key ignores the order its arguments were written in', () => {
  assert.equal(callKey('read_file', { path: 'a', start_line: '1' }), callKey('read_file', { start_line: '1', path: 'a' }))
  assert.notEqual(callKey('read_file', { path: 'a' }), callKey('read_file', { path: 'b' }))
})
