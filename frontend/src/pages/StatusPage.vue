<script setup lang="ts">
import type { TableColumn } from '@nuxt/ui'
import { usePageData } from '@/composables/usePageData'
import { fetchStatus, formatClock } from '@/mocks'
import type { ConnectionState, StatusEvent, StatusLevel } from '@/types'

const CONNECTION_META: Record<ConnectionState, { label: string, color: 'success' | 'warning' | 'error' }> = {
  connected: { label: 'Connected', color: 'success' },
  degraded: { label: 'Degraded', color: 'warning' },
  offline: { label: 'Offline', color: 'error' },
}

const LEVEL_COLOR: Record<StatusLevel, 'info' | 'success' | 'warning' | 'error'> = {
  info: 'info',
  success: 'success',
  warning: 'warning',
  error: 'error',
}

const page = usePageData(fetchStatus)
const { state, data, error, fetchedAt, refreshing, stale, refresh } = page

const columns: TableColumn<StatusEvent>[] = [
  { accessorKey: 'at', header: 'Time' },
  { accessorKey: 'level', header: 'Level' },
  { accessorKey: 'message', header: 'Event' },
]
</script>

<template>
  <section aria-label="Status">
    <div class="mb-3 flex flex-wrap items-center gap-2">
      <div>
        <h2 class="text-base font-semibold">
          Status
        </h2>
        <p class="text-xs text-muted">
          <template v-if="fetchedAt">
            Updated {{ formatClock(new Date(fetchedAt).toISOString()) }}
          </template>
          <template v-else>
            Awaiting first load
          </template>
        </p>
      </div>
      <div class="ms-auto flex items-center gap-2">
        <UBadge
          v-if="stale && state === 'ready'"
          color="warning"
          variant="subtle"
          size="sm"
        >
          Stale
        </UBadge>
        <UBadge
          v-if="refreshing"
          color="info"
          variant="subtle"
          size="sm"
        >
          Updating
        </UBadge>
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="outline"
          size="sm"
          :loading="refreshing"
          aria-label="Refresh status"
          @click="refresh"
        />
      </div>
    </div>

    <!-- Loading -->
    <div
      v-if="state === 'loading'"
      class="space-y-2"
      aria-label="Loading status"
      aria-busy="true"
    >
      <div class="grid grid-cols-2 gap-2 lg:grid-cols-4">
        <USkeleton
          v-for="n in 4"
          :key="n"
          class="h-20 w-full"
        />
      </div>
      <USkeleton class="h-40 w-full" />
    </div>

    <!-- Error -->
    <UAlert
      v-else-if="state === 'error'"
      color="error"
      variant="subtle"
      icon="i-lucide-triangle-alert"
      title="Status could not be loaded"
      :description="error"
    >
      <template #actions>
        <UButton
          color="error"
          variant="outline"
          size="sm"
          icon="i-lucide-refresh-cw"
          @click="refresh"
        >
          Retry
        </UButton>
      </template>
    </UAlert>

    <template v-else-if="data">
      <UAlert
        v-if="error"
        class="mb-3"
        color="warning"
        variant="subtle"
        icon="i-lucide-triangle-alert"
        title="Refresh failed; showing last known data"
        :description="error"
      />

      <div class="grid grid-cols-2 gap-2 lg:grid-cols-4">
        <UCard>
          <p class="text-xs text-muted">
            Connection
          </p>
          <UBadge
            :color="CONNECTION_META[data.connection].color"
            variant="subtle"
            class="mt-1"
          >
            {{ CONNECTION_META[data.connection].label }}
          </UBadge>
        </UCard>
        <UCard>
          <p class="text-xs text-muted">
            Provider
          </p>
          <p class="mt-1 truncate text-sm font-medium">
            {{ data.provider }}
          </p>
        </UCard>
        <UCard>
          <p class="text-xs text-muted">
            Last sync
          </p>
          <p class="mt-1 text-sm font-medium tabular-nums">
            {{ formatClock(data.lastSyncAt) }}
          </p>
        </UCard>
        <UCard>
          <p class="text-xs text-muted">
            Open / Failed
          </p>
          <p class="mt-1 text-sm font-medium tabular-nums">
            {{ data.totals.queued + data.totals.running + data.totals.paused }} / {{ data.totals.failed }}
          </p>
        </UCard>
      </div>

      <h3 class="mb-2 mt-4 text-sm font-semibold">
        Recent events
      </h3>
      <div v-if="data.recent.length === 0">
        <UEmpty
          icon="i-lucide-activity"
          title="No recent events"
          description="New activity will appear here after the next sync."
        />
      </div>
      <UCard
        v-else
        :ui="{ body: 'p-0 sm:p-0' }"
      >
        <UTable
          :data="data.recent"
          :columns="columns"
        >
          <template #at-cell="{ row }">
            <span class="text-xs tabular-nums">{{ formatClock(row.original.at) }}</span>
          </template>
          <template #level-cell="{ row }">
            <UBadge
              :color="LEVEL_COLOR[row.original.level]"
              variant="subtle"
              size="sm"
              class="capitalize"
            >
              {{ row.original.level }}
            </UBadge>
          </template>
        </UTable>
      </UCard>
    </template>
  </section>
</template>
