import { computed, onMounted, onUnmounted, ref } from 'vue'
import type { Ref } from 'vue'
import { isStale } from '@/mocks'
import type { LoadState } from '@/types'

export interface PageData<T> {
  state: Ref<LoadState>
  data: Ref<T | null>
  error: Ref<string>
  fetchedAt: Ref<number | null>
  refreshing: Ref<boolean>
  stale: Ref<boolean>
  refresh: () => Promise<void>
}

/**
 * Loading / ready / empty / error lifecycle with stale-while-revalidate.
 * Refresh keeps previous data on screen and only flips to error when there
 * is nothing to show yet.
 */
export function usePageData<T>(loader: () => Promise<{ data: T, fetchedAt: number }>): PageData<T> {
  const state = ref<LoadState>('loading')
  const data = ref<T | null>(null) as Ref<T | null>
  const error = ref('')
  const fetchedAt = ref<number | null>(null)
  const refreshing = ref(false)
  const now = ref(Date.now())

  let timer: number | undefined

  const stale = computed(() => fetchedAt.value !== null && isStale(fetchedAt.value, now.value))

  async function refresh(): Promise<void> {
    refreshing.value = true
    try {
      const result = await loader()
      data.value = result.data
      fetchedAt.value = result.fetchedAt
      error.value = ''
      state.value = (Array.isArray(result.data) && result.data.length === 0) ? 'empty' : 'ready'
    }
    catch (err) {
      error.value = err instanceof Error ? err.message : 'Request failed.'
      if (data.value === null) state.value = 'error'
    }
    finally {
      refreshing.value = false
    }
  }

  onMounted(() => {
    void refresh()
    timer = window.setInterval(() => { now.value = Date.now() }, 15_000)
  })

  onUnmounted(() => {
    window.clearInterval(timer)
  })

  return { state, data, error, fetchedAt, refreshing, stale, refresh }
}
