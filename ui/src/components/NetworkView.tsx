// The Network tab: joining the MISAKA network, as a ladder the user can see themselves on.
//
// Three ideas organise it:
//
// * **The class list is the headline.** "What can this machine mine?" is the question that brings
//   people here, and each chain-registered class answers with its share, its artifact requirement,
//   and this machine's readiness — the same fit-first UX the model list has.
// * **Roles are rungs, and each states its real prerequisites.** Observing needs a reachable
//   node. Verifying needs a running node — on this chain, syncing IS verifying. Producing needs a
//   bonded key, and the panel explains the bond instead of pretending a button can conjure one.
// * **Everything a button does is a visible command line.** A person putting a bonded key on the
//   line gets to read the exact flags before anything runs, and can reproduce them without the
//   Studio afterwards.

import { useCallback, useEffect, useRef, useState } from 'react'
import { ModelMarketPanel } from './ModelMarketPanel'
import { MiningQueuePanel } from './MiningQueuePanel'
import { PromptMiningPanel } from './PromptMiningPanel'
import { RemoteMinerPanel } from './RemoteMinerPanel'
import { PromptStackPanel } from './PromptStackPanel'
import { api } from '../lib/api'
import { bytes, count } from '../lib/format'
import type { NetworkOverview, NodeClassRow, NodeStatus, PalwClassStatus, Settings } from '../lib/types'
import { useStudio } from '../store/studio'
import { CopyButton, EmptyState, Field, Icon, Section, Spinner } from './common'

export function NetworkView() {
  const [overview, setOverview] = useState<NetworkOverview | null>(null)
  const [error, setError] = useState<string | null>(null)
  const toast = useStudio((s) => s.toast)
  const settings = useStudio((s) => s.settings)
  const saveSettings = useStudio((s) => s.saveSettings)
  const setDownload = useStudio((s) => s.setDownload)
  const timer = useRef<ReturnType<typeof setInterval> | null>(null)
  // One read at a time. The overview probes the public nodes and can take longer than the poll
  // interval (measured: 10.8 s with no node reachable), and overlapping reads piled up until they
  // held every connection the browser allows to this host — after which the chat's own requests
  // queued behind them and the whole window looked hung.
  const reading = useRef(false)

  const refresh = useCallback(async () => {
    if (reading.current) return
    reading.current = true
    try {
      setOverview(await api.network())
      setError(null)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      reading.current = false
    }
  }, [])

  // Poll while the tab is open: the chain moves on its own, and a static snapshot of it is stale by definition.
  useEffect(() => {
    void refresh()
    timer.current = setInterval(() => void refresh(), 10_000)
    return () => {
      if (timer.current) clearInterval(timer.current)
    }
  }, [refresh])

  const downloadArtifact = async (name: string) => {
    try {
      const progress = await api.downloadClassArtifact(name)
      setDownload(progress)
      toast('info', `Downloading ${progress.file} — verified against the chain-pinned digest when it lands`)
    } catch (e) {
      toast('error', (e as Error).message)
    }
  }

  if (!overview) {
    return (
      <EmptyState icon="globe" title="Reading the network state">
        {error ?? 'One moment…'}
      </EmptyState>
    )
  }

  return (
    <div className="h-full overflow-y-auto p-4">
      <div className="grid gap-4 xl:grid-cols-3">
        <div className="space-y-4 xl:col-span-2">
          {settings && <PromptStackPanel settings={settings} save={saveSettings} />}
          <PublicChainPanel nodes={overview.public_nodes} />

          <section className="card p-5">
            <div className="flex items-baseline justify-between">
              <h3 className="text-sm font-semibold">Mining classes</h3>
              <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">testnet-12 genesis registry</span>
            </div>
            <p className="mt-1 text-xs text-ink-500 dark:text-ink-400">
              A prompt is mined by verified inference in one of these chain-registered classes. The model classes need their
              converted artifact, and the worker refuses any file that does not verify to the registered root.
            </p>
            <div className="mt-4 space-y-3">
              {overview.classes.map((cls) => (
                <ClassCard key={cls.spec.name} cls={cls} nodeRows={[]} onDownload={downloadArtifact} />
              ))}
            </div>
          </section>
        </div>

        <div className="space-y-4">
          <PromptMiningPanel />
          <MiningQueuePanel />
          <ModelMarketPanel />
          {settings && <RemoteMinerPanel settings={settings} save={saveSettings} />}
          {settings && <NetworkSettingsPanel settings={settings} save={saveSettings} />}
        </div>
      </div>
    </div>
  )
}

/** The chain as the public nodes report it. One node's word apiece — never a proof. */
function PublicChainPanel({ nodes }: { nodes: NodeStatus[] }) {
  return (
    <section className="card p-5">
      <div className="flex items-baseline justify-between">
        <h3 className="text-sm font-semibold">The chain, from its public nodes</h3>
        <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">UNVERIFIED_REMOTE_STATE</span>
      </div>
      <p className="mt-1 text-xs text-ink-500 dark:text-ink-400">
        This Studio runs no node. It reads the chain through the network&apos;s public wRPC nodes — the same ones the gateway, the
        rail and the remote miner use.
      </p>
      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        {nodes.map((n) => (
          <div key={n.rpc_url} className="rounded-lg border border-ink-200 p-3 text-xs dark:border-ink-800">
            <div className="flex items-center justify-between">
              <span className="font-medium">{n.source}</span>
              <span className={n.reachable ? 'text-emerald-600 dark:text-emerald-400' : 'text-red-600 dark:text-red-400'}>
                {n.reachable ? (n.is_synced ? 'synced' : 'syncing') : 'unreachable'}
              </span>
            </div>
            <div className="mono mt-1 break-all text-[0.65rem] text-ink-500 dark:text-ink-400">{n.rpc_url}</div>
            {n.reachable ? (
              <dl className="mt-2 grid grid-cols-2 gap-2">
                <StatRow label="DAA score" value={n.virtual_daa_score !== null ? count(n.virtual_daa_score) : '—'} />
                <StatRow label="Network" value={n.network ?? '—'} />
              </dl>
            ) : (
              <p className="mt-2 text-red-600 dark:text-red-400">{n.error}</p>
            )}
            {n.reachable && <ChainClock status={n} />}
          </div>
        ))}
        {nodes.length === 0 && <p className="text-xs text-ink-500">This network publishes no public wRPC.</p>}
      </div>
    </section>
  )
}

/** The network, the bond key and the endpoint override — everything node-free mining needs that is not per-panel. */
function NetworkSettingsPanel({ settings, save }: { settings: Settings; save: (s: Settings) => Promise<void> }) {
  const [draft, setDraft] = useState(settings.node)
  const [keyBusy, setKeyBusy] = useState(false)
  const [keyError, setKeyError] = useState<string | null>(null)
  useEffect(() => setDraft(settings.node), [settings.node])
  const dirty = JSON.stringify(draft) !== JSON.stringify(settings.node)
  const set = <K extends keyof Settings['node']>(key: K, value: Settings['node'][K]) => setDraft({ ...draft, [key]: value })
  const text = (value: string) => (value.trim() === '' ? null : value.trim())

  return (
    <Section title="Network and key" description="The Studio mines without a node: these settings are shared by prompt mining and the remote miner.">
      <Field label="Network">
        <select className="input mt-1" value={draft.network} onChange={(e) => set('network', e.target.value as Settings['node']['network'])}>
          <option value="testnet12">testnet-12 — the live PALW network</option>
        </select>
      </Field>
      <Field label="Bond key file" hint="32-byte ML-DSA-87 seed. Mint one here (written 0600 under the Studio's data directory, never shown), or bring your own (`misaka key gen`). The rail and the remote miner read it; the Studio passes the path.">
        <div className="mt-1 flex gap-2">
          <input className="input flex-1" placeholder="~/.misaka/miner.seed" value={draft.producer_key_path ?? ''} onChange={(e) => set('producer_key_path', text(e.target.value))} />
          <button
            type="button"
            className="btn-outline whitespace-nowrap"
            disabled={keyBusy}
            onClick={async () => {
              setKeyBusy(true)
              setKeyError(null)
              try {
                const r = await api.producerKey()
                set('producer_key_path', r.producer_key_path)
              } catch (e) {
                setKeyError(e instanceof Error ? e.message : String(e))
              } finally {
                setKeyBusy(false)
              }
            }}
          >
            {keyBusy ? <Spinner /> : 'Mint'}
          </button>
        </div>
        {keyError && <p className="mt-1 text-[0.7rem] text-red-600 dark:text-red-400">{keyError}</p>}
      </Field>
      <Field label="wRPC endpoint" hint="Empty uses the network's first public node. The CLI, the gateway and the rail all use it.">
        <input className="input mt-1" placeholder="wss://wallet.misakascan.com/t12/wrpc/borsh" value={draft.misaka_rpc ?? ''} onChange={(e) => set('misaka_rpc', text(e.target.value))} />
      </Field>
      <Field label="misaka CLI path" hint="Empty looks beside the Studio and on PATH.">
        <input className="input mt-1" placeholder="misaka" value={draft.misaka_cli_path ?? ''} onChange={(e) => set('misaka_cli_path', text(e.target.value))} />
      </Field>
      <div className="mt-3">
        <button type="button" className="btn-primary" disabled={!dirty} onClick={() => void save({ ...settings, node: draft })}>
          Save
        </button>
      </div>
    </Section>
  )
}


/** How long since the newest block, and whether a still chain right now is normal. */
function ChainClock({ status }: { status: NodeStatus }) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000)
    return () => clearInterval(id)
  }, [])
  if (status.sink_timestamp_ms === null) return null
  const age = Math.max(0, Math.round((now - status.sink_timestamp_ms) / 1000))
  const ago = age < 90 ? `${age}s` : age < 5400 ? `${Math.round(age / 60)} min` : `${(age / 3600).toFixed(1)} h`
  const standDown = status.sink_stand_down_secs
  // The heartbeat lane is the chain's clock and it runs at two speeds. After a BONDED block it
  // stands down for an hour, so the score can sit unchanged while the network is perfectly
  // healthy — the state this node was in for most of an hour after it won its first block, with
  // nothing on screen to say so.
  const quietByDesign = standDown !== null && standDown > 600
  return (
    <div className="mt-2 rounded-lg bg-ink-100 p-2 text-[0.7rem] dark:bg-ink-800/60">
      <span className="text-ink-700 dark:text-ink-200">Newest block {ago} ago</span>
      {quietByDesign && (
        <span className="text-ink-500 dark:text-ink-400">
          {' '}— and it is a block someone won, so the heartbeat lane stands down for an hour. Until then the
          chain only moves when a block is won: a still score here is the network producing, not a stalled node.
        </span>
      )}
      {standDown !== null && !quietByDesign && (
        <span className="text-ink-500 dark:text-ink-400">
          {' '}— the newest block is a heartbeat, so the chain is running on its own clock at {standDown}s.
        </span>
      )}
    </div>
  )
}

function StatRow({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="text-[0.65rem] uppercase tracking-wide text-ink-500 dark:text-ink-400">{label}</dt>
      <dd className="mt-0.5 font-medium tabular-nums">{value}</dd>
    </div>
  )
}

/** Does this node row belong to that built-in spec? The one place the rule is written.
 *
 * The floor is matched by its base flag — every ConsensusV2 chain has exactly one, and its id is
 * chain-specific (a locally minted chain's floor differs from live testnet-11's). Everything else
 * matches by id prefix: the node prints the full id, the snapshot may only know a prefix. A row no
 * spec claims is a class registered after genesis, and gets a card of its own.
 */
function matchesSpec(spec: PalwClassStatus['spec'], row: NodeClassRow): boolean {
  return spec.is_base ? row.base : spec.class_id_hex ? row.class_id.startsWith(spec.class_id_hex.slice(0, 16)) : false
}

function ClassCard({
  cls,
  nodeRows,
  onDownload,
}: {
  cls: PalwClassStatus
  nodeRows: NodeClassRow[]
  onDownload: (name: string) => void
}) {
  const spec = cls.spec
  // The node's own dump line for this class. The floor is matched by its base flag — every
  // ConsensusV2 chain has exactly one, and its id is chain-specific (a locally minted chain's
  // floor differs from live testnet-11's). Everything else matches by id prefix: the node
  // prints the full id, the snapshot may only know a prefix.
  const live = nodeRows.find((row) => matchesSpec(spec, row))

  const readiness = cls.readiness
  const badge =
    readiness.state === 'ready_built_in' ? (
      <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">ready — nothing to download</span>
    ) : readiness.state === 'artifact_present' ? (
      <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">
        artifact present{readiness.verified ? ' · verified' : ''}
      </span>
    ) : readiness.state === 'artifact_mismatch' ? (
      <span className="badge bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-300">wrong size on disk</span>
    ) : readiness.downloadable ? (
      <span className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">artifact not downloaded</span>
    ) : (
      <span className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">convert locally</span>
    )

  return (
    <div className="rounded-xl border border-ink-200 p-4 dark:border-ink-800">
      <div className="flex flex-wrap items-center gap-2">
        <h4 className="mono text-sm font-semibold">{spec.name}</h4>
        <span className="badge bg-arc-500/15 text-arc-700 dark:text-arc-300" title="Fraction of finalized work this class provided (ADR-0137). A result, not a lottery input and not an epoch budget.">
          {live?.share_permille !== null && live?.share_permille !== undefined
            ? `${live.share_permille}‰ of Final work`
            : 'share from Finals'}
        </span>
        {spec.is_base && <span className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300">floor · residual cadence, unpaid</span>}
        {badge}
        {live && (
          <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300" title="From the connected node's own class table">
            on chain: {live.status}
            {live.share_permille !== null && ` · ${live.share_permille}‰`}
          </span>
        )}
      </div>

      <p className="mt-2 text-xs leading-relaxed text-ink-600 dark:text-ink-300">{spec.description}</p>
      <p className="mt-2 rounded-lg bg-ink-50 p-2 text-[0.7rem] leading-relaxed text-ink-500 dark:bg-ink-900/60 dark:text-ink-400">
        {spec.is_base
          ? 'The floor fills whatever cadence the model classes leave and is paid nothing for it — residual liveness, not a share grant.'
          : "A block buys one unit of work from any model. This class's ticket is its compute over the work target; a winning inference can make one block. Share is the Final work it provided, not a budget on how many blocks it may win."}
        {' '}The Explorer keeps claim and block counts separate.
      </p>

      <div className="mt-2 flex flex-wrap gap-x-4 gap-y-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
        {spec.artifact.kind === 'download' && (
          <>
            <span className="mono">{spec.artifact.filename}</span>
            <span>{bytes(spec.artifact.size_bytes)}</span>
            <span className="mono" title="SHA-256 the download is verified against">
              sha256 {spec.artifact.sha256.slice(0, 12)}…
            </span>
          </>
        )}
        {spec.artifact.kind === 'convert_locally' && (
          <>
            <span className="mono">{spec.artifact.filename}</span>
            <span>~{bytes(spec.artifact.approx_size_bytes)}</span>
            <span>
              from <span className="mono">{spec.artifact.source_repo}</span>
            </span>
          </>
        )}
        {spec.class_id_hex && (
          <span className="mono" title={spec.class_id_complete ? 'class id' : 'class id (documented prefix; the node prints it in full)'}>
            class {spec.class_id_hex.slice(0, 12)}…{spec.class_id_complete ? '' : ' (prefix)'}
          </span>
        )}
      </div>

      {cls.memory_note && (
        <p className="mt-2 flex gap-2 rounded-lg bg-amber-50 p-2 text-[0.7rem] text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
          {cls.memory_note}
        </p>
      )}

      <div className="mt-3 flex flex-wrap gap-2">
        {spec.artifact.kind === 'download' && (
          <button type="button" className={cls.memory_note ? 'btn-ghost' : 'btn-outline'} onClick={() => onDownload(spec.name)}>
            <Icon name="download" className="size-3.5" />
            {`Install anyway — ${bytes(spec.artifact.size_bytes)}`}
          </button>
        )}
        {spec.artifact.kind === 'convert_locally' && readiness.state === 'artifact_missing' && (
          <div className="flex items-center gap-1">
            <code className="mono rounded bg-ink-100 px-2 py-1 text-[0.65rem] dark:bg-ink-800">{spec.artifact.convert_command}</code>
            <CopyButton text={spec.artifact.convert_command} label="Copy conversion command" />
          </div>
        )}
      </div>
    </div>
  )
}
