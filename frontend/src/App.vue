<script setup lang="ts">
import { watch } from 'vue'
import { RouterView } from 'vue-router'
import MaintenanceScreen from './components/MaintenanceScreen.vue'
import ScrollButtons from './components/ScrollButtons.vue'
import SelectionTtsPopup from './components/SelectionTtsPopup.vue'
import TtsStatusPill from './components/TtsStatusPill.vue'
import { useSettingsStore } from './stores/settings'
import { useSystemStatusStore } from './stores/systemStatus'

const settingsStore = useSettingsStore()
const systemStatusStore = useSystemStatusStore()
systemStatusStore.start()

watch(
  () => settingsStore.siteName,
  (siteName) => {
    document.title = siteName
  },
  { immediate: true },
)
</script>

<template>
  <MaintenanceScreen v-if="systemStatusStore.blocking" />
  <template v-else>
    <RouterView />
    <ScrollButtons />
    <SelectionTtsPopup />
    <TtsStatusPill />
  </template>
</template>
