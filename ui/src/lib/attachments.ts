// Files attached in the composer.
//
// Text only, by design: the model is sent an attachment as a fenced block after the message (see
// `history.attachmentBlock`), so a PNG would arrive as a page of mojibake that costs the window and
// tells the model nothing. A file is refused when its name says it is binary, or when its first
// bytes hold a NUL — the test git uses — and it is cut at 200 KB, already more than most local
// windows hold. A cut file says so: on its chip, and in the block the model reads.
//
// What is attached is never mined. Background mining queues only what the person typed; an
// attachment's content stays on this machine (see `useStudio.send`).
//
// Plain TypeScript (type-only imports), so the unit tests run it directly under Node.

import type { Attachment } from './types'

/** The most of one file that is attached. */
export const MAX_ATTACHMENT_BYTES = 200 * 1024
/** How much of a file is read to decide whether it is text. */
const SNIFF_BYTES = 8000

const IMAGE_EXTENSIONS = new Set(['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'tif', 'tiff', 'heic', 'heif', 'avif', 'psd'])

const BINARY_EXTENSIONS = new Set([
  ...IMAGE_EXTENSIONS,
  // documents and archives
  'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'odt', 'ods', 'odp', 'key', 'numbers', 'pages', 'epub',
  'zip', 'gz', 'tgz', 'bz2', 'xz', 'zst', '7z', 'rar', 'tar', 'jar', 'war', 'apk', 'ipa', 'dmg', 'iso', 'img', 'deb', 'rpm',
  // compiled code and libraries
  'exe', 'dll', 'so', 'dylib', 'a', 'o', 'obj', 'lib', 'bin', 'class', 'pyc', 'pyo', 'wasm', 'node',
  // fonts and media
  'woff', 'woff2', 'ttf', 'otf', 'eot', 'mp3', 'mp4', 'm4a', 'm4v', 'wav', 'flac', 'ogg', 'opus', 'aac', 'mov', 'avi', 'mkv', 'webm',
  // model weights and data stores
  'gguf', 'safetensors', 'pt', 'pth', 'ckpt', 'onnx', 'npy', 'npz', 'pkl', 'pickle', 'h5', 'palwart', 'sqlite', 'sqlite3', 'db', 'parquet',
])

/** Fence languages by extension — highlight.js names, or the common alias it also accepts. */
const LANGUAGE_BY_EXTENSION: Record<string, string> = {
  ts: 'typescript', mts: 'typescript', cts: 'typescript', tsx: 'tsx',
  js: 'javascript', mjs: 'javascript', cjs: 'javascript', jsx: 'jsx',
  py: 'python', pyi: 'python', rb: 'ruby', rs: 'rust', go: 'go', java: 'java', kt: 'kotlin', kts: 'kotlin',
  swift: 'swift', scala: 'scala', c: 'c', h: 'c', cc: 'cpp', cpp: 'cpp', cxx: 'cpp', hpp: 'cpp', hh: 'cpp', hxx: 'cpp',
  cs: 'csharp', m: 'objectivec', mm: 'objectivec', php: 'php', pl: 'perl', lua: 'lua', r: 'r', dart: 'dart',
  ex: 'elixir', exs: 'elixir', erl: 'erlang', hs: 'haskell', clj: 'clojure', ml: 'ocaml', fs: 'fsharp', zig: 'zig',
  sh: 'bash', bash: 'bash', zsh: 'bash', fish: 'fish', ps1: 'powershell', bat: 'dos', cmd: 'dos',
  html: 'html', htm: 'html', xml: 'xml', svg: 'xml', plist: 'xml', vue: 'xml', svelte: 'xml',
  css: 'css', scss: 'scss', sass: 'scss', less: 'less',
  json: 'json', jsonc: 'json', json5: 'json', yaml: 'yaml', yml: 'yaml', toml: 'toml', ini: 'ini', cfg: 'ini', conf: 'ini',
  properties: 'properties', env: 'bash', md: 'markdown', markdown: 'markdown', mdx: 'markdown', tex: 'latex',
  sql: 'sql', graphql: 'graphql', gql: 'graphql', proto: 'protobuf', tf: 'hcl', hcl: 'hcl', nix: 'nix',
  diff: 'diff', patch: 'diff', cmake: 'cmake', gradle: 'groovy', groovy: 'groovy', vim: 'vim', mk: 'makefile',
  csv: 'csv', tsv: 'csv',
}

/** Files known by their whole name rather than an extension. */
const LANGUAGE_BY_NAME: Record<string, string> = {
  dockerfile: 'dockerfile', containerfile: 'dockerfile', makefile: 'makefile', gnumakefile: 'makefile',
  cmakelists: 'cmake', 'cmakelists.txt': 'cmake', gemfile: 'ruby', rakefile: 'ruby', podfile: 'ruby', vagrantfile: 'ruby',
  '.bashrc': 'bash', '.zshrc': 'bash', '.profile': 'bash', '.env': 'bash', '.gitignore': 'bash', 'cargo.lock': 'toml',
}

/** The last path segment: `src/lib/a.ts` → `a.ts`. */
export function baseName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path
}

/** The lower-case extension without its dot; `''` for none (`Makefile`, `.env`). */
export function extensionOf(name: string): string {
  const base = baseName(name)
  const dot = base.lastIndexOf('.')
  return dot > 0 ? base.slice(dot + 1).toLowerCase() : ''
}

/** The fence language a file's name suggests, or null for plain text. */
export function languageFor(name: string): string | null {
  const base = baseName(name).toLowerCase()
  return LANGUAGE_BY_NAME[base] ?? LANGUAGE_BY_EXTENSION[extensionOf(base)] ?? null
}

export function isBinaryName(name: string): boolean {
  return BINARY_EXTENSIONS.has(extensionOf(name))
}

/** Whether these bytes — a file's start — are binary: a NUL anywhere in them. */
export function looksBinary(bytes: Uint8Array): boolean {
  const end = Math.min(bytes.length, SNIFF_BYTES)
  for (let i = 0; i < end; i++) if (bytes[i] === 0) return true
  return false
}

/** Why a file cannot be attached, in the words the toast uses. */
export function binaryReason(name: string): string {
  return IMAGE_EXTENSIONS.has(extensionOf(name))
    ? `${baseName(name)} is an image — only text files can be attached.`
    : `${baseName(name)} is a binary file — only text files can be attached.`
}

export function attachmentId(): string {
  return `att-${Math.random().toString(36).slice(2, 10)}${Date.now().toString(36)}`
}

/** What a file must offer to be read: a browser `File` (or `Blob` with a name). */
export type ReadableFile = {
  name: string
  size: number
  slice: (start?: number, end?: number) => { arrayBuffer: () => Promise<ArrayBuffer> }
}

export type ReadResult = { ok: true; attachment: Attachment } | { ok: false; reason: string }

/**
 * Decode at most `maxBytes` of UTF-8.
 *
 * Streaming mode is the point: a cut can land inside a multi-byte character, and the decoder then
 * holds those bytes back instead of ending the text in U+FFFD.
 */
export function decodeUtf8(bytes: Uint8Array, maxBytes = MAX_ATTACHMENT_BYTES): { text: string; truncated: boolean } {
  const truncated = bytes.length > maxBytes
  const decoder = new TextDecoder('utf-8')
  const text = truncated ? decoder.decode(bytes.subarray(0, maxBytes), { stream: true }) : decoder.decode(bytes)
  // A byte-order mark is an encoding detail, not text the model should read.
  return { text: text.replace(/^﻿/, ''), truncated }
}

/** Read a dropped, pasted or chosen file into an attachment — or say why it cannot be one. */
export async function readAttachment(file: ReadableFile, options: { path?: string | null } = {}): Promise<ReadResult> {
  if (isBinaryName(file.name)) return { ok: false, reason: binaryReason(file.name) }
  let bytes: Uint8Array
  try {
    // Only what can be attached is read: a 4 GB log is not loaded to keep its first 200 KB.
    bytes = new Uint8Array(await file.slice(0, MAX_ATTACHMENT_BYTES + 1).arrayBuffer())
  } catch (error) {
    return { ok: false, reason: `${baseName(file.name)} could not be read: ${(error as Error).message}` }
  }
  if (looksBinary(bytes)) return { ok: false, reason: binaryReason(file.name) }
  const { text, truncated } = decodeUtf8(bytes)
  return {
    ok: true,
    attachment: {
      id: attachmentId(),
      name: baseName(file.name),
      size: file.size,
      content: text,
      language: languageFor(file.name),
      ...(truncated || file.size > MAX_ATTACHMENT_BYTES ? { truncated: true } : {}),
      ...(options.path ? { path: options.path } : {}),
    },
  }
}

/** An attachment from text already in hand — a workspace file the runtime read. */
export function attachmentFromText(
  path: string,
  content: string,
  options: { size?: number; truncated?: boolean; workspacePath?: string | null } = {},
): Attachment {
  const encoded = new TextEncoder().encode(content)
  const { text, truncated } = decodeUtf8(encoded)
  return {
    id: attachmentId(),
    name: baseName(path),
    size: options.size ?? encoded.length,
    content: text,
    language: languageFor(path),
    ...(truncated || options.truncated ? { truncated: true } : {}),
    ...(options.workspacePath ? { path: options.workspacePath } : {}),
  }
}

/** Whether `candidate` is already in `list`: the same workspace path, or the same name and size. */
export function isDuplicate(list: Attachment[], candidate: Attachment): boolean {
  return list.some((a) =>
    candidate.path ? a.path === candidate.path : !a.path && a.name === candidate.name && a.size === candidate.size,
  )
}
