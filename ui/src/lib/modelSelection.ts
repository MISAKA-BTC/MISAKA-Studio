import type { ModelView } from './types'

type Model = Pick<ModelView, 'id' | 'path' | 'fit'>

/** User choices survive an unload, failed load, offline runtime, or a missing model file.
 * Only a session with no choice yet needs a default. Prefer a local chat model over a mining
 * artifact when no configured or running model can supply that default. */
export function defaultModelId(input: {
  preferredModelId: string | null
  models: Model[]
  runtime?: { model_id: string | null } | null
  settings?: { load_on_start: string | null } | null
}): string | null {
  if (input.preferredModelId) return input.preferredModelId
  const installed = (id: string | null | undefined) => input.models.find((m) => m.id === id)
  const running = installed(input.runtime?.model_id)
  if (running) return running.id
  const startup = installed(input.settings?.load_on_start)
  const usable = input.models.filter((m) => m.fit.verdict !== 'does_not_fit')
  const local = (m: Model) => /\.gguf$/i.test(m.path)
  return (startup && local(startup) && startup.fit.verdict !== 'does_not_fit' ? startup.id : null)
    ?? usable.find(local)?.id
    ?? (startup?.fit.verdict !== 'does_not_fit' ? startup?.id : null)
    ?? usable[0]?.id
    ?? input.models[0]?.id
    ?? null
}
