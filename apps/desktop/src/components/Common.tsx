import { useEffect, useRef, type ReactNode } from 'react'
import { statusTone, type StatusTone } from '../lib/ui'

export function StatusPill({
  label,
  tone = statusTone(label),
}: {
  label: string
  tone?: StatusTone
}) {
  const glyph =
    tone === 'positive'
      ? '✓'
      : tone === 'negative'
        ? '!'
        : tone === 'warning'
          ? '•'
          : tone === 'info'
            ? 'i'
            : '–'
  return (
    <span className={`status-pill status-pill--${tone}`}>
      <span aria-hidden="true">{glyph}</span>
      {label}
    </span>
  )
}

export function EmptyState({
  title,
  body,
  children,
}: {
  title: string
  body: string
  children?: ReactNode
}) {
  return (
    <div className="empty-state">
      <div className="empty-state__mark" aria-hidden="true">
        ·
      </div>
      <h3>{title}</h3>
      <p>{body}</p>
      {children ? <div className="empty-state__actions">{children}</div> : null}
    </div>
  )
}

export function SectionHeader({
  eyebrow,
  title,
  detail,
  actions,
}: {
  eyebrow?: string
  title: string
  detail?: string
  actions?: ReactNode
}) {
  return (
    <header className="section-header">
      <div>
        {eyebrow ? <p className="eyebrow">{eyebrow}</p> : null}
        <h2>{title}</h2>
        {detail ? <p className="section-header__detail">{detail}</p> : null}
      </div>
      {actions ? <div className="section-header__actions">{actions}</div> : null}
    </header>
  )
}

export function ModalDialog({
  title,
  description,
  children,
  onClose,
  className = '',
  closeLabel = 'Close dialog',
  focusRestoration,
}: {
  title: string
  description?: string
  children: ReactNode
  onClose?: () => void
  className?: string
  closeLabel?: string
  focusRestoration?: { enabled: boolean }
}) {
  const dialogRef = useRef<HTMLElement>(null)
  const returnFocusRef = useRef<HTMLElement | null>(
    document.activeElement instanceof HTMLElement ? document.activeElement : null,
  )
  const titleId = `dialog-title-${title.toLocaleLowerCase().replace(/[^a-z0-9]+/gu, '-')}`
  const descriptionId = `${titleId}-description`

  useEffect(() => {
    const dialog = dialogRef.current
    const returnTarget = returnFocusRef.current
    const preferred = dialog?.querySelector<HTMLElement>('[data-autofocus]')
    const first = dialog?.querySelector<HTMLElement>(
      'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href]',
    )
    ;(preferred ?? first ?? dialog)?.focus()
    return () => {
      if (focusRestoration?.enabled === false) return
      window.setTimeout(() => {
        if (document.querySelector('[role="dialog"]')) return
        if (returnTarget?.isConnected) {
          returnTarget.focus()
          return
        }
        document.querySelector<HTMLElement>('[data-dialog-fallback]')?.focus()
      }, 0)
    }
  }, [focusRestoration])

  return (
    <div
      className="dialog-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget && onClose) onClose()
      }}
    >
      <section
        ref={dialogRef}
        className={`modal-dialog ${className}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descriptionId : undefined}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key === 'Escape' && onClose) {
            event.preventDefault()
            onClose()
            return
          }
          if (event.key !== 'Tab') return
          const focusable = Array.from(
            event.currentTarget.querySelectorAll<HTMLElement>(
              'button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [href]',
            ),
          )
          if (focusable.length === 0) {
            event.preventDefault()
            event.currentTarget.focus()
            return
          }
          const first = focusable[0]
          const last = focusable.at(-1)
          if (event.shiftKey && document.activeElement === first) {
            event.preventDefault()
            last?.focus()
          } else if (!event.shiftKey && document.activeElement === last) {
            event.preventDefault()
            first.focus()
          }
        }}
      >
        <header className="modal-dialog__header">
          <div>
            <p className="eyebrow">Context Manager</p>
            <h2 id={titleId}>{title}</h2>
            {description ? <p id={descriptionId}>{description}</p> : null}
          </div>
          {onClose ? (
            <button type="button" className="icon-button" onClick={onClose} aria-label={closeLabel}>
              <span aria-hidden="true">×</span>
            </button>
          ) : null}
        </header>
        {children}
      </section>
    </div>
  )
}

export function ConfirmationDialog({
  title,
  description,
  confirmLabel,
  tone = 'primary',
  busy = false,
  detail,
  onCancel,
  onConfirm,
}: {
  title: string
  description: string
  confirmLabel: string
  tone?: 'primary' | 'danger'
  busy?: boolean
  detail?: ReactNode
  onCancel: () => void
  onConfirm: () => void
}) {
  return (
    <ModalDialog title={title} description={description} onClose={busy ? undefined : onCancel}>
      {detail ? <div className="confirmation-detail">{detail}</div> : null}
      <footer className="dialog-actions">
        <button type="button" className="secondary-button" disabled={busy} onClick={onCancel}>
          Cancel
        </button>
        <button
          type="button"
          className={tone === 'danger' ? 'danger-button' : 'primary-button'}
          data-autofocus
          disabled={busy}
          onClick={onConfirm}
        >
          {busy ? 'Working…' : confirmLabel}
        </button>
      </footer>
    </ModalDialog>
  )
}

export function DirtyDecisionDialog({
  itemLabel,
  busy,
  onSave,
  onDiscard,
  onStay,
}: {
  itemLabel: string
  busy: boolean
  onSave: () => void
  onDiscard: () => void
  onStay: () => void
}) {
  return (
    <ModalDialog
      title="Unsaved changes"
      description={`${itemLabel} has changes that are not stored yet.`}
      onClose={busy ? undefined : onStay}
    >
      <p className="dialog-copy">
        Save before moving on, discard the draft, or stay in the editor.
      </p>
      <footer className="dialog-actions dialog-actions--three">
        <button type="button" className="secondary-button" disabled={busy} onClick={onStay}>
          Stay
        </button>
        <button type="button" className="danger-quiet-button" disabled={busy} onClick={onDiscard}>
          Discard
        </button>
        <button
          type="button"
          className="primary-button"
          data-autofocus
          disabled={busy}
          onClick={onSave}
        >
          {busy ? 'Saving…' : 'Save'}
        </button>
      </footer>
    </ModalDialog>
  )
}
