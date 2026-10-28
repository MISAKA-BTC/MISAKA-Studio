// The sampling parameters, as a drawer under the chat's header.
//
// A drawer rather than a page because the parameters are what a person changes *while* chatting,
// between one answer and the next — the chat stays in view below it. Like the side panel it is
// chrome, not a layer: it stays open until it is closed, and Escape keeps meaning "stop".

import { useUi } from '../../store/ui'
import { ParametersPanel } from '../ParametersPanel'
import { IconButton } from '../ui'

export function ParametersDrawer() {
  const open = useUi((s) => s.parametersOpen)
  const setParametersOpen = useUi((s) => s.setParametersOpen)
  if (!open) return null
  return (
    <div className="max-h-[55vh] shrink-0 overflow-y-auto border-b border-ink-200 bg-white dark:border-ink-800 dark:bg-ink-900">
      <div className="mx-auto max-w-[760px] px-4 py-4">
        <div className="mb-3 flex items-center justify-between">
          <h2 className="text-xs font-semibold text-ink-700 dark:text-ink-200">Parameters</h2>
          <IconButton icon="x" label="Close parameters" onClick={() => setParametersOpen(false)} />
        </div>
        <ParametersPanel />
      </div>
    </div>
  )
}
