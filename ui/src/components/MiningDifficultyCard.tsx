// The question every miner asks first — how many tries is a block? — answered in the node's own
// numbers, for the two ways a pool slot earns.
//
// A prompt is not a lottery ticket. It is one claim, carried by whichever block comes next, paid by
// its work units when the claim is Final. The slot's own draws ARE a lottery: a draw wins a block
// when it passes the class ticket and the Layer-0 target, and the node states both odds in its log
// every five minutes. This card multiplies them out so "1 in 6,300 draws at 0.9 draws/s ≈ 2 h" is
// read off rather than worked out.

import type { PoolStatus } from '../lib/types'

function fmtDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '—'
  if (seconds < 90) return `${Math.round(seconds)} s`
  if (seconds < 5400) return `${Math.round(seconds / 60)} min`
  if (seconds < 172800) return `${(seconds / 3600).toFixed(1)} h`
  return `${(seconds / 86400).toFixed(1)} days`
}

function fmtOdds(p: number | null): string {
  if (p === null || !(p > 0)) return '—'
  return `1 in ${Math.round(1 / p).toLocaleString()}`
}

export function MiningDifficultyCard({ pool }: { pool: PoolStatus }) {
  if (!pool.joined) return null
  const d = pool.difficulty ?? null
  const lane = pool.fp
  const promptMining = lane !== null && lane.mode === 'fp' && lane.gateway_running && lane.submitter_running
  const claims = lane?.claims_submitted ?? 0

  return (
    <div className="rounded-lg border border-ink-200 p-2 dark:border-ink-800">
      <div className="text-[0.7rem] font-medium">Difficulty — what a block costs</div>

      <div className="mt-2 grid gap-2 sm:grid-cols-2">
        <div className="rounded-md bg-ink-50 p-2 dark:bg-ink-900/60">
          <div className="text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">Your prompts</div>
          <div className="mt-0.5 text-sm font-medium">1 question = 1 claim</div>
          <div className="mt-0.5 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
            No lottery: every prompt you send becomes one committed claim, carried by whichever block comes next (minutes,
            at this chain&apos;s pace) and paid by its work units once Final.
            {promptMining ? ` ${claims} claim${claims === 1 ? '' : 's'} submitted so far.` : ' Turn on prompt mining to use it.'}
          </div>
        </div>

        <div className="rounded-md bg-ink-50 p-2 dark:bg-ink-900/60">
          <div className="text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">The slot&apos;s block lottery</div>
          {d && d.draws_per_block ? (
            <>
              <div className="mt-0.5 text-sm font-medium">
                1 block per ~{Math.round(d.draws_per_block).toLocaleString()} draws
                {d.expected_seconds_per_block ? <> · ≈ {fmtDuration(d.expected_seconds_per_block)} each</> : null}
              </div>
              <div className="mt-0.5 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
                A draw is one inference on the class. It wins when it passes the class ticket ({fmtOdds(d.class_ticket_p)}) and
                the network&apos;s Layer-0 target ({fmtOdds(d.layer0_p)}).
                {d.draws_per_s ? <> This slot draws {d.draws_per_s.toFixed(2)}/s.</> : null}
                {' '}This run: {d.draws_this_run.toLocaleString()} draws · {d.class_ticket_wins_this_run} ticket win
                {d.class_ticket_wins_this_run === 1 ? '' : 's'} · {d.produced_this_run} block{d.produced_this_run === 1 ? '' : 's'}.
              </div>
            </>
          ) : (
            <div className="mt-0.5 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
              The slot has not reported its odds yet — it states them in its log every five minutes once it is drawing.
            </div>
          )}
        </div>
      </div>
    </div>
  )
}
