import { useEffect, useMemo, useRef, useState } from 'react'
import { EmptyState, ModalDialog } from './Common'

export type PrimaryView = 'inbox' | 'library' | 'effective' | 'search' | 'connections'

export type QuickOpenTarget =
  | { type: 'view'; view: PrimaryView }
  | { type: 'scope'; scopeId: string }
  | { type: 'entry'; entryId: string; scopeId: string }
  | { type: 'review'; reviewId: string; scopeId: string }
  | { type: 'revision'; revisionId: string; entityId: string }
  | { type: 'run'; runId: string }
  | { type: 'connection'; connectionId: string }
  | { type: 'new-entry'; scopeId: string }

export type QuickOpenKind =
  | 'command'
  | 'scope'
  | 'entry'
  | 'review'
  | 'revision'
  | 'run'
  | 'connection'

export interface QuickOpenItem {
  id: string
  kind: QuickOpenKind
  title: string
  detail: string
  searchText: string
  target: QuickOpenTarget
  rank: number
}

function normalized(value: string) {
  return value.trim().toLocaleLowerCase()
}

export function QuickOpen({
  items,
  onActivate,
  onClose,
}: {
  items: QuickOpenItem[]
  onActivate: (item: QuickOpenItem) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [selectedIndex, setSelectedIndex] = useState(0)
  const inputRef = useRef<HTMLInputElement>(null)
  const focusRestorationRef = useRef({ enabled: true })
  function activate(item: QuickOpenItem) {
    focusRestorationRef.current.enabled = false
    onActivate(item)
  }
  const filtered = useMemo(() => {
    const raw = normalized(query)
    const commandsOnly = raw.startsWith('>')
    const needle = commandsOnly ? raw.slice(1).trim() : raw
    return items
      .filter((item) => !commandsOnly || item.kind === 'command')
      .map((item) => {
        if (!needle) return { item, match: 0 }
        const title = normalized(item.title)
        const haystack = normalized(`${item.searchText} ${item.detail}`)
        const match = title.startsWith(needle)
          ? 0
          : title.includes(needle)
            ? 1
            : haystack.includes(needle)
              ? 2
              : -1
        return { item, match }
      })
      .filter(({ match }) => match >= 0)
      .sort((left, right) => left.match - right.match || left.item.rank - right.item.rank)
      .slice(0, 14)
      .map(({ item }) => item)
  }, [items, query])

  useEffect(() => {
    setSelectedIndex(0)
  }, [query])

  useEffect(() => {
    if (selectedIndex >= filtered.length) {
      setSelectedIndex(Math.max(0, filtered.length - 1))
    }
  }, [filtered.length, selectedIndex])

  useEffect(() => {
    inputRef.current?.focus()
  }, [])

  useEffect(() => {
    document
      .getElementById(`quick-open-option-${selectedIndex}`)
      ?.scrollIntoView?.({ block: 'nearest' })
  }, [selectedIndex])

  return (
    <ModalDialog
      title="Quick Open"
      description="Jump to a view, scope, entry, review, revision, run, or connection."
      className="quick-open-dialog"
      onClose={onClose}
      closeLabel="Close Quick Open"
      focusRestoration={focusRestorationRef.current}
    >
      <label className="quick-open-search">
        <span className="sr-only">Find context or a command</span>
        <span className="search-glyph" aria-hidden="true">
          ⌕
        </span>
        <input
          ref={inputRef}
          type="search"
          role="combobox"
          aria-label="Find context or a command"
          aria-controls="quick-open-results"
          aria-expanded="true"
          aria-activedescendant={
            filtered.length > 0 ? `quick-open-option-${selectedIndex}` : undefined
          }
          placeholder="Search local context or type > for commands"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onKeyDown={(event) => {
            if (event.key === 'Escape') {
              event.preventDefault()
              onClose()
              return
            }
            if (filtered.length === 0) return
            if (event.key === 'ArrowDown') {
              event.preventDefault()
              setSelectedIndex((selectedIndex + 1) % filtered.length)
            } else if (event.key === 'ArrowUp') {
              event.preventDefault()
              setSelectedIndex((selectedIndex - 1 + filtered.length) % filtered.length)
            } else if (event.key === 'Enter') {
              event.preventDefault()
              activate(filtered[selectedIndex] ?? filtered[0])
            }
          }}
        />
        <kbd aria-hidden="true">⌘K</kbd>
      </label>

      <div className="quick-open-results">
        {filtered.length === 0 ? (
          <EmptyState
            title="No local match"
            body="Try an entry title, scope, review, run, connection, or command."
          />
        ) : (
          <ul id="quick-open-results" role="listbox" aria-label="Quick Open results">
            {filtered.map((item, index) => (
              <li key={item.id} role="presentation">
                <button
                  id={`quick-open-option-${index}`}
                  type="button"
                  role="option"
                  tabIndex={-1}
                  aria-selected={selectedIndex === index}
                  className={`quick-open-result ${
                    selectedIndex === index ? 'quick-open-result--selected' : ''
                  }`}
                  onMouseMove={() => setSelectedIndex(index)}
                  onClick={() => activate(item)}
                >
                  <span className="quick-open-result__copy">
                    <strong>{item.title}</strong>
                    <small>{item.detail}</small>
                  </span>
                  <span className="quick-open-result__kind">{item.kind}</span>
                </button>
              </li>
            ))}
          </ul>
        )}
      </div>
      <footer className="quick-open-hints" aria-hidden="true">
        <span>
          <kbd>↑</kbd>
          <kbd>↓</kbd> move
        </span>
        <span>
          <kbd>↵</kbd> open
        </span>
        <span>
          <kbd>esc</kbd> close
        </span>
      </footer>
    </ModalDialog>
  )
}
