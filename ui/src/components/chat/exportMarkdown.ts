// A thread as a Markdown document — to keep, to paste into an issue, to send to someone.
//
// What the person and the model said, in order, with reasoning folded into `<details>` and errors
// kept as quotes: the export is the transcript as it happened, failures included, not a cleaned-up
// version of it. Attachments are named, not inlined — the file is on the person's disk already, and
// a 200 KB block in the middle of a conversation makes the conversation unreadable. Tool results
// in agent threads are left out for the same reason; the steps that called the tools say so.

import { splitThinking } from '../../lib/reasoning'
import type { Conversation } from '../../lib/types'

export function conversationMarkdown(conversation: Conversation): string {
  const out: string[] = [`# ${conversation.title}`, '']
  const meta = [
    conversation.mode === 'agent' ? `Agent thread${conversation.workspace ? ` in \`${conversation.workspace}\`` : ''}` : 'Chat',
    conversation.modelId ? `model \`${conversation.modelId}\`` : null,
    new Date(conversation.createdAt).toLocaleString(),
  ].filter(Boolean)
  out.push(`_${meta.join(' · ')}_`, '')
  if (conversation.pinned?.length) {
    out.push('**Pinned notes**', '', ...conversation.pinned.map((pin) => `- ${pin}`), '')
  }

  for (const message of conversation.messages) {
    if (message.role === 'system' || message.toolResults) continue
    if (message.role === 'user') {
      out.push('## You', '')
      if (message.content) out.push(message.content, '')
      if (message.attachments?.length) out.push(`_Attached: ${message.attachments.map((a) => `\`${a.path ?? a.name}\``).join(', ')}_`, '')
      continue
    }
    const { thinking, answer } = splitThinking(message.content)
    const reasoning = message.reasoning ?? thinking
    out.push(`## ${message.stats?.model ?? conversation.modelId ?? 'MISAKA'}`, '')
    if (reasoning) out.push('<details><summary>Thinking</summary>', '', reasoning.trim(), '', '</details>', '')
    if (answer.trim()) out.push(answer.trim(), '')
    for (const call of message.toolCalls ?? []) out.push(`- \`${call.name}\` ${call.summary ?? ''} — ${call.status}`.trimEnd())
    if (message.toolCalls?.length) out.push('')
    if (message.error) out.push(`> ⚠ ${message.error.replace(/\n/g, '\n> ')}`, '')
  }
  return `${out.join('\n').trimEnd()}\n`
}

/** A file name from a title: what file systems refuse taken out, everything else (日本語 too) kept. */
export function fileNameFor(title: string): string {
  const base = title.replace(/[\\/:*?"<>|\u0000-\u001f]+/g, ' ').replace(/\s+/g, ' ').trim().slice(0, 80)
  return `${base || 'thread'}.md`
}

/** Save the thread as a `.md` file through the browser's download. */
export function downloadMarkdown(conversation: Conversation): string {
  const name = fileNameFor(conversation.title)
  const blob = new Blob([conversationMarkdown(conversation)], { type: 'text/markdown;charset=utf-8' })
  const url = URL.createObjectURL(blob)
  const link = document.createElement('a')
  link.href = url
  link.download = name
  document.body.appendChild(link)
  link.click()
  link.remove()
  setTimeout(() => URL.revokeObjectURL(url), 1000)
  return name
}
