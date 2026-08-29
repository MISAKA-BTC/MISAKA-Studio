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
import type { PalwClassStatus } from '../lib/types'
import { useStudio } from '../store/studio'
import { CopyButton, Icon, Spinner } from './common'

/**
 * The class the runtime installs on first run — `palw::DEFAULT_CLASS`, repeated here only to put
 * a badge on it. The install itself is the runtime's decision and does not consult this.
 */
const DEFAULT_CLASS = 'PALW-QWEN25-A16'

/** `https://huggingface.co/Qwen/Qwen2.5-1.5B-Instruct`, honouring an `HF_ENDPOINT` mirror. */
function repoUrl(endpoint: string | undefined, repo: string): string {
  const base = endpoint && /^https?:\/\//.test(endpoint) ? endpoint.replace(/\/+$/, '') : 'https://huggingface.co'
  return `${base}/${repo}`
}

/**
 * The class list, re-read whenever a download settles.
 *
 * An artifact that just landed must stop saying "not installed" on its own: the alternative is a
 * list that stays wrong until someone reloads the window, which is exactly the moment they would
 * conclude the download had failed.
 */
function useClassStatuses(): { classes: PalwClassStatus[] | null; error: string | null } {
  const [classes, setClasses] = useState<PalwClassStatus[] | null>(null)
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
  }, [])

  useEffect(() => {
    void refresh()
  }, [refresh, settled])

  return { classes, error }
}

/**
 * The class artifacts that are actually on this machine, for the Installed tab.
 *
 * They live in the models directory beside the GGUFs and are invisible to the model scanner —
 * different extension, different runtime, not something you can chat with. Without this, a 34 GiB
 * file could sit on disk with nothing in the app willing to admit it was there.
 */
export function InstalledMiningArtifacts() {
  const { classes } = useClassStatuses()
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
                <span className="badge bg-arc-500/15 text-arc-700 dark:text-arc-300">{cls.spec.share_permille}‰ share</span>
                {cls.spec.name === DEFAULT_CLASS && <span className="badge bg-arc-600 text-white">default class</span>}
                {readiness.state === 'artifact_present' ? (
                  <span className="badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">on disk</span>
                ) : (
                  <span className="badge bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-300">wrong size on disk</span>
                )}
              </div>
              <div className="mt-1.5 flex flex-wrap gap-x-4 gap-y-1 text-[0.7rem] text-ink-500 dark:text-ink-400">
                <span>{bytes(size)}</span>
                <span className="mono truncate">{path}</span>
              </div>
              {/* Presence is a filename, not an identity. The node re-derives the registered root
                  at startup and refuses a mismatch, so this list stops short of calling a file
                  verified — that word belongs to the check that actually ran. */}
              <p className="mt-1.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
                {readiness.state !== 'artifact_present'
                  ? 'A truncated download or a different conversion. Delete it and install again; the node would refuse this file at startup.'
                  : cls.spec.name === DEFAULT_CLASS
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
  const { classes, error } = useClassStatuses()
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
        <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">testnet-11 genesis registry</span>
      </div>
      <p className="mt-1 text-xs leading-relaxed text-ink-500 dark:text-ink-400">
        A block on the MISAKA network is won by verified inference in one of these chain-registered classes, and each one names
        the Hugging Face repository it is installed from. The share is that class's cut of the emission. Everything else in
        Discover is a model to chat with; only these produce blocks.
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
          <MiningRow key={cls.spec.name} cls={cls} onInstall={install} />
        ))}
      </div>
    </section>
  )
}

function MiningRow({ cls, onInstall }: { cls: PalwClassStatus; onInstall: (name: string) => void }) {
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
        <span className="badge bg-arc-500/15 text-arc-700 dark:text-arc-300">{spec.share_permille}‰ share</span>
        {spec.name === DEFAULT_CLASS && <span className="badge bg-arc-600 text-white">default · installed on first run</span>}
        {spec.is_base && <span className="badge bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300">floor · always producible</span>}
        {badge}
      </div>

      <p className="mt-1.5 text-xs leading-relaxed text-ink-600 dark:text-ink-300">{spec.description}</p>

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
            <span className="mono">{artifact.extension}</span>
            <span>~{bytes(artifact.approx_size_bytes)} once converted</span>
          </>
        )}
        {artifact.kind === 'derived_from_seed' && <span>no file — every node derives this class's artifact from a seed</span>}
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

      {artifact.kind === 'download' && readiness.state !== 'artifact_present' && (
        <div className="mt-2.5">
          {/* Offered even when the artifact is larger than this machine's memory. The note above
              already says it will not run here, and hiding the button would leave someone
              installing onto an external disk with no way to do it. */}
          <button
            type="button"
            className={cls.memory_note ? 'btn-ghost' : 'btn-outline'}
            disabled={inFlight}
            onClick={() => onInstall(spec.name)}
          >
            {inFlight ? <Spinner className="size-3.5" /> : <Icon name="download" className="size-3.5" />}
            {inFlight
              ? 'Installing…'
              : cls.memory_note
                ? `Install anyway — ${bytes(artifact.size_bytes)}`
                : `Install ${bytes(artifact.size_bytes)}`}
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
