// The field a thread's title becomes while it is renamed — in the Chat header and the Code header
// alike, so Enter, Escape and clicking away mean the same thing in both.

import { useState } from 'react'
import type { Conversation } from '../../lib/types'
import { useStudio } from '../../store/studio'

export function TitleInput({ conversation, onDone, label = 'Thread title' }: { conversation: Conversation; onDone: () => void; label?: string }) {
  const renameConversation = useStudio((s) => s.renameConversation)
  const [value, setValue] = useState(conversation.title)
  const commit = () => {
    if (value.trim() && value.trim() !== conversation.title) renameConversation(conversation.id, value)
    onDone()
  }
  return (
    <input
      autoFocus
      className="h-7 w-full min-w-0 rounded-md border border-arc-500/60 bg-white px-2 text-[0.8rem] font-medium outline-none ring-2 ring-arc-500/15 dark:bg-ink-900"
      value={value}
      aria-label={label}
      onFocus={(event) => event.target.select()}
      onChange={(event) => setValue(event.target.value)}
      onBlur={commit}
      onKeyDown={(event) => {
        if (event.nativeEvent.isComposing || event.keyCode === 229) return
        if (event.key === 'Enter') {
          event.preventDefault()
          commit()
        } else if (event.key === 'Escape') {
          // Claimed here, so the window does not also read it as "stop the reply".
          event.preventDefault()
          event.stopPropagation()
          onDone()
        }
      }}
    />
  )
}
