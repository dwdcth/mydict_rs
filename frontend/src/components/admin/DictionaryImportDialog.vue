<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import DictsDirImportPanel from './DictsDirImportPanel.vue'
import UploadImportPanel from './UploadImportPanel.vue'

const visible = defineModel<boolean>({ required: true })
const emit = defineEmits<{ imported: [] }>()

const importMode = ref<'upload' | 'dicts-dir'>('upload')
const dirsPanel = ref<InstanceType<typeof DictsDirImportPanel> | null>(null)
const uploadPanel = ref<InstanceType<typeof UploadImportPanel> | null>(null)

const batchRunning = computed(() => uploadPanel.value?.running ?? dirsPanel.value?.running ?? false)
const dirsScanning = computed(
  () => uploadPanel.value?.analyzing ?? dirsPanel.value?.scanning ?? false,
)

watch(visible, (open) => {
  if (!open) return
  importMode.value = 'upload'
})

function handleTabChange(name: string | number) {
  importMode.value = name === 'dicts-dir' ? 'dicts-dir' : 'upload'
}

async function submit() {
  if (importMode.value === 'dicts-dir') {
    await dirsPanel.value?.submit()
    return
  }
  await uploadPanel.value?.submit()
  // 全部成功后由用户手动关闭；这里不强制，便于查看分组结果
}
</script>

<template>
  <el-dialog
    v-model="visible"
    title="导入词典"
    width="var(--size-dialog-md)"
    :close-on-click-modal="!batchRunning"
    :close-on-press-escape="!batchRunning"
    :show-close="!batchRunning"
  >
    <el-tabs :model-value="importMode" @tab-change="handleTabChange">
      <el-tab-pane label="上传文件" name="upload" :disabled="batchRunning" />
      <el-tab-pane label="从服务器目录导入" name="dicts-dir" :disabled="batchRunning" />
    </el-tabs>

    <el-form label-position="top">
      <template v-if="importMode === 'upload'">
        <UploadImportPanel ref="uploadPanel" @imported="emit('imported')" />
      </template>
      <DictsDirImportPanel v-else ref="dirsPanel" @imported="emit('imported')" />
    </el-form>

    <template #footer>
      <el-button v-if="batchRunning" @click="uploadPanel?.cancel(); dirsPanel?.cancel()">停止导入剩余</el-button>
      <el-button :disabled="batchRunning" @click="visible = false">取消</el-button>
      <el-button
        type="primary"
        :loading="batchRunning"
        :disabled="importMode === 'dicts-dir' && dirsScanning"
        @click="submit"
      >
        {{ importMode === 'dicts-dir' ? '批量导入' : '开始导入' }}
      </el-button>
    </template>
  </el-dialog>
</template>


