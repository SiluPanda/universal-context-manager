import { fireEvent, render, screen, waitFor } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import App from './App'
import { createDesktopApi } from './api/desktopApi'
import { cloneMockDashboard } from './api/mockData'

describe('App', () => {
  it('renders the desktop control plane overview with scoped packs', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)

    expect(
      await screen.findByRole('heading', { name: 'Desktop control plane' }),
    ).toBeInTheDocument()
    expect(await screen.findByText('Migration review bundle')).toBeInTheDocument()
    expect(screen.getByText('Local daemon connected')).toBeInTheDocument()
  })

  it('supports queue approvals from the review workspace', async () => {
    render(<App api={createDesktopApi({ forceMock: true })} />)

    fireEvent.click(await screen.findByRole('button', { name: /Search & review/i }))
    expect(await screen.findByText('Add explicit restore safety note')).toBeInTheDocument()

    fireEvent.click(screen.getByRole('button', { name: 'Approve' }))

    await waitFor(() => {
      expect(screen.getByText(/Applied approved review update/i)).toBeInTheDocument()
    })
    await waitFor(() => {
      expect(screen.queryByText('Add explicit restore safety note')).not.toBeInTheDocument()
    })
  })

  it('shows the pack editor for an empty first-run scope', async () => {
    const seed = cloneMockDashboard()
    seed.packs = []
    seed.reviewQueue = []
    seed.activity = []
    seed.revisions = []
    seed.selectedScopeId = 'global-root'

    render(<App api={createDesktopApi({ forceMock: true, seed })} />)

    expect(await screen.findByText('No packs in this scope')).toBeInTheDocument()
    expect(screen.getByRole('textbox', { name: 'Pack name' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Save pack' })).toBeInTheDocument()
  })
})
