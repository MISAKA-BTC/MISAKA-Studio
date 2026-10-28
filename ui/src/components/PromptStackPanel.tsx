// Prompt mining with no node here — the reason MISAKA Studio exists.
//
// The Studio runs the free-prompt gateway (its worker answers the prompt and commits the work; it holds no key) and the
// rail's watcher (it signs each committed job with the bond key and relays it), both pointed at the network's public
// wRPC. Every command is shown before it runs. The one decision the Studio never makes for the user is signing on a
// public node's word: the rail's signing gate refuses UNVERIFIED_REMOTE until the user accepts it here.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import type { FreePromptStackSettings, PromptStackPreflight, PromptStackView, Settings } from '../lib/types'
import { useStudio } from '../store/studio'
import { CopyButton, Field, Spinner, Toggle } from './common'

const DEFAULTS: FreePromptStackSettings = {
  gateway_path: null,
  rail_path: null,
  worker_path: null,
  listen: '127.0.0.1:8790',
  artifact: null,
  tokenizer: null,
  bond: null,
  evidence_provider: null,
  accept_unverified: false,
}

export function PromptStackPanel({ settings, save }: { settings: Settings; save: (s: Settings) => Promise<void> }) {
  const toast = useStudio((s) => s.toast)
  const saved = { ...DEFAULTS, ...(settings.node.free_prompt ?? {}) }
  const savedKey = JSON.stringify(saved)
  const [draft, setDraft] = useState<FreePromptStackSettings>(saved)
  const [view, setView] = useState<PromptStackView | null>(null)
  const [preflight, setPreflight] = useState<PromptStackPreflight | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [showAdvanced, setShowAdvanced] = useState(false)
  useEffect(() => setDraft(JSON.parse(savedKey)), [savedKey])
  const dirty = JSON.stringify(draft) !== savedKey
  const set = <K extends keyof FreePromptStackSettings>(key: K, value: FreePromptStackSettings[K]) => setDraft({ ...draft, [key]: value })
  const text = (value: string) => (value.trim() === '' ? null : value.trim())

  const refresh = useCallback(async () => {
    try {
      setView(await api.promptStack())
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    }
  }, [])
  useEffect(() => {
    void refresh()
    const id = window.setInterval(refresh, 5000)
    return () => window.clearInterval(id)
  }, [refresh])

  const run = async (label: string, f: () => Promise<void>) => {
    setBusy(label)
    setError(null)
    try {
      await f()
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e))
    } finally {
      setBusy(null)
    }
  }

  const running = Boolean(view?.gateway.running || view?.rail.running)
  const planned = view && 'Ok' in view.planned ? view.planned.Ok : null
  const plannedError = view && 'Err' in view.planned ? view.planned.Err : null
  const shown = running ? [view?.gateway.command_line, view?.rail.command_line].filter((c): c is string[] => Boolean(c)) : (planned ?? [])

  return (
    <section className="card p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-semibold">Prompt mining — no node</h3>
        <span className={running ? 'badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300' : 'badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300'}>
          {running ? 'running' : 'stopped'}
        </span>
      </div>
      <p className="mt-1 text-xs text-ink-500 dark:text-ink-400">
        The answer you read and the work the chain pays for come from one inference. The gateway runs it on this machine and
        publishes its evidence; the rail signs the commitment with your bond key and relays it through the public wRPC. No
        node runs here.
      </p>
      <div className="mt-3 rounded-lg border border-amber-300/60 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-500/30 dark:bg-amber-950/40 dark:text-amber-200">
        A relay&apos;s accept is not inclusion, and a commitment is not a reward: a claim pays only once it is <b>Final</b>. Facts
        read from the public nodes are <b>UNVERIFIED_REMOTE_STATE</b>.
      </div>

      <Field label="Executor bond" hint="<txid>:<index>. Empty lets the rail find the bond registered to your key.">
        <input className="input mt-1" placeholder="<txid>:<index>" value={draft.bond ?? ''} onChange={(e) => set('bond', text(e.target.value))} />
      </Field>
      <Toggle
        label="I accept signing on a public node's word"
        hint="Passes --accept-unverified-state UNVERIFIED_REMOTE to the rail. A node that is not yours is not a verified view; without this the rail signs nothing."
        checked={draft.accept_unverified}
        onChange={(v) => set('accept_unverified', v)}
      />
      <button type="button" className="btn-ghost mt-2 text-xs" onClick={() => setShowAdvanced(!showAdvanced)}>
        {showAdvanced ? 'Hide paths' : 'Paths and endpoints'}
      </button>
      {showAdvanced && (
        <div className="mt-2 grid gap-2">
          <Field label="Artifact" hint="Empty uses qwen25-1.5b-a16-8k.palwart in the models directory.">
            <input className="input mt-1" value={draft.artifact ?? ''} onChange={(e) => set('artifact', text(e.target.value))} />
          </Field>
          <Field label="Tokenizer" hint="Empty uses the class tokenizer beside the artifact.">
            <input className="input mt-1" value={draft.tokenizer ?? ''} onChange={(e) => set('tokenizer', text(e.target.value))} />
          </Field>
          <Field label="Evidence provider" hint="Empty uses the network's published provider.">
            <input className="input mt-1" placeholder="http://169.58.232.113:8088" value={draft.evidence_provider ?? ''} onChange={(e) => set('evidence_provider', text(e.target.value))} />
          </Field>
          <Field label="Gateway listens on" hint="node.palw_gateway_url must reach it; loopback keeps prompts on this machine.">
            <input className="input mt-1" value={draft.listen} onChange={(e) => set('listen', e.target.value)} />
          </Field>
          <Field label="Binaries" hint="Empty looks beside the Studio and on PATH. Build in misakas main: cargo build --release -p misaka-palw-gateway --bin misaka-palw-gateway --bin misaka-palw-fp-rail -p misaka-palw-base0 --bin palw-a16-fp-worker">
            <div className="mt-1 grid gap-2">
              <input className="input" placeholder="misaka-palw-gateway" value={draft.gateway_path ?? ''} onChange={(e) => set('gateway_path', text(e.target.value))} />
              <input className="input" placeholder="misaka-palw-fp-rail" value={draft.rail_path ?? ''} onChange={(e) => set('rail_path', text(e.target.value))} />
              <input className="input" placeholder="palw-a16-fp-worker" value={draft.worker_path ?? ''} onChange={(e) => set('worker_path', text(e.target.value))} />
            </div>
          </Field>
        </div>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        <button type="button" className="btn-primary" disabled={!dirty || busy !== null} onClick={() => run('save', () => save({ ...settings, node: { ...settings.node, free_prompt: draft } }))}>
          {busy === 'save' ? <Spinner /> : 'Save'}
        </button>
        <button type="button" className="btn-outline" disabled={busy !== null} onClick={() => run('check', async () => setPreflight(await api.promptStackPreflight()))}>
          {busy === 'check' ? <Spinner /> : 'Check'}
        </button>
        {running ? (
          <button
            type="button"
            className="btn-outline"
            disabled={busy !== null}
            onClick={() =>
              run('stop', async () => {
                await api.stopPromptStack()
                toast('info', 'Prompt mining stopped. Jobs already committed stay in the outbox for the next start.')
                await refresh()
              })
            }
          >
            Stop
          </button>
        ) : (
          <button
            type="button"
            className="btn-primary"
            disabled={dirty || busy !== null || planned === null || !saved.accept_unverified}
            title={dirty ? 'Save first' : (plannedError ?? (!saved.accept_unverified ? 'Accept signing on a public node first' : undefined))}
            onClick={() => run('start', async () => setView(await api.startPromptStack()))}
          >
            {busy === 'start' ? <Spinner /> : 'Start prompt mining'}
          </button>
        )}
      </div>
      {error && <p className="mt-2 text-xs text-red-600 dark:text-red-400">{error}</p>}
      {plannedError && !running && <p className="mt-2 text-xs text-ink-500 dark:text-ink-400">Not ready: {plannedError}</p>}

      {preflight && (
        <div className="mt-3 rounded-md border border-ink-200 p-2 text-[0.7rem] dark:border-ink-800">
          <div>
            Public node: {preflight.public_node?.reachable ? `${preflight.public_node.network} DAA ${preflight.public_node.virtual_daa_score}` : 'unreachable'} · evidence provider:{' '}
            {preflight.evidence_provider_reachable ? 'reachable' : 'unreachable'}
          </div>
          {preflight.problems.length === 0 && <div className="mt-1 text-emerald-700 dark:text-emerald-300">Everything checkable from here is in place.</div>}
          {preflight.problems.map((p) => (
            <div key={p} className="mt-1 text-amber-700 dark:text-amber-300">
              • {p}
            </div>
          ))}
        </div>
      )}

      {shown.length > 0 && (
        <div className="mt-3 space-y-2">
          {shown.map((cmd) => (
            <div key={cmd.join(' ')}>
              <div className="flex items-center justify-between text-xs font-medium text-ink-600 dark:text-ink-300">
                <span>{running ? 'Running' : 'Will run'}: {(cmd[0] ?? '').split('/').pop()}</span>
                <CopyButton text={cmd.join(' ')} />
              </div>
              <pre className="mono mt-1 max-h-32 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink-50 p-2 text-[0.65rem] dark:bg-ink-900">{cmd.join(' ')}</pre>
            </div>
          ))}
        </div>
      )}

      {view && (view.carriers.length > 0 || view.rail_log.length > 0 || view.gateway_log.length > 0) && (
        <div className="mt-3 space-y-2 text-xs">
          {view.carriers.length > 0 && (
            <div>
              <div className="font-medium">Carriers the rail built ({view.carriers.length})</div>
              {view.carriers.slice(-5).map((c, i) => (
                <div key={i} className="mono text-[0.65rem] text-ink-500 dark:text-ink-400">
                  claim {String(c.fp_claim_id ?? '').slice(0, 16)}… · fee {String(c.fee_sompi ?? '—')} · relayed {JSON.stringify(c.relayed ?? null)}
                </div>
              ))}
            </div>
          )}
          {view.gateway_log.length > 0 && (
            <pre className="mono max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink-900 p-2 text-[0.65rem] text-ink-200">{view.gateway_log.join('\n')}</pre>
          )}
          {view.rail_log.length > 0 && (
            <pre className="mono max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink-900 p-2 text-[0.65rem] text-ink-200">{view.rail_log.join('\n')}</pre>
          )}
        </div>
      )}
    </section>
  )
}
