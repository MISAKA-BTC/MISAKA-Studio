// Focus that comes back.
//
// A dialog opened from the keyboard should hand the keyboard back where it found it: ⌘K from the
// composer, Esc, and the cursor is in the composer again. `Modal` does not do this itself, so the
// chrome's own dialogs ask for it.

import { useEffect, useState } from 'react'

/**
 * Put the focus back where it was when this component mounted, once it unmounts — unless something
 * else took the focus in the meantime (a command that focused the composer, another dialog).
 */
export function useRestoreFocus(): void {
  // Read while rendering, before the dialog's own fields take the focus.
  const [previous] = useState(() => (typeof document === 'undefined' ? null : (document.activeElement as HTMLElement | null)))
  useEffect(() => {
    return () => {
      const now = document.activeElement
      if (previous && previous !== document.body && previous.isConnected && (now === null || now === document.body)) {
        previous.focus({ preventScroll: true })
      }
    }
  }, [previous])
}
