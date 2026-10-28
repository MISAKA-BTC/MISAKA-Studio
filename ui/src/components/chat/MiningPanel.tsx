// The Mining tab of the chat's right panel: what this thread has mined, and the queue behind it.
//
// The badge under each message says what became of that one prompt; this is the same record for
// the whole thread, newest first, with the claim ids to copy and the mined answers — the text the
// chain holds, which need not match what the chat said. The queue itself, with retry and drop, is
// the existing panel, below.

import type { Conversation } from '../../lib/types'
import { MiningQueuePanel } from '../MiningQueuePanel'
import { MinedPrompt, MiningOverview, useChatMiningState } from './mining'

export function MiningPanel({ conversation }: { conversation: Conversation | null }) {
  const state = useChatMiningState(conversation)
  const mined = (conversation?.messages ?? []).filter((m) => m.role === 'user' && m.mining).reverse()

  return (
    <div className="space-y-5 p-3">
      <MiningOverview conversation={conversation} state={state} />

      <section>
        <h3 className="mb-1.5 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">
          Mined from this thread{mined.length > 0 ? ` · ${mined.length}` : ''}
        </h3>
        {mined.length === 0 ? (
          <p className="text-[0.7rem] leading-relaxed text-ink-500 dark:text-ink-400">
            {state.kind === 'inline'
              ? 'Inline, the answers themselves are the mining — there is no queue entry per prompt to list here.'
              : 'No prompt from this thread has been queued yet.'}
          </p>
        ) : (
          <ul className="space-y-1.5">
            {mined.map((message) => (
              <MinedPrompt key={message.id} message={message} />
            ))}
          </ul>
        )}
      </section>

      <section>
        <h3 className="mb-1.5 text-[0.65rem] font-semibold uppercase tracking-wide text-ink-400 dark:text-ink-500">The whole queue</h3>
        <MiningQueuePanel />
      </section>
    </div>
  )
}
