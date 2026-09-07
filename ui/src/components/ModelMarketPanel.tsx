// **Adding a model to the chain, and opening its market** — the two acts, kept apart because the
// chain keeps them apart.
//
// Registering a class is a node's act: it needs the converted artifact, an active bond, that
// bond's key and a funded fee outpoint, and it files one `ClassRegistered`. Seeding the market is
// a spend: at least 100,000 MSK locked into the line's sink, forever, in exchange for nothing —
// the seeder receives no position and never gets the money back.
//
// So this panel never renders a button whose consequences it has not first stated. The floor, the
// balance and the shortfall are read off the chain before the seed control exists, and the
// irreversibility is written next to the control rather than behind it.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import type { RegistrationReadiness, SeedOutcome, SeedReadiness } from '../lib/types'
import { Section, Spinner } from './common'

const SOMPI = 100_000_000

function msk(sompi: number | null): string {
  if (sompi === null || !Number.isFinite(sompi)) return '—'
  return (sompi / SOMPI).toLocaleString(undefined, { maximumFractionDigits: 8 })
}

export function ModelMarketPanel() {
  const [registration, setRegistration] = useState<RegistrationReadiness | null>(null)
  const [seed, setSeed] = useState<SeedReadiness | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [outcome, setOutcome] = useState<SeedOutcome | null>(null)
  const [modelId, setModelId] = useState('')
  const [showCommand, setShowCommand] = useState(false)

  const refresh = useCallback(async () => {
    // Independently: a node that cannot answer about registration should not blank the seed half.
    const [r, s] = await Promise.allSettled([api.classRegistration(), api.seedReadiness()])
    if (r.status === 'fulfilled') setRegistration(r.value)
    if (s.status === 'fulfilled') setSeed(s.value)
  }, [])

  useEffect(() => {
    void refresh()
    const timer = setInterval(() => void refresh(), 20_000)
    return () => clearInterval(timer)
  }, [refresh])

  const arm = async () => {
    setBusy('register')
    setError(null)
    try {
      setRegistration(await api.registerClass(modelId.trim() || null))
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  // Two presses, and the first one is not a formality: the dry run comes back from the chain's own
  // code path with the amount it would lock, which is the last chance to read the number before it
  // is gone for good.
  const runSeed = async (confirm: boolean) => {
    if (!seed?.line_id) return
    setBusy(confirm ? 'seed' : 'dry')
    setError(null)
    try {
      const result = await api.seedMarket(seed.line_id, String(seed.seed_min_sompi / SOMPI), confirm)
      setOutcome(result)
      if (confirm) await refresh()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  return (
    <Section
      title="Add a model, open its market"
      description="Two acts the chain keeps apart: registering the class so it can be mined, and locking the seed that opens its curve."
    >
      <div className="grid gap-2 lg:grid-cols-2">
        {/* ---------- half one: the class ---------- */}
        <div className="rounded-lg border border-ink-200 p-2 dark:border-ink-800">
          <div className="text-[0.7rem] font-medium">1 · Register the class</div>
          <div className="mt-0.5 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
            Submits one <code>ClassRegistered</code> for this node&apos;s artifact, so the chain adjudicates your model and
            blocks can be mined on it. A node&apos;s act, run once — it needs the artifact, an active bond, that bond&apos;s key
            and a funded fee outpoint.
          </div>

          {registration === null ? (
            <div className="mt-2"><Spinner /></div>
          ) : (
            <>
              <dl className="mt-2 space-y-1 text-[0.7rem]">
                <Row label="Artifact" value={registration.artifact} />
                <Row label="Bond" value={registration.bond} />
                <Row label="Fee outpoint" value={registration.fee_outpoint} />
                <Row label="Producer key" value={registration.has_key ? 'set' : null} />
              </dl>

              {registration.blocked_because ? (
                <p className="mt-2 rounded-md bg-amber-50 p-2 text-[0.7rem] leading-relaxed text-amber-800 dark:bg-amber-950/40 dark:text-amber-200">
                  {registration.blocked_because}
                </p>
              ) : registration.armed ? (
                <p className="mt-2 rounded-md bg-emerald-50 p-2 text-[0.7rem] leading-relaxed text-emerald-800 dark:bg-emerald-950/40 dark:text-emerald-200">
                  Armed. The next producer start files the registration and disarms itself, so it cannot file twice.
                </p>
              ) : (
                <div className="mt-2 space-y-1.5">
                  <input
                    className="w-full rounded-md border border-ink-200 bg-white px-2 py-1 text-[0.7rem] dark:border-ink-800 dark:bg-ink-950"
                    placeholder="Model id — only if the shape matches more than one class"
                    value={modelId}
                    onChange={(e) => setModelId(e.target.value)}
                  />
                  <button
                    className="rounded-md bg-ink-900 px-2 py-1 text-[0.7rem] font-medium text-white disabled:opacity-50 dark:bg-ink-100 dark:text-ink-900"
                    disabled={busy !== null}
                    onClick={() => void arm()}
                  >
                    {busy === 'register' ? 'Arming…' : 'Arm for the next start'}
                  </button>
                </div>
              )}

              {registration.command.length > 0 && (
                <>
                  <button
                    className="mt-2 text-[0.65rem] text-ink-500 underline dark:text-ink-400"
                    onClick={() => setShowCommand((v) => !v)}
                  >
                    {showCommand ? 'Hide' : 'Show'} the command this runs
                  </button>
                  {showCommand && (
                    <pre className="mt-1 overflow-x-auto rounded-md bg-ink-50 p-2 text-[0.6rem] leading-relaxed dark:bg-ink-900/60">
                      kaspad {registration.command.join(' ')}
                    </pre>
                  )}
                </>
              )}
            </>
          )}
        </div>

        {/* ---------- half two: the market ---------- */}
        <div className="rounded-lg border border-ink-200 p-2 dark:border-ink-800">
          <div className="text-[0.7rem] font-medium">2 · Open the market</div>
          <div className="mt-0.5 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
            Locks at least {msk(seed?.seed_min_sompi ?? 10_000_000_000_000)} MSK into the line&apos;s reserve and opens
            500,000 positions on the curve. <strong>The seed is locked for good</strong>: it takes no fee, mints no position
            for you, and nothing on the chain ever pays it back.
          </div>

          {seed === null ? (
            <div className="mt-2"><Spinner /></div>
          ) : (
            <>
              <dl className="mt-2 space-y-1 text-[0.7rem]">
                <Row label="Line" value={seed.line_id} mono />
                <Row label="Paying from" value={seed.from_address} mono />
                <Row label="Can spend" value={seed.spendable_sompi === null ? null : `${msk(seed.spendable_sompi)} MSK`} />
                <Row label="Floor" value={`${msk(seed.seed_min_sompi)} MSK`} />
                {seed.short_by_sompi !== null && seed.short_by_sompi > 0 && (
                  <Row label="Short by" value={`${msk(seed.short_by_sompi)} MSK`} />
                )}
                <Row
                  label="Market"
                  value={seed.already_seeded === null ? 'could not ask the chain' : seed.already_seeded ? 'already open' : 'not yet open'}
                />
              </dl>

              {seed.blocked_because ? (
                <p className="mt-2 rounded-md bg-amber-50 p-2 text-[0.7rem] leading-relaxed text-amber-800 dark:bg-amber-950/40 dark:text-amber-200">
                  {seed.blocked_because}
                </p>
              ) : (
                <div className="mt-2 flex gap-1.5">
                  <button
                    className="rounded-md border border-ink-200 px-2 py-1 text-[0.7rem] disabled:opacity-50 dark:border-ink-800"
                    disabled={busy !== null}
                    onClick={() => void runSeed(false)}
                  >
                    {busy === 'dry' ? 'Checking…' : 'Preview'}
                  </button>
                  <button
                    className="rounded-md bg-rose-600 px-2 py-1 text-[0.7rem] font-medium text-white disabled:opacity-50"
                    disabled={busy !== null || outcome === null}
                    title={outcome === null ? 'Preview it first — this cannot be undone' : undefined}
                    onClick={() => void runSeed(true)}
                  >
                    {busy === 'seed' ? 'Locking…' : `Lock ${msk(seed.seed_min_sompi)} MSK for good`}
                  </button>
                </div>
              )}

              {outcome && (
                <p className="mt-2 rounded-md bg-ink-50 p-2 text-[0.7rem] leading-relaxed dark:bg-ink-900/60">
                  {outcome.detail}
                </p>
              )}
            </>
          )}
        </div>
      </div>

      {error && (
        <p className="mt-2 rounded-md bg-rose-50 p-2 text-[0.7rem] leading-relaxed text-rose-800 dark:bg-rose-950/40 dark:text-rose-200">
          {error}
        </p>
      )}
    </Section>
  )
}

function Row({ label, value, mono }: { label: string; value: string | null; mono?: boolean }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="shrink-0 text-ink-500 dark:text-ink-400">{label}</dt>
      <dd className={`truncate text-right ${mono ? 'font-mono text-[0.65rem]' : ''} ${value ? '' : 'text-ink-400'}`}>
        {value ?? 'not set'}
      </dd>
    </div>
  )
}
