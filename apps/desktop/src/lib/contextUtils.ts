import type { ContextPack, PreviewSource, PreviewSection, WorkspaceNode } from '../types'

export interface FlatScopeNode {
  id: string
  label: string
  kind: WorkspaceNode['kind']
  description: string
  status: string
  depth: number
  parentId?: string
}

export function flattenWorkspace(
  workspace: WorkspaceNode[],
  parentId?: string,
  depth = 0,
): FlatScopeNode[] {
  return workspace.flatMap((node) => [
    {
      id: node.id,
      label: node.label,
      kind: node.kind,
      description: node.description,
      status: node.status,
      depth,
      parentId,
    },
    ...flattenWorkspace(node.children, node.id, depth + 1),
  ])
}

export function findScopePath(
  workspace: WorkspaceNode[],
  scopeId: string,
): WorkspaceNode[] {
  for (const node of workspace) {
    if (node.id === scopeId) {
      return [node]
    }

    const nested = findScopePath(node.children, scopeId)
    if (nested.length > 0) {
      return [node, ...nested]
    }
  }

  return []
}

export function composePreviewFromPacks(
  workspace: WorkspaceNode[],
  packs: ContextPack[],
  scopeId: string,
  maxPreviewTokens: number,
) {
  const scopePath = findScopePath(workspace, scopeId)
  const scopeIds = new Set(scopePath.map((node) => node.id))
  const sortedPacks = packs
    .filter((pack) => scopeIds.has(pack.scopeId) && pack.status !== 'draft')
    .sort((left, right) => left.updatedAt.localeCompare(right.updatedAt))

  const sections: PreviewSection[] = sortedPacks.map((pack) => ({
    id: `preview-${pack.id}`,
    title:
      pack.scopeKind === 'task'
        ? 'Task-specific context'
        : pack.scopeKind === 'project'
          ? 'Project context'
          : 'Global context',
    packName: pack.name,
    scopeLabel: pack.scopeLabel,
    scopeKind: pack.scopeKind,
    tokens: pack.tokenEstimate,
    body: pack.body,
  }))

  const sources: PreviewSource[] = sortedPacks.map((pack) => ({
    packId: pack.id,
    packName: pack.name,
    scopeLabel: pack.scopeLabel,
    excerpt: pack.summary,
    tokens: pack.tokenEstimate,
  }))

  const totalTokens = sections.reduce((sum, section) => sum + section.tokens, 0)
  const warnings: string[] = []
  if (packs.some((pack) => scopeIds.has(pack.scopeId) && pack.status === 'draft')) {
    warnings.push('Draft packs exist for this scope and are excluded from the composed preview.')
  }
  if (totalTokens > maxPreviewTokens) {
    warnings.push(`Preview exceeds the ${maxPreviewTokens.toLocaleString()} token budget; trim before export.`)
  }

  const currentScope = scopePath.at(-1)
  return {
    scopeId,
    headline: currentScope
      ? `${currentScope.label} composed preview`
      : 'Composed preview',
    totalTokens,
    warnings,
    sections,
    sources,
  }
}

export function summarizeExcerpt(value: string, maxLength = 110) {
  if (value.length <= maxLength) {
    return value
  }

  return `${value.slice(0, maxLength - 1).trimEnd()}…`
}
