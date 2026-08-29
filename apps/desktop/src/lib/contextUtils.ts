import type { WorkspaceNode } from '../types'

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
    if (node.id === scopeId) return [node]
    const nested = findScopePath(node.children, scopeId)
    if (nested.length > 0) return [node, ...nested]
  }
  return []
}

export function summarizeExcerpt(value: string, maxLength = 110) {
  if (value.length <= maxLength) return value
  return `${value.slice(0, maxLength - 1).trimEnd()}…`
}
