// Where the loaded model's weights are, said in a few characters — the model pill's last word.
//
// What the engine did, not what it was asked: a CPU run says "CPU" whatever the setting, and a
// number that is only the request is marked with a `?`.

import { bytes } from '../../lib/format'
import type { Offload } from '../../lib/types'

/** `29/29 on MTL0`, `CPU`, or `GPU: engine decides` — with a `?` when nothing verified it. */
export function offloadLabel(offload: Offload, blockCount: number | null): string {
  const unverified = offload.evidence === 'unverified' ? '?' : ''
  if (offload.layers === null) return 'GPU: engine decides'
  if (offload.layers === 0) return 'CPU'
  const total = offload.total_layers ?? (blockCount ? blockCount + 1 : null)
  const device = offload.device?.split(' (')[0] ?? 'GPU'
  return total ? `${Math.min(offload.layers, total)}/${total} on ${device}${unverified}` : `${offload.layers} layers on ${device}${unverified}`
}

export function offloadTitle(offload: Offload): string {
  const where = offload.device ? `Weights on ${offload.device}` : 'Weights in system memory'
  const evidence = { engine_log: "from the engine's own load log", device_list: "from the engine's device list", unverified: 'unverified — the engine is too old to say' }[
    offload.evidence
  ]
  const sizes =
    offload.accelerator_bytes !== null && offload.cpu_bytes !== null
      ? ` · ${bytes(offload.accelerator_bytes)} on the accelerator, ${bytes(offload.cpu_bytes)} on the CPU`
      : ''
  return `${where} (${evidence})${sizes}`
}
