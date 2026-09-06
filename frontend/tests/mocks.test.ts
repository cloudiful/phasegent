import { describe, expect, test } from 'bun:test'
import {
  MOCK_TASKS,
  filterTasks,
  formatClock,
  isStale,
  summarizeTasks,
  validateEndpoint,
} from '../src/mocks'

describe('filterTasks', () => {
  test('returns every task for the all filter', () => {
    expect(filterTasks(MOCK_TASKS, 'all')).toHaveLength(MOCK_TASKS.length)
  })

  test('selects a single status', () => {
    const running = filterTasks(MOCK_TASKS, 'running')
    expect(running.length).toBeGreaterThan(0)
    expect(running.every(task => task.status === 'running')).toBe(true)
  })

  test('returns an empty list when nothing matches', () => {
    const doneOnly = filterTasks(MOCK_TASKS, 'done')
    const filtered = filterTasks(doneOnly, 'queued')
    expect(filtered).toEqual([])
  })
})

describe('summarizeTasks', () => {
  test('counts sum to the input length', () => {
    const totals = summarizeTasks(MOCK_TASKS)
    const sum = totals.queued + totals.running + totals.paused + totals.done + totals.failed
    expect(sum).toBe(MOCK_TASKS.length)
  })

  test('empty input yields zero counts', () => {
    expect(summarizeTasks([])).toEqual({ queued: 0, running: 0, paused: 0, done: 0, failed: 0 })
  })
})

describe('isStale', () => {
  test('fresh timestamps are not stale', () => {
    expect(isStale(1_000, 2_000, 120_000)).toBe(false)
  })

  test('timestamps past the budget are stale', () => {
    expect(isStale(0, 120_001, 120_000)).toBe(true)
  })
})

describe('validateEndpoint', () => {
  test('blank endpoint is rejected', () => {
    expect(validateEndpoint('   ')).not.toBeNull()
  })

  test('non-URL endpoint is rejected', () => {
    expect(validateEndpoint('not-a-url')).not.toBeNull()
  })

  test('https endpoint is accepted', () => {
    expect(validateEndpoint('https://redmine.example.invalid')).toBeNull()
  })
})

describe('formatClock', () => {
  test('formats an ISO timestamp as HH:MM:SS', () => {
    expect(formatClock('2026-09-06T19:40:00Z')).toMatch(/^\d{2}:\d{2}:\d{2}$/)
  })

  test('passes invalid input through unchanged', () => {
    expect(formatClock('bogus')).toBe('bogus')
  })
})
