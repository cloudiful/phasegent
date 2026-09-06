<script setup lang="ts">
import { computed } from 'vue'
import { useRoute } from 'vue-router'
import type { NavigationMenuItem } from '@nuxt/ui'

const route = useRoute()

const navItems = computed<NavigationMenuItem[]>(() => [
  { label: 'Tasks', icon: 'i-lucide-list-checks', to: '/tasks' },
  { label: 'Status', icon: 'i-lucide-activity', to: '/status' },
  { label: 'Settings', icon: 'i-lucide-settings', to: '/settings' },
])

const sectionTitle = computed(() => (route.meta.title as string | undefined) ?? 'Tasks')
</script>

<template>
  <UApp>
    <div class="flex h-full overflow-hidden bg-default text-default">
      <!-- Persistent left navigation rail (wide windows). -->
      <aside
        class="hidden w-52 shrink-0 flex-col border-r border-default bg-elevated/40 md:flex"
        aria-label="Primary"
      >
        <div class="flex h-12 items-center gap-2 border-b border-default px-4">
          <span
            class="size-2 rounded-full bg-primary"
            aria-hidden="true"
          />
          <span class="text-sm font-semibold">phasegent</span>
        </div>
        <nav
          aria-label="Sections"
          class="flex-1 p-2"
        >
          <UNavigationMenu
            orientation="vertical"
            :items="navItems"
            class="w-full"
          />
        </nav>
      </aside>

      <div class="flex min-w-0 flex-1 flex-col">
        <!-- Compact header. -->
        <header class="flex h-12 shrink-0 items-center gap-3 border-b border-default px-4">
          <span
            class="size-2 rounded-full bg-primary md:hidden"
            aria-hidden="true"
          />
          <span class="text-sm font-semibold md:hidden">phasegent</span>
          <h1 class="hidden text-sm font-semibold md:block">
            {{ sectionTitle }}
          </h1>
          <div class="ms-auto flex items-center gap-2">
            <UBadge
              color="success"
              variant="subtle"
              size="sm"
              icon="i-lucide-circle-check"
            >
              Ready
            </UBadge>
            <UColorModeButton size="sm" />
          </div>
        </header>

        <!-- Compact horizontal nav for narrow windows. -->
        <nav
          aria-label="Sections"
          class="shrink-0 border-b border-default px-2 py-1 md:hidden"
        >
          <UNavigationMenu
            orientation="horizontal"
            :items="navItems"
          />
        </nav>

        <!-- Dense content column. -->
        <main
          class="min-h-0 flex-1 overflow-y-auto"
          :aria-label="sectionTitle"
        >
          <UContainer class="max-w-5xl py-4">
            <RouterView />
          </UContainer>
        </main>
      </div>
    </div>
  </UApp>
</template>
