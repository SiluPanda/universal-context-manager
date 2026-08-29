import type { ReactNode } from 'react'
import type { ScopeKind } from '../types'

export type StatusTone = 'positive' | 'warning' | 'negative' | 'neutral' | 'info'

export interface ConfirmationRequest {
  title: string
  description: string
  confirmLabel: string
  tone?: 'primary' | 'danger'
  detail?: ReactNode
  action: () => Promise<void> | void
}

export function formatTimestamp(value?: string) {
  if (!value) return 'Not recorded'
  const date = new Date(value)
  if (Number.isNaN(date.valueOf())) return value
  return new Intl.DateTimeFormat('en-US', {
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  }).format(date)
}

export function formatBytes(value: number) {
  if (value < 1_024) return `${value} B`
  if (value < 1_048_576) return `${(value / 1_024).toFixed(1)} KB`
  return `${(value / 1_048_576).toFixed(1)} MB`
}

export function formatQueueAge(seconds: number) {
  if (seconds < 60) return 'less than a minute'
  if (seconds < 3_600) return `${Math.floor(seconds / 60)}m`
  if (seconds < 86_400) return `${Math.floor(seconds / 3_600)}h`
  return `${Math.floor(seconds / 86_400)}d`
}

export function scopeLayerLabel(kind: ScopeKind) {
  if (kind === 'global') return 'All projects'
  if (kind === 'project') return 'This repository'
  return 'This task'
}

export function scopeLayerDetail(kind: ScopeKind) {
  if (kind === 'global') return 'Shared intentionally across registered projects'
  if (kind === 'project') return 'Durable repository context'
  return 'Derived task context'
}

export function statusTone(value: string): StatusTone {
  const normalized = value.toLocaleLowerCase()
  if (
    ['active', 'healthy', 'completed', 'approved', 'applied', 'new', 'readable'].includes(
      normalized,
    )
  ) {
    return 'positive'
  }
  if (
    [
      'draft',
      'review',
      'degraded',
      'starting',
      'migration_required',
      'pending',
      'medium',
      'conflict',
      'duplicate',
      'partial failure',
      'follow-up required',
      'counts unavailable',
    ].includes(normalized)
  ) {
    return 'warning'
  }
  if (
    [
      'deleted',
      'offline',
      'failed',
      'blocked',
      'high',
      'incompatible',
      'not_installed',
      'stopped',
      'rejected',
    ].includes(normalized)
  ) {
    return 'negative'
  }
  if (normalized === 'low') return 'positive'
  if (normalized === 'ignored') return 'neutral'
  return 'info'
}

export function isTextEditingTarget(target: EventTarget | null) {
  if (!(target instanceof HTMLElement)) return false
  return (
    target.matches('input, textarea, select, [contenteditable="true"]') ||
    Boolean(target.closest('input, textarea, select, [contenteditable="true"]'))
  )
}
