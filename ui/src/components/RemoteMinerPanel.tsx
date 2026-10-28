// Mining with no node here (RFC-0009 stage A): the Studio runs `palw-remote-miner` against the network's public wRPC
// nodes, with the official executor `palw-attempt-exec` on this machine.
//
// The panel follows the Network tab's rule: everything the button does is a visible command line. Two values are the
// user's and never the Studio's: the checkpoint (the trust root the nodes are held to — the Studio only suggests one it
// read from two nodes) and the consent to mine on a view that is not fully verified, which on testnet-12 is the best a
// node-free miner can reach while the fork-choice fence is dormant.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import { shortHash } from '../lib/format'
import type { CheckpointSuggestion, RemoteMinerPreflight, RemoteMinerSettings, RemoteMinerView, Settings } from '../lib/types'
import { useStudio } from '../store/studio'
import { CopyButton, Field, Section, Spinner, Toggle } from './common'

const DEFAULTS: RemoteMinerSettings = {
  remote_miner_path: null,
  attempt_exec_path: null,
  rpc: [],
  bond: null,
  class: null,
  artifact: null,
  pay_address: null,
  key_path: null,
  checkpoint: null,
  state_dir: null,
  verify_headers: true,
  accept_unverified: null,
  poll_secs: null,
}

const FLOOR = ''
const CLASS_8K =
  'ebf44d0aa09ff7d1310a7855ab4005c275cdce557e32c269b0f3a984ea80ca73ad1ea0c9b1c0539c8ae04abb5fe24399e67e05bb0895a3dee82253e772246d01'

export function RemoteMinerPanel({ settings, save }: { settings: Settings; save: (s: Settings) => Promise<void> }) {
  const toast = useStudio((s) => s.toast)
  const saved = { ...DEFAULTS, ...(settings.node.remote ?? {}) }
  const [draft, setDraft] = useState<RemoteMinerSettings>(saved)
  const [view, setView] = useState<RemoteMinerView | null>(null)
  const [preflight, setPreflight] = useState<RemoteMinerPreflight | null>(null)
  const [suggestion, setSuggestion] = useState<CheckpointSuggestion | null>(null)
  const [log, setLog] = useState<string[]>([])
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const savedKey = JSON.stringify(saved)
  useEffect(() => setDraft(JSON.parse(savedKey)), [savedKey])
  const dirty = JSON.stringify(draft) !== savedKey

  const set = <K extends keyof RemoteMinerSettings>(key: K, value: RemoteMinerSettings[K]) => setDraft({ ...draft, [key]: value })
  const text = (value: string) => (value.trim() === '' ? null : value.trim())

  const refresh = useCallback(async () => {
    try {
      const v = await api.remoteMiner()
      setView(v)
      if (v.running || v.command_line) setLog(await api.remoteMinerLog(80))
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

  const persist = () => run('save', async () => save({ ...settings, node: { ...settings.node, remote: draft } }))

  const running = view?.running ?? false
  const planned = view && 'Ok' in view.planned ? view.planned.Ok : null
  const plannedError = view && 'Err' in view.planned ? view.planned.Err : null
  const facts = view?.facts

  return (
    <Section
      title="Node-free mining"
      description="Mine testnet-12 without running kaspad here: palw-remote-miner reads the chain through two public wRPC nodes, runs palw-attempt-exec on this machine, signs only a won draw and submits it to both nodes. The nodes validate the block themselves."
    >
      <div className="rounded-lg border border-amber-300/60 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-500/30 dark:bg-amber-950/40 dark:text-amber-200">
        Every fact read from the nodes is <b>UNVERIFIED_REMOTE_STATE</b> until a proof opens it. The fork-choice fence is dormant on testnet-12, so the best view is
        <b> HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED</b>. Each published attempt's capture is kept in the state dir:{' '}
        <b>keep this machine on and serving it until the claim is Final.</b>
      </div>

      <div className="mt-3 grid gap-2 sm:grid-cols-2">
        {(view?.public_wrpc ?? []).map((n) => (
          <div key={n.name} className="rounded-md bg-ink-50 p-2 text-[0.7rem] dark:bg-ink-900">
            <div className="font-medium text-ink-700 dark:text-ink-200">{n.name}</div>
            <div className="break-all font-mono text-ink-500 dark:text-ink-400">{n.wrpc_borsh}</div>
          </div>
        ))}
      </div>

      <Field label="Bond" hint="The registered bond outpoint, <txid>:<index>. Register one without a node: misaka bond register-export / sign / submit, relayed through the public wRPC.">
        <input className="input mt-1" placeholder="<txid>:<index>" value={draft.bond ?? ''} onChange={(e) => set('bond', text(e.target.value))} />
      </Field>
      <Field label="Reward address" hint="Where the attempt pays. The miner refuses a template that pays anyone else.">
        <input className="input mt-1" placeholder="misakatest:…" value={draft.pay_address ?? ''} onChange={(e) => set('pay_address', text(e.target.value))} />
      </Field>
      <Field label="Key file" hint="The bond's ML-DSA-87 seed. Empty uses the bond key file from Network and key. The Studio passes the path and never reads the file.">
        <input className="input mt-1" placeholder={settings.node.producer_key_path ?? '~/.misaka/miner.seed'} value={draft.key_path ?? ''} onChange={(e) => set('key_path', text(e.target.value))} />
      </Field>
      <Field label="Class">
        <select className="input mt-1" value={draft.class ?? FLOOR} onChange={(e) => set('class', e.target.value === FLOOR ? null : e.target.value)}>
          <option value={FLOOR}>PALW-BASE-0 — the floor, no artifact</option>
          <option value={CLASS_8K}>PALW-QWEN25-A16-8K — Qwen2.5-1.5B, 8k context (artifact required)</option>
        </select>
      </Field>
      {draft.class && (
        <Field label="Artifact" hint="qwen25-1.5b-a16-8k.palwart, the same container the public producer holds (Models → Discover).">
          <input className="input mt-1" placeholder="/path/qwen25-1.5b-a16-8k.palwart" value={draft.artifact ?? ''} onChange={(e) => set('artifact', text(e.target.value))} />
        </Field>
      )}
      <Field label="Checkpoint" hint="<daa>:<block hash> — the trust root every node is held to. Suggest reads one node's sink and asks the other for it; compare it with a source you trust before saving.">
        <div className="mt-1 flex gap-2">
          <input className="input flex-1 font-mono" placeholder="15540:8564c53f…" value={draft.checkpoint ?? ''} onChange={(e) => set('checkpoint', text(e.target.value))} />
          <button
            type="button"
            className="btn-outline whitespace-nowrap"
            disabled={busy !== null}
            onClick={() => run('checkpoint', async () => setSuggestion(await api.remoteMinerCheckpoint()))}
          >
            {busy === 'checkpoint' ? <Spinner /> : 'Suggest'}
          </button>
        </div>
      </Field>
      {suggestion && (
        <div className="mt-2 rounded-md bg-ink-50 p-2 text-[0.7rem] text-ink-600 dark:bg-ink-900 dark:text-ink-300">
          <div>
            DAA {suggestion.daa_score}, block <span className="font-mono">{shortHash(suggestion.block_hash)}</span> — read from {suggestion.read_from}, held by{' '}
            {suggestion.confirmed_by.join(', ')}.
          </div>
          <div className="mt-1">{suggestion.note}</div>
          <button type="button" className="btn-outline mt-2" onClick={() => set('checkpoint', suggestion.checkpoint)}>
            Use this checkpoint
          </button>
        </div>
      )}
      <Toggle
        label="Verify the header chain from the checkpoint"
        hint="L1 headers and L3 state proofs from your checkpoint. Without it the view is the quorum's word alone (UNVERIFIED_REMOTE)."
        checked={draft.verify_headers}
        onChange={(v) => set('verify_headers', v)}
      />
      <Toggle
        label="I accept mining on a view whose fork choice is not verified"
        hint={
          draft.verify_headers
            ? 'Passes --accept-unverified-state HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED. Without it the miner stops at its signing gate on testnet-12.'
            : 'Passes --accept-unverified-state UNVERIFIED_REMOTE: you sign on the nodes’ word alone.'
        }
        checked={draft.accept_unverified !== null}
        onChange={(v) => set('accept_unverified', v ? (draft.verify_headers ? 'HEADER_VERIFIED_FORK_CHOICE_UNVERIFIED' : 'UNVERIFIED_REMOTE') : null)}
      />
      <Field label="Binaries" hint="Empty looks beside the Studio and on PATH. Build in misakas main: cargo build --release -p kaspad --bin palw-attempt-exec -p misaka-palw-remote --features rpc --bin palw-remote-miner">
        <div className="mt-1 grid gap-2 sm:grid-cols-2">
          <input className="input" placeholder="palw-remote-miner" value={draft.remote_miner_path ?? ''} onChange={(e) => set('remote_miner_path', text(e.target.value))} />
          <input className="input" placeholder="palw-attempt-exec" value={draft.attempt_exec_path ?? ''} onChange={(e) => set('attempt_exec_path', text(e.target.value))} />
        </div>
      </Field>

      <div className="mt-3 flex flex-wrap gap-2">
        <button type="button" className="btn-primary" disabled={!dirty || busy !== null} onClick={persist}>
          {busy === 'save' ? <Spinner /> : 'Save'}
        </button>
        <button type="button" className="btn-outline" disabled={busy !== null} onClick={() => run('preflight', async () => setPreflight(await api.remoteMinerPreflight()))}>
          {busy === 'preflight' ? <Spinner /> : 'Check'}
        </button>
        {running ? (
          <button
            type="button"
            className="btn-outline"
            disabled={busy !== null}
            onClick={() =>
              run('stop', async () => {
                await api.stopRemoteMiner()
                toast('info', 'Node-free miner stopped. Keep its state dir: the Panel still reads the captures from it.')
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
            disabled={dirty || busy !== null || planned === null}
            title={dirty ? 'Save first' : (plannedError ?? undefined)}
            onClick={() =>
              run('start', async () => {
                setView(await api.startRemoteMiner())
              })
            }
          >
            {busy === 'start' ? <Spinner /> : 'Start mining'}
          </button>
        )}
      </div>
      {error && <p className="mt-2 text-xs text-red-600 dark:text-red-400">{error}</p>}
      {plannedError && !running && <p className="mt-2 text-xs text-ink-500 dark:text-ink-400">Not ready: {plannedError}</p>}

      {preflight && (
        <div className="mt-3 rounded-md border border-ink-200 p-2 text-[0.7rem] dark:border-ink-800">
          <div className="font-medium">
            Quorum: {preflight.quorum ? 'two nodes agree' : 'not reached'} <span className="text-ink-400">({preflight.label})</span>
          </div>
          {preflight.nodes.map((n) => (
            <div key={n.endpoint} className="font-mono text-ink-500 dark:text-ink-400">
              {n.reachable ? `${n.network} DAA ${n.virtual_daa_score} sink ${shortHash(n.sink ?? '')}` : `unreachable: ${n.error}`} — {n.endpoint}
            </div>
          ))}
          {preflight.problems.map((p) => (
            <div key={p} className="mt-1 text-amber-700 dark:text-amber-300">
              • {p}
            </div>
          ))}
        </div>
      )}

      {(view?.command_line ?? planned) && (
        <div className="mt-3">
          <div className="flex items-center justify-between text-xs font-medium text-ink-600 dark:text-ink-300">
            <span>{view?.command_line ? 'Running command' : 'Command'}</span>
            <CopyButton text={(view?.command_line ?? planned ?? []).join(' ')} />
          </div>
          <pre className="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink-50 p-2 font-mono text-[0.65rem] dark:bg-ink-900">
            {(view?.command_line ?? planned ?? []).join(' ')}
          </pre>
          <div className="mt-1 text-[0.7rem] text-ink-500 dark:text-ink-400">State dir (captures): {view?.state_dir}</div>
        </div>
      )}

      {facts && (view?.command_line || running) && (
        <div className="mt-3 grid gap-1 text-xs">
          <div>
            {running ? 'Running' : `Stopped${view?.exited ? ` (${view.exited})` : ''}`} · mode {facts.mode ?? '—'} · draws lost {facts.draws_lost} · published {facts.published} · halts {facts.halts}
          </div>
          {facts.last_halt && <div className="text-amber-700 dark:text-amber-300">Last halt: {facts.last_halt}</div>}
          {facts.last_fatal && <div className="text-red-600 dark:text-red-400">Fatal: {facts.last_fatal}</div>}
          {facts.blocks.length > 0 && (
            <div className="font-mono text-[0.7rem]">Published: {facts.blocks.slice(-5).map((b) => shortHash(b)).join(', ')} (an ACK is not inclusion)</div>
          )}
          {log.length > 0 && (
            <pre className="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md bg-ink-50 p-2 font-mono text-[0.65rem] dark:bg-ink-900">{log.join('\n')}</pre>
          )}
        </div>
      )}
    </Section>
  )
}
