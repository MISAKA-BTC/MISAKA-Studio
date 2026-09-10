// Settings.
//
// Everything here writes to the runtime's settings file through the API, not to browser storage —
// the runtime is what acts on these, and a setting that lived only in the window would be a
// setting the headless `misaka-studiod` ignored.
//
// Two are guarded rather than merely offered: binding the server to a non-loopback address without
// an API key is refused by the runtime (it would be an open inference endpoint), and keeping
// transcripts is off by default and says exactly what it does before it is turned on.
//
// Beside the fields, what is actually running (ADR-0096 Decision 11). The file says what the
// engine SHOULD be built from; `/api/v1/settings/effective` says what it WAS built from, and where
// each value came from — a flag, an environment variable, the file, or discovery. The two are
// read together on open and after every save, and a section whose running object disagrees with
// the file says so on its header, by field.

import { useCallback, useEffect, useState } from 'react'
import { api } from '../lib/api'
import { asString, asStringList, differences, isRecord, pick, show } from '../lib/effective'
import { bytes } from '../lib/format'
import type { BackendInfo, EffectiveSettings, Settings } from '../lib/types'
import { useStudio } from '../store/studio'
import { DiffMarker, DiffNote, EffectiveBlock, EffectiveLine, NodeCommand } from './Effective'
import { Field, Icon, Section, Toggle } from './common'

/** A number field's text as a whole number within bounds — an emptied field is `fallback`, not NaN. */
function clampInt(text: string, min: number, max: number, fallback: number): number {
  const value = Math.floor(Number(text))
  if (!Number.isFinite(value)) return fallback
  return Math.min(max, Math.max(min, value))
}

/** What the runtime fills in for a settings file written before the manifest existed. */
const DEFAULT_COMPONENTS: NonNullable<Settings['components']> = { manifest: null, auto_check: true }

export function SettingsView() {
  const settings = useStudio((s) => s.settings)
  const save = useStudio((s) => s.saveSettings)
  const system = useStudio((s) => s.system)
  const models = useStudio((s) => s.models)
  const setView = useStudio((s) => s.setView)
  const [draft, setDraft] = useState<Settings | null>(settings)
  const [backends, setBackends] = useState<BackendInfo[]>([])
  const [effective, setEffective] = useState<EffectiveSettings | null>(null)
  const [effectiveError, setEffectiveError] = useState<string | null>(null)

  useEffect(() => setDraft(settings), [settings])
  useEffect(() => {
    api.backends().then(setBackends).catch(() => setBackends([]))
  }, [])

  // Read on open and after every save: a save may have rebuilt the engine, and the view must
  // show the engine that exists now, not the one that existed when the page opened.
  const readEffective = useCallback(async () => {
    try {
      setEffective(await api.settingsEffective())
      setEffectiveError(null)
    } catch (e) {
      setEffective(null)
      setEffectiveError((e as Error).message)
    }
  }, [])
  useEffect(() => {
    void readEffective()
  }, [readEffective])
  const saveAndReread = async (next: Settings) => {
    await save(next)
    await readEffective()
  }

  if (!draft) return null
  const dirty = JSON.stringify(draft) !== JSON.stringify(settings)

  const set = <K extends keyof Settings>(key: K, value: Settings[K]) => setDraft({ ...draft, [key]: value })
  const components = draft.components ?? DEFAULT_COMPONENTS

  // The engine's running values, for the lines placed beside their fields. Everything else the
  // runtime returned is printed generically by the block under the engine list.
  const backend = effective?.backend ?? null
  const engine = backend?.effective ?? null
  const engineName = asString(pick(engine, 'name'))
  const backendDiffers = new Set(backend ? differences('backend', backend).map((d) => d.path) : [])
  const engineLine = (label: string, path: string, source: string | undefined) => {
    const value = pick(engine, path)
    if (value === undefined) return null
    return <EffectiveLine label={label} value={show(value)} source={source} differs={backendDiffers.has(path)} />
  }

  // The node: the whole record is the runtime's, and the argument list is the command.
  const node = effective?.node ?? null
  const nodeBinary = asString(pick(node?.effective, 'binary'))
  const nodeArgs = asStringList(pick(node?.effective, 'args'))
  const configuredNodeBinary = asString(pick(node?.configured, 'binary.path'))
  const configuredNodeArgs = asStringList(pick(node?.configured, 'args'))
  const configuredNodeError = (() => {
    const args = pick(node?.configured, 'args')
    return isRecord(args) && typeof args.error === 'string' ? args.error : null
  })()

  return (
    <div className="h-full overflow-y-auto">
      <div className="mx-auto max-w-3xl space-y-4 p-4 pb-24">
        {effectiveError && (
          <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">
            Running values are not available from this runtime ({effectiveError}); the fields below show the file only.
          </p>
        )}

        <Section title="Models" description="Where GGUF files live. Moving this rescans; it does not move any files.">
          <Field label="Model directory">
            <input className="input mt-1" value={draft.models_dir} onChange={(e) => set('models_dir', e.target.value)} />
          </Field>
          {system && (
            <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">
              Application data lives in <span className="mono">{system.data_dir}</span>.
            </p>
          )}
        </Section>

        <Section
          title="Backend"
          description="Which engine runs the model. Changing it unloads whatever is loaded."
          marker={backend && <DiffMarker subsystems={[['backend', backend]]} />}
        >
          {backend && <DiffNote name="backend" subsystem={backend} onResave={() => void saveAndReread(settings ?? draft)} />}
          <Field label="Engine">
            <select
              className="input mt-1"
              value={draft.backend.kind}
              onChange={(e) => set('backend', { ...draft.backend, kind: e.target.value as Settings['backend']['kind'] })}
            >
              <option value="auto">Auto — MLX on Apple Silicon, llama.cpp elsewhere</option>
              <option value="llama_cpp">llama.cpp (llama-server)</option>
              <option value="mlx">MLX (Apple Silicon)</option>
              {/* The engine that reads a `.palwart`. It was missing from this list while the load
                  error for one told people to select it here. */}
              <option value="misaka">MISAKA integer runtime — the engine a PALW class registers</option>
              {/* The lane that prices the work: one execution answers you and commits a claim. */}
              <option value="gateway">MISAKA free-prompt gateway — the answer is the mining work</option>
              <option value="mock">Mock — canned replies, no model needed</option>
            </select>
            {backend && engineName && (
              <EffectiveLine label="running engine:" value={engineName} source={backend.source.kind} differs={backendDiffers.has('fingerprint.kind')} />
            )}
          </Field>

          {backends.length > 0 && (
            <ul className="space-y-1.5 text-xs">
              {backends.map((backend) => (
                <li key={backend.name} className="flex items-start gap-2">
                  <span className={`mt-1 size-1.5 shrink-0 rounded-full ${backend.availability.state === 'available' ? 'bg-emerald-500' : 'bg-ink-400'}`} />
                  <span>
                    <span className="mono font-medium">{backend.name}</span>{' '}
                    {backend.availability.state === 'available' ? (
                      <span className="text-ink-500 dark:text-ink-400">{backend.availability.detail}</span>
                    ) : (
                      <span className="text-ink-500 dark:text-ink-400">
                        {backend.availability.reason}. {backend.availability.remedy}
                      </span>
                    )}
                  </span>
                </li>
              ))}
            </ul>
          )}

          {/* The hand-placed lines: what the engine was spawned from, where it posts, what it
              holds and how wide. The tokenizer and the timeout sit under their own fields below;
              everything else the runtime returned follows generically. */}
          {backend && (
            <EffectiveBlock
              name="backend"
              subsystem={backend}
              omit={['name', 'fingerprint.kind', 'fingerprint.program', 'fingerprint.url', 'fingerprint.tokenizer', 'fingerprint.startup_timeout_secs', 'model_id', 'context_size']}
            >
              {engineLine('program:', 'fingerprint.program', backend.source.program)}
              {engineLine('URL:', 'fingerprint.url', backend.source.url)}
              {engineLine('model:', 'model_id', backend.source.model_id)}
              {engineLine('n_ctx:', 'context_size', backend.source.context_size)}
            </EffectiveBlock>
          )}

          <Field label="llama-server path" hint="Leave empty to use the one on PATH, or the one packaged beside the app.">
            <input
              className="input mt-1"
              placeholder="/usr/local/bin/llama-server"
              value={draft.backend.llama_server_path ?? ''}
              onChange={(e) => set('backend', { ...draft.backend, llama_server_path: e.target.value || null })}
            />
            {engineName === 'llamacpp' && engineLine('running:', 'fingerprint.program', backend?.source.program)}
          </Field>

          <Field
            label="misaka-palw-serve path"
            hint="Retired: the node tree removed this binary on 2026-09-02 (one runtime — v3-serve on both family workers). The integer engine now runs through misaka-palw-gateway in --answer-never-commit mode over the family worker (ADR-0096 Decision 10); a path here is rewritten into the two paths the gateway needs, and said so once. Kept until that migration has run."
          >
            <input
              className="input mt-1"
              placeholder="/path/to/misaka-palw-serve"
              value={draft.backend.misaka_serve_path ?? ''}
              onChange={(e) => set('backend', { ...draft.backend, misaka_serve_path: e.target.value || null })}
            />
            {engineName === 'misaka' && engineLine('running:', 'fingerprint.program', backend?.source.program)}
          </Field>

          <Field
            label="Tokenizer for the integer runtime"
            hint="A .palwart commits to what the ids MEAN and never ships the tokenizer, so the file comes from here. Empty looks for tokenizer.json beside the artifact."
          >
            <input
              className="input mt-1"
              placeholder="/path/to/tokenizer.json"
              value={draft.backend.misaka_tokenizer_path ?? ''}
              onChange={(e) => set('backend', { ...draft.backend, misaka_tokenizer_path: e.target.value || null })}
            />
            {/* The runtime names the source even when no file was pinned ("discovery … at load"),
                so the line is keyed on the source and prints the value it has, if any. */}
            {backend?.source.tokenizer && (
              <EffectiveLine
                label="running:"
                value={show(pick(engine, 'fingerprint.tokenizer'))}
                source={backend.source.tokenizer}
                differs={backendDiffers.has('fingerprint.tokenizer')}
              />
            )}
          </Field>

          <Field
            label="Load at startup"
            hint="Loaded as soon as the runtime is up, so the app opens ready to answer. The engine is a child process of a loaded model — naming one here is how an engine starts on its own."
          >
            <select
              className="input mt-1"
              value={draft.load_on_start ?? ''}
              onChange={(e) => set('load_on_start', e.target.value || null)}
            >
              <option value="">Nothing — load a model when I ask</option>
              {models.map((model) => (
                <option key={model.id} value={model.id}>
                  {model.id}
                </option>
              ))}
            </select>
          </Field>

          <Field label="GPU offload">
            <select
              className="input mt-1"
              value={draft.backend.gpu_layers.mode}
              onChange={(e) => {
                const mode = e.target.value as 'auto' | 'all' | 'none' | 'fixed'
                set('backend', { ...draft.backend, gpu_layers: mode === 'fixed' ? { mode, layers: 20 } : { mode } })
              }}
            >
              <option value="auto">Auto — as many layers as fit</option>
              <option value="all">All layers</option>
              <option value="none">CPU only</option>
              <option value="fixed">A fixed number of layers</option>
            </select>
          </Field>
          {draft.backend.gpu_layers.mode === 'fixed' && (
            <Field label="Layers on the GPU">
              <input
                className="input mt-1"
                type="number"
                min={0}
                value={draft.backend.gpu_layers.layers}
                onChange={(e) => set('backend', { ...draft.backend, gpu_layers: { mode: 'fixed', layers: Number(e.target.value) } })}
              />
            </Field>
          )}

          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Threads" hint="Empty lets the engine choose.">
              <input
                className="input mt-1"
                type="number"
                min={1}
                placeholder="auto"
                value={draft.backend.threads ?? ''}
                onChange={(e) => set('backend', { ...draft.backend, threads: e.target.value === '' ? null : Number(e.target.value) })}
              />
            </Field>
            <Field label="Load timeout (seconds)" hint="A large model on a slow disk genuinely takes minutes.">
              <input
                className="input mt-1"
                type="number"
                min={30}
                value={draft.backend.startup_timeout_secs}
                onChange={(e) => set('backend', { ...draft.backend, startup_timeout_secs: Number(e.target.value) })}
              />
              {engineLine('running:', 'fingerprint.startup_timeout_secs', backend?.source.startup_timeout_secs)}
            </Field>
          </div>

          <Field label="Flash attention" hint="A large memory saving on long contexts. Auto lets the engine decide, which is what it does best — and is the only setting every engine version accepts.">
            <select
              className="input mt-1"
              value={draft.backend.flash_attention}
              onChange={(e) => set('backend', { ...draft.backend, flash_attention: e.target.value as Settings['backend']['flash_attention'] })}
            >
              <option value="auto">Auto — let the engine choose</option>
              <option value="on">On</option>
              <option value="off">Off</option>
            </select>
          </Field>

          <div className="space-y-2.5">
            <Toggle
              label="Memory-map the model"
              checked={draft.backend.use_mmap}
              onChange={(use_mmap) => set('backend', { ...draft.backend, use_mmap })}
              hint="Faster loads and lower memory. Turn off only if a model fails to load."
            />
            <Toggle
              label="Lock the model in RAM"
              checked={draft.backend.use_mlock}
              onChange={(use_mlock) => set('backend', { ...draft.backend, use_mlock })}
              hint="Stops the OS swapping weights out mid-generation — and stops anything loading that does not fit."
            />
          </div>
        </Section>

        <Section title="API" description="The OpenAI-compatible endpoint other applications can point at.">
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Host">
              <input className="input mt-1" value={draft.server.host} onChange={(e) => set('server', { ...draft.server, host: e.target.value })} />
            </Field>
            <Field label="Port">
              <input
                className="input mt-1"
                type="number"
                value={draft.server.port}
                onChange={(e) => set('server', { ...draft.server, port: Number(e.target.value) })}
              />
            </Field>
          </div>
          <Field label="API key" hint="Required when the host is not a loopback address; optional otherwise.">
            <input
              className="input mt-1"
              type="password"
              placeholder="none"
              value={draft.server.api_key ?? ''}
              onChange={(e) => set('server', { ...draft.server, api_key: e.target.value || null })}
            />
          </Field>
          {draft.server.host !== '127.0.0.1' && draft.server.host !== 'localhost' && !draft.server.api_key && (
            <p className="flex gap-2 rounded-lg bg-amber-50 p-2 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
              <Icon name="warning" className="mt-0.5 size-4 shrink-0" />
              Binding to {draft.server.host} without an API key would let anyone on the network use this model. The runtime will refuse
              to save this.
            </p>
          )}
          <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">
            Changes take effect the next time the runtime starts. Point any OpenAI client at{' '}
            <span className="mono">
              http://{draft.server.host}:{draft.server.port}/v1
            </span>
            .
          </p>
        </Section>

        {/* The node's settings are edited in the Network tab, beside the node. What belongs here
            is the running node's own record of what it was started with — the argument list is
            read once, at start, so it is the one place the file and the process drift apart
            silently. */}
        <Section
          title="Node"
          description="What the supervised node was started with, from its own record. Its settings are edited in the Network tab and apply at the next start."
          marker={node && <DiffMarker subsystems={[['node', node]]} />}
        >
          {node && <DiffNote name="node" subsystem={node} />}
          {node && (
            <EffectiveBlock name="node" subsystem={node} omit={['args', 'started_at_unix']} sinceLabel="started">
              {nodeBinary && nodeArgs && (
                <NodeCommand
                  binary={nodeBinary}
                  args={nodeArgs}
                  caption={<>The command line it is running — {node.source.args ?? 'as started'}.</>}
                />
              )}
            </EffectiveBlock>
          )}
          {node && node.differs && configuredNodeBinary && configuredNodeArgs && (
            <NodeCommand binary={configuredNodeBinary} args={configuredNodeArgs} caption="What the settings would start now:" />
          )}
          {node && node.effective === null && configuredNodeBinary && configuredNodeArgs && (
            <NodeCommand binary={configuredNodeBinary} args={configuredNodeArgs} caption="Starting it from the Network tab would run:" />
          )}
          {node && configuredNodeError && (
            <p className="text-[0.7rem] text-amber-800 dark:text-amber-300">The settings do not build a command line: {configuredNodeError}</p>
          )}
          {!node && !effectiveError && <p className="text-[0.7rem] text-ink-500 dark:text-ink-400">Reading the running values…</p>}
        </Section>

        {/* ADR-0096 Decisions 4 and 5. These are `node` settings because the lane is the node's:
            the row is 512 tokens and an inference is a claim, and none of that is this window's
            to change. What the window chooses is what to do at the edge of it. */}
        <Section
          title="Lane"
          description="What the app does when a request asks the free-prompt lane for more than one job can commit. Every answer says what ran, under the message."
          marker={
            effective && (
              <DiffMarker
                subsystems={[
                  ['gateway', effective.gateway],
                  ['pool', effective.pool],
                ]}
              />
            )
          }
        >
          {effective && <DiffNote name="gateway" subsystem={effective.gateway} onResave={() => void saveAndReread(settings ?? draft)} />}
          {effective && <DiffNote name="pool" subsystem={effective.pool} onResave={() => void saveAndReread(settings ?? draft)} />}
          <Field
            label="Sampling policy"
            hint="The lane decodes greedily on every shipped network — a temperature is not a rule the seat can replay. Mapping sends the request through and prints what ran beside what was asked; refusing answers as the gateway does, by name, before the inference."
          >
            <select
              className="input mt-1"
              value={draft.node.sampling_policy ?? 'greedy_with_notice'}
              onChange={(e) => set('node', { ...draft.node, sampling_policy: e.target.value as Settings['node']['sampling_policy'] })}
            >
              <option value="greedy_with_notice">Map to greedy and say so</option>
              <option value="refuse">Refuse non-greedy requests</option>
            </select>
          </Field>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field
              label="Summarize when more than N turns would be dropped"
              hint="The row is 512 tokens, so a long thread is trimmed oldest-first to fit. A trim that would drop more turns than this becomes a summary job of its own — an inference and a claim like any other, never made up by the app."
            >
              <input
                className="input mt-1"
                type="number"
                min={0}
                value={draft.node.summarize_after_turns ?? 4}
                onChange={(e) => set('node', { ...draft.node, summarize_after_turns: clampInt(e.target.value, 0, 1000, 4) })}
              />
            </Field>
            <Field
              label="Continue legs"
              hint="When an answer hits its length ceiling, how many follow-up jobs continue it (0 to 4). Each leg is its own claim; the seams are listed under the message."
            >
              <input
                className="input mt-1"
                type="number"
                min={0}
                max={4}
                value={draft.node.continue_max_legs ?? 2}
                onChange={(e) => set('node', { ...draft.node, continue_max_legs: clampInt(e.target.value, 0, 4, 2) })}
              />
            </Field>
          </div>
          {/* The lane's transport: the gateway the chat engine posts to and the pool slot it
              answers under. Both are set from the Network tab; this is what the engine holds. */}
          {effective && <EffectiveBlock name="gateway" subsystem={effective.gateway} />}
          {effective && <EffectiveBlock name="pool" subsystem={effective.pool} />}
        </Section>

        <Section
          title="Hugging Face"
          description="Where models are searched for and downloaded from."
          marker={effective && <DiffMarker subsystems={[['catalog', effective.catalog]]} />}
        >
          {effective && <DiffNote name="catalog" subsystem={effective.catalog} />}
          <Field label="Endpoint" hint="Change this for a mirror or an internal proxy.">
            <input
              className="input mt-1"
              value={draft.huggingface.endpoint}
              onChange={(e) => set('huggingface', { ...draft.huggingface, endpoint: e.target.value })}
            />
          </Field>
          <Field label="Access token" hint="Needed for gated repositories, and it raises the rate limit.">
            <input
              className="input mt-1"
              type="password"
              placeholder="none"
              value={draft.huggingface.token ?? ''}
              onChange={(e) => set('huggingface', { ...draft.huggingface, token: e.target.value || null })}
            />
          </Field>
          {effective && <EffectiveBlock name="catalog" subsystem={effective.catalog} />}
        </Section>

        {/* ADR-0096 Decision 10. The manifest is the one table that names every binary and
            artifact with its digest; the Components page is what reads it. */}
        <Section
          title="Components"
          description="One components.json names every binary and class artifact the Studio can spawn or map, with the digest each must hash to. The Components page holds what is on disk to it."
        >
          <Field
            label="Manifest"
            hint="A local path or an https:// URL to a components.json (schema misaka/components/v1); http:// is refused. Empty: the Studio knows only what is on disk, every file it finds is `not-in-manifest`, and nothing can be installed from the Components page."
          >
            <input
              className="input mt-1"
              placeholder="https://…/components-aarch64-apple-darwin.json, or /path/to/components.json"
              value={components.manifest ?? ''}
              onChange={(e) => set('components', { ...components, manifest: e.target.value || null })}
            />
          </Field>
          <Toggle
            label="Read the manifest without being asked"
            checked={components.auto_check}
            onChange={(auto_check) => set('components', { ...components, auto_check })}
            hint="On, every read of the Components page fetches the manifest. Off — for a metered or offline machine — it is fetched only when the page's Check or Verify button asks, or when a component is installed; `misaka-studiod --check` reads it whenever it is set."
          />
          <button type="button" className="btn-ghost" onClick={() => setView('components')}>
            <Icon name="shield" className="size-3.5" />
            Open the Components page
          </button>
        </Section>

        <Section
          title="Provenance"
          description="What the Studio records about its own inferences."
          marker={effective && <DiffMarker subsystems={[['records', effective.records]]} />}
        >
          {effective && <DiffNote name="records" subsystem={effective.records} />}
          <Toggle
            label="Record an inference record per completion"
            checked={draft.provenance.record_inferences}
            onChange={(record_inferences) => set('provenance', { ...draft.provenance, record_inferences })}
            hint="Model identity, runtime identity, and commitments to the prompt and the answer. This is what a future verification layer reads."
          />
          <Toggle
            label="Keep prompt and completion text with each record"
            checked={draft.provenance.keep_transcripts}
            onChange={(keep_transcripts) => set('provenance', { ...draft.provenance, keep_transcripts })}
            hint="Off by default. Records commit to the text with a hash; storing the text as well makes the log a second copy of every conversation. Turn it on only if you need runs to be replayable."
          />
          <Field label="Records kept">
            <input
              className="input mt-1"
              type="number"
              min={100}
              step={100}
              value={draft.provenance.max_records}
              onChange={(e) => set('provenance', { ...draft.provenance, max_records: Number(e.target.value) })}
            />
          </Field>
          {effective && <EffectiveBlock name="records" subsystem={effective.records} />}
        </Section>

        <Section title="Appearance" description="">
          <Field label="Theme">
            <select className="input mt-1" value={draft.ui.theme} onChange={(e) => set('ui', { ...draft.ui, theme: e.target.value as Settings['ui']['theme'] })}>
              <option value="system">Match the system</option>
              <option value="light">Light</option>
              <option value="dark">Dark</option>
            </select>
          </Field>
          <Toggle label="Show the provenance panel" checked={draft.ui.show_provenance} onChange={(show_provenance) => set('ui', { ...draft.ui, show_provenance })} />
          <Toggle label="Show performance figures while generating" checked={draft.ui.show_performance} onChange={(show_performance) => set('ui', { ...draft.ui, show_performance })} />
        </Section>

        {system && (
          <p className="text-center text-[0.7rem] text-ink-500 dark:text-ink-400">
            {system.hardware.cpu_name} · {bytes(system.hardware.total_memory)} · {system.hardware.os}
          </p>
        )}
      </div>

      {dirty && (
        <div className="sticky bottom-0 border-t border-ink-200 bg-white/90 px-4 py-3 backdrop-blur dark:border-ink-800 dark:bg-ink-900/90">
          <div className="mx-auto flex max-w-3xl items-center justify-end gap-2">
            <button type="button" className="btn-ghost" onClick={() => setDraft(settings)}>
              Discard
            </button>
            <button type="button" className="btn-primary" onClick={() => void saveAndReread(draft)}>
              Save settings
            </button>
          </div>
        </div>
      )}
    </div>
  )
}
