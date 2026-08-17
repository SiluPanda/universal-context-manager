import { describe, expect, it } from 'vitest'
import { createDesktopApi } from './desktopApi'

describe('desktopApi mock fallback', () => {
  it('saves packs and updates search results without Tauri', async () => {
    const api = createDesktopApi({ forceMock: true })
    const saved = await api.savePack({
      scopeId: 'task-atlas-review',
      name: 'Focused restore notes',
      status: 'active',
      summary: 'Short restore summary for operators.',
      tags: ['restore', 'operator'],
      body: 'Document how restore keeps immutable audit provenance and rebuilds the adapter bridge.',
    })

    expect(saved.name).toBe('Focused restore notes')

    const results = await api.searchIndex('immutable audit provenance')
    expect(results.some((result) => result.title === 'Focused restore notes')).toBe(true)
  })
})
