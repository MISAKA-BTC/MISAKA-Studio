// The mining list, at the top of Discover.
//
// Discover answers "what can I install?", and on this network that question has two halves that
// look alike and are not:
//
// * Any GGUF on Hugging Face — a model to **talk to**. Unbounded, so it is a search box.
// * The chain-registered execution classes — the models this network **pays for**. A short fixed
//   list, so it can be shown in full, and it belongs above the search precisely because nobody
//   can guess these repository names.
//
// Both install from Hugging Face and both land in the models directory; what separates them is
// that a class artifact is pinned. The digest here comes from the chain's registry, the download
// is verified against it before the file counts as installed, and the node re-derives the
// registered root at startup and refuses a mismatch. So "installed" in this list means the same
// thing the node means by it — which is the only reason it is worth printing.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import { bytes } from '../lib/format'
import { NETWORK_LABEL } from '../lib/types'
import type { NodeClassRow, NodeNetwork, PalwArtifactHeader, PalwClassStatus } from '../lib/types'
import { useStudio } from '../store/studio'
import { CopyButton, Icon, Spinner } from './common'
import { ClassContext } from './ContextBadge'

/**
 * The model class the runtime points a node at when its file is present — `palw::default_class_for`,
 * repeated here only to put a badge on it. The decision is the runtime's and does not consult this.
 */
const DEFAULT_CLASSES = ['PALW-QWEN25-A16-8K', 'PALW-QWEN25-A16']
const isDefaultClass = (name: string) => DEFAULT_CLASSES.includes(name)

/** `https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct`, honouring an `HF_ENDPOINT` mirror. */
function repoUrl(endpoint: string | undefined, repo: string): string {
  const base = endpoint && /^https?:\/\//.test(endpoint) ? endpoint.replace(/\/+$/, '') : 'https://huggingface.co'
  return `${base}/${repo}`
}

/** The node's dump row for this spec, if a node is up. Floor by base flag; others by class-id prefix. */
function matchingNodeRow(spec: PalwClassStatus['spec'], rows: NodeClassRow[]): NodeClassRow | undefined {
  return rows.find((row) => (spec.is_base ? row.base : spec.class_id_hex ? row.class_id.startsWith(spec.class_id_hex.slice(0, 16)) : false))
}

/**
 * Live share is Final work (ADR-0137 D7), not the genesis permille table.
 *
 * The dump's `share=` is what the node currently prints for the class. When nothing is connected
 * we do not fall back to the Relaunch 5f grant — that number is a lottery input the work target
 * retired.
 */
function FinalWorkShare({ spec, rows }: { spec: PalwClassStatus['spec']; rows: NodeClassRow[] }) {
  const live = matchingNodeRow(spec, rows)?.share_permille ?? null
  if (live !== null) {
    return (
      <span
        className="badge bg-arc-500/15 text-arc-700 dark:text-arc-300"
        title="Fraction of finalized work this class provided in the reader's window (ADR-0137). A result, not a lottery input and not an epoch budget."
      >
        {live}‰ of Final work
      </span>
    )
  }
  return (
    <span
      className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300"
      title="Share is still reported: the fraction of Final work this class provided. A node prints it; the genesis permille table is not it."
    >
      share from Finals
    </span>
  )
}

/**
 * The class list, re-read whenever a download settles.
 *
 * An artifact that just landed must stop saying "not installed" on its own: the alternative is a
 * list that stays wrong until someone reloads the window, which is exactly the moment they would
 * conclude the download had failed.
 */
export function useClassStatuses(): {
  classes: PalwClassStatus[] | null
  nodeRows: NodeClassRow[]
  network: NodeNetwork | null
  error: string | null
} {
  const [classes, setClasses] = useState<PalwClassStatus[] | null>(null)
  const [network, setNetwork] = useState<NodeNetwork | null>(null)
  const [nodeRows, setNodeRows] = useState<NodeClassRow[]>([])
  const [error, setError] = useState<string | null>(null)
  const downloads = useStudio((s) => s.downloads)
  const settled = downloads.filter((d) => d.status === 'completed' || d.status === 'failed' || d.status === 'cancelled').length

  const refresh = useCallback(async () => {
    try {
      setClasses(await api.networkClasses())
      setError(null)
    } catch (e) {
      setError((e as Error).message)
    }
    try {
      const overview = await api.network()
      setNodeRows(overview.node.classes_from_node)
      setNetwork(overview.network)
    } catch {
      setNodeRows([])
    }
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh, settled])

  return { classes, nodeRows, network, error }
}

/**
 * The class artifacts that are actually on this machine, for the Installed tab.
 *
 * They live in the models directory beside the GGUFs and are invisible to the model scanner —
 * different extension, different runtime, not something you can chat with. Without this, a 34 GiB
 * file could sit on disk with nothing in the app willing to admit it was there.
 */
export function InstalledMiningArtifacts() {
  const { classes, nodeRows } = useClassStatuses()
  const held = (classes ?? []).filter((c) => c.readiness.state === 'artifact_present' || c.readiness.state === 'artifact_mismatch')
  if (held.length === 0) return null

  return (
    <section className="card m-4 mb-0 p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-sm font-semibold">Mining class artifacts</h3>
        <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">not chat models — these produce blocks</span>
      </div>
      <div className="mt-3 space-y-2">
        {held.map((cls) => {
          const { readiness } = cls
          const path = readiness.state === 'artifact_present' || readiness.state === 'artifact_mismatch' ? readiness.path : null
          const size = readiness.state === 'artifact_present' || readiness.state === 'artifact_mismatch' ? readiness.size_bytes : null
          return (
            <div key={cls.spec.name} className="rounded-xl border border-ink-200 p-3 dark:border-ink-800">
              <div className="flex flex-wrap items-center gap-2">
                <h4 className="mono text-sm font-semibold">{cls.spec.name}</h4>
                <FinalWorkShare spec={cls.spec} rows={nodeRows} />
                {isDefaultClass(cls.spec.name) && <span className="badge bg-arc-600 text-white">default class</span>}
                <ClassContext registered={cls.spec.context_tokens} header={cls.artifact_header} />
                {readiness.state === 'artifact_present' ? (
                  <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">on disk</span>
                ) : (
                  <span className="badge bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-300">wrong size on disk</span>
                )}
              </div>
              <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
                <span>{bytes(size)}</span>
                <span>registered at {cls.spec.context_tokens.toLocaleString()} tokens</span>
                {cls.artifact_header && <FileShape header={cls.artifact_header} />}
                <span className="mono truncate">{path}</span>
              </div>
              {/* Presence is a filename, not an identity. The node re-derives the registered root
                  at startup and refuses a mismatch, so this list stops short of calling a file
                  verified — that word belongs to the check that actually ran. */}
              <p className="mt-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
                {readiness.state !== 'artifact_present'
                  ? 'A truncated download or a different conversion. Delete it and install again; the node would refuse this file at startup.'
                  : isDefaultClass(cls.spec.name)
                    ? 'The default class: starting the node as a producer mines this without any further configuration. The node verifies the registered root at startup — a file that does not match is refused there, not here.'
                    : 'Name this path as the class artifact in Network settings to mine this class instead. The node verifies the registered root at startup — a file that does not match is refused there, not here.'}
              </p>
            </div>
          )
        })}
      </div>
    </section>
  )
}

export function MiningCatalog() {
  const { classes, nodeRows, network, error } = useClassStatuses()
  const toast = useStudio((s) => s.toast)
  const setDownload = useStudio((s) => s.setDownload)

  const install = async (name: string) => {
    try {
      const progress = await api.downloadClassArtifact(name)
      setDownload(progress)
      toast('info', `Downloading ${progress.file} — verified against the chain-pinned digest when it lands`)
    } catch (e) {
      toast('error', (e as Error).message)
    }
  }

  return (
    <section className="card p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-sm font-semibold">Models you can mine with</h3>
        <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">{network ? `${NETWORK_LABEL[network]} genesis registry` : 'genesis registry'}</span>
      </div>
      <p className="mt-1 text-xs leading-relaxed text-ink-500 dark:text-ink-400">
        A block on the MISAKA network is won by verified inference in one of these chain-registered classes, and each one names
        the Hugging Face repository it is installed from. Everything else in Discover is a model to chat with; only these produce
        blocks. Share is still reported: it is the fraction of finalized work a class provided, never a cut of the emission, never
        a lottery input, never an epoch block budget. A block buys one unit of work from any model. Each successful model draw can
        win a separate block; the Explorer keeps claim and block identities apart.
      </p>

      {error && (
        <p className="mt-3 flex gap-2 rounded-lg bg-amber-50 p-2 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <Icon name="warning" className="mt-0.5 size-4 shrink-0" />
          <span>
            The class list could not be read from the runtime ({error}). The registry itself is a chain fact, not a runtime one —
            the list below is simply unavailable until the runtime answers again.
          </span>
        </p>
      )}

      {!classes && !error && (
        <div className="mt-3 flex items-center gap-2 text-sm text-ink-500 dark:text-ink-400">
          <Spinner className="size-4" /> Reading the class registry…
        </div>
      )}

      <div className="mt-3 space-y-2">
        {classes?.map((cls) => (
          <MiningRow key={cls.spec.name} cls={cls} nodeRows={nodeRows} onInstall={install} />
        ))}
      </div>
    </section>
  )
}

/** What the file's own header says: its rotary table and its layers. */
function FileShape({ header }: { header: PalwArtifactHeader }) {
  return (
    <span title={`From the artifact's header: ${header.n_layers} layers, ${header.n_heads} heads (${header.n_kv_heads} kv), vocab ${header.vocab.toLocaleString()}`}>
      file: {header.max_position.toLocaleString()}-position rotary table · {header.n_layers} layers
    </span>
  )
}

function MiningRow({ cls, nodeRows, onInstall }: { cls: PalwClassStatus; nodeRows: NodeClassRow[]; onInstall: (name: string) => void }) {
  const { spec, readiness } = cls
  const system = useStudio((s) => s.system)
  const downloads = useStudio((s) => s.downloads)
  const artifact = spec.artifact
  // An install already running. The progress bar is above this list, but the button is where the
  // eye is after clicking it, and one that still says "Install" invites a second click.
  const inFlight =
    artifact.kind === 'download' &&
    downloads.some((d) => d.file.endsWith(artifact.filename) && (d.status === 'downloading' || d.status === 'verifying'))
  const repo = artifact.kind === 'download' ? artifact.hf_repo : artifact.kind === 'convert_locally' ? artifact.source_repo : null

  // Worded for this list, not the Network tab's: here the question is "is it on this machine yet",
  // and "ready" would read as a claim about the node, which this list cannot make.
  const badge =
    readiness.state === 'ready_built_in' ? (
      <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">nothing to install</span>
    ) : readiness.state === 'artifact_present' ? (
      <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">
        installed{readiness.verified ? ' · verified' : ''}
      </span>
    ) : readiness.state === 'artifact_mismatch' ? (
      <span className="badge bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-300">wrong size on disk</span>
    ) : readiness.downloadable ? (
      <span className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300">not installed</span>
    ) : (
      <span className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">convert locally</span>
    )

  return (
    <div className="rounded-xl border border-ink-200 p-3 dark:border-ink-800">
      <div className="flex flex-wrap items-center gap-2">
        <h4 className="mono text-sm font-semibold">{spec.name}</h4>
        <FinalWorkShare spec={spec} rows={nodeRows} />
        {isDefaultClass(spec.name) && <span className="badge bg-arc-600 text-white">default model class</span>}
        {spec.is_base && <span className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300">floor · residual cadence, unpaid</span>}
        <ClassContext registered={spec.context_tokens} header={cls.artifact_header} />
        {badge}
      </div>

      <p className="mt-1.5 text-xs leading-relaxed text-ink-600 dark:text-ink-300">{spec.description}</p>

      <p className="mt-2 rounded-lg bg-ink-50 p-2 text-[0.7rem] leading-relaxed text-ink-500 dark:bg-ink-900/60 dark:text-ink-400">
        {spec.is_base
          ? 'The floor fills whatever cadence the model classes leave and is paid nothing for it — residual liveness, not a share grant.'
          : "A block buys one unit of work from any model. This class's ticket is its compute over the work target; a winning inference can make one block. Share is the Final work it provided, not a budget on how many blocks it may win."}
        {' '}The Explorer shows the separate claim ↔ block relationship.
      </p>

      <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
        {repo && (
          <a className="mono inline-flex items-center gap-1 text-arc-700 hover:underline dark:text-arc-300" href={repoUrl(system?.catalog_endpoint, repo)} target="_blank" rel="noreferrer">
            {repo}
            <Icon name="external" className="size-3" />
          </a>
        )}
        {artifact.kind === 'download' && (
          <>
            <span className="mono">{artifact.filename}</span>
            <span>{bytes(artifact.size_bytes)}</span>
            <span className="mono" title="SHA-256 the download is verified against">
              sha256 {artifact.sha256.slice(0, 12)}…
            </span>
          </>
        )}
        {artifact.kind === 'convert_locally' && (
          <>
            <span className="mono">{artifact.filename}</span>
            <span>{artifact.exact ? bytes(artifact.exact.size_bytes) : `~${bytes(artifact.approx_size_bytes)}`} once converted</span>
            {artifact.exact && (
              <span className="mono" title="SHA-256 a correct conversion produces — the conversion is deterministic">
                sha256 {artifact.exact.sha256.slice(0, 12)}…
              </span>
            )}
          </>
        )}
        {artifact.kind === 'derived_from_seed' && <span>no file — every node derives this class's artifact from a seed</span>}
        <span>registered at {spec.context_tokens.toLocaleString()} tokens</span>
        {cls.artifact_header && <FileShape header={cls.artifact_header} />}
        {readiness.state === 'artifact_present' && <span className="mono truncate">{readiness.path}</span>}
      </div>

      {cls.memory_note && (
        <p className="mt-2 flex gap-2 rounded-lg bg-amber-50 p-2 text-[0.7rem] text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
          {cls.memory_note}
        </p>
      )}

      {readiness.state === 'artifact_mismatch' && (
        <p className="mt-2 text-[0.7rem] text-red-700 dark:text-red-300">
          <span className="mono">{readiness.path}</span> is {bytes(readiness.size_bytes)} where the registry pins{' '}
          {bytes(readiness.expected_bytes)} — a truncated download or a different conversion. Delete it before installing again;
          the node would refuse this file at startup.
        </p>
      )}

      {artifact.kind === 'download' && (
        <div className="mt-2.5">
          {/* Every class in this list is installed from Hugging Face. The button stays up when the
              file is already here or larger than RAM: hiding it would leave a class that cannot be
              (re)installed from the card that names it. */}
          <button
            type="button"
            className={cls.memory_note ? 'btn-ghost' : 'btn-outline'}
            disabled={inFlight}
            onClick={() => onInstall(spec.name)}
          >
            {inFlight ? <Spinner className="size-3.5" /> : <Icon name="download" className="size-3.5" />}
            {inFlight ? 'Installing…' : `Install anyway — ${bytes(artifact.size_bytes)}`}
          </button>
        </div>
      )}

      {artifact.kind !== 'derived_from_seed' && readiness.state !== 'artifact_present' && (
        <div className="mt-2.5">
          <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">
            {artifact.kind === 'download'
              ? 'Or rebuild it from the public weights and trust nobody — the conversion is deterministic, so it lands on the same registered root or it is not this class:'
              : 'No artifact is published for this class — it is built from the public weights above, and the conversion is what makes it byte-identical to the registered root:'}
          </p>
          <div className="mt-1 flex items-center gap-1">
            <code className="mono min-w-0 flex-1 truncate rounded bg-ink-100 px-2 py-1 text-[0.65rem] dark:bg-ink-800" title={artifact.convert_command}>
              {artifact.convert_command}
            </code>
            <CopyButton text={artifact.convert_command} label="Copy conversion command" />
          </div>
        </div>
      )}
    </div>
  )
}
