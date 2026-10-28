// The wait before the first token, said out loud.

import { useEffect, useState } from 'react'
import { useStudio } from '../../store/studio'
import { Spinner } from '../common'

/**
 * What "no text yet" means.
 *
 * On the free-prompt lane there is nothing to show until the run ENDS: the answer is one committed
 * execution, and the gateway hands back every token at once when it finishes — measured, all four
 * SSE frames arriving in the same tenth of a second after ten seconds of silence. At roughly a
 * token a second a full answer is minutes, so a bare "Thinking…" is indistinguishable from a hang,
 * and was reported as one.
 *
 * The elapsed count is the honest part: it says the wait is running, which a spinner alone does
 * not. llama.cpp streams, so there the wait is a second and this reads the same as before.
 */
export function Waiting() {
  const backend = useStudio((s) => s.runtime?.backend)
  const [seconds, setSeconds] = useState(0)
  useEffect(() => {
    const timer = setInterval(() => setSeconds((n) => n + 1), 1000)
    return () => clearInterval(timer)
  }, [])
  const mining = backend === 'gateway'
  return (
    <div className="flex items-start gap-2 py-1 text-sm text-ink-500 dark:text-ink-400" role="status">
      <Spinner className="mt-0.5 size-3.5 shrink-0" />
      <span>
        {mining ? 'Mining your answer…' : 'Thinking…'}
        {seconds >= 3 && <span className="tabular-nums"> {seconds}s</span>}
        {mining && seconds >= 8 && (
          <span className="mt-0.5 block text-[0.7rem] leading-relaxed">
            This lane runs the whole job before any text exists — the answer and the claim behind it are one execution, at
            about a token a second. Shorten it with <strong>max tokens</strong> under Parameters.
          </span>
        )}
      </span>
    </div>
  )
}
