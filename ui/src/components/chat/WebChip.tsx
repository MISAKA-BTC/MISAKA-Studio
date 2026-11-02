import { useState } from 'react'
import { api } from '../../lib/api'
import type { Conversation } from '../../lib/types'
import { webEnabled } from '../../lib/web'
import { useStudio } from '../../store/studio'
import { Icon } from '../common'

export function WebChip({ conversation, createThread, disabled }: { conversation: Conversation | null; createThread: () => string; disabled: boolean }) {
  const settings = useStudio((s) => s.settings)
  const backend = useStudio((s) => s.runtime?.backend)
  const [saving, setSaving] = useState(false)
  const enabled = webEnabled(conversation ?? undefined, settings, backend)
  const unavailable = backend === 'gateway'
  const toggle = async () => {
    if (!settings || saving) return
    const studio = useStudio.getState()
    if (unavailable) { studio.toast('info', 'Load a local model first. Gateway answers are mined and cannot use Web research.'); return }
    setSaving(true)
    try {
      if (!enabled) {
        const browser = await api.web.browser()
        if (!browser.installed) throw new Error('Install Tor Browser or set its application path in Settings → Web access.')
        if (!settings.web || settings.web.mode === 'off') {
          const saved = await api.saveSettings({ ...settings, web: { ...settings.web, mode: 'tor', tor_proxy: settings.web?.tor_proxy ?? '127.0.0.1:9150', tor_browser_path: settings.web?.tor_browser_path ?? null } })
          useStudio.setState({ settings: saved })
        }
      }
      const id = conversation?.id ?? createThread()
      studio.updateConversation(id, enabled ? { web: false } : { web: true, mine: false })
    } catch (error) { studio.toast('error', (error as Error).message) }
    finally { setSaving(false) }
  }
  return <button type="button" className={`chip shrink-0 ${enabled ? 'bg-violet-100 text-violet-700 dark:bg-violet-950 dark:text-violet-300' : 'text-ink-500'}`}
    disabled={disabled || saving} aria-pressed={enabled} onClick={() => void toggle()}
    title={unavailable ? 'A local model is required for Web research' : enabled ? `Web research on (${settings?.web?.mode}). Mining is off. Pages open in Tor Browser.` : 'Enable Web research and turn mining off for this conversation. Pages open in Tor Browser.'}>
    <Icon name="globe" className="size-3.5" /><span>{saving ? '…' : enabled ? settings?.web?.mode === 'tor' ? 'Web · Tor' : 'Web · Direct' : 'Web off'}</span>
  </button>
}
