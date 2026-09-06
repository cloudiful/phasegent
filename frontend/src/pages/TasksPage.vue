<script setup lang="ts">
import { computed, ref } from 'vue'
import type { TableColumn } from '@nuxt/ui'
import { usePageData } from '@/composables/usePageData'
import { TASK_STATUSES, TASK_STATUS_LABEL, fetchTasks, filterTasks, formatClock } from '@/mocks'
import type { TaskItem, TaskStatus } from '@/types'

const STATUS_COLOR: Record<TaskStatus, 'neutral' | 'info' | 'warning' | 'success' | 'error'> = {
  queued: 'neutral',
  running: 'info',
  paused: 'warning',
  done: 'success',
  failed: 'error',
}

const filter = ref<TaskStatus | 'all'>('all')
const filterItems = ['all', ...TASK_STATUSES]

const page = usePageData(fetchTasks)
const { state, data, error, fetchedAt, refreshing, stale, refresh } = page

const visible = computed<TaskItem[]>(() => filterTasks(data.value ?? [], filter.value))

const columns: TableColumn<TaskItem>[] = [
  { accessorKey: 'title', header: 'Task' },
  { accessorKey: 'phase', header: 'Phase' },
  { accessorKey: 'status', header: 'Status' },
  { accessorKey: 'progress', header: 'Progress' },
  { accessorKey: 'updatedAt', header: 'Updated' },
]

function clearFilter(): void {
  filter.value = 'all'
}

function filterLabel(value: string): string {
  return value === 'all' ? 'All statuses' : TASK_STATUS_LABEL[value as TaskStatus]
}
</script>

<template>
  <section aria-label="Tasks">
    <div class="mb-3 flex flex-wrap items-center gap-2">
      <div>
        <h2 class="text-base font-semibold">
          Tasks
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
        <USelect
          v-model="filter"
          :items="filterItems"
          size="sm"
          class="w-36"
          aria-label="Filter tasks by status"
          :disabled="state === 'loading'"
        />
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="outline"
          size="sm"
          :loading="refreshing"
          aria-label="Refresh tasks"
          @click="refresh"
        />
      </div>
    </div>

    <!-- Loading -->
    <div
      v-if="state === 'loading'"
      class="space-y-2"
      aria-label="Loading tasks"
      aria-busy="true"
    >
      <USkeleton
        v-for="n in 5"
        :key="n"
        class="h-10 w-full"
      />
    </div>

    <!-- Error (nothing loaded yet) -->
    <UAlert
      v-else-if="state === 'error'"
      color="error"
      variant="subtle"
      icon="i-lucide-triangle-alert"
      title="Tasks could not be loaded"
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

    <template v-else>
      <!-- Inline notice when a background refresh fails. -->
      <UAlert
        v-if="error"
        class="mb-3"
        color="warning"
        variant="subtle"
        icon="i-lucide-triangle-alert"
        title="Refresh failed; showing last known data"
        :description="error"
      />

      <!-- Empty -->
      <div v-if="visible.length === 0">
        <UEmpty
          icon="i-lucide-inbox"
          title="No tasks match this filter"
          description="Try a different status or clear the filter to see all tasks."
        />
        <div class="mt-3 flex justify-center">
          <UButton
            color="neutral"
            variant="outline"
            size="sm"
            @click="clearFilter"
          >
            Clear filter
          </UButton>
        </div>
      </div>

      <!-- Data -->
      <UCard
        v-else
        :ui="{ body: 'p-0 sm:p-0' }"
      >
        <UTable
          :data="visible"
          :columns="columns"
        >
          <template #title-cell="{ row }">
            <div class="min-w-0">
              <p class="truncate font-medium">
                {{ row.original.title }}
              </p>
              <p class="font-mono text-xs text-muted">
                {{ row.original.id }}
              </p>
            </div>
          </template>
          <template #status-cell="{ row }">
            <UBadge
              :color="STATUS_COLOR[row.original.status]"
              variant="subtle"
              size="sm"
            >
              {{ filterLabel(row.original.status) }}
            </UBadge>
          </template>
          <template #progress-cell="{ row }">
            <div class="flex min-w-24 items-center gap-2">
              <UProgress
                :model-value="row.original.progress"
                size="sm"
                class="w-20"
              />
              <span class="text-xs text-muted tabular-nums">{{ row.original.progress }}%</span>
            </div>
          </template>
          <template #updated-cell="{ row }">
            <span class="text-xs tabular-nums">{{ formatClock(row.original.updatedAt) }}</span>
          </template>
        </UTable>
      </UCard>
    </template>
  </section>
</template>
