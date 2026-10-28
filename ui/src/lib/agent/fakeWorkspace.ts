// An in-memory folder behind the workspace API, for the agent's tests.
//
// It answers the way `workspace.rs` does — the same errors for a missing file, the same line
// ranges and byte budget for `read`, the same edit rules (through the mirrored `applyEdit`) — so a
// test that passes here exercises the code paths the runtime will. Every call is recorded, which is
// how a test proves that an edit waiting for approval did not touch the disk.

import type { WorkspaceEdit, WorkspaceEntry, WorkspaceExec, WorkspaceFile, WorkspaceMatch, WorkspaceSearch, WorkspaceTree, WorkspaceWrite } from '../types.ts'
import { applyEdit } from './edit.ts'
import type { WorkspaceApi } from './tools.ts'

export class FakeApiError extends Error {
  status: number
  constructor(message: string, status = 400) {
    super(message)
    this.name = 'ApiError'
    this.status = status
  }
}

type ExecHandler = (command: string, signal?: AbortSignal) => WorkspaceExec | Promise<WorkspaceExec>

export type FakeWorkspace = WorkspaceApi & {
  files: Map<string, string>
  calls: { method: string; args: unknown[] }[]
  remove: (root: string, path: string) => Promise<{ removed: string }>
  onExec: (handler: ExecHandler) => void
}

function normalize(path: string): string {
  const parts: string[] = []
  for (const part of path.trim().split('/')) {
    if (part === '' || part === '.') continue
    if (part === '..') {
      if (!parts.length) throw new FakeApiError(`path escapes the workspace: ${path}`)
      parts.pop()
    } else parts.push(part)
  }
  return parts.join('/') || '.'
}

function lines(text: string): string[] {
  const out: string[] = []
  let start = 0
  while (start < text.length) {
    const newline = text.indexOf('\n', start)
    if (newline === -1) {
      out.push(text.slice(start))
      break
    }
    out.push(text.slice(start, newline + 1))
    start = newline + 1
  }
  return out
}

export function fakeWorkspace(initial: Record<string, string> = {}): FakeWorkspace {
  const files = new Map(Object.entries(initial))
  const calls: { method: string; args: unknown[] }[] = []
  let execHandler: ExecHandler = (command) => ({ command, exit_code: 0, stdout: '', stderr: '', timed_out: false, duration_ms: 5, truncated: false })

  const dirs = () => {
    const set = new Set<string>()
    for (const path of files.keys()) {
      const parts = path.split('/')
      for (let i = 1; i < parts.length; i++) set.add(parts.slice(0, i).join('/'))
    }
    return set
  }

  return {
    files,
    calls,
    onExec(handler) {
      execHandler = handler
    },

    async tree(_root, path = '.', depth = 2, limit = 400): Promise<WorkspaceTree> {
      calls.push({ method: 'tree', args: [path, depth, limit] })
      const base = normalize(path)
      if (base !== '.' && !dirs().has(base)) {
        if (files.has(base)) throw new FakeApiError(`${base} is a file, not a directory — read it instead`)
        throw new FakeApiError(`${base} does not exist`)
      }
      const prefix = base === '.' ? '' : `${base}/`
      const entries: WorkspaceEntry[] = []
      for (const dir of dirs()) {
        if (dir.startsWith(prefix) && dir !== base && dir.slice(prefix.length).split('/').length <= depth) entries.push({ path: dir, is_dir: true, size: null })
      }
      for (const [file, content] of files) {
        if (file.startsWith(prefix) && file.slice(prefix.length).split('/').length <= depth) entries.push({ path: file, is_dir: false, size: content.length })
      }
      // Explorer order: per component, directories first, then by name.
      const key = (e: WorkspaceEntry) => e.path.split('/').map((part, i, all) => `${i === all.length - 1 && !e.is_dir ? '1' : '0'}${part.toLowerCase()}`)
      entries.sort((a, b) => {
        const ka = key(a)
        const kb = key(b)
        for (let i = 0; i < Math.min(ka.length, kb.length); i++) if (ka[i] !== kb[i]) return ka[i]! < kb[i]! ? -1 : 1
        return ka.length - kb.length
      })
      return { root: '/fake', path: base, entries: entries.slice(0, limit), truncated: entries.length > limit }
    },

    async read(_root, path, range = {}): Promise<WorkspaceFile> {
      calls.push({ method: 'read', args: [path, range] })
      const rel = normalize(path)
      const content = files.get(rel)
      if (content === undefined) {
        if (dirs().has(rel)) throw new FakeApiError(`${rel} is a directory — list it with tree`)
        throw new FakeApiError(`${rel} does not exist`)
      }
      if (content.includes('\0')) return { path: rel, content: '', start_line: 0, end_line: 0, total_lines: 0, size: content.length, truncated: false, binary: true }
      const all = lines(content)
      const start = Math.max(1, range.start_line ?? 1)
      if (range.end_line !== undefined && range.end_line < start) throw new FakeApiError(`end_line ${range.end_line} is before start_line ${start}`)
      if (all.length > 0 && start > all.length) throw new FakeApiError(`start_line ${start} is past the end of ${rel}, which has ${all.length} lines`)
      const maxBytes = range.max_bytes ?? 256 * 1024
      let out = ''
      let last = 0
      let truncated = false
      for (let line = start; line <= Math.min(all.length, range.end_line ?? all.length); line++) {
        const text = all[line - 1]!
        if (out.length + text.length > maxBytes) {
          if (!out) {
            out = text.slice(0, maxBytes)
            last = line
          }
          truncated = true
          break
        }
        out += text
        last = line
      }
      return { path: rel, content: out, start_line: last === 0 ? 0 : start, end_line: last, total_lines: all.length, size: content.length, truncated, binary: false }
    },

    async search(_root, pattern, options = {}): Promise<WorkspaceSearch> {
      calls.push({ method: 'search', args: [pattern, options] })
      if (!pattern) throw new FakeApiError('pattern is empty')
      let regex: RegExp
      try {
        regex = new RegExp(options.regex ? pattern : pattern.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), options.case_insensitive ? 'i' : '')
      } catch (error) {
        throw new FakeApiError(`invalid regex (set regex: false to search for the text literally): ${(error as Error).message}`)
      }
      const base = normalize(options.path ?? '.')
      const glob = options.glob ? new RegExp(`^${options.glob.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*')}$`) : null
      const matches: WorkspaceMatch[] = []
      let truncated = false
      let scanned = 0
      const max = options.max_results ?? 100
      for (const [file, content] of [...files].sort(([a], [b]) => (a < b ? -1 : 1))) {
        if (base !== '.' && file !== base && !file.startsWith(`${base}/`)) continue
        if (glob && !glob.test(file.split('/').pop()!)) continue
        scanned++
        content.split('\n').forEach((line, index) => {
          if (truncated || !regex.test(line.replace(/\r$/, ''))) return
          if (matches.length === max) {
            truncated = true
            return
          }
          matches.push({ path: file, line: index + 1, text: line.trim() })
        })
      }
      return { matches, truncated, files_scanned: scanned }
    },

    async write(_root, path, content): Promise<WorkspaceWrite> {
      calls.push({ method: 'write', args: [path, content] })
      const rel = normalize(path)
      if (rel === '.') throw new FakeApiError('path names the workspace root itself; give a file path inside it')
      const previous = files.get(rel) ?? null
      files.set(rel, content)
      return { path: rel, created: previous === null, previous, content, bytes: content.length }
    },

    async edit(_root, path, oldString, newString, replaceAll = false): Promise<WorkspaceEdit> {
      calls.push({ method: 'edit', args: [path, oldString, newString, replaceAll] })
      const rel = normalize(path)
      const previous = files.get(rel)
      if (previous === undefined) throw new FakeApiError(`${rel} does not exist — use write to create it`)
      const applied = applyEdit(previous, oldString, newString, replaceAll, rel)
      if (!applied.ok) throw new FakeApiError(applied.error.replace('use write_file;', 'use write;'))
      files.set(rel, applied.content)
      return { path: rel, previous, content: applied.content, replacements: applied.replacements }
    },

    async remove(_root, path) {
      calls.push({ method: 'remove', args: [path] })
      const rel = normalize(path)
      if (!files.delete(rel)) throw new FakeApiError(`${rel} does not exist`)
      return { removed: rel }
    },

    async exec(_root, command, timeoutSecs, signal): Promise<WorkspaceExec> {
      calls.push({ method: 'exec', args: [command, timeoutSecs] })
      if (signal?.aborted) throw Object.assign(new Error('The operation was aborted.'), { name: 'AbortError' })
      const result = execHandler(command, signal)
      if (!(result instanceof Promise)) return result
      return await new Promise<WorkspaceExec>((resolve, reject) => {
        signal?.addEventListener('abort', () => reject(Object.assign(new Error('The operation was aborted.'), { name: 'AbortError' })))
        result.then(resolve, reject)
      })
    },
  }
}
