// The window.
//
// Everything long-lived is wired here: the initial fetch, the two SSE subscriptions, and the
// theme. Each is set up once for the life of the window rather than per view, so switching tabs
// does not tear down the metrics stream and lose the sparkline history.

import { useEffect } from 'react'
import { subscribe } from './lib/api'
import type { DownloadProgress, RuntimeSample } from './lib/types'
import { useStudio } from './store/studio'
import { ChatView } from './components/ChatView'
import { ModelsView } from './components/ModelsView'
import { MonitorView } from './components/MonitorView'
import { NetworkView } from './components/NetworkView'
import { SettingsView } from './components/SettingsView'
import { Sidebar } from './components/Sidebar'
import { Icon } from './components/common'

export default function App() {
  const view = useStudio((s) => s.view)
  const bootstrap = useStudio((s) => s.bootstrap)
  const setSample = useStudio((s) => s.setSample)
  const setDownload = useStudio((s) => s.setDownload)
  const setConnected = useStudio((s) => s.setConnected)
  const theme = useStudio((s) => s.settings?.ui.theme)

  useEffect(() => {
    void bootstrap()
  }, [bootstrap])

  useEffect(() => {
    const stopMetrics = subscribe<RuntimeSample>('/api/v1/metrics/stream', setSample, () => setConnected(false))
    const stopDownloads = subscribe<DownloadProgress>('/api/v1/downloads/stream', setDownload)
    return () => {
      stopMetrics()
      stopDownloads()
    }
  }, [setSample, setDownload, setConnected])

  // The theme is mirrored into localStorage so index.html's pre-paint script can apply it before
  // React exists — otherwise every start flashes white before the dark theme loads.
  useEffect(() => {
    const resolve = () => {
      const dark = theme === 'dark' || ((theme === 'system' || theme === undefined) && window.matchMedia('(prefers-color-scheme: dark)').matches)
      document.documentElement.classList.toggle('dark', dark)
    }
    resolve()
    try {
      localStorage.setItem('misaka-studio.theme', theme ?? 'system')
    } catch {
      /* private windows and locked-down profiles both throw here; the theme still applies */
    }
    if (theme && theme !== 'system') return
    const media = window.matchMedia('(prefers-color-scheme: dark)')
    media.addEventListener('change', resolve)
    return () => media.removeEventListener('change', resolve)
  }, [theme])

  return (
    <div className="flex h-full overflow-hidden">
      <Sidebar />
      {/* `min-h-0` is the whole fix for a page that scrolled forever. A flex item defaults to
          `min-height: auto`, which means it refuses to be shorter than its content — so a tall
          view (the Network tab is ~2200px) grew this element past the window instead of being
          clipped by it, and every view's own `h-full overflow-y-auto` then resolved against a
          container taller than itself and never scrolled internally. The document scrolled
          instead, past the end of the app. `overflow-hidden` is the belt to that braces: the
          shell already clips, and saying so here keeps a future overflowing child from escaping
          the same way. */}
      <main className="min-h-0 min-w-0 flex-1 overflow-hidden">
        {view === 'chat' && <ChatView />}
        {view === 'models' && <ModelsView />}
        {view === 'network' && <NetworkView />}
        {view === 'monitor' && <MonitorView />}
        {view === 'settings' && <SettingsView />}
      </main>
      <Toasts />
    </div>
  )
}

function Toasts() {
  const toasts = useStudio((s) => s.toasts)
  const dismiss = useStudio((s) => s.dismissToast)
  if (toasts.length === 0) return null

  return (
    <div className="pointer-events-none fixed bottom-4 right-4 z-50 flex max-h-[calc(100vh-2rem)] w-80 flex-col justify-end gap-2 overflow-y-auto">
      {toasts.map((toast) => (
        <div
          key={toast.id}
          className={`card pointer-events-auto flex items-start gap-2 p-3 text-sm shadow-lg ${
            toast.kind === 'error'
              ? 'border-red-300 text-red-700 dark:border-red-900 dark:text-red-300'
              : toast.kind === 'success'
                ? 'border-emerald-300 text-emerald-800 dark:border-emerald-900 dark:text-emerald-300'
                : ''
          }`}
        >
          {toast.kind === 'error' && <Icon name="warning" className="mt-0.5 size-4 shrink-0" />}
          {/* The message scrolls inside the toast, and keeps its line breaks. An error stays until
              it is dismissed, so the dismiss button is the only way off the screen — and a fifteen
              line engine log grew the toast taller than the window, carried that button off the top
              edge, and left the user with a notification that could not be closed at all. */}
          <span className="max-h-40 min-w-0 flex-1 overflow-y-auto whitespace-pre-wrap break-words">{toast.text}</span>
          <button type="button" className="btn-ghost shrink-0 px-1 py-0.5" onClick={() => dismiss(toast.id)}>
            <Icon name="x" className="size-3.5" />
          </button>
        </div>
      ))}
    </div>
  )
}
