import { useEffect, useRef, useState } from 'react'
import type { DesktopApi } from '../api/desktopApi'
import { friendlyDesktopError } from '../api/desktopApi'
import type { SearchResult } from '../types'
import { EmptyState, SectionHeader, StatusPill } from './Common'
import { formatTimestamp } from '../lib/ui'

export function SearchView({
  api,
  onActivate,
  onError,
}: {
  api: DesktopApi
  onActivate: (result: SearchResult) => void
  onError: (message: string) => void
}) {
  const [query, setQuery] = useState('')
  const [results, setResults] = useState<SearchResult[]>([])
  const [loading, setLoading] = useState(false)
  const [failed, setFailed] = useState(false)
  const [selectedIndex, setSelectedIndex] = useState(0)
  const [resultsQuery, setResultsQuery] = useState('')
  const inputRef = useRef<HTMLInputElement>(null)
  const queryRef = useRef('')
  const requestGenerationRef = useRef(0)

  useEffect(() => {
    inputRef.current?.focus()
  }, [])

  useEffect(() => {
    const trimmed = query.trim()
    const generation = requestGenerationRef.current
    if (!trimmed) {
      setResults([])
      setLoading(false)
      setFailed(false)
      setSelectedIndex(0)
      setResultsQuery('')
      return
    }
    let cancelled = false
    const timeout = window.setTimeout(async () => {
      try {
        setLoading(true)
        setFailed(false)
        const next = await api.searchIndex(trimmed)
        if (
          !cancelled &&
          generation === requestGenerationRef.current &&
          queryRef.current.trim() === trimmed
        ) {
          setResults(next)
          setSelectedIndex(0)
          setResultsQuery(trimmed)
        }
      } catch (error) {
        if (!cancelled && generation === requestGenerationRef.current) {
          setFailed(true)
          setResults([])
          setResultsQuery('')
          onError(friendlyDesktopError(error))
        }
      } finally {
        if (!cancelled && generation === requestGenerationRef.current) setLoading(false)
      }
    }, 180)
    return () => {
      cancelled = true
      window.clearTimeout(timeout)
    }
  }, [api, onError, query])

  useEffect(() => {
    document
      .getElementById(`search-result-${selectedIndex}`)
      ?.scrollIntoView?.({ block: 'nearest' })
  }, [selectedIndex])

  return (
    <div className="view-stack search-view">
      <SectionHeader
        title="Search"
        detail="Search entries, reviews, revisions, runs, and connections."
      />

      <label className="global-search-box">
        <span className="sr-only">Search local context</span>
        <span className="search-glyph" aria-hidden="true">
          ⌕
        </span>
        <input
          ref={inputRef}
          type="search"
          aria-label="Search local context"
          value={query}
          placeholder="Search entries, reviews, revisions, runs, and connections"
          onChange={(event) => {
            const nextQuery = event.target.value
            requestGenerationRef.current += 1
            queryRef.current = nextQuery
            setQuery(nextQuery)
            setResults([])
            setResultsQuery('')
            setSelectedIndex(0)
            setFailed(false)
            setLoading(Boolean(nextQuery.trim()))
          }}
          onKeyDown={(event) => {
            if (
              results.length === 0 ||
              loading ||
              resultsQuery !== query.trim()
            ) {
              return
            }
            if (event.key === 'ArrowDown') {
              event.preventDefault()
              setSelectedIndex((selectedIndex + 1) % results.length)
            } else if (event.key === 'ArrowUp') {
              event.preventDefault()
              setSelectedIndex((selectedIndex - 1 + results.length) % results.length)
            } else if (event.key === 'Enter') {
              event.preventDefault()
              onActivate(results[selectedIndex] ?? results[0])
            }
          }}
          aria-controls="global-search-results"
          aria-activedescendant={
            results.length > 0 ? `search-result-${selectedIndex}` : undefined
          }
        />
        {loading ? <span className="mini-spinner" aria-label="Searching" /> : <kbd>↵</kbd>}
      </label>

      <section className="search-result-panel" aria-labelledby="search-result-heading">
        <header className="pane-heading">
          <div>
            <h3 id="search-result-heading">Results</h3>
            <p>
              {loading
                ? 'Searching the local index…'
                : query.trim()
                  ? `${results.length} actionable result${results.length === 1 ? '' : 's'}`
                  : 'Type a private local query'}
            </p>
          </div>
        </header>

        {failed ? (
          <EmptyState
            title="Search is unavailable"
            body="The local index could not be queried. Refresh Connections and try again."
          />
        ) : null}
        {!failed && !query.trim() ? (
          <EmptyState
            title="Search stays on this Mac"
            body="Queries are sent only to the configured local backend and are not displayed as telemetry."
          />
        ) : null}
        {!failed && query.trim() && !loading && results.length === 0 ? (
          <EmptyState
            title="No local results"
            body="Try a title, stable key, source, actor, run, or connection name."
          />
        ) : null}
        {results.length > 0 && resultsQuery === query.trim() ? (
          <ol id="global-search-results" className="global-results">
            {results.map((result, index) => (
              <li key={`${result.kind}-${result.id}`}>
                <button
                  id={`search-result-${index}`}
                  type="button"
                  className={selectedIndex === index ? 'is-selected' : ''}
                  onMouseMove={() => setSelectedIndex(index)}
                  onFocus={() => setSelectedIndex(index)}
                  onClick={() => onActivate(result)}
                >
                  <span className="global-result__score" aria-hidden="true">
                    {String(result.score).padStart(2, '0')}
                  </span>
                  <span className="sr-only">Relevance score {result.score}.</span>
                  <span className="global-result__copy">
                    <span className="row-heading">
                      <strong>{result.title}</strong>
                      <StatusPill label={result.kind} />
                    </span>
                    <span>{result.excerpt}</span>
                    <small>
                      {result.scopeLabel} · {formatTimestamp(result.updatedAt)}
                    </small>
                  </span>
                  <span className="global-result__arrow" aria-hidden="true">
                    →
                  </span>
                </button>
              </li>
            ))}
          </ol>
        ) : null}
      </section>
    </div>
  )
}
