import type { DesktopError, DesktopErrorCode } from '../types'

export class DesktopApiError extends Error {
  readonly detail: DesktopError

  constructor(detail: DesktopError) {
    super(detail.message)
    this.name = 'DesktopApiError'
    this.detail = detail
  }

  get code() {
    return this.detail.code
  }

  get retryable() {
    return this.detail.retryable
  }
}

const knownErrorCodes = new Set<DesktopErrorCode>([
  'secret_detected',
  'unavailable',
  'invalid_import',
  'conflict',
  'permission_denied',
  'not_found',
  'invalid_input',
  'incompatible',
  'confirmation_required',
  'path_grant_required',
  'path_grant_invalid',
  'path_grant_expired',
  'internal',
])

function errorFromMessage(message: string): DesktopError {
  const normalized = message.toLocaleLowerCase()
  if (
    normalized.includes('secret rejected') ||
    normalized.includes('secret detected') ||
    normalized.includes('secret_detected')
  ) {
    return { code: 'secret_detected', message: 'Potential secret detected.', retryable: false }
  }
  if (
    normalized.includes('permission denied') ||
    normalized.includes('permission_denied') ||
    normalized.includes('not accessible')
  ) {
    return {
      code: 'permission_denied',
      message: 'The selected local path is not accessible with current permissions.',
      retryable: false,
    }
  }
  if (normalized.includes('confirmation is required')) {
    return { code: 'confirmation_required', message, retryable: false }
  }
  if (normalized.includes('path grant is required')) {
    return {
      code: 'path_grant_required',
      message: 'A native path grant is required.',
      retryable: false,
    }
  }
  if (normalized.includes('path grant has expired')) {
    return {
      code: 'path_grant_expired',
      message: 'The native path grant expired.',
      retryable: false,
    }
  }
  if (normalized.includes('path grant')) {
    return {
      code: 'path_grant_invalid',
      message: 'The native path grant is invalid.',
      retryable: false,
    }
  }
  if (normalized.includes('conflict') || normalized.includes('changed after')) {
    return { code: 'conflict', message, retryable: false }
  }
  if (normalized.includes('not found') || normalized.includes('unknown ')) {
    return { code: 'not_found', message, retryable: false }
  }
  if (normalized.includes('incompatible') || normalized.includes('newer than this desktop')) {
    return { code: 'incompatible', message, retryable: false }
  }
  if (
    normalized.includes('unavailable') ||
    normalized.includes('transport') ||
    normalized.includes('socket') ||
    normalized.includes('timeout')
  ) {
    return { code: 'unavailable', message, retryable: true }
  }
  if (
    normalized.includes('import') ||
    normalized.includes('bundle') ||
    normalized.includes('unsupported')
  ) {
    return { code: 'invalid_import', message, retryable: false }
  }
  if (
    normalized.includes('invalid') ||
    normalized.includes('must ') ||
    normalized.includes('requires ')
  ) {
    return { code: 'invalid_input', message, retryable: false }
  }
  return {
    code: 'internal',
    message: 'The desktop operation could not be completed.',
    retryable: false,
  }
}

export function normalizeDesktopError(error: unknown): DesktopApiError {
  if (error instanceof DesktopApiError) {
    return error
  }

  const serialized =
    typeof error === 'string' ? error : error instanceof Error ? error.message : undefined
  if (serialized?.trim().startsWith('{')) {
    try {
      return normalizeDesktopError(JSON.parse(serialized))
    } catch {
      // Fall through to safe message classification.
    }
  }

  if (typeof error === 'object' && error !== null) {
    const candidate = error as Partial<DesktopError> & { message?: unknown }
    if (
      typeof candidate.code === 'string' &&
      knownErrorCodes.has(candidate.code as DesktopErrorCode) &&
      typeof candidate.message === 'string' &&
      typeof candidate.retryable === 'boolean'
    ) {
      return new DesktopApiError({
        code: candidate.code as DesktopErrorCode,
        message:
          candidate.code === 'secret_detected' ? 'Potential secret detected.' : candidate.message,
        retryable: candidate.retryable,
      })
    }
  }

  return new DesktopApiError(
    errorFromMessage(serialized ?? String(error)),
  )
}

export function friendlyDesktopError(error: unknown): string {
  const normalized = normalizeDesktopError(error)
  switch (normalized.code) {
    case 'secret_detected':
      return 'Nothing was stored. Remove credentials or tokens and reference them through a secret manager instead.'
    case 'invalid_import':
      return 'This import could not be validated. Choose a supported UCM bundle or instruction file and preview it again.'
    case 'incompatible':
      return 'This desktop build is not compatible with the detected local data or tool version. Update the matching component before retrying.'
    case 'permission_denied':
      return 'The selected path is not accessible. Check its macOS permissions or choose another local path.'
    case 'conflict':
      return 'The local data changed after your preview or selection. Refresh, review the latest state, and try again.'
    case 'unavailable':
      return 'The local service is unavailable. Check Connections, refresh diagnostics, and retry when the daemon is reachable.'
    case 'confirmation_required':
      return 'Confirm this operation in the desktop dialog before continuing.'
    case 'path_grant_required':
      return 'Choose the local path again in the native macOS dialog before continuing.'
    case 'path_grant_expired':
      return 'The one-time path authorization expired. Choose the path again in the native macOS dialog.'
    case 'path_grant_invalid':
      return 'That one-time path authorization does not match this operation or was already used. Choose the path again.'
    case 'not_found':
      return 'That local item is no longer available. Refresh the view and choose another item.'
    case 'invalid_input':
      return 'Review the highlighted values and try again.'
    default:
      return 'The operation could not be completed. Refresh the local state and try again.'
  }
}
