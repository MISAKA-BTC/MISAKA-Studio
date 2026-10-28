// What an `edit_file` call would do to a file, worked out before anything is written.
//
// The runtime applies edits (`workspace.rs::apply_edit`); this is that function, mirrored, for two
// reasons. An edit that waits for approval must show its diff BEFORE it is applied — approving a
// change nobody can see is not approval. And an edit that cannot apply (old_string not there, or
// there three times) is answered at once, instead of being put in front of the person to approve
// and failing afterwards.
//
// So it must agree with the runtime exactly: the same three attempts in the same order (as sent,
// then in the file's line endings, then ignoring line-end whitespace), the same refusals, the same
// words. Whatever the runtime then returns is what the transcript and the Changes panel record;
// this only decides what to ask and what to show.
//
// One forgiveness is added on top, and applied by sending the runtime the corrected strings: a
// small model often copies `read_file`'s line-number prefixes ("  12\t") into old_string. When the
// text as sent is not in the file but every line of it carries such a prefix, the prefixes are
// taken off both strings and the edit is tried again.

export type EditPlan =
  | {
      ok: true
      /** The file as it would be after the edit. */
      content: string
      replacements: number
      /** What to send the runtime — the call's own strings, or with line numbers taken off. */
      oldString: string
      newString: string
      strippedLineNumbers: boolean
    }
  | { ok: false; error: string; reason: 'invalid' | 'not_found' | 'not_unique' }

type Applied = { ok: true; content: string; replacements: number } | { ok: false; error: string; reason: 'invalid' | 'not_found' | 'not_unique' }

type LineEndings = 'none' | 'lf' | 'crlf' | 'mixed'

function occurrences(text: string, needle: string): number {
  return text.split(needle).length - 1
}

function lineEndings(text: string): LineEndings {
  const newlines = occurrences(text, '\n')
  const crlf = occurrences(text, '\r\n')
  if (newlines === 0) return 'none'
  if (newlines === crlf) return 'crlf'
  if (crlf === 0) return 'lf'
  return 'mixed'
}

const toLf = (text: string) => text.split('\r\n').join('\n')
const toCrlf = (text: string) => toLf(text).split('\n').join('\r\n')

/** Occurrences counting overlaps — `aa` is in `aaa` twice, and "the one place" is not defined there. */
function countOverlapping(haystack: string, needle: string): number {
  let count = 0
  let from = 0
  for (;;) {
    const at = haystack.indexOf(needle, from)
    if (at === -1) break
    count++
    from = at + 1
    if (from >= haystack.length) break
  }
  return count
}

function notUnique(count: number, rel: string): Applied {
  return { ok: false, reason: 'not_unique', error: `old_string matches ${count} places in ${rel}; include more surrounding lines to make it unique, or set replace_all` }
}

function replaceExact(content: string, needle: string, replacement: string, replaceAll: boolean, rel: string): Applied | null {
  const count = countOverlapping(content, needle)
  if (count === 0) return null
  if (count === 1) {
    const at = content.indexOf(needle)
    return { ok: true, content: content.slice(0, at) + replacement + content.slice(at + needle.length), replacements: 1 }
  }
  if (replaceAll) {
    // Left to right, without overlaps — Rust's `str::replace`. (Not `String.replace`, which would
    // read `$&` in the replacement as a pattern.)
    const parts = content.split(needle)
    return { ok: true, content: parts.join(replacement), replacements: parts.length - 1 }
  }
  return notUnique(count, rel)
}

const isLineEndSpace = (code: number) => code === 32 || code === 9 || code === 13

/** The file with line-end whitespace removed and `\n` endings, remembering where each kept
 *  character came from. A file without a final newline is given one, so a needle ending in `\n`
 *  still matches its last line. */
function looseText(content: string): { text: string; origin: number[]; originalLength: number } {
  const parts: string[] = []
  const origin: number[] = []
  const length = content.length
  let lineStart = 0
  for (;;) {
    const newline = content.indexOf('\n', lineStart)
    const lineEnd = newline === -1 ? length : newline
    let keptEnd = lineEnd
    while (keptEnd > lineStart && isLineEndSpace(content.charCodeAt(keptEnd - 1))) keptEnd--
    parts.push(content.slice(lineStart, keptEnd), '\n')
    for (let i = lineStart; i < keptEnd; i++) origin.push(i)
    if (newline === -1) {
      origin.push(length)
      break
    }
    origin.push(newline)
    lineStart = newline + 1
    if (lineStart === length) break
  }
  return { text: parts.join(''), origin, originalLength: length }
}

/** Non-overlapping matches as ranges of the ORIGINAL text; `atLineEnd` keeps only those that end a line. */
function findLoose(loose: ReturnType<typeof looseText>, needle: string, atLineEnd: boolean): [number, number][] {
  const spans: [number, number][] = []
  if (!needle || !loose.text) return spans
  let from = 0
  while (from < loose.text.length) {
    const start = loose.text.indexOf(needle, from)
    if (start === -1) break
    const end = start + needle.length
    if (!atLineEnd || loose.text[end] === '\n') {
      const last = loose.origin[end - 1]!
      const originalEnd = last >= loose.originalLength ? loose.originalLength : last + 1
      spans.push([Math.min(loose.origin[start]!, loose.originalLength), originalEnd])
      from = end
    } else {
      from = start + 1
    }
  }
  return spans
}

function looseNeedle(old: string, trimLast: boolean): string {
  const parts = toLf(old).split('\n')
  const last = parts.length - 1
  return parts.map((part, i) => (i < last || trimLast ? part.replace(/[ \t\r]+$/, '') : part)).join('\n')
}

function replaceLoose(content: string, old: string, replacement: string, replaceAll: boolean, rel: string): Applied | null {
  const loose = looseText(content)
  const keepLast = looseNeedle(old, false)
  let spans = keepLast.trim() === '' ? [] : findLoose(loose, keepLast, false)
  if (spans.length === 0) {
    const trimLast = looseNeedle(old, true)
    if (trimLast !== keepLast && trimLast.trim() !== '') spans = findLoose(loose, trimLast, true)
  }
  if (spans.length === 0) return null
  if (spans.length > 1 && !replaceAll) return notUnique(spans.length, rel)
  let out = ''
  let copied = 0
  for (const [start, end] of spans) {
    out += content.slice(copied, start) + replacement
    copied = end
  }
  out += content.slice(copied)
  return { ok: true, content: out, replacements: spans.length }
}

/** Rust's `str::lines`: no trailing empty line, and a `\r` before each `\n` dropped. */
function rustLines(text: string): string[] {
  const parts = text.split('\n')
  if (parts[parts.length - 1] === '') parts.pop()
  return parts.map((line) => (line.endsWith('\r') ? line.slice(0, -1) : line))
}

/** The line where `old` would match if indentation did not count — for the message only. */
function indentationOnlyMatch(content: string, old: string): number | null {
  const all = rustLines(old).map((line) => line.trim())
  const first = all.findIndex((line) => line !== '')
  if (first === -1) return null
  let last = all.length - 1
  while (last > first && all[last] === '') last--
  const needle = all.slice(first, last + 1)
  const lines = rustLines(content).map((line) => line.trim())
  if (needle.length === 1) {
    const index = lines.findIndex((line) => line.includes(needle[0]!))
    return index === -1 ? null : index + 1
  }
  const finalIndex = needle.length - 1
  const maxStart = Math.max(0, lines.length - needle.length)
  for (let start = 0; start <= maxStart; start++) {
    const hit = needle.every((wanted, k) => {
      const line = lines[start + k]
      if (line === undefined) return false
      if (k === 0) return line.endsWith(wanted)
      if (k === finalIndex) return line.startsWith(wanted)
      return line === wanted
    })
    if (hit) return start + 1
  }
  return null
}

/** `workspace.rs::apply_edit`, in TypeScript. `rel` names the file in the messages. */
export function applyEdit(content: string, old: string, replacementText: string, replaceAll: boolean, rel: string): Applied {
  if (old === '') {
    return {
      ok: false,
      reason: 'invalid',
      error: `old_string is empty. To create ${rel} use write_file; to insert text, put the line it goes after in old_string and that line plus the new text in new_string`,
    }
  }
  if (old === replacementText) {
    return { ok: false, reason: 'invalid', error: `old_string and new_string are identical — there is nothing to change in ${rel}` }
  }
  const style = lineEndings(content)
  const replacement =
    style === 'crlf' && replacementText.includes('\n') ? toCrlf(replacementText) : style === 'lf' && replacementText.includes('\r\n') ? toLf(replacementText) : replacementText

  const exact = replaceExact(content, old, replacement, replaceAll, rel)
  if (exact) return exact
  const converted =
    (style === 'crlf' || style === 'mixed') && old.includes('\n') ? toCrlf(old) : style === 'lf' && old.includes('\r\n') ? toLf(old) : null
  if (converted !== null && converted !== old) {
    const again = replaceExact(content, converted, replacement, replaceAll, rel)
    if (again) return again
  }
  const loose = replaceLoose(content, old, replacement, replaceAll, rel)
  if (loose) return loose

  let error = `old_string was not found in ${rel}. Read the file again and copy the exact text, including whitespace.`
  const line = indentationOnlyMatch(content, old)
  if (line !== null) {
    error += ` Text that matches it except for indentation starts at line ${line} — copy that indentation exactly (tabs and spaces are different characters).`
  }
  return { ok: false, reason: 'not_found', error }
}

/** `read_file`'s prefix on a line: the number, right-aligned, then a tab. */
const NUMBERED = /^ *\d+\t/

/** `text` without `read_file` line numbers, when every non-empty line carries one; else null. */
export function stripLineNumbers(text: string): string | null {
  const lines = text.split('\n')
  const marked = lines.filter((line) => line.trim() !== '')
  if (marked.length === 0 || !marked.every((line) => NUMBERED.test(line))) return null
  return lines.map((line) => line.replace(NUMBERED, '')).join('\n')
}

/** What `edit_file` would do to `content`: the runtime's rules, then the line-number forgiveness. */
export function planEdit(content: string, oldString: string, newString: string, replaceAll: boolean, rel: string): EditPlan {
  const first = applyEdit(content, oldString, newString, replaceAll, rel)
  if (first.ok) return { ...first, oldString, newString, strippedLineNumbers: false }
  if (first.reason === 'not_found') {
    const old = stripLineNumbers(oldString)
    if (old !== null) {
      const replacement = stripLineNumbers(newString) ?? newString
      const second = applyEdit(content, old, replacement, replaceAll, rel)
      if (second.ok) return { ...second, oldString: old, newString: replacement, strippedLineNumbers: true }
    }
  }
  return first
}
