// The composer's drafts — one per thread, kept while the window is open.
//
// A store rather than component state for two reasons. Files dropped anywhere on the chat must reach
// the composer, which is a sibling of the drop zone, not a child. And the composer moves: it sits in
// the middle of an empty thread and at the bottom of a busy one, and a draft that lived in the
// component would be lost every time it changed places — or every time someone looked at another
// thread and came back. Not persisted: attachments can be 200 KB apiece, and localStorage is where
// the conversations live.

import { create } from 'zustand'
import { isDuplicate, readAttachment } from '../../lib/attachments'
import type { Attachment } from '../../lib/types'
import { useStudio } from '../../store/studio'
import { useUi, type AppMode } from '../../store/ui'

export type Draft = { text: string; attachments: Attachment[] }

/** The draft of "no thread yet" on the Chat side: sending from it creates the chat. */
export const NEW_THREAD = '__new__'
/** The same on the Code side, kept apart: a task half-typed for the agent must not turn up as a
 *  chat message when the window switches sides, nor a question as a task. */
export const NEW_SESSION = '__new_session__'

/** A thread's draft; with no thread, the draft of the side named — by default the side in view,
 *  read when called, so a drop or a paste lands in the composer that is on screen. */
export const draftKey = (conversationId: string | null | undefined, side: AppMode = useUi.getState().appMode): string =>
  conversationId ?? (side === 'code' ? NEW_SESSION : NEW_THREAD)

/** One empty draft, shared, so a selector that finds nothing returns the same object every time. */
const EMPTY: Draft = { text: '', attachments: [] }

type ComposerState = {
  drafts: Record<string, Draft>
  setText: (key: string, text: string) => void
  addAttachments: (key: string, attachments: Attachment[]) => void
  removeAttachment: (key: string, id: string) => void
  clear: (key: string) => void
  /** Carry a draft to another thread — typed on a side's empty page, sent in the thread made for it. */
  move: (from: string, to: string) => void
}

const update = (drafts: Record<string, Draft>, key: string, patch: (draft: Draft) => Draft) => {
  const next = patch(drafts[key] ?? EMPTY)
  if (!next.text && next.attachments.length === 0) {
    const rest = { ...drafts }
    delete rest[key]
    return rest
  }
  return { ...drafts, [key]: next }
}

export const useComposer = create<ComposerState>()((set) => ({
  drafts: {},
  setText: (key, text) => set((s) => ({ drafts: update(s.drafts, key, (d) => ({ ...d, text })) })),
  addAttachments: (key, attachments) =>
    set((s) => ({
      drafts: update(s.drafts, key, (d) => {
        const next = [...d.attachments]
        for (const attachment of attachments) if (!isDuplicate(next, attachment)) next.push(attachment)
        return { ...d, attachments: next }
      }),
    })),
  removeAttachment: (key, id) =>
    set((s) => ({ drafts: update(s.drafts, key, (d) => ({ ...d, attachments: d.attachments.filter((a) => a.id !== id) })) })),
  clear: (key) => set((s) => ({ drafts: update(s.drafts, key, () => EMPTY) })),
  move: (from, to) =>
    set((s) => {
      if (from === to) return {}
      const draft = s.drafts[from]
      if (!draft) return {}
      const without = update(s.drafts, from, () => EMPTY)
      return { drafts: update(without, to, (d) => ({ text: d.text || draft.text, attachments: [...d.attachments, ...draft.attachments] })) }
    }),
}))

export function useDraft(key: string): Draft {
  return useComposer((s) => s.drafts[key] ?? EMPTY)
}

/**
 * Read files into a thread's draft: the paperclip, a drop, a paste. A file that cannot be attached
 * is said once, in a toast that clears itself — it is not an error, the file is simply not text.
 */
export async function attachFiles(key: string, files: File[]): Promise<void> {
  const toast = useStudio.getState().toast
  const read: Attachment[] = []
  for (const file of files) {
    const result = await readAttachment(file)
    if (!result.ok) {
      toast('info', result.reason)
      continue
    }
    read.push(result.attachment)
    if (result.attachment.truncated) toast('info', `${result.attachment.name} is larger than 200 KB — only its first 200 KB is attached.`)
  }
  if (read.length) useComposer.getState().addAttachments(key, read)
}
