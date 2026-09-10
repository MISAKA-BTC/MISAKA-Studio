// Components: every binary and artifact the Studio can spawn or map, against the manifest.
//
// ADR-0096 Decision 10. Four tables used to say what to install, and one of them named a binary
// the node tree had deleted on 2026-09-02; nothing in either repository knew. This page is the one
// table, read from `/api/v1/components`: per row, where the file was found (in the one search
// order's own words), what the manifest says it should be, and whether the two agree. Three things
// are on request rather than automatic — reading the manifest when `auto_check` is off, hashing
// every found file (a class artifact is 34 GiB), and asking each binary for its version — because
// a table that costs a minute of disk to open is a table nobody opens.
//
// Artifacts are rows here for the digest check only. Their readiness — present, mismatched,
// downloadable, convertible — is the Network tab's class card, which this page links to rather
// than repeats.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import { bytes, eta, rate, relativeTime, shortHash } from '../lib/format'
import type { ComponentFinding, ComponentReport, ComponentState, ComponentsListing, DownloadProgress } from '../lib/types'
import { useStudio } from '../store/studio'
import { EmptyState, Icon, Spinner } from './common'
import { useClassStatuses } from './MiningCatalog'

type Read = { verify: boolean; check: boolean }

/** The file's name on disk — what a download row (`file`) and a class spec (`filename`) both carry. */
function basename(path: string): string {
  return path.split(/[\\/]/).pop() ?? path
}

const STATE: Record<ComponentState, { label: string; tone: string; title: string }> = {
  installed: {
    label: 'installed · verified',
    tone: 'bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300',
    title: "Found, and its SHA-256 equals the manifest row's.",
  },
  'installed-unverified': {
    label: 'installed · unverified',
    tone: 'bg-arc-500/15 text-arc-700 dark:text-arc-300',
    title: "Found with the manifest row's size. The digest is computed by Verify digests, not on every read.",
  },
  mismatch: {
    label: 'mismatch',
    tone: 'bg-red-100 text-red-800 dark:bg-red-950/60 dark:text-red-300',
    title: 'Found, and the size or the digest differs from the manifest row: not this component, whatever its name.',
  },
  missing: {
    label: 'missing',
    tone: 'bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300',
    title: 'Not found at any step of the search order: configured path, beside the executable, engines/, models_dir (artifacts), PATH (binaries).',
  },
  retired: {
    label: 'retired',
    tone: 'bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300',
    title: 'An id the node tree stopped building. Nothing installs it; the note says what replaced it.',
  },
  'not-in-manifest': {
    label: 'found · not in manifest',
    tone: 'bg-ink-100 text-ink-600 dark:bg-ink-800 dark:text-ink-300',
    title: 'Found, and there is no manifest row to hold it to.',
  },
}

export function ComponentsView() {
  const [listing, setListing] = useState<ComponentsListing | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<'refresh' | 'check' | 'verify' | null>(null)
  const [last, setLast] = useState<{ read: Read; at: number } | null>(null)
  const downloads = useStudio((s) => s.downloads)
  const settings = useStudio((s) => s.settings)
  const settled = downloads.filter((d) => d.status === 'completed' || d.status === 'failed' || d.status === 'cancelled').length

  const read = useCallback(async (options: Read, mode: 'refresh' | 'check' | 'verify') => {
    setBusy(mode)
    try {
      setListing(await api.components(options))
      setLast({ read: options, at: Date.now() / 1000 })
      setError(null)
    } catch (e) {
      setError((e as Error).message)
    } finally {
      setBusy(null)
    }
  }, [])

  // The cheap read, on open and whenever a download settles: a component that just landed must
  // stop saying `missing` on its own, the way the class list does.
  useEffect(() => {
    void read({ verify: false, check: false }, 'refresh')
  }, [read, settled])

  const autoCheck = settings?.components?.auto_check ?? true

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-5xl space-y-4 p-4 pb-16">
        <div className="flex flex-wrap items-center gap-2">
          <div className="min-w-0 flex-1">
            <h2 className="text-base font-semibold">Components</h2>
            <p className="text-xs text-ink-500 dark:text-ink-400">
              Every binary and class artifact this Studio can spawn or map, where it was found, and whether its bytes are the ones
              the manifest names.
              {last && (
                <>
                  {' '}
                  Read {relativeTime(last.at)} · {last.read.verify ? 'digests verified' : 'sizes only — digests not computed'}.
                </>
              )}
            </p>
          </div>
          <button type="button" className="btn-ghost" disabled={busy !== null} onClick={() => void read({ verify: false, check: false }, 'refresh')}>
            {busy === 'refresh' ? <Spinner className="size-3.5" /> : <Icon name="refresh" className="size-3.5" />}
            Refresh
          </button>
          <button
            type="button"
            className="btn-outline"
            disabled={busy !== null}
            title={
              autoCheck
                ? 'Read the manifest again and run the cross-repository check over it.'
                : 'The automatic check is off (Settings › Components); this reads the manifest for this request only.'
            }
            onClick={() => void read({ verify: false, check: true }, 'check')}
          >
            {busy === 'check' ? <Spinner className="size-3.5" /> : <Icon name="search" className="size-3.5" />}
            Check
          </button>
          <button
            type="button"
            className="btn-outline"
            disabled={busy !== null}
            title="Hash every found file and ask every binary for --version. Slow: a class artifact is 34 GiB, and the whole read waits for it."
            onClick={() => void read({ verify: true, check: true }, 'verify')}
          >
            {busy === 'verify' ? <Spinner className="size-3.5" /> : <Icon name="shield" className="size-3.5" />}
            Verify digests
          </button>
        </div>

        {busy === 'verify' && (
          <p className="flex gap-2 rounded-lg bg-ink-100 p-2 text-xs text-ink-600 dark:bg-ink-900 dark:text-ink-300">
            <Spinner className="mt-0.5 size-3.5 shrink-0" />
            Hashing every found file and asking every binary for its version. This reads whole files — minutes for a class artifact.
          </p>
        )}

        {error && (
          <div className="card border-red-300 p-3 text-sm text-red-700 dark:border-red-900 dark:text-red-300">
            <div className="flex gap-2">
              <Icon name="warning" className="mt-0.5 size-4 shrink-0" />
              <span>{error}</span>
            </div>
          </div>
        )}

        {!listing && !error && (
          <div className="flex items-center gap-2 p-4 text-sm text-ink-500 dark:text-ink-400">
            <Spinner className="size-4" /> Reading the components table…
          </div>
        )}

        {listing && (
          <>
            <ManifestCard listing={listing} />
            <ComponentsTable listing={listing} downloads={downloads} />
          </>
        )}
      </div>
    </div>
  )
}

/** The manifest as this request saw it, and what the cross-repository check found in it. */
function ManifestCard({ listing }: { listing: ComponentsListing }) {
  const setView = useStudio((s) => s.setView)
  const manifest = listing.manifest
  return (
    <section className="card p-4">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <h3 className="text-sm font-semibold">Manifest</h3>
        <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">
          this build: <span className="mono">{listing.host_platform}</span>
        </span>
      </div>
      {manifest.source === null ? (
        <p className="mt-2 text-xs text-ink-600 dark:text-ink-300">
          No manifest is configured, so everything found on disk is <span className="mono">not-in-manifest</span> and nothing can be
          installed from here.{' '}
          <button type="button" className="text-arc-700 hover:underline dark:text-arc-300" onClick={() => setView('settings')}>
            Set one under Settings › Components.
          </button>
        </p>
      ) : (
        <>
          <p className="mt-2 break-all text-xs text-ink-600 dark:text-ink-300">
            <span className="mono">{manifest.source}</span>
            {manifest.loaded ? (
              <>
                {' '}
                · release <span className="mono">{manifest.release}</span> · network <span className="mono">{manifest.network}</span>
              </>
            ) : (
              <> · not loaded</>
            )}
          </p>
          {manifest.error && (
            <p className="mt-2 flex gap-2 rounded-lg bg-amber-50 p-2 text-[0.7rem] text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
              <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
              <span className="whitespace-pre-wrap break-words">{manifest.error}</span>
            </p>
          )}
          {manifest.loaded && manifest.findings.length === 0 && (
            <p className="mt-2 text-[0.7rem] text-emerald-700 dark:text-emerald-300">
              Every id the Studio can spawn is a row, and no retired id is.
            </p>
          )}
          {manifest.findings.length > 0 && (
            <ul className="mt-2 space-y-1 text-[0.7rem]">
              {manifest.findings.map((finding, i) => (
                <FindingLine key={`${finding.finding}-${finding.id}-${i}`} finding={finding} />
              ))}
            </ul>
          )}
        </>
      )}
    </section>
  )
}

/** One finding, in the sentence `misaka-studiod --check` prints for it. A retirement is a warning
 *  naming what was retired and why; a missing spawnable is the one a release gate stops on. */
function FindingLine({ finding }: { finding: ComponentFinding }) {
  switch (finding.finding) {
    case 'missing_spawnable':
      return (
        <li className="flex gap-2 rounded-lg bg-red-50 p-2 text-red-700 dark:bg-red-950/40 dark:text-red-300">
          <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
          <span>
            MISSING <span className="mono">{finding.id}</span> ({finding.kind}): the Studio can spawn it and the manifest has no row for it —
            the release does not build it, or stopped.
          </span>
        </li>
      )
    case 'retired_still_spawned':
      return (
        <li className="flex gap-2 rounded-lg bg-amber-50 p-2 text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
          <span>
            RETIRED <span className="mono">{finding.id}</span>: {finding.note}; a code path here still names it.
          </span>
        </li>
      )
    case 'retired_in_manifest':
      return (
        <li className="flex gap-2 rounded-lg bg-amber-50 p-2 text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
          <Icon name="warning" className="mt-0.5 size-3.5 shrink-0" />
          <span>
            RETIRED <span className="mono">{finding.id}</span>: {finding.note}; the manifest must not carry a row for it.
          </span>
        </li>
      )
  }
}

function ComponentsTable({ listing, downloads }: { listing: ComponentsListing; downloads: DownloadProgress[] }) {
  if (listing.components.length === 0) {
    return (
      <EmptyState icon="shield" title="No components">
        The runtime listed nothing it can spawn or map.
      </EmptyState>
    )
  }
  return (
    <section className="card overflow-x-auto">
      <table className="w-full text-xs">
        <thead>
          <tr className="border-b border-ink-200 text-left text-[0.7rem] uppercase tracking-wide text-ink-500 dark:border-ink-800 dark:text-ink-400">
            <th className="px-3 py-2 font-medium">Component</th>
            <th className="px-3 py-2 font-medium">State</th>
            <th className="px-3 py-2 font-medium">Installed</th>
            <th className="px-3 py-2 font-medium">Manifest</th>
            <th className="px-3 py-2 font-medium" />
          </tr>
        </thead>
        <tbody>
          {listing.components.map((row) => (
            <ComponentRow key={row.id} row={row} downloads={downloads} />
          ))}
        </tbody>
      </table>
    </section>
  )
}

function ComponentRow({ row, downloads }: { row: ComponentReport; downloads: DownloadProgress[] }) {
  const state = STATE[row.state]
  const file = basename(row.installed.path)
  // An install already running for this file. Matched on the file name because a component
  // install (`component/<file>`) and the class download (`<repo path>/<file>`) both end in it.
  const inFlight = downloads.find((d) => (d.status === 'downloading' || d.status === 'verifying') && d.file.endsWith(file))
  return (
    <tr className="border-b border-ink-100 align-top last:border-b-0 dark:border-ink-800">
      <td className="px-3 py-2.5">
        <div className="mono font-medium">{row.id}</div>
        <div className="mt-0.5 text-[0.7rem] text-ink-500 dark:text-ink-400">{row.kind}</div>
      </td>
      <td className="px-3 py-2.5">
        <span className={`badge ${state.tone}`} title={state.title}>
          {state.label}
        </span>
      </td>
      <td className="max-w-xs px-3 py-2.5">
        <div className="mono truncate" title={row.installed.path}>
          {row.installed.path}
        </div>
        <div className="mt-0.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
          {row.installed.found ? `found: ${row.installed.candidate}` : 'not found'}
          {row.installed.size !== undefined && ` · ${bytes(row.installed.size)}`}
          {row.installed.version && ` · ${row.installed.version}`}
        </div>
        {row.installed.sha256 && (
          <div className="mono mt-0.5 text-[0.7rem] text-ink-500 dark:text-ink-400" title={row.installed.sha256}>
            sha256 {shortHash(row.installed.sha256, 10, 6)}
          </div>
        )}
        {row.note && <div className="mt-1 text-[0.7rem] text-amber-800 dark:text-amber-300">{row.note}</div>}
      </td>
      <td className="max-w-xs px-3 py-2.5">
        {row.manifest ? (
          <>
            <div>
              {row.manifest.version} · {bytes(row.manifest.size)} · <span className="mono">{row.manifest.platform}</span>
            </div>
            <div className="mono mt-0.5 text-[0.7rem] text-ink-500 dark:text-ink-400" title={row.manifest.sha256}>
              sha256 {shortHash(row.manifest.sha256, 10, 6)}
            </div>
            {row.manifest.member && (
              <div className="mt-0.5 text-[0.7rem] text-ink-500 dark:text-ink-400">
                inside <span className="mono">{row.manifest.member}</span> — the Studio does not extract archives yet
              </div>
            )}
            <div className="mono mt-0.5 truncate text-[0.7rem] text-ink-500 dark:text-ink-400" title={row.manifest.url}>
              {row.manifest.url}
            </div>
          </>
        ) : (
          <span className="text-[0.7rem] text-ink-500 dark:text-ink-400">no row</span>
        )}
      </td>
      <td className="whitespace-nowrap px-3 py-2.5 text-right">
        {inFlight ? <InstallProgress progress={inFlight} /> : <RowAction row={row} />}
      </td>
    </tr>
  )
}

/** An install under way, and the way to stop it. The store's SSE stream carries the outcome. */
function InstallProgress({ progress }: { progress: DownloadProgress }) {
  const remaining = progress.total && progress.bytes_per_second > 1 ? (progress.total - progress.downloaded) / progress.bytes_per_second : null
  const cancel = async () => {
    try {
      await api.cancelDownload(progress.id)
    } catch {
      /* the stream reports the outcome */
    }
  }
  return (
    <div className="flex items-center justify-end gap-2 text-[0.7rem] text-ink-500 dark:text-ink-400">
      <Spinner className="size-3.5" />
      <span>
        {progress.status === 'verifying'
          ? 'verifying the digest…'
          : `${bytes(progress.downloaded)} of ${bytes(progress.total)} · ${rate(progress.bytes_per_second)}${remaining !== null ? ` · ${eta(remaining)}` : ''}`}
      </span>
      <button type="button" className="btn-ghost px-1.5 py-1" title="Cancel" onClick={() => void cancel()}>
        <Icon name="x" className="size-3.5" />
      </button>
    </div>
  )
}

function RowAction({ row }: { row: ComponentReport }) {
  const toast = useStudio((s) => s.toast)
  const setDownload = useStudio((s) => s.setDownload)
  const setView = useStudio((s) => s.setView)
  const focusClass = useStudio((s) => s.focusClass)
  const { classes } = useClassStatuses()

  if (row.kind === 'artifact') {
    // The class card is where an artifact's readiness lives — download, conversion command,
    // memory note. The id here is the file's stem; the card is named by the class, so the file
    // name is the join.
    const file = basename(row.installed.path)
    const cls = classes?.find((c) => c.spec.artifact.kind === 'download' && c.spec.artifact.filename === file)
    return (
      <button
        type="button"
        className="btn-ghost"
        title="Readiness, download and conversion live on the class card in the Network tab."
        onClick={() => {
          focusClass(cls?.spec.name ?? null)
          setView('network')
        }}
      >
        <Icon name="globe" className="size-3.5" />
        Network › {cls?.spec.name ?? 'classes'}
      </button>
    )
  }

  if (row.manifest && (row.state === 'missing' || row.state === 'mismatch')) {
    const install = async () => {
      try {
        const progress = await api.installComponent(row.id)
        setDownload(progress)
        toast('info', `Installing ${row.id} — verified against the manifest's digest when it lands`)
      } catch (e) {
        // The runtime refuses by name — a retired id, another platform's row, an archive member,
        // a file already at the destination — and its sentence is the one worth reading.
        toast('error', (e as Error).message)
      }
    }
    return (
      <button type="button" className="btn-outline" onClick={() => void install()}>
        <Icon name="download" className="size-3.5" />
        {row.state === 'mismatch' ? 'Update' : 'Install'} {bytes(row.manifest.size)}
      </button>
    )
  }

  return null
}
