// The running value beside the field — ADR-0096 Decision 11.
//
// Every line here is read from `/api/v1/settings/effective`, never derived from the settings the
// window holds: the point is to show what the runtime is DOING, in the runtime's own words, next
// to what the file says it should do. A line names the value and where it came from ("settings
// file (server.port)", "--port", "env MISAKA_STUDIO_PORT", "discovery (beside the executable)").
// A subsystem whose running object disagrees with the file gets a marker on its section header
// AND a sentence naming the fields, because "different" without "which" is a marker nobody can
// act on — and on 2026-09-05 every panel named the new pool slot while the chat mined on the old.

import type { ReactNode } from 'react'
import {
  SUBSYSTEM_LABELS,
  asStringList,
  describeArgsDifference,
  differences,
  flatten,
  isRecord,
  remedy,
  shellCommand,
  show,
  sourceOf,
  type Difference,
} from '../lib/effective'
import { relativeTime } from '../lib/format'
import type { EffectiveSubsystem, EffectiveSubsystemName } from '../lib/types'
import { CopyButton, Icon } from './common'

/** One running value with its provenance. Amber when it is one of the fields that differ. */
export function EffectiveLine({
  label,
  value,
  source,
  differs = false,
}: {
  label: string
  value: ReactNode
  source?: string | undefined
  differs?: boolean
}) {
  return (
    <p className={`flex flex-wrap items-baseline gap-x-1.5 text-[0.7rem] ${differs ? 'text-amber-800 dark:text-amber-300' : 'text-ink-500 dark:text-ink-400'}`}>
      <span className="shrink-0">{label}</span>
      <span className="mono min-w-0 break-all">{value}</span>
      {source && <span className="min-w-0">— {source}</span>}
      {differs && <span className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">differs</span>}
    </p>
  )
}

/** The header marker: which of a section's subsystems run with values the file no longer holds. */
export function DiffMarker({ subsystems }: { subsystems: [EffectiveSubsystemName, EffectiveSubsystem][] }) {
  const differing = subsystems.filter(([, subsystem]) => subsystem.differs)
  if (differing.length === 0) return null
  return (
    <span
      className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300"
      title="The running object was built from values the settings no longer hold. The section says which fields, and what applies the file's values."
    >
      <Icon name="warning" className="mr-1 size-3" />
      {differing.map(([name]) => SUBSYSTEM_LABELS[name]).join(', ')} running with different values
    </span>
  )
}

/** The settings side of a difference, as a phrase. An argument list is compared as a set — what
 *  the file would add and drop — because two forty-flag lines printed whole are not readable. */
function describeConfigured(difference: Difference): string {
  if (difference.field === 'args') {
    const running = asStringList(difference.effective)
    const configured = asStringList(difference.configured)
    if (running && configured) return describeArgsDifference(configured, running)
    if (isRecord(difference.configured) && typeof difference.configured.error === 'string') {
      return `the settings no longer build a command line: ${difference.configured.error}`
    }
  }
  return `running ${show(difference.effective)} · settings ${show(difference.configured)}`
}

/**
 * The sentence under a section header: which fields differ, both values, and what applies the
 * file's. `onResave` is offered only for the engine-built subsystems, where saving again IS the
 * remedy (`apply_settings` rebuilds on a fingerprint that differs from the running one).
 */
export function DiffNote({
  name,
  subsystem,
  onResave,
}: {
  name: EffectiveSubsystemName
  subsystem: EffectiveSubsystem
  onResave?: () => void
}) {
  if (!subsystem.differs) return null
  const fields = differences(name, subsystem)
  const label = SUBSYSTEM_LABELS[name]
  const engineBuilt = name === 'backend' || name === 'gateway' || name === 'pool'
  return (
    <div className="flex flex-wrap items-start gap-2 rounded-lg bg-amber-50 p-2 text-[0.7rem] text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
      <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
      <div className="min-w-0 flex-1 space-y-1">
        <p>
          The {label} is running with values the settings no longer hold
          {fields.length === 0 ? ', in a field this view does not pair by name' : ''} — {remedy(name)}.
        </p>
        {fields.length > 0 && (
          <ul className="space-y-0.5">
            {fields.map((difference) => (
              <li key={difference.path} className="mono break-all">
                {difference.field}: {describeConfigured(difference)}
              </li>
            ))}
          </ul>
        )}
      </div>
      {engineBuilt && onResave && (
        <button type="button" className="btn-outline shrink-0 px-2 py-1 text-[0.7rem]" onClick={onResave}>
          Save again to apply
        </button>
      )}
    </div>
  )
}

function omitted(path: string, omit: string[]): boolean {
  return omit.some((prefix) => path === prefix || path.startsWith(`${prefix}.`))
}

/**
 * Every field the runtime returned for a subsystem, generically: the effective object's leaves
 * with their sources, as returned, no field invented. Hand-placed lines go in `children` and
 * their paths in `omit`, so nothing is printed twice. A subsystem with `effective: null` says
 * "not running" and, when the runtime explained why, repeats the explanation verbatim.
 */
export function EffectiveBlock({
  name,
  subsystem,
  omit = [],
  sinceLabel = 'built',
  children,
}: {
  name: EffectiveSubsystemName
  subsystem: EffectiveSubsystem
  omit?: string[]
  sinceLabel?: string
  children?: ReactNode
}) {
  const differing = new Set(differences(name, subsystem).map((difference) => difference.path))
  const explanation = subsystem.source.effective
  return (
    <div className="rounded-lg border border-dashed border-ink-200 p-2.5 dark:border-ink-800">
      <div className="flex flex-wrap items-baseline justify-between gap-2 text-[0.7rem]">
        <span className="font-medium text-ink-600 dark:text-ink-300">Running now · {SUBSYSTEM_LABELS[name]}</span>
        {subsystem.since !== null && (
          <span className="text-ink-500 dark:text-ink-400">
            {sinceLabel} {relativeTime(subsystem.since)}
          </span>
        )}
      </div>
      {subsystem.effective === null ? (
        <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">not running{explanation ? ` — ${explanation}` : ''}</p>
      ) : (
        <div className="mt-1 space-y-0.5">
          {children}
          {flatten(subsystem.effective)
            .filter((leaf) => !omitted(leaf.path, omit))
            .map((leaf) => (
              <EffectiveLine
                key={leaf.path}
                label={`${leaf.path}:`}
                value={show(leaf.value)}
                source={sourceOf(subsystem.source, leaf.path)}
                differs={differing.has(leaf.path)}
              />
            ))}
        </div>
      )}
    </div>
  )
}

/** The node's argument list as the command an operator can run by hand: monospace, quoted for a
 *  shell, and copyable — because "everything a button does is a visible command line". */
export function NodeCommand({ binary, args, caption }: { binary: string; args: string[]; caption?: ReactNode }) {
  const command = shellCommand(binary, args)
  return (
    <div>
      {caption && <p className="mb-1 text-[0.7rem] text-ink-500 dark:text-ink-400">{caption}</p>}
      <div className="flex items-start gap-1">
        <pre className="mono min-w-0 flex-1 whitespace-pre-wrap break-all rounded-lg bg-ink-900 p-3 text-[0.68rem] leading-relaxed text-ink-200 dark:bg-black/50">
          {command}
        </pre>
        <CopyButton text={command} label="Copy command" />
      </div>
    </div>
  )
}
