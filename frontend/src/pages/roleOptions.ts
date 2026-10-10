// Selectable role list for the settings page. Extracted so the role
// inventory stays a single source of truth for the selector and its hint
// text, independent of the page component.

import type { RoleId } from '@/types'

export interface RoleOption {
  label: string
  value: RoleId
  hint: string
}

export const ROLE_ITEMS: RoleOption[] = [
  { label: 'Admin', value: 'admin', hint: 'Bootstrap Redmine projects and provision agent users.' },
  { label: 'Orchestrator', value: 'orchestrator', hint: 'Plan phases and advance workflow state.' },
  { label: 'Executor', value: 'executor', hint: 'Carry out assigned phases and report results.' },
  { label: 'Reviewer', value: 'reviewer', hint: 'Audit the code, verify acceptance, and run the tests.' },
  { label: 'Explore', value: 'explore', hint: 'Read issues for reconnaissance and publish an authorized recon record.' },
]

export function roleHint(role: RoleId): string {
  return ROLE_ITEMS.find(item => item.value === role)?.hint ?? ''
}