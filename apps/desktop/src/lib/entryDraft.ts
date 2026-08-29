import type { ContextEntry, ContextPack, EntryFormat, SaveEntryInput } from '../types'

export interface EntryDraft {
  id?: string
  scopeId: string
  packId: string
  packName: string
  key: string
  title: string
  kind: string
  format: EntryFormat
  body: string
  tags: string
  locked: boolean
}

export function emptyEntryDraft(scopeId: string, pack?: ContextPack): EntryDraft {
  return {
    scopeId,
    packId: pack?.id ?? '',
    packName: pack?.name ?? '',
    key: '',
    title: '',
    kind: 'instruction',
    format: 'markdown',
    body: '',
    tags: '',
    locked: false,
  }
}

export function draftFromEntry(entry: ContextEntry): EntryDraft {
  return {
    id: entry.id,
    scopeId: entry.scopeId,
    packId: entry.packId,
    packName: entry.packName,
    key: entry.key,
    title: entry.title ?? '',
    kind: entry.kind,
    format: entry.format,
    body: entry.body,
    tags: entry.tags.join(', '),
    locked: entry.locked,
  }
}

function normalizedTags(value: string) {
  return value
    .split(',')
    .map((tag) => tag.trim())
    .filter(Boolean)
}

export function entryDraftToInput(draft: EntryDraft): SaveEntryInput {
  return {
    id: draft.id,
    scopeId: draft.scopeId,
    packId: draft.packId || undefined,
    packName: draft.packName || undefined,
    key: draft.key.trim(),
    title: draft.title.trim() || undefined,
    kind: draft.kind.trim(),
    format: draft.format,
    body: draft.body,
    tags: normalizedTags(draft.tags),
    locked: draft.locked,
    actor: 'desktop-operator',
    note: draft.id ? 'Saved from the desktop entry editor.' : 'Created in the desktop entry editor.',
  }
}

export function isEntryDraftDirty(draft: EntryDraft, entry?: ContextEntry) {
  if (!entry) {
    return Boolean(
      draft.key.trim() ||
        draft.title.trim() ||
        draft.body.trim() ||
        draft.tags.trim() ||
        draft.locked ||
        draft.format !== 'markdown' ||
        draft.kind !== 'instruction',
    )
  }
  return (
    draft.scopeId !== entry.scopeId ||
    draft.packId !== entry.packId ||
    draft.key !== entry.key ||
    draft.title !== (entry.title ?? '') ||
    draft.kind !== entry.kind ||
    draft.format !== entry.format ||
    draft.body !== entry.body ||
    normalizedTags(draft.tags).join('\u0000') !== entry.tags.join('\u0000') ||
    draft.locked !== entry.locked
  )
}
