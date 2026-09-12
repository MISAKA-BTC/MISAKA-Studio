// What the effective view says, in the words a panel prints — ADR-0096 Decision 11.
//
// The runtime returns, per subsystem, `configured` and `effective` as untyped JSON, a per-field
// `source` map, and `differs`: its own verdict that the running object was built from values the
// settings no longer hold. It does not say WHICH fields — the comparison happens on the Rust side,
// over a fingerprint struct, and comes back as one boolean. Naming the fields is done here, by
// pairing the two JSON shapes the way the runtime's `effective.rs` pairs them, and only after the
// runtime has said they differ. The boolean is the authority; this is its caption.

import type { EffectiveSubsystem, EffectiveSubsystemName, Json } from './types'

export type Leaf = { path: string; value: Json }

export function isRecord(value: Json | null | undefined): value is { [key: string]: Json } {
  return value !== null && value !== undefined && typeof value === 'object' && !Array.isArray(value)
}

/** The dotted leaves of a JSON value. Arrays stay leaves: an argument list is one value, not twelve. */
export function flatten(value: Json | null | undefined, prefix = ''): Leaf[] {
  if (value === undefined) return []
  if (!isRecord(value)) return [{ path: prefix, value }]
  const out: Leaf[] = []
  for (const [key, inner] of Object.entries(value)) out.push(...flatten(inner, prefix ? `${prefix}.${key}` : key))
  return out
}

/** A dotted path read out of a JSON value; undefined when any step is missing. */
export function pick(value: Json | null | undefined, path: string): Json | undefined {
  let at: Json | undefined = value ?? undefined
  for (const step of path.split('.')) {
    if (!isRecord(at)) return undefined
    at = at[step]
  }
  return at
}

export function asString(value: Json | undefined): string | null {
  return typeof value === 'string' ? value : null
}

export function asStringList(value: Json | undefined): string[] | null {
  return Array.isArray(value) && value.every((v) => typeof v === 'string') ? (value as string[]) : null
}

/** A leaf as a panel prints it. `null` is the runtime saying "none", and is printed as that. */
export function show(value: Json | undefined): string {
  if (value === undefined) return '—'
  if (value === null) return 'none'
  if (typeof value === 'string') return value === '' ? '""' : value
  if (typeof value === 'number' || typeof value === 'boolean') return String(value)
  return JSON.stringify(value)
}

/**
 * The source line for an effective leaf. The runtime keys `source` by the leaf's own name
 * (`program`, `url`, `token`) while the effective object nests it (`fingerprint.program`) or
 * spells the token as its fingerprint (`token_sha256_prefix`), so the lookup tries the full
 * path, then the leaf, then the leaf without the `_sha256_prefix` suffix.
 */
export function sourceOf(source: Record<string, string>, path: string): string | undefined {
  const leaf = path.split('.').pop() ?? path
  const suffix = '_sha256_prefix'
  return source[path] ?? source[leaf] ?? (leaf.endsWith(suffix) ? source[leaf.slice(0, -suffix.length)] : undefined)
}

/** One field the runtime holds differently from the file. `path` is where it sits in `effective`. */
export type Difference = { field: string; path: string; configured: Json | undefined; effective: Json | undefined }

/**
 * The pairs `effective.rs` compares, for every subsystem but the backend (whose two fingerprints
 * are compared key by key below). Each entry is the field's name, its path in `configured`, and
 * its path in `effective`. A pair whose effective side is null is skipped: the running object
 * does not hold that field (a pool row with no gateway engine running has no token), and a
 * "differs" the runtime raised on another field must not be pinned on this one.
 */
const PAIRS: Record<Exclude<EffectiveSubsystemName, 'backend'>, [field: string, configured: string, effective: string][]> = {
  node: [
    ['binary', 'binary.path', 'binary'],
    ['args', 'args', 'args'],
  ],
  records: [
    ['path', 'path', 'path'],
    ['enabled', 'enabled', 'enabled'],
    ['max_records', 'max_records', 'max_records'],
  ],
  catalog: [
    ['endpoint', 'endpoint', 'endpoint'],
    ['token', 'token_sha256_prefix', 'token_sha256_prefix'],
  ],
  gateway: [
    ['url', 'url', 'url'],
    ['token', 'token_sha256_prefix', 'token_sha256_prefix'],
  ],
  pool: [
    ['slot_id', 'pool_slot_id', 'slot_id'],
    ['token', 'pool_slot_token_sha256_prefix', 'token_sha256_prefix'],
  ],
}

function same(a: Json | undefined, b: Json | undefined): boolean {
  return JSON.stringify(a ?? null) === JSON.stringify(b ?? null)
}

/**
 * Which fields differ, by name — empty unless the runtime said `differs`, and possibly empty
 * even then, when the difference is in a field this pairing does not cover (the caller says so
 * rather than inventing one).
 */
export function differences(name: EffectiveSubsystemName, subsystem: EffectiveSubsystem): Difference[] {
  if (!subsystem.differs || subsystem.effective === null) return []
  if (name === 'backend') {
    // Both fingerprints come from the same constructor, so a key present on one side and absent
    // on the other is a real difference (a URL the settings now carry that the engine was built
    // without), and the comparison runs over the union.
    const configured = new Map(flatten(pick(subsystem.configured, 'fingerprint'), 'fingerprint').map((l) => [l.path, l.value]))
    const effective = new Map(flatten(pick(subsystem.effective, 'fingerprint'), 'fingerprint').map((l) => [l.path, l.value]))
    const out: Difference[] = []
    for (const path of new Set([...configured.keys(), ...effective.keys()])) {
      if (same(configured.get(path), effective.get(path))) continue
      out.push({ field: path.replace(/^fingerprint\./, ''), path, configured: configured.get(path), effective: effective.get(path) })
    }
    return out
  }
  const out: Difference[] = []
  for (const [field, configuredPath, effectivePath] of PAIRS[name]) {
    const effective = pick(subsystem.effective, effectivePath)
    if (effective === undefined || effective === null) continue
    const configured = pick(subsystem.configured, configuredPath)
    if (!same(configured, effective)) out.push({ field, path: effectivePath, configured, effective })
  }
  return out
}

/** Two argument lists, as a sentence about what the settings would add and drop. */
export function describeArgsDifference(configured: string[], effective: string[]): string {
  const added = configured.filter((a) => !effective.includes(a))
  const dropped = effective.filter((a) => !configured.includes(a))
  if (added.length === 0 && dropped.length === 0) return 'the same arguments in a different order'
  const parts: string[] = []
  if (added.length > 0) parts.push(`the settings would add ${added.join(' ')}`)
  if (dropped.length > 0) parts.push(`${added.length > 0 ? 'and drop' : 'the settings would drop'} ${dropped.join(' ')}`)
  return parts.join(' ')
}

/** One word for a POSIX shell: quoted only when it has to be, so the common case reads as typed. */
function shellWord(word: string): string {
  return /^[A-Za-z0-9_@%+=:,./-]+$/.test(word) ? word : `'${word.replace(/'/g, `'\\''`)}'`
}

/** The command an operator can paste: the binary and its arguments, quoted for a POSIX shell. */
export function shellCommand(binary: string, args: string[]): string {
  return [binary, ...args].map(shellWord).join(' ')
}

/**
 * What applies the file's values, per subsystem — read off `apply_settings`, which rebuilds the
 * engine whenever the fingerprint the new settings would build differs from the running one
 * (so saving again is enough), rebuilds the catalog and the record store only when their
 * settings CHANGE (so saving the same file again does nothing there), and never touches a
 * running node (its arguments are read once, at start).
 */
export function remedy(name: EffectiveSubsystemName): string {
  switch (name) {
    case 'backend':
    case 'gateway':
    case 'pool':
      return 'save the settings again to rebuild the engine from them'
    case 'node':
      return 'stop and start the node from the Network tab to apply them'
    case 'records':
    case 'catalog':
      return 'change and save this section, or restart the runtime, to apply them'
  }
}

export const SUBSYSTEM_LABELS: Record<EffectiveSubsystemName, string> = {
  backend: 'the chat engine',
  node: 'the node',
  records: 'the record store',
  catalog: 'the catalog',
  pool: 'the pool slot',
  gateway: 'the gateway engine',
}
