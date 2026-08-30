/**
 * Mining with a prompt — the free-prompt lane (ADR-0044).
 *
 * The claim this panel exists to make good on: **the run that answers you is the run that does
 * the work.** There is no second, mining-only inference anywhere behind it. The gateway hands
 * back the answer and the commitment inputs from one execution, and both halves are shown here
 * together because they came from one execution.
 *
 * The claim this panel refuses to make: that any of it has been mined. A commitment is the first
 * step of a lattice — submit, bind, receipt, challenge, court — and only a claim that comes out
 * *Final* licenses a block. So the result carries its chain state by name, and the header says
 * what has and has not reached a chain. Ambiguity here would be the same failure the Network
 * tab's mining banner was built to prevent, one screen over.
 */
import { useEffect, useState } from 'react'
import { api } from '../lib/api'
import type { PromptMiningRun, PromptMiningStatus } from '../lib/types'

/** A 128-hex root is unreadable and still worth showing; the head is what a person compares. */
function Root({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline gap-2 text-xs">
      <span className="w-28 shrink-0 text-neutral-500 dark:text-neutral-400">{label}</span>
      <span className="mono truncate" title={value}>
        {value.slice(0, 32)}…
      </span>
    </div>
  )
}

function GatewayLine({ status }: { status: PromptMiningStatus }) {
  if (status.unreachable !== null) {
    return (
      <div className="rounded-lg border border-neutral-200 bg-neutral-50 p-3 text-sm dark:border-neutral-800 dark:bg-neutral-900">
        <p className="font-semibold">No gateway at {status.gateway_url}</p>
        <p className="mt-1 text-neutral-600 dark:text-neutral-400">
          The gateway is what runs the inference and commits it. Start one, or point{' '}
          <span className="mono">node.palw_gateway_url</span> at a hosted one.
        </p>
        <p className="mono mt-2 text-xs text-neutral-500">{status.unreachable}</p>
      </div>
    )
  }

  const klass = status.class
  return (
    <div className="rounded-lg border border-neutral-200 bg-neutral-50 p-3 text-sm dark:border-neutral-800 dark:bg-neutral-900">
      <p className="font-semibold">Gateway ready — {status.gateway_url}</p>
      {status.health?.class_id ? (
        <div className="mt-2 space-y-1">
          <Root label="class" value={status.health.class_id} />
          {status.health.bond && <Root label="executor bond" value={status.health.bond} />}
          {klass?.state === 'registered' && (
            <p className="text-xs text-emerald-700 dark:text-emerald-300">
              This class is registered on the network as <strong>{klass.name}</strong>.
            </p>
          )}
          {klass?.state === 'not_registered' && (
            <p className="text-xs text-amber-700 dark:text-amber-300">
              This class is not one the network registers, so its commitments cannot be admitted.
            </p>
          )}
          {klass?.state === 'unknown' && (
            <p className="text-xs text-neutral-600 dark:text-neutral-400">
              Whether the network registers this class cannot be told from here: {klass.complete_ids} of{' '}
              {klass.total_classes} class ids are published in full and the rest are prefixes, so a non-match proves
              nothing. The node performs the real check, against the artifact root.
            </p>
          )}
        </div>
      ) : (
        <p className="mt-1 text-xs text-neutral-600 dark:text-neutral-400">
          This gateway does not advertise a class or a bond, so there is no way from here to tell what it is
          accountable to.
        </p>
      )}
    </div>
  )
}

function Result({ run }: { run: PromptMiningRun }) {
  return (
    <div className="space-y-3">
      <div className="rounded-lg border border-neutral-200 p-3 dark:border-neutral-800">
        <p className="text-xs font-semibold uppercase tracking-wide text-neutral-500">The answer</p>
        <p className="mt-1 whitespace-pre-wrap text-sm">{run.answer}</p>
      </div>

      <div className="rounded-lg border border-neutral-200 p-3 dark:border-neutral-800">
        <p className="text-xs font-semibold uppercase tracking-wide text-neutral-500">
          The same run, as work
        </p>
        <div className="mt-2 space-y-1">
          <div className="flex items-baseline gap-2 text-xs">
            <span className="w-28 shrink-0 text-neutral-500 dark:text-neutral-400">compute units</span>
            <span className="mono">{run.cu}</span>
            {run.completion_tokens !== null && (
              <span className="text-neutral-500">
                · {run.prompt_tokens ?? '—'} in / {run.completion_tokens} out
              </span>
            )}
          </div>
          <Root label="job" value={run.fp_job_id} />
          <Root label="trace root" value={run.trace_root} />
          <Root label="output root" value={run.output_root} />
          <Root label="schedule root" value={run.schedule_root} />
        </div>
        <p className="mono mt-2 break-all text-[11px] text-neutral-500">{run.artifact}</p>
      </div>

      {/* The honest end of the story, and the only place the panel talks about chains. */}
      <div className="rounded-lg border border-amber-200 bg-amber-50 p-3 text-sm dark:border-amber-900 dark:bg-amber-950">
        <p className="font-semibold text-amber-900 dark:text-amber-200">Committed — not submitted, not mined</p>
        <p className="mt-1 text-amber-800 dark:text-amber-300">
          The commitment is signed and sitting in the outbox. Nothing has been sent to a network: the executor rail
          builds the transaction and deliberately stops before submitting it. Even once submitted, a free-prompt
          claim licenses a block only after it certifies — bind, receipt, challenge and court — which is windows of
          chain time, not seconds.
        </p>
      </div>
    </div>
  )
}

export function PromptMiningPanel() {
  const [status, setStatus] = useState<PromptMiningStatus | null>(null)
  const [prompt, setPrompt] = useState('')
  const [running, setRunning] = useState(false)
  const [run, setRun] = useState<PromptMiningRun | null>(null)
  const [error, setError] = useState<string | null>(null)

  useEffect(() => {
    let live = true
    api
      .promptMining()
      .then((s) => live && setStatus(s))
      .catch((e) => live && setError(String(e)))
    return () => {
      live = false
    }
  }, [])

  async function submit() {
    setRunning(true)
    setError(null)
    setRun(null)
    try {
      setRun(await api.promptMiningRun(prompt, 128))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setRunning(false)
    }
  }

  const ready = status !== null && status.unreachable === null

  return (
    <section className="space-y-3">
      <div>
        <h3 className="text-sm font-semibold">Mine with a prompt</h3>
        <p className="mt-1 text-xs text-neutral-600 dark:text-neutral-400">
          One inference, both halves: the model answers you, and the same run produces the commitment that prices
          the work. No second lane runs behind this.
        </p>
      </div>

      {status !== null && <GatewayLine status={status} />}

      <textarea
        className="h-24 w-full resize-y rounded-lg border border-neutral-300 bg-white p-2 text-sm dark:border-neutral-700 dark:bg-neutral-950"
        placeholder="Ask anything — the answer is the work."
        value={prompt}
        onChange={(e) => setPrompt(e.target.value)}
        disabled={!ready || running}
      />

      <div className="flex items-center gap-3">
        <button
          className="rounded-lg bg-neutral-900 px-3 py-1.5 text-sm font-medium text-white disabled:opacity-40 dark:bg-white dark:text-neutral-900"
          onClick={submit}
          disabled={!ready || running || prompt.trim() === ''}
        >
          {running ? 'Running the inference…' : 'Run'}
        </button>
        {running && (
          <span className="text-xs text-neutral-500">
            One execution, and it is the priced one — it takes as long as it takes.
          </span>
        )}
      </div>

      {error !== null && (
        <p className="rounded-lg border border-red-200 bg-red-50 p-3 text-sm text-red-800 dark:border-red-900 dark:bg-red-950 dark:text-red-200">
          {error}
        </p>
      )}

      {run !== null && <Result run={run} />}
    </section>
  )
}
