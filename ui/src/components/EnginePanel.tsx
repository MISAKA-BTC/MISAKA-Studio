// The engine and its GPU.
//
// The question this panel answers is the one the field asked: "is the GPU being used, and if not,
// why not?" The hardware probe knows what card is in the machine; only the engine knows whether
// its build can drive it — a `llama-server` from a package repository is usually CPU-only and
// takes `--n-gpu-layers 99` without a word. So the verdict here comes from the engine's own
// device list, and the fix is one click away: upstream publishes a build per accelerator, and the
// runtime downloads, verifies, unpacks and probes the right one before it changes any setting.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import { bytes } from '../lib/format'
import type { EngineDevice, EnginesView, InstallStatus } from '../lib/types'
import { useStudio } from '../store/studio'
import { Icon, Spinner } from './common'

const SOURCE_LABEL = {
  configured: 'from Settings',
  beside_app: 'beside the app',
  path: 'on PATH',
  missing: 'not found',
} as const

const VERDICT_TONE = {
  gpu: 'bg-emerald-500',
  cpu_only: 'bg-amber-500',
  unknown: 'bg-ink-400',
  missing: 'bg-red-500',
} as const

function deviceLine(device: EngineDevice): string {
  const memory = device.free_mib !== null ? ` · ${bytes(device.free_mib * 1024 * 1024)} free` : ''
  const kind = device.kind === 'gpu' ? '' : ` (${device.kind === 'accel' ? 'CPU accelerator, not an offload target' : device.kind})`
  return `${device.id} · ${device.description}${memory}${kind}`
}

const ACTIVE: InstallStatus['state'][] = ['resolving', 'downloading', 'extracting', 'verifying']

export function EnginePanel({ onUsePath }: { onUsePath: (path: string) => void }) {
  const bootstrap = useStudio((s) => s.bootstrap)
  const toast = useStudio((s) => s.toast)
  const setView = useStudio((s) => s.setView)
  const [view, setViewState] = useState<EnginesView | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [flavor, setFlavor] = useState<string>('')
  const [starting, setStarting] = useState(false)

  const reload = useCallback(async () => {
    try {
      const next = await api.engines()
      setViewState(next)
      setError(null)
      setFlavor((current) => current || next.flavors.find((f) => f.recommended)?.id || next.flavors[0]?.id || '')
    } catch (e) {
      setError((e as Error).message)
    }
  }, [])

  useEffect(() => {
    void reload()
  }, [reload])

  // While an install runs, watch it; when it ends, re-read everything (the setting moved) and
  // refresh the store's copy of the settings so the path field shows the new engine.
  const installing = view ? ACTIVE.includes(view.install.state) : false
  useEffect(() => {
    if (!installing) return
    const timer = setInterval(async () => {
      try {
        const status = await api.engineInstallStatus()
        if (ACTIVE.includes(status.state)) {
          setViewState((current) => (current ? { ...current, install: status } : current))
          return
        }
        clearInterval(timer)
        await reload()
        await bootstrap()
        if (status.state === 'done') toast('success', `Engine installed: ${status.installed?.path ?? status.detail}`)
        else toast('error', status.error ?? 'the engine install failed')
      } catch {
        /* the next tick tries again */
      }
    }, 1500)
    return () => clearInterval(timer)
  }, [installing, reload, bootstrap, toast])

  const install = async () => {
    if (!flavor) return
    setStarting(true)
    try {
      const status = await api.installEngine(flavor)
      setViewState((current) => (current ? { ...current, install: status } : current))
      toast('info', 'Downloading the engine — progress is under Models while it runs.')
    } catch (e) {
      toast('error', (e as Error).message)
    } finally {
      setStarting(false)
    }
  }

  if (error && !view) {
    return <p className="text-xs text-red-600 dark:text-red-400">Could not read the engine status: {error}</p>
  }
  if (!view) return <Spinner className="size-4 text-arc-600" />

  const selected = view.flavors.find((f) => f.id === flavor) ?? null
  const gpus = view.devices?.filter((d) => d.kind !== 'cpu') ?? []

  return (
    <div className="space-y-3 rounded-lg border border-ink-200 p-3 dark:border-ink-800">
      <div className="flex items-start gap-2">
        <span className={`mt-1.5 size-2 shrink-0 rounded-full ${VERDICT_TONE[view.verdict]}`} />
        <div className="min-w-0 text-xs">
          <div className="font-medium">{view.summary}</div>
          <div className="mt-1 text-ink-500 dark:text-ink-400">
            <span className="mono">{view.program}</span> · {SOURCE_LABEL[view.source]}
            {view.version && <> · {view.version.replace(/^version:\s*/, '')}</>}
          </div>
          {gpus.length > 0 && (
            <ul className="mt-1 space-y-0.5 text-ink-600 dark:text-ink-300">
              {gpus.map((device) => (
                <li key={device.id} className="mono">
                  {deviceLine(device)}
                </li>
              ))}
            </ul>
          )}
          {view.hardware_gpu && (
            <div className="mt-1 text-ink-500 dark:text-ink-400">
              Detected in this machine: {view.hardware_gpu}
              {view.verdict === 'cpu_only' && ' — idle with this engine.'}
            </div>
          )}
        </div>
      </div>

      {view.flavors.length > 0 && (
        <div className="space-y-2 border-t border-ink-200 pt-3 dark:border-ink-800">
          <div className="text-xs font-medium text-ink-600 dark:text-ink-300">
            Install a llama.cpp build for this machine
            {view.release.tag && <span className="ml-1 font-normal text-ink-500 dark:text-ink-400">(upstream release {view.release.tag})</span>}
          </div>
          <div className="flex flex-col gap-2 sm:flex-row">
            <select className="input flex-1" value={flavor} onChange={(e) => setFlavor(e.target.value)} disabled={installing}>
              {view.flavors.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.label}
                  {f.download_bytes !== null ? ` · ${bytes(f.download_bytes, 0)}` : ''}
                  {f.recommended ? ' · recommended' : ''}
                </option>
              ))}
            </select>
            <button type="button" className="btn-primary whitespace-nowrap" onClick={() => void install()} disabled={installing || starting || !flavor || !!view.release.error}>
              {installing || starting ? <Spinner className="size-3.5" /> : <Icon name="download" className="size-3.5" />}
              Download and install
            </button>
          </div>
          {selected && <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">Needs: {selected.requires}</p>}
          {view.recommendation && <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">Why the recommendation: {view.recommendation}</p>}
          {view.release.error && (
            <p className="flex gap-2 rounded-lg bg-amber-50 p-2 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
              <Icon name="warning" className="mt-0.5 size-4 shrink-0" />
              The release list could not be fetched: {view.release.error}
            </p>
          )}
          {installing && (
            <p className="flex items-center gap-2 text-xs text-ink-600 dark:text-ink-300">
              <Spinner className="size-3.5" />
              {view.install.state === 'downloading' ? (
                <>
                  {view.install.detail} —{' '}
                  <button type="button" className="underline" onClick={() => setView('models')}>
                    progress under Models
                  </button>
                </>
              ) : (
                view.install.detail
              )}
            </p>
          )}
          {view.install.state === 'failed' && view.install.error && (
            <p className="flex gap-2 rounded-lg bg-red-50 p-2 text-xs text-red-800 dark:bg-red-950/40 dark:text-red-300">
              <Icon name="warning" className="mt-0.5 size-4 shrink-0" />
              {view.install.error}
            </p>
          )}
          {view.install.state === 'done' && view.install.installed && (
            <p className="text-xs text-emerald-700 dark:text-emerald-400">
              Installed {view.install.installed.tag} ({view.install.installed.flavor}) — the engine path below now points at it.
            </p>
          )}
        </div>
      )}

      {view.installed.length > 0 && (
        <div className="space-y-1 border-t border-ink-200 pt-3 dark:border-ink-800">
          <div className="text-xs font-medium text-ink-600 dark:text-ink-300">Installed builds</div>
          <ul className="space-y-1 text-xs">
            {view.installed.map((engine) => {
              const gpu = engine.devices?.find((d) => d.kind === 'gpu')
              const current = view.program === engine.path
              return (
                <li key={engine.path} className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate">
                    <span className="font-medium">
                      {engine.tag} · {engine.flavor}
                    </span>
                    <span className="text-ink-500 dark:text-ink-400"> · {gpu ? `drives ${gpu.id} (${gpu.description})` : 'lists no GPU device'}</span>
                    {current && <span className="ml-1 badge bg-emerald-100 text-emerald-800 dark:bg-emerald-950 dark:text-emerald-300">in use</span>}
                  </span>
                  {!current && (
                    <button type="button" className="btn-ghost px-2 py-1" onClick={() => onUsePath(engine.path)}>
                      Use
                    </button>
                  )}
                </li>
              )
            })}
          </ul>
        </div>
      )}
    </div>
  )
}
