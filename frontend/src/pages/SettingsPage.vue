<script setup lang="ts">
import { computed, ref } from 'vue'
import { DEFAULT_SETTINGS, loadSettings, saveSettings, testConnection } from '@/mocks'
import type { RoleId, SettingsState } from '@/types'

const ROLE_ITEMS: { label: string, value: RoleId, hint: string }[] = [
  { label: 'Orchestrator', value: 'orchestrator', hint: 'Plan phases and advance workflow state.' },
  { label: 'Executor', value: 'executor', hint: 'Carry out assigned phases and report results.' },
  { label: 'Reviewer', value: 'reviewer', hint: 'Review phase output independently.' },
  { label: 'Viewer', value: 'viewer', hint: 'Read-only access to tasks and status.' },
]

const toast = useToast()

const settings = ref<SettingsState>(loadSettings())
const saving = ref(false)
const checking = ref(false)
const checkResult = ref<{ ok: boolean, message: string } | null>(null)
const lastCheckedAt = ref<number | null>(null)

const roleHint = computed(() => ROLE_ITEMS.find(item => item.value === settings.value.role)?.hint ?? '')

async function checkConnection(): Promise<void> {
  checking.value = true
  try {
    checkResult.value = await testConnection(settings.value.endpoint)
    lastCheckedAt.value = Date.now()
  }
  finally {
    checking.value = false
  }
}

function persist(showToast = true): void {
  saving.value = true
  try {
    saveSettings(settings.value)
    if (showToast) {
      toast.add({ title: 'Settings saved', color: 'success', icon: 'i-lucide-circle-check' })
    }
  }
  finally {
    saving.value = false
  }
}

function resetDefaults(): void {
  settings.value = { ...DEFAULT_SETTINGS }
  checkResult.value = null
  persist()
}
</script>

<template>
  <section
    aria-label="Settings"
    class="space-y-3"
  >
    <div>
      <h2 class="text-base font-semibold">
        Settings
      </h2>
      <p class="text-xs text-muted">
        Stored locally on this machine.
      </p>
    </div>

    <UCard>
      <template #header>
        <h3 class="text-sm font-semibold">
          Role and access
        </h3>
      </template>
      <UFormField
        label="Active role"
        :description="roleHint"
        name="role"
      >
        <USelect
          v-model="settings.role"
          :items="ROLE_ITEMS"
          value-key="value"
          label-key="label"
          class="w-full sm:w-64"
        />
      </UFormField>
    </UCard>

    <UCard>
      <template #header>
        <h3 class="text-sm font-semibold">
          Provider
        </h3>
      </template>
      <div class="space-y-3">
        <UFormField
          label="Provider"
          name="provider"
        >
          <UInput
            v-model="settings.provider"
            class="w-full sm:w-64"
            autocomplete="off"
          />
        </UFormField>
        <UFormField
          label="Endpoint"
          name="endpoint"
        >
          <div class="flex flex-col gap-2 sm:flex-row">
            <UInput
              v-model="settings.endpoint"
              class="w-full sm:max-w-md"
              inputmode="url"
              autocomplete="off"
              placeholder="https://redmine.example.invalid"
            />
            <UButton
              color="neutral"
              variant="outline"
              size="sm"
              icon="i-lucide-plug-zap"
              :loading="checking"
              class="shrink-0 self-start"
              @click="checkConnection"
            >
              Test connection
            </UButton>
          </div>
        </UFormField>
        <UAlert
          v-if="checkResult"
          :color="checkResult.ok ? 'success' : 'error'"
          variant="subtle"
          :icon="checkResult.ok ? 'i-lucide-circle-check' : 'i-lucide-triangle-alert'"
          :title="checkResult.ok ? 'Connection check passed' : 'Connection check failed'"
          :description="checkResult.message"
        />
        <p
          v-else-if="lastCheckedAt === null"
          class="text-xs text-muted"
        >
          Not checked yet.
        </p>
      </div>
    </UCard>

    <UCard>
      <template #header>
        <h3 class="text-sm font-semibold">
          Appearance
        </h3>
      </template>
      <div class="flex flex-col gap-3 sm:flex-row sm:items-center">
        <UFormField
          label="Theme"
          name="theme"
          class="sm:w-64"
        >
          <UColorModeSelect class="w-full" />
        </UFormField>
      </div>
    </UCard>

    <div class="flex gap-2">
      <UButton
        color="primary"
        size="sm"
        icon="i-lucide-check"
        :loading="saving"
        @click="persist()"
      >
        Save
      </UButton>
      <UButton
        color="neutral"
        variant="outline"
        size="sm"
        @click="resetDefaults"
      >
        Reset defaults
      </UButton>
    </div>
  </section>
</template>
