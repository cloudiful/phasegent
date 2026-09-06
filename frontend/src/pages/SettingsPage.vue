<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { usePageData } from '@/composables/usePageData'
import {
  clearNonSecretSetting,
  clearRoleCredential,
  fetchConfigSnapshot,
  fetchProvisioningStatus,
  setNonSecretSetting,
  setRoleCredential,
  snapshotEndpointForRole,
  snapshotProviderForRole,
  snapshotRoleEntry,
  testConnection,
} from '@/ipc'
import type { RoleId } from '@/types'

const ROLE_ITEMS: { label: string, value: RoleId, hint: string }[] = [
  { label: 'Admin', value: 'admin', hint: 'Bootstrap Redmine projects and provision agent users.' },
  { label: 'Orchestrator', value: 'orchestrator', hint: 'Plan phases and advance workflow state.' },
  { label: 'Executor', value: 'executor', hint: 'Carry out assigned phases and report results.' },
  { label: 'Reviewer', value: 'reviewer', hint: 'Review phase output independently.' },
  { label: 'Tester', value: 'tester', hint: 'Verify behavior and record test evidence.' },
]

const toast = useToast()

const snapshotPage = usePageData(fetchConfigSnapshot)
const { state: snapshotState, data: snapshot, error: snapshotError, refreshing: snapshotRefreshing, stale: snapshotStale, refresh: refreshSnapshot } = snapshotPage

const activeRole = ref<RoleId>('executor')
const provider = ref('')
const endpoint = ref('')
const credentialInput = ref('')
const credentialProvider = ref('redmine')
const saving = ref(false)
const clearing = ref(false)
const savingCredential = ref(false)
const clearingCredential = ref(false)
const checking = ref(false)
const checkResult = ref<{ ok: boolean, message: string } | null>(null)
const saveError = ref('')
const credentialError = ref('')
const credentialInfo = ref<{ present: boolean, length: number } | null>(null)
const provisioning = ref<{ user_id?: number | null, login?: string | null } | null>(null)
const initialized = ref(false)

const roleHint = computed(() => ROLE_ITEMS.find(item => item.value === activeRole.value)?.hint ?? '')
const roleEntry = computed(() => snapshotRoleEntry(snapshot.value, activeRole.value))
const credentialPresenceText = computed(() => {
  if (!roleEntry.value) return 'Not loaded yet.'
  const key = credentialProvider.value === 'forgejo' ? roleEntry.value.forgejo_credential : credentialProvider.value === 'gitlab' ? roleEntry.value.gitlab_credential : roleEntry.value.redmine_credential
  if (!key) return 'Not configured.'
  if (!key.present) return 'Not configured.'
  return `Configured (length ${key.length ?? 0}). Value is never displayed.`
})

function syncProviderEndpoint(): void {
  if (!snapshot.value) return
  provider.value = snapshotProviderForRole(snapshot.value, activeRole.value)
  endpoint.value = snapshotEndpointForRole(snapshot.value, activeRole.value)
  if (!initialized.value) initialized.value = true
}

function syncCredentialPresence(): void {
  if (!snapshot.value) return
  const entry = snapshotRoleEntry(snapshot.value, activeRole.value)
  if (entry) {
    const key = credentialProvider.value === 'forgejo' ? entry.forgejo_credential : credentialProvider.value === 'gitlab' ? entry.gitlab_credential : entry.redmine_credential
    credentialInfo.value = key ? { present: key.present, length: key.length ?? 0 } : null
  }
  if (!initialized.value) initialized.value = true
}

function syncFromSnapshot(): void {
  syncProviderEndpoint()
  syncCredentialPresence()
}

// Provider/endpoint follow only snapshot or active-role changes so switching
// the credential provider never overwrites unsaved provider/endpoint edits.
// Credential presence follows snapshot, active-role, and credential-provider
// changes separately.
watch([snapshot, activeRole], () => syncFromSnapshot())
watch(credentialProvider, () => syncCredentialPresence())

onMounted(() => {
  void (async () => {
    await refreshSnapshot()
    syncFromSnapshot()
    void refreshProvisioning()
  })()
})

async function refreshProvisioning(): Promise<void> {
  try {
    const status = await fetchProvisioningStatus(activeRole.value)
    provisioning.value = status ? { user_id: status.user_id ?? null, login: status.login ?? null } : null
  }
  catch {
    provisioning.value = null
  }
}

watch(activeRole, () => {
  checkResult.value = null
  saveError.value = ''
  credentialError.value = ''
  credentialInput.value = ''
  void refreshProvisioning()
})

async function checkConnection(): Promise<void> {
  checking.value = true
  try {
    checkResult.value = await testConnection(endpoint.value)
  }
  finally {
    checking.value = false
  }
}

async function persist(): Promise<void> {
  saving.value = true
  saveError.value = ''
  try {
    const trimmedProvider = provider.value.trim()
    const trimmedEndpoint = endpoint.value.trim()
    if (trimmedProvider !== '') {
      await setNonSecretSetting(activeRole.value, 'PHASEGENT_PROVIDER', trimmedProvider)
    }
    if (trimmedEndpoint !== '') {
      await setNonSecretSetting(activeRole.value, 'PHASEGENT_API_BASE', trimmedEndpoint)
    }
    if (trimmedProvider === '' && trimmedEndpoint === '') {
      saveError.value = 'Enter a provider or endpoint before saving.'
      return
    }
    toast.add({ title: 'Settings saved', color: 'success', icon: 'i-lucide-circle-check' })
    await refreshSnapshot()
  }
  catch (err) {
    saveError.value = err instanceof Error ? err.message : 'Save failed.'
  }
  finally {
    saving.value = false
  }
}

async function clearEndpoint(): Promise<void> {
  clearing.value = true
  saveError.value = ''
  try {
    await clearNonSecretSetting(activeRole.value, 'PHASEGENT_API_BASE')
    endpoint.value = ''
    toast.add({ title: 'Endpoint cleared', color: 'success', icon: 'i-lucide-circle-check' })
    await refreshSnapshot()
  }
  catch (err) {
    saveError.value = err instanceof Error ? err.message : 'Clear failed.'
  }
  finally {
    clearing.value = false
  }
}

async function saveCredential(): Promise<void> {
  savingCredential.value = true
  credentialError.value = ''
  try {
    const value = credentialInput.value
    if (value.trim() === '') {
      credentialError.value = 'Enter a credential before saving.'
      return
    }
    const result = await setRoleCredential(activeRole.value, credentialProvider.value, value)
    credentialInput.value = ''
    credentialInfo.value = result
    toast.add({ title: 'Credential saved', color: 'success', icon: 'i-lucide-circle-check' })
    await refreshSnapshot()
  }
  catch (err) {
    credentialError.value = err instanceof Error ? err.message : 'Credential save failed.'
  }
  finally {
    savingCredential.value = false
  }
}

async function clearCredentialAction(): Promise<void> {
  clearingCredential.value = true
  credentialError.value = ''
  try {
    await clearRoleCredential(activeRole.value, credentialProvider.value)
    credentialInfo.value = { present: false, length: 0 }
    credentialInput.value = ''
    toast.add({ title: 'Credential cleared', color: 'success', icon: 'i-lucide-circle-check' })
    await refreshSnapshot()
  }
  catch (err) {
    credentialError.value = err instanceof Error ? err.message : 'Credential clear failed.'
  }
  finally {
    clearingCredential.value = false
  }
}
</script>

<template>
  <section
    aria-label="Settings"
    class="space-y-3"
  >
    <div class="flex flex-wrap items-center gap-2">
      <div>
        <h2 class="text-base font-semibold">
          Settings
        </h2>
        <p class="text-xs text-muted">
          Stored locally on this machine. Secrets are write-only and never displayed.
        </p>
      </div>
      <div class="ms-auto flex items-center gap-2">
        <UBadge
          v-if="snapshotStale && snapshotState === 'ready'"
          color="warning"
          variant="subtle"
          size="sm"
        >
          Stale
        </UBadge>
        <UButton
          icon="i-lucide-refresh-cw"
          color="neutral"
          variant="outline"
          size="sm"
          :loading="snapshotRefreshing"
          aria-label="Refresh settings"
          @click="refreshSnapshot"
        />
      </div>
    </div>

    <UAlert
      v-if="snapshotState === 'error'"
      color="error"
      variant="subtle"
      icon="i-lucide-triangle-alert"
      title="Settings could not be loaded"
      :description="snapshotError"
    >
      <template #actions>
        <UButton
          color="error"
          variant="outline"
          size="sm"
          icon="i-lucide-refresh-cw"
          @click="refreshSnapshot"
        >
          Retry
        </UButton>
      </template>
    </UAlert>

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
          v-model="activeRole"
          :items="ROLE_ITEMS"
          value-key="value"
          label-key="label"
          class="w-full sm:w-64"
        />
      </UFormField>
      <p
        v-if="provisioning && (provisioning.login || provisioning.user_id)"
        class="mt-2 text-xs text-muted"
      >
        Provisioned Redmine user: {{ provisioning.login ?? `#${provisioning.user_id}` }}
      </p>
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
            v-model="provider"
            class="w-full sm:w-64"
            autocomplete="off"
            placeholder="redmine"
          />
        </UFormField>
        <UFormField
          label="Endpoint"
          name="endpoint"
        >
          <div class="flex flex-col gap-2 sm:flex-row">
            <UInput
              v-model="endpoint"
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
        <UAlert
          v-if="saveError"
          color="error"
          variant="subtle"
          icon="i-lucide-triangle-alert"
          title="Save failed"
          :description="saveError"
        />
      </div>
    </UCard>

    <UCard>
      <template #header>
        <h3 class="text-sm font-semibold">
          Credentials
        </h3>
      </template>
      <div class="space-y-3">
        <p class="text-xs text-muted">
          {{ credentialPresenceText }}
        </p>
        <div class="flex flex-col gap-2 sm:flex-row sm:items-end">
          <UFormField
            label="Credential provider"
            name="credential-provider"
            class="sm:w-48"
          >
            <USelect
              v-model="credentialProvider"
              :items="['forgejo', 'redmine', 'gitlab']"
              class="w-full"
            />
          </UFormField>
          <UFormField
            label="New credential"
            name="credential"
            class="flex-1"
          >
            <UInput
              v-model="credentialInput"
              type="password"
              autocomplete="new-password"
              placeholder="Paste token, then Save credential"
              class="w-full"
            />
          </UFormField>
        </div>
        <UAlert
          v-if="credentialError"
          color="error"
          variant="subtle"
          icon="i-lucide-triangle-alert"
          title="Credential action failed"
          :description="credentialError"
        />
        <div class="flex gap-2">
          <UButton
            color="primary"
            size="sm"
            icon="i-lucide-key-round"
            :loading="savingCredential"
            @click="saveCredential"
          >
            Save credential
          </UButton>
          <UButton
            color="neutral"
            variant="outline"
            size="sm"
            :loading="clearingCredential"
            @click="clearCredentialAction"
          >
            Clear credential
          </UButton>
        </div>
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
        @click="persist"
      >
        Save
      </UButton>
      <UButton
        color="neutral"
        variant="outline"
        size="sm"
        :loading="clearing"
        @click="clearEndpoint"
      >
        Clear endpoint
      </UButton>
    </div>
  </section>
</template>
