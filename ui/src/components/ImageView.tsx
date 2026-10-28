// Text-to-image (and image-to-image/editing) generation.
//
// A diffusion checkpoint is loaded and unloaded the same way a chat model is from ModelBar, but
// the request/response shape is a resolution and a reference-image list, not a token stream — see
// `crates/misaka-studio-runtime/src/backend/diffusion.rs`. Reference images are what makes a
// multi-reference or editing checkpoint (Qwen-Image-Edit and similar) actually usable here rather
// than just a text-to-image box with an unused feature.

import { useRef, useState } from 'react'
import { bytes } from '../lib/format'
import { useStudio } from '../store/studio'
import { EmptyState, FitBadge, Icon, Spinner } from './common'

type Resolution = { label: string; width: number; height: number }

const DEFAULT_RESOLUTION: Resolution = { label: '1024 × 1024 (square)', width: 1024, height: 1024 }

const RESOLUTIONS: Resolution[] = [
  DEFAULT_RESOLUTION,
  { label: '1024 × 1536 (portrait)', width: 1024, height: 1536 },
  { label: '1536 × 1024 (landscape)', width: 1536, height: 1024 },
  { label: '512 × 512 (fast)', width: 512, height: 512 },
]

/** A `File` as the bare base64 payload the runtime wants — everything before the comma in a
 *  `data:image/png;base64,…` URL is metadata the server has no use for. */
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => resolve((reader.result as string).slice((reader.result as string).indexOf(',') + 1))
    reader.onerror = () => reject(reader.error)
    reader.readAsDataURL(file)
  })
}

function ImageModelBar() {
  const models = useStudio((s) => s.imageModels)
  const runtime = useStudio((s) => s.imageRuntime)
  const loadingId = useStudio((s) => s.loadingImageModelId)
  const loadImageModel = useStudio((s) => s.loadImageModel)
  const unloadImageModel = useStudio((s) => s.unloadImageModel)
  const refreshImageModels = useStudio((s) => s.refreshImageModels)

  const loaded = models.find((m) => m.id === runtime?.model_id) ?? null
  const busy = loadingId !== null

  return (
    <div className="flex flex-wrap items-center gap-3 border-b border-ink-200 px-4 py-2.5 dark:border-ink-800">
      <select
        className="input max-w-72 py-1"
        value={runtime?.model_id ?? ''}
        disabled={busy || models.length === 0}
        onChange={(event) => {
          const id = event.target.value
          if (id) void loadImageModel(id)
          else void unloadImageModel()
        }}
      >
        {models.length === 0 && <option value="">No image models installed</option>}
        {models.length > 0 && <option value="">— no model loaded —</option>}
        {models.map((model) => (
          <option key={model.id} value={model.id}>
            {model.id}
          </option>
        ))}
      </select>
      {busy && <Spinner className="size-4 text-arc-600" />}
      {loaded && (
        <div className="hidden min-w-0 items-center gap-2 text-xs text-ink-500 md:flex dark:text-ink-400">
          <span>{bytes(loaded.size_bytes)}</span>
          {loaded.architecture && <span className="mono">{loaded.architecture}</span>}
          <FitBadge fit={loaded.fit} summary={loaded.fit_summary} />
        </div>
      )}
      <div className="ml-auto flex items-center gap-2">
        {runtime?.model_id && (
          <button type="button" className="btn-ghost" onClick={() => void unloadImageModel()} title="Unload the model and free its memory">
            <Icon name="power" className="size-3.5" />
            <span className="hidden sm:inline">Unload</span>
          </button>
        )}
        <button type="button" className="btn-ghost" onClick={() => void refreshImageModels()} title="Rescan the models directory">
          <Icon name="refresh" className="size-3.5" />
        </button>
      </div>
    </div>
  )
}

type Reference = { name: string; previewUrl: string; base64: string }

function GenerateForm() {
  const runtime = useStudio((s) => s.imageRuntime)
  const generating = useStudio((s) => s.generatingImage)
  const generateImage = useStudio((s) => s.generateImage)
  const [prompt, setPrompt] = useState('')
  const [negativePrompt, setNegativePrompt] = useState('')
  const [resolution, setResolution] = useState(DEFAULT_RESOLUTION)
  const [steps, setSteps] = useState(20)
  const [guidanceScale, setGuidanceScale] = useState(4.5)
  const [seed, setSeed] = useState('')
  const [references, setReferences] = useState<Reference[]>([])
  const fileInput = useRef<HTMLInputElement>(null)

  const canGenerate = runtime?.model_id != null && prompt.trim().length > 0 && !generating

  const addFiles = async (files: FileList | null) => {
    if (!files) return
    const added = await Promise.all(
      Array.from(files)
        .filter((f) => f.type.startsWith('image/'))
        .map(async (file) => ({ name: file.name, previewUrl: URL.createObjectURL(file), base64: await fileToBase64(file) })),
    )
    setReferences((prev) => [...prev, ...added])
  }

  const submit = async () => {
    if (!canGenerate) return
    await generateImage({
      prompt: prompt.trim(),
      negativePrompt: negativePrompt.trim() || undefined,
      width: resolution.width,
      height: resolution.height,
      steps,
      guidanceScale,
      seed: seed.trim() ? Number(seed) : null,
      referenceImages: references.map((r) => r.base64),
    })
  }

  return (
    <div className="card space-y-3 p-4">
      <textarea
        className="input min-h-20 w-full resize-y"
        placeholder="Describe the image…"
        value={prompt}
        onChange={(event) => setPrompt(event.target.value)}
      />
      <textarea
        className="input min-h-12 w-full resize-y"
        placeholder="Negative prompt (optional)"
        value={negativePrompt}
        onChange={(event) => setNegativePrompt(event.target.value)}
      />

      <div className="flex flex-wrap items-center gap-3">
        <select
          className="input py-1"
          value={resolution.label}
          onChange={(event) => setResolution(RESOLUTIONS.find((r) => r.label === event.target.value) ?? DEFAULT_RESOLUTION)}
        >
          {RESOLUTIONS.map((r) => (
            <option key={r.label} value={r.label}>
              {r.label}
            </option>
          ))}
        </select>
        <label className="flex items-center gap-1.5 text-xs text-ink-500 dark:text-ink-400">
          Steps
          <input
            type="number"
            className="input w-16 py-1"
            min={1}
            max={100}
            value={steps}
            onChange={(event) => setSteps(Number(event.target.value))}
          />
        </label>
        <label className="flex items-center gap-1.5 text-xs text-ink-500 dark:text-ink-400">
          Guidance
          <input
            type="number"
            className="input w-16 py-1"
            step={0.5}
            min={0}
            value={guidanceScale}
            onChange={(event) => setGuidanceScale(Number(event.target.value))}
          />
        </label>
        <label className="flex items-center gap-1.5 text-xs text-ink-500 dark:text-ink-400">
          Seed
          <input
            type="number"
            className="input w-24 py-1"
            placeholder="random"
            value={seed}
            onChange={(event) => setSeed(event.target.value)}
          />
        </label>
      </div>

      <div>
        <div className="mb-1.5 flex items-center justify-between text-xs text-ink-500 dark:text-ink-400">
          <span>Reference images — multi-reference generation and editing</span>
          <button type="button" className="btn-ghost px-2 py-1" onClick={() => fileInput.current?.click()}>
            <Icon name="plus" className="size-3.5" />
            Add
          </button>
          <input ref={fileInput} type="file" accept="image/*" multiple hidden onChange={(event) => void addFiles(event.target.files)} />
        </div>
        {references.length > 0 && (
          <div className="flex flex-wrap gap-2">
            {references.map((ref, i) => (
              <div key={ref.previewUrl} className="group relative">
                <img src={ref.previewUrl} alt={ref.name} className="size-16 rounded-lg object-cover" />
                <button
                  type="button"
                  className="absolute -right-1.5 -top-1.5 rounded-full bg-ink-900 p-0.5 text-white opacity-0 transition-opacity group-hover:opacity-100"
                  onClick={() => setReferences((prev) => prev.filter((_, j) => j !== i))}
                >
                  <Icon name="x" className="size-3" />
                </button>
              </div>
            ))}
          </div>
        )}
      </div>

      <button type="button" className="btn-primary" disabled={!canGenerate} onClick={() => void submit()}>
        {generating ? <Spinner className="size-3.5" /> : <Icon name="image" className="size-3.5" />}
        {generating ? 'Generating…' : 'Generate'}
      </button>
      {!runtime?.model_id && <p className="text-xs text-ink-500 dark:text-ink-400">Load an image model above first.</p>}
    </div>
  )
}

function Gallery() {
  const generations = useStudio((s) => s.imageGenerations)
  if (generations.length === 0) return null
  return (
    <div className="space-y-4">
      {generations.map((gen) => (
        <div key={gen.id} className="card p-4">
          <div className="mb-2 text-sm">{gen.prompt}</div>
          <div className="flex flex-wrap gap-3">
            {gen.images.map((src, i) => (
              <a key={i} href={src} download={`${gen.id}-${i}.png`}>
                <img src={src} alt={gen.prompt} className="max-h-64 rounded-lg border border-ink-200 dark:border-ink-800" />
              </a>
            ))}
          </div>
        </div>
      ))}
    </div>
  )
}

export function ImageView() {
  const models = useStudio((s) => s.imageModels)
  const imageRuntime = useStudio((s) => s.imageRuntime)

  return (
    <div className="flex h-full flex-col overflow-hidden">
      <ImageModelBar />
      <div className="min-h-0 flex-1 overflow-y-auto p-4">
        <div className="mx-auto max-w-3xl space-y-4">
          {models.length === 0 ? (
            <EmptyState icon="image" title="No image models installed">
              Download a diffusers-format checkpoint — a folder with <code className="mono">model_index.json</code> — into your models
              directory. Set up the generation sidecar with <code className="mono">sidecars/diffusion/server.py</code>; see{' '}
              <code className="mono">sidecars/diffusion/README.md</code> for setup, including a mock mode to try this panel with no
              downloaded weights.
            </EmptyState>
          ) : (
            <>
              {imageRuntime && !imageRuntime.backend_available && (
                <p className="badge bg-amber-100 text-amber-800 dark:bg-amber-950/60 dark:text-amber-300">
                  Image backend unavailable — set image_backend.kind in Settings, or run sidecars/diffusion/server.py.
                </p>
              )}
              <GenerateForm />
              <Gallery />
            </>
          )}
        </div>
      </div>
    </div>
  )
}
