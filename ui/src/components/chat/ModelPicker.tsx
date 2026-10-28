// Which model answers: a chip in the composer, a status pill in the header, one popover behind both.
//
// The popover is a list, not a select, because the things that decide a choice — quantisation,
// size, whether it fits this machine — belong beside each name. The context size is chosen here
// too, at load time, since that is the only time it can change: an agent needs a window that holds
// a file and its own instructions, and 4K does not.

import { useEffect, useRef, useState, type RefObject } from 'react'
import { bytes, tokens } from '../../lib/format'
import { fuzzyFilter } from '../../lib/slash'
import type { FitVerdict, ModelView } from '../../lib/types'
import { chatModelError, chatModelId, useStudio } from '../../store/studio'
import { FitBadge, Icon, QuantBadge, Spinner } from '../common'
import { Popover, Segmented, StatusDot, type Placement } from '../ui'
import { offloadLabel, offloadTitle } from './offload'

type ContextChoice = 'default' | '8k' | '16k' | '32k'
const CONTEXT_SIZE: Record<ContextChoice, number | undefined> = { default: undefined, '8k': 8192, '16k': 16384, '32k': 32768 }
/** The least an agent thread can work in: a file, the tool protocol and the task at once. */
const AGENT_MIN_CONTEXT = 16384

function shortFit(fit: FitVerdict): string {
  switch (fit.verdict) {
    case 'fits':
      return 'fits'
    case 'tight':
      return 'tight fit'
    case 'partial_offload':
      return 'partly on CPU'
    case 'does_not_fit':
      return "won't fit"
  }
}

/** The popover's content: filter, installed models, context size, unload, and the way to more. */
export function ModelPickerPanel({ close }: { close: () => void }) {
  const models = useStudio((s) => s.models)
  const runtime = useStudio((s) => s.runtime)
  const selectedId = useStudio(chatModelId)
  const error = useStudio(chatModelError)
  const generating = useStudio((s) => s.streaming !== null)
  const loadingModelId = useStudio((s) => s.loadingModelId)
  const loadModel = useStudio((s) => s.loadModel)
  const unloadModel = useStudio((s) => s.unloadModel)
  const setView = useStudio((s) => s.setView)
  const agentThread = useStudio((s) => s.conversations.find((c) => c.id === s.activeConversationId)?.mode === 'agent')
  const [query, setQuery] = useState('')
  const [context, setContext] = useState<ContextChoice>('default')
  const [active, setActive] = useState(0)
  const list = useRef<HTMLDivElement>(null)

  const filtered = fuzzyFilter(query, models, (m) => `${m.id} ${m.name}`)
  useEffect(() => setActive(0), [query])
  useEffect(() => {
    list.current?.querySelector('[data-active="true"]')?.scrollIntoView({ block: 'nearest' })
  }, [active])

  const choose = (model: ModelView) => {
    if (generating || model.fit.verdict === 'does_not_fit') return
    const contextSize = CONTEXT_SIZE[context]
    close()
    // Selecting a resident model must still update this chat's choice.
    void loadModel(model.id, contextSize)
  }

  const smallForAgent = agentThread && runtime?.model_id && runtime.context_size !== null && runtime.context_size < AGENT_MIN_CONTEXT

  return (
    <div className="flex max-h-[inherit] flex-col">
      {error && <p role="alert" className="break-words px-3 py-2 text-xs text-amber-700 dark:text-amber-400">選択は保持されています。{error}</p>}
      <div className="border-b border-ink-200 p-2 dark:border-ink-700">
        <div className="flex items-center gap-2 rounded-lg bg-ink-50 px-2 dark:bg-ink-900">
          <Icon name="search" className="size-3.5 shrink-0 text-ink-400" />
          <input
            autoFocus
            className="h-8 min-w-0 flex-1 bg-transparent text-[0.8rem] outline-none placeholder:text-ink-400"
            placeholder="Filter installed models…"
            aria-label="Filter installed models"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.nativeEvent.isComposing) return
              if (event.key === 'ArrowDown') {
                event.preventDefault()
                setActive((i) => Math.min(i + 1, Math.max(0, filtered.length - 1)))
              } else if (event.key === 'ArrowUp') {
                event.preventDefault()
                setActive((i) => Math.max(i - 1, 0))
              } else if (event.key === 'Enter') {
                event.preventDefault()
                const model = filtered[active]
                if (model) choose(model)
              }
            }}
          />
        </div>
      </div>

      <div ref={list} className="min-h-0 flex-1 overflow-y-auto p-1" role="listbox" aria-label="Installed models">
        {models.length === 0 && (
          <p className="px-2.5 py-3 text-xs text-ink-500 dark:text-ink-400">
            No models are installed yet. <strong>Models → Discover</strong> finds one on Hugging Face, and says what fits this machine
            before anything downloads.
          </p>
        )}
        {models.length > 0 && filtered.length === 0 && <p className="px-2.5 py-3 text-xs text-ink-500 dark:text-ink-400">No installed model matches.</p>}
        {filtered.map((model, index) => {
          const loaded = runtime?.model_id === model.id
          const selected = selectedId === model.id
          const loading = loadingModelId === model.id
          const unavailable = model.fit.verdict === 'does_not_fit'
          return (
            <button
              key={model.id}
              type="button"
              role="option"
              aria-selected={selected}
              aria-disabled={generating || unavailable}
              className="menu-item items-start"
              data-active={index === active}
              onMouseEnter={() => setActive(index)}
              onClick={() => choose(model)}
              title={generating ? '回答が終わるか停止してから変更できます' : model.fit_summary}
            >
              <span className="flex w-4 shrink-0 justify-center pt-0.5">
                {selected ? <Icon name="check" className="size-3.5 text-arc-600 dark:text-arc-400" /> : loading ? <Spinner className="size-3.5 text-arc-600" /> : null}
              </span>
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium text-ink-800 dark:text-ink-100">{model.id}</span>
                <span className="mt-1 flex flex-wrap items-center gap-1 text-[0.65rem] text-ink-500 dark:text-ink-400">
                  <QuantBadge quantization={model.quantization} architecture={model.architecture} />
                  <span>{bytes(model.size_bytes)}</span>
                  {model.context_length ? <span>· {tokens(model.context_length)} trained</span> : null}
                  {model.fit && <FitBadge fit={model.fit} summary={shortFit(model.fit)} />}
                  {selected && <span>· 選択中</span>}
                  {loading && <span>· 準備中…</span>}
                  {unavailable && <span>· この環境では利用できません</span>}
                  {loaded && runtime?.context_size ? <span className="text-arc-700 dark:text-arc-300">· loaded at {tokens(runtime.context_size)}</span> : null}
                </span>
              </span>
            </button>
          )
        })}
      </div>

      <div className="space-y-2 border-t border-ink-200 p-2 dark:border-ink-700">
        <div className="flex items-center gap-2">
          <span className="text-[0.7rem] font-medium text-ink-600 dark:text-ink-300">Context</span>
          <Segmented<ContextChoice>
            value={context}
            onChange={setContext}
            options={[
              { value: 'default', label: 'Default', title: 'The size set under Parameters, or the one the runtime picks for this model and machine' },
              { value: '8k', label: '8K' },
              { value: '16k', label: '16K' },
              { value: '32k', label: '32K' },
            ]}
          />
        </div>
        <p className={`text-[0.65rem] leading-relaxed ${smallForAgent ? 'text-amber-700 dark:text-amber-400' : 'text-ink-500 dark:text-ink-400'}`}>
          {smallForAgent
            ? `The loaded window is ${tokens(runtime?.context_size)} — an agent needs 16K or more. Choose 16K or 32K, then the model, to reload it.`
            : 'Applies when a model loads. Agent threads need 16K or more; a larger window costs memory.'}
        </p>
        <div className="flex items-center gap-1">
          {runtime?.model_id && (
            <button
              type="button"
              className="btn-ghost px-2 py-1 text-xs"
              disabled={generating || loadingModelId !== null}
              onClick={() => {
                close()
                void unloadModel()
              }}
              title="メモリを解放します。モデルの選択は保持され、次の送信時に再読み込みします。"
            >
              <Icon name="power" className="size-3.5" />
              メモリを解放
            </button>
          )}
          <button
            type="button"
            className="btn-ghost ml-auto px-2 py-1 text-xs"
            onClick={() => {
              close()
              setView('models')
            }}
          >
            <Icon name="cube" className="size-3.5" />
            Manage models…
          </button>
        </div>
      </div>
    </div>
  )
}

/** A popover of `ModelPickerPanel` on any trigger. */
function PickerPopover({
  anchor,
  open,
  onClose,
  placement,
}: {
  anchor: RefObject<HTMLButtonElement | null>
  open: boolean
  onClose: () => void
  placement: Placement
}) {
  return (
    <Popover anchor={anchor} open={open} onClose={onClose} placement={placement} className="flex w-[26rem] max-w-[calc(100vw-1rem)] flex-col p-0">
      <ModelPickerPanel close={onClose} />
    </Popover>
  )
}

/** The composer's chip. */
export function ModelPicker() {
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const models = useStudio((s) => s.models)
  const modelId = useStudio(chatModelId)
  const error = useStudio(chatModelError)
  const loadingModelId = useStudio((s) => s.loadingModelId)
  const loading = loadingModelId !== null && loadingModelId === modelId
  const label = modelId ?? (models.length > 0 ? 'モデルを選択' : 'モデルを追加')

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="chip min-w-[5.5rem] max-w-[15rem] shrink"
        aria-label={`Model: ${label}. Change model`}
        aria-expanded={open}
        title={error ?? (loading ? `${modelId} — 準備中…` : modelId ? `${modelId} — 選択中` : 'モデルを追加してください')}
        onClick={() => setOpen((v) => !v)}
      >
        {loading ? <Spinner className="size-3 shrink-0 text-amber-500" /> : <StatusDot tone={error ? 'amber' : modelId ? 'green' : 'gray'} />}
        <span className="min-w-0 truncate">{label}</span>
        <Icon name="chevronDown" className="size-3 shrink-0 opacity-60" />
      </button>
      <PickerPopover anchor={anchor} open={open} onClose={() => setOpen(false)} placement="top-end" />
    </>
  )
}

/** The header's pill: what is loaded, at what window, on which device — and the same picker. */
export function ModelStatusPill() {
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const models = useStudio((s) => s.models)
  const runtime = useStudio((s) => s.runtime)
  const modelId = useStudio(chatModelId)
  const error = useStudio(chatModelError)
  const loadingModelId = useStudio((s) => s.loadingModelId)
  const loading = loadingModelId !== null && loadingModelId === modelId
  const loaded = runtime?.model_id === modelId ? models.find((m) => m.id === modelId) ?? null : null

  const label = modelId ?? 'モデルを追加'
  const offload = loaded ? runtime?.offload ?? null : null

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className="chip min-w-0 max-w-full gap-2"
        aria-label={`${label}. Change model`}
        aria-expanded={open}
        title={error ?? (loading ? '準備中…' : offload ? offloadTitle(offload) : '選択中 — 送信時に自動で準備します')}
        onClick={() => setOpen((v) => !v)}
      >
        <StatusDot tone={loading || error ? 'amber' : modelId ? 'green' : 'gray'} pulse={loading} />
        <span className="min-w-0 truncate text-ink-800 dark:text-ink-100">{label}</span>
        {loading && <span className="shrink-0 text-xs text-ink-500">準備中…</span>}
        {loaded && !loading && runtime && (
          <span className="hidden shrink-0 items-center gap-1.5 font-normal text-ink-500 lg:inline-flex dark:text-ink-400">
            {loaded.quantization && <span className="mono">{loaded.quantization.label}</span>}
            {runtime.context_size ? <span>· {tokens(runtime.context_size)} ctx</span> : null}
            {offload ? (
              // What the engine did, not what it was asked: a CPU run says "CPU" whatever the
              // setting, and a number that is only the request is marked as such.
              <span className={`inline-flex items-center gap-1 ${runtime.offload_note ? 'text-amber-700 dark:text-amber-400' : ''}`}>
                · {offloadLabel(offload, loaded.block_count)}
                {runtime.offload_note && <Icon name="warning" className="size-3.5" />}
              </span>
            ) : runtime.gpu_layers !== null && loaded.block_count ? (
              <span>
                · {Math.min(runtime.gpu_layers, loaded.block_count)}/{loaded.block_count} on GPU
              </span>
            ) : null}
          </span>
        )}
        <Icon name="chevronDown" className="size-3 shrink-0 opacity-60" />
      </button>
      <PickerPopover anchor={anchor} open={open} onClose={() => setOpen(false)} placement="bottom-start" />
    </>
  )
}

/** Failure belongs to the selected model, so it stays visible after a toast would disappear. */
export function ModelSelectionNotice() {
  const id = useStudio(chatModelId)
  const error = useStudio(chatModelError)
  const busy = useStudio((s) => s.loadingModelId !== null || s.streaming !== null)
  if (!id || !error) return null
  return (
    <div role="alert" className="mt-2 rounded-lg bg-amber-50 p-2.5 text-xs text-amber-800 dark:bg-amber-950/40 dark:text-amber-300">
      <p className="break-words">{id} の選択は保持されています。{error}</p>
      <button type="button" className="btn-outline mt-2 px-2 py-1 text-xs" disabled={busy} onClick={() => void useStudio.getState().loadModel(id)}>再試行</button>
    </div>
  )
}
