// Bond setup — from an empty app to a bonded producer, one step at a time.
//
// A producer on testnet-12 is one key: it signs the bond, it is the operator, and its own address
// is where the collateral is spent from and where rewards land. The card walks the four facts in
// the order a person meets them — a key, its address, one deposit big enough, the registration —
// and then lets the runtime finish on its own (save the bond, declare what it judges, wait for
// that to be in a block, restart as a producer). Every number on it is read, not assumed: the
// address from the CLI, the funds from the node's utxo index, the bond from the node's own line.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import type { BondPhase, BondSetup } from '../lib/types'
import { NETWORK_LABEL } from '../lib/types'
import { CopyButton, Spinner } from './common'

const SOMPI = 100_000_000

function msk(sompi: number | null | undefined): string {
  if (sompi === null || sompi === undefined) return '—'
  return `${(sompi / SOMPI).toLocaleString(undefined, { maximumFractionDigits: 2 })} MSK`
}

const STEPS: { phase: BondPhase[]; label: string }[] = [
  { phase: ['need_key'], label: 'Key' },
  { phase: ['need_funds'], label: 'Deposit' },
  { phase: ['ready_to_register', 'registering'], label: 'Register' },
  { phase: ['finishing'], label: 'Declare & start' },
  { phase: ['bonded'], label: 'Mining' },
]

export function BondSetupCard({ onChanged }: { onChanged?: () => void }) {
  const [setup, setSetup] = useState<BondSetup | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<'key' | 'register' | 'finish' | 'faucet' | null>(null)
  // `undefined` = not chosen yet (the saved setting, else the node's own sizing); `null` = the node sizes it.
  const [choice, setChoice] = useState<number | null | undefined>(undefined)
  const [custom, setCustom] = useState('')

  const refresh = useCallback(async () => {
    try {
      setSetup(await api.bondSetup())
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])

  useEffect(() => {
    void refresh()
    // Faster while something is happening on chain, slower while waiting on the person.
    const moving = setup?.job.running || setup?.phase === 'registering' || setup?.phase === 'finishing'
    const timer = setInterval(() => void refresh(), moving ? 4000 : 10000)
    return () => clearInterval(timer)
  }, [refresh, setup?.job.running, setup?.phase])

  if (!setup) return null
  const current = STEPS.findIndex((s) => s.phase.includes(setup.phase))
  const floor = setup.floor_sompi
  // The amount named on the command line, or null for the node's own sizing.
  const named: number | null =
    custom.trim() !== '' ? Math.round(Number(custom) * SOMPI) : choice !== undefined ? choice : setup.collateral_sompi
  const customBad = custom.trim() !== '' && (!Number.isFinite(Number(custom)) || Number(custom) <= 0)
  const belowFloor = named !== null && floor !== null && named < floor
  // What the deposit has to reach. The node's own figure once it has printed one; before that, for
  // its own sizing, the last measured value — shown as "about", never used to refuse.
  const nodeSized = setup.node_wanted_sompi ?? setup.choices.find((c) => c.collateral_sompi === null)?.approx_sompi ?? null
  const collateral = named ?? nodeSized
  const exact = named !== null || setup.node_wanted_sompi !== null
  const needed = collateral !== null && collateral > 0 ? collateral + setup.recommended_margin_sompi : null
  const largest = setup.funds?.largest_output_sompi ?? null
  const enough = named === null || largest === null || largest >= named + setup.margin_sompi
  const belowLifetime = named !== null && nodeSized !== null && named < nodeSized

  const act = async (kind: 'key' | 'register' | 'finish', run: () => Promise<unknown>) => {
    setBusy(kind)
    setError(null)
    try {
      await run()
      await refresh()
      onChanged?.()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  if (setup.phase === 'bonded' && !setup.job.error) {
    return (
      <div className="card mb-4 flex flex-wrap items-center gap-x-4 gap-y-1 p-3 text-xs">
        <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">bonded</span>
        <span className="mono break-all">{setup.bond}</span>
        {setup.address && <span className="mono break-all text-ink-500 dark:text-ink-400">{setup.address}</span>}
        {setup.job.history.length > 0 && <span className="text-ink-500 dark:text-ink-400">{setup.job.history[setup.job.history.length - 1]}</span>}
      </div>
    )
  }

  return (
    <section className="card mb-4 p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-sm font-semibold">Bond setup — mine on {NETWORK_LABEL[setup.network]}</h3>
        <ol className="flex flex-wrap gap-1 text-[0.65rem]">
          {STEPS.map((step, i) => (
            <li
              key={step.label}
              className={`rounded-full px-2 py-0.5 ${
                i < current
                  ? 'bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300'
                  : i === current
                    ? 'bg-arc-600 text-white'
                    : 'bg-ink-100 text-ink-500 dark:bg-ink-800 dark:text-ink-400'
              }`}
            >
              {i + 1}. {step.label}
            </li>
          ))}
        </ol>
      </div>
      <p className="mt-1 text-xs leading-relaxed text-ink-500 dark:text-ink-400">
        A producer is one key. Its address receives your deposit; the node locks the collateral from it as the bond, and the
        rewards come back to the same address. One key registers one bond for the life of the chain.
      </p>

      {/* 1. Key */}
      {!setup.key_present && (
        <div className="mt-3">
          <button type="button" className="btn-primary" disabled={busy !== null} onClick={() => void act('key', () => api.producerKey())}>
            {busy === 'key' ? 'Generating…' : 'Create a mining key and address'}
          </button>
          <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
            32 bytes from the OS random source, written 0600 under the Studio's data directory and never shown. Back that
            file up: it is the bond, and the only thing that can sign for it or retire it.
          </p>
        </div>
      )}

      {/* 2. Address and deposit */}
      {setup.key_present && (
        <div className="mt-3 rounded-lg bg-ink-50 p-3 dark:bg-ink-900/60">
          <div className="text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">Your mining address — send MSK here</div>
          {setup.address ? (
            <div className="mt-0.5 flex flex-wrap items-center gap-2">
              <span className="mono break-all text-xs">{setup.address}</span>
              <CopyButton text={setup.address} label="Copy" className="btn-outline" />
            </div>
          ) : (
            <p className="mt-0.5 text-[0.7rem] text-amber-700 dark:text-amber-300">
              {setup.address_error ?? 'The address appears once the `misaka` CLI or the node can derive it from the key.'}
            </p>
          )}
          <div className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-xs sm:grid-cols-4">
            <Stat label="At the address" value={setup.funds ? msk(setup.funds.total_sompi) : '—'} />
            <Stat label="Largest single deposit" value={largest !== null ? msk(largest) : '—'} />
            <Stat label="Needed (collateral + fees)" value={needed !== null ? `${exact ? '' : 'about '}${msk(needed)}` : 'the node decides'} />
            <Stat label="Key file" value={setup.key_path?.split('/').pop() ?? '—'} />
          </div>
          {setup.funds_error && <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">{setup.funds_error} — start the node to see deposits.</p>}
          {setup.funds && largest !== null && needed !== null && largest < needed && setup.funds.total_sompi >= needed && (
            <p className="mt-1 text-[0.7rem] text-amber-700 dark:text-amber-300">
              The address holds enough in total, but the registration spends one output. Merge them first:{' '}
              <code>misaka wallet utxo consolidate --key-file {setup.key_path} --yes</code>
            </p>
          )}
          {setup.funds && setup.funds.coinbase_sompi > 0 && (
            <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
              {msk(setup.funds.coinbase_sompi)} of it is mining rewards, which are not counted toward the registration.
            </p>
          )}
        </div>
      )}

      {/* 3. Amount and registration */}
      {setup.key_present && !setup.bond && !setup.job.running && setup.phase !== 'registering' && (
        <div className="mt-3">
          <div className="text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">Collateral to lock</div>
          <div className="mt-1 grid gap-2 sm:grid-cols-2">
            {setup.choices.map((c) => {
              const selected = custom === '' && named === c.collateral_sompi
              return (
                <button
                  key={c.label}
                  type="button"
                  className={`rounded-lg border p-2 text-left text-xs ${
                    selected ? 'border-arc-600 bg-arc-500/10' : 'border-ink-200 hover:border-ink-400 dark:border-ink-800'
                  }`}
                  onClick={() => {
                    setChoice(c.collateral_sompi)
                    setCustom('')
                  }}
                >
                  <div className="font-medium">
                    {c.label} · {c.collateral_sompi === null && setup.node_wanted_sompi === null ? 'about ' : ''}
                    {msk(c.collateral_sompi === null ? (setup.node_wanted_sompi ?? c.approx_sompi) : c.collateral_sompi)}
                  </div>
                  <div className={`mt-0.5 text-[0.7rem] ${c.below_lifetime_sizing ? 'text-amber-700 dark:text-amber-300' : 'text-ink-500 dark:text-ink-400'}`}>{c.note}</div>
                </button>
              )
            })}
          </div>
          <input
            className="input mt-2 w-44"
            placeholder="or another amount, MSK"
            inputMode="decimal"
            value={custom}
            onChange={(e) => setCustom(e.target.value)}
          />
          <p className="mt-1 text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
            A claim's exposure stays on the bond until the claim is Final, so the collateral has to hold every claim in flight at once;
            more collateral is more claims at once. Collateral cannot be topped up later — a bigger bond needs a new key.
            {floor !== null && ` ${NETWORK_LABEL[setup.network]} refuses a producer bond under ${msk(floor)}.`} Deposit about{' '}
            {msk(setup.recommended_margin_sompi)} more than the collateral: it pays the registration and the declaration and stays as the
            fee float.
          </p>
          <button
            type="button"
            className="btn-primary mt-2"
            disabled={busy !== null || customBad || belowFloor || !enough}
            onClick={() => void act('register', () => api.bondRegister(named))}
          >
            {busy === 'register' ? 'Starting…' : named === null ? 'Register the bond (the node sizes it)' : `Register the bond with ${msk(named)}`}
          </button>
          {belowFloor && <p className="mt-1 text-[0.7rem] text-red-600 dark:text-red-400">Below the floor — the chain would refuse it.</p>}
          {belowLifetime && !belowFloor && (
            <p className="mt-1 text-[0.7rem] text-amber-700 dark:text-amber-300">
              Under the node's own sizing ({msk(nodeSized)}): it will register, and the node warns its producer may then hold forever with
              no room for another claim.
            </p>
          )}
          {!enough && !belowFloor && named !== null && (
            <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">Waiting for a deposit of at least {msk(named + setup.margin_sompi)} in one output.</p>
          )}
          {named === null && largest !== null && needed !== null && largest < needed && (
            <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
              The largest deposit is under {exact ? '' : 'about '}{msk(needed)}. You can start the registration now: the node waits for the
              funds and prints the exact amount it needs.
            </p>
          )}
          {setup.funds === null && (
            <p className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
              The node is not answering, so the deposit cannot be checked here. Registering starts it: it syncs, waits for the
              funds, and says what it is waiting for.
            </p>
          )}
        </div>
      )}

      {/* 4. Progress */}
      {(setup.job.running || setup.phase === 'registering' || setup.job.history.length > 0) && (
        <div className="mt-3 rounded-lg border border-ink-200 p-3 text-xs dark:border-ink-800">
          {setup.job.step && (
            <div className="flex items-center gap-2">
              <Spinner className="size-3.5" /> {setup.job.step}
            </div>
          )}
          {setup.registration_wait && !setup.reported_bond && (
            <p className="mt-1 text-[0.7rem] text-amber-700 dark:text-amber-300">The node: {setup.registration_wait}</p>
          )}
          {setup.job.history.length > 0 && (
            <ol className="mt-2 list-decimal space-y-0.5 pl-4 text-[0.7rem] text-ink-500 dark:text-ink-400">
              {setup.job.history.map((line, i) => (
                <li key={i}>{line}</li>
              ))}
            </ol>
          )}
          {setup.job.error && <p className="mt-2 text-[0.7rem] text-red-600 dark:text-red-400">{setup.job.error}</p>}
          {!setup.job.running && (setup.reported_bond || setup.bond) && (
            <button type="button" className="btn-outline mt-2" disabled={busy !== null} onClick={() => void act('finish', () => api.bondFinish())}>
              {busy === 'finish' ? 'Finishing…' : 'Finish: declare and start producing'}
            </button>
          )}
        </div>
      )}

      {error && <p className="mt-2 text-[0.7rem] text-red-600 dark:text-red-400">{error}</p>}
    </section>
  )
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <div className="text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">{label}</div>
      <div className="tabular-nums">{value}</div>
    </div>
  )
}
