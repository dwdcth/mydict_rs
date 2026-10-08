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

// 词典字体兜底：聚合各启用词典 CSS 的 @font-face（PUA 生僻字字形），
// 配合 typography.css 里栈末的 MydictDictFont 做逐字回退。命不中的字
// 浏览器不会下载字体文件，无额外开销。v 随聚合逻辑变化递增（击穿缓存）
if (typeof document !== 'undefined' && !document.getElementById('mydict-ui-fonts')) {
  const link = document.createElement('link')
  link.id = 'mydict-ui-fonts'
  link.rel = 'stylesheet'
  link.href = '/api/dict/ui-fonts.css?v=2'
  document.head.appendChild(link)
}

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
