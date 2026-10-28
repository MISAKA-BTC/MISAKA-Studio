// Prompt mining in the composer: always visible, one word for its state, the details a click away.
//
// It sits beside the model picker on purpose. Whether the next prompt will be mined is a property
// of the message about to be sent, decided by the same two things — which engine answers, and
// where the queue stands — and someone who wonders should not have to go to the Network tab to
// find out. The chip never claims more than the queue says (see `chatMiningState`).

import { useRef, useState } from 'react'
import type { Conversation } from '../../lib/types'
import { Icon } from '../common'
import { Popover } from '../ui'
import { MiningOverview, useChatMiningState } from './mining'

/** `compact`: a crowded toolbar (an agent thread's) — the label shows only when the card is wide. */
export function MiningChip({ conversation, compact = false }: { conversation: Conversation | null; compact?: boolean }) {
  const anchor = useRef<HTMLButtonElement>(null)
  const [open, setOpen] = useState(false)
  const state = useChatMiningState(conversation)

  const close = () => setOpen(false)
  const live = state.kind === 'background' || state.kind === 'off-here' ? state.live : 0
  const label = { inline: 'Inline', background: 'Mining', 'off-here': 'Off here', setup: 'Set up' }[state.kind]
  const title = {
    inline: 'Inline mining: answers are the mining work',
    background: 'Prompts in this thread are mined in the background',
    'off-here': 'Background mining is on, but not for this thread',
    setup: 'Prompt mining is not running for this chat',
  }[state.kind]
  const active = state.kind === 'inline' || state.kind === 'background'

  return (
    <>
      <button
        ref={anchor}
        type="button"
        className={`chip shrink-0 ${
          active
            ? 'bg-emerald-50 text-emerald-700 hover:bg-emerald-100 hover:text-emerald-800 dark:bg-emerald-950/40 dark:text-emerald-400 dark:hover:bg-emerald-950/70 dark:hover:text-emerald-300'
            : 'text-ink-500 dark:text-ink-400'
        }`}
        aria-label={`Prompt mining: ${label}`}
        aria-expanded={open}
        title={title}
        onClick={() => setOpen((v) => !v)}
      >
        <Icon name="pickaxe" className="size-3.5" />
        <span className={compact ? 'hidden @2xl:inline' : 'hidden @md:inline'}>{label}</span>
        {live > 0 && (
          <span className="rounded bg-amber-100 px-1 text-[0.65rem] font-semibold tabular-nums text-amber-800 dark:bg-amber-950/70 dark:text-amber-300">
            {live}
          </span>
        )}
      </button>
      <Popover anchor={anchor} open={open} onClose={close} placement="top-end" className="w-80 p-3">
        <MiningOverview conversation={conversation} state={state} onNavigate={close} />
      </Popover>
    </>
  )
}
