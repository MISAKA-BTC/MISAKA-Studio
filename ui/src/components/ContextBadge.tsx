// How much text a model holds, said the same way everywhere a model or a class is listed.
//
// On this network the number ranges from 12 positions (the floor class) through 512 (the dense
// A16 and QWEN36 classes) to classes registered at 2M, and for a class it decides what a job can
// carry — so it sits in the badge row beside the size and the share, not in a line of small print.
// Where it came from goes in the tooltip: a GGUF's trained length, an artifact's rotary table, or
// the context a class was registered at are three different facts.

import { tokens } from '../lib/format'
import type { PalwArtifactHeader } from '../lib/types'

export function ContextBadge({ value, title, tone = 'plain' }: { value: number; title: string; tone?: 'plain' | 'warn' }) {
  const colours =
    tone === 'warn'
      ? 'bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300'
      : 'bg-sky-100 text-sky-800 dark:bg-sky-950/60 dark:text-sky-300'
  return (
    <span className={`badge ${colours}`} title={title}>
      {tokens(value)} ctx
    </span>
  )
}

/**
 * A class's registered context, and what the file on disk can hold beside it.
 *
 * The two agree for every class registered today; a file whose rotary table is shorter than the
 * registration cannot serve the class, and that is worth a warning colour rather than a footnote.
 */
export function ClassContext({ registered, header }: { registered: number; header: PalwArtifactHeader | null }) {
  const short = header !== null && header.max_position < registered
  return (
    <>
      <ContextBadge
        value={registered}
        tone={short ? 'warn' : 'plain'}
        title={`Registered context: the class was registered at ${registered.toLocaleString()} tokens — the prompt and the answer of one job share this window. A registration choice, not a property of the weights.`}
      />
      {short && (
        <span className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">
          file holds only {tokens(header.max_position)}
        </span>
      )}
    </>
  )
}
