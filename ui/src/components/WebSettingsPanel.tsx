import { useState } from 'react'
import { api } from '../lib/api'
import type { Settings } from '../lib/types'
import { Field, Section } from './common'

export function WebSettingsPanel({ web, onChange, onSave }: { web: Settings['web']; onChange: (web: Settings['web']) => void; onSave: () => Promise<void> }) {
  const [checking, setChecking] = useState(false)
  const [status, setStatus] = useState('')
  const [failed, setFailed] = useState(false)
  const check = async () => {
    setChecking(true); setStatus('Opening Tor Browser and checking the saved connection…'); setFailed(false)
    try {
      await onSave()
      const result = await api.web.check()
      setStatus(result.route === 'tor' ? `Tor exit verified by Tor Project. Exit IP: ${result.ip}` : `Direct research connection. Public IP: ${result.ip}. Browser tabs still open in Tor Browser.`)
    } catch (error) { setStatus((error as Error).message); setFailed(true) }
    finally { setChecking(false) }
  }
  return <Section title="Web access & Tor Browser" description="Search and read public web pages in Chat and Code. Enable Web in the composer; this turns mining off for that conversation. Every search and page opens an external Tor Browser tab.">
    <Field label="Model's Web connection">
      <select className="input mt-1" value={web.mode} onChange={(e) => { onChange({ ...web, mode: e.target.value as Settings['web']['mode'] }); setStatus('') }}>
        <option value="off">Off</option><option value="tor">Tor — proxy connection and DNS, no direct fallback</option><option value="direct">Direct — research uses your normal internet connection</option>
      </select>
    </Field>
    <Field label="Tor SOCKS listener" hint="Tor Browser: 127.0.0.1:9150. Standalone Tor: 127.0.0.1:9050. Only loopback IPs are accepted.">
      <input className="input mt-1 mono" value={web.tor_proxy} onChange={(e) => onChange({ ...web, tor_proxy: e.target.value })} />
    </Field>
    <Field label="Tor Browser application / launcher path" hint="Leave blank to detect the installation. On macOS select Tor Browser.app; on Windows Browser/firefox.exe; on Linux Browser/start-tor-browser.">
      <input className="input mt-1" placeholder="Auto-detect Tor Browser" value={web.tor_browser_path ?? ''} onChange={(e) => onChange({ ...web, tor_browser_path: e.target.value || null })} />
    </Field>
    <p className="text-xs text-ink-500">Install Tor Browser from torproject.org and finish its Connect setup. Tor mode covers the built-in Web tools and their DNS. Model downloads, mining and terminal commands use separate connections. This does not make the whole app or computer anonymous. Pages requiring sign-in or JavaScript can be viewed in Tor Browser, but the model reads public text only.</p>
    <button type="button" className="btn" disabled={checking || web.mode === 'off'} onClick={() => void check()}>{checking ? 'Checking…' : 'Save & test connection'}</button>
    {status && <p role="status" className={`text-xs ${failed ? 'text-red-600' : 'text-ink-600 dark:text-ink-300'}`}>{status}</p>}
  </Section>
}
