<script setup lang="ts">
import { computed, reactive, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { Folder, UploadFilled } from '@element-plus/icons-vue'
import * as dictApi from '../../api/admin/dictionaries'
import {
  PollAbortedError,
  errorMessage,
  resultNumber,
  resultString,
  useImportTask,
} from '../../composables/useImportTask'
import { langLabel } from '../../utils/language'
import { formatSize } from '../../utils/format'
import type { DictionaryFormat, DictsDirGroup } from '../../types/dictionary'

const emit = defineEmits<{ imported: [] }>()

type GroupImportStatus = 'pending' | 'imported' | 'importing' | 'success' | 'error' | 'blocked'

const FORMAT_LABELS: Record<DictionaryFormat, string> = {
  mdict: 'MDict',
  stardict: 'StarDict',
  ecdict: 'ECDICT',
}

const { waitForImportTask, isUnmounted } = useImportTask()

const analyzing = ref(false)
/** 上传进度（0-100；到 99 后是服务器解压/识别，100=完成） */
const uploadPercent = ref(0)
const uploadId = ref('')
const groups = ref<DictsDirGroup[]>([])
const skipped = ref<string[]>([])
const selectedGroupKeys = ref<string[]>([])
const groupNames = reactive<Record<string, string>>({})
const groupStatus = reactive<Record<string, GroupImportStatus>>({})
const groupError = reactive<Record<string, string>>({})
const groupLangs = reactive<Record<string, string>>({})
const groupWordCounts = reactive<Record<string, number>>({})
const batchRunning = ref(false)
const batchCancelled = ref(false)
const batchSummary = ref('')
// lite：只落词头、释义按需读源文件（磁盘约 1x、导入秒级）
// full：释义落库（FTS 等高级查询的前提；导入更慢、占库容）
const importMode = ref<'lite' | 'full'>('lite')

const fileInput = ref<HTMLInputElement | null>(null)
const folderInput = ref<HTMLInputElement | null>(null)
const dragging = ref(false)

const importableGroups = computed(() => groups.value.filter((group) => group.importable))
const allSelected = computed(
  () =>
    importableGroups.value.length > 0 &&
    importableGroups.value.every((group) => selectedGroupKeys.value.includes(group.key)),
)
const someSelected = computed(
  () => selectedGroupKeys.value.length > 0 && !allSelected.value,
)
const selectedTotalSize = computed(() =>
  importableGroups.value
    .filter((group) => selectedGroupKeys.value.includes(group.key))
    .reduce((sum, group) => sum + group.total_size, 0),
)

function pickFiles() {
  fileInput.value?.click()
}

function pickFolder() {
  folderInput.value?.click()
}

function onInputChange(event: Event) {
  const input = event.target as HTMLInputElement
  const files = input.files ? Array.from(input.files) : []
  // webkitdirectory 时每个 File 带 webkitRelativePath（目录结构原样保留）
  const items = files.map((file) => ({
    file,
    relPath: (file as File & { webkitRelativePath?: string }).webkitRelativePath || file.name,
  }))
  input.value = ''
  if (items.length) void analyze(items)
}

async function onDrop(event: DragEvent) {
  dragging.value = false
  const items = await collectFromDataTransfer(event.dataTransfer)
  if (items.length) void analyze(items)
}

/** 拖拽支持整个文件夹（递归读取目录项）；浏览器不支持时退回普通文件列表 */
async function collectFromDataTransfer(dt: DataTransfer | null): Promise<{ file: File; relPath: string }[]> {
  if (!dt) return []
  const out: { file: File; relPath: string }[] = []
  const entries = Array.from(dt.items ?? [])
    .map((item) => (item as DataTransferItem & { webkitGetAsEntry?: () => FileSystemEntry | null }).webkitGetAsEntry?.())
    .filter((entry): entry is FileSystemEntry => Boolean(entry))
  if (entries.length) {
    for (const entry of entries) await walkEntry(entry, '', out)
    return out
  }
  return Array.from(dt.files ?? []).map((file) => ({ file, relPath: file.name }))
}

async function walkEntry(entry: FileSystemEntry, prefix: string, out: { file: File; relPath: string }[]) {
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as FileSystemFileEntry).file(resolve, reject),
    )
    out.push({ file, relPath: prefix + entry.name })
  } else if (entry.isDirectory) {
    const reader = (entry as FileSystemDirectoryEntry).createReader()
    for (;;) {
      const batch = await new Promise<FileSystemEntry[]>((resolve, reject) =>
        reader.readEntries(resolve, reject),
      )
      if (batch.length === 0) break
      for (const child of batch) await walkEntry(child, `${prefix}${entry.name}/`, out)
    }
  }
}

async function analyze(items: { file: File; relPath: string }[]) {
  analyzing.value = true
  uploadPercent.value = 0
  batchSummary.value = ''
  try {
    const form = new FormData()
    for (const item of items) {
      // 用相对路径当文件名：服务器按目录结构落盘、解压 zip、自动分组
      form.append('files', item.file, item.relPath)
    }
    const analysis = await dictApi.analyzeUpload(form, (percent) => {
      // >99% 后进入服务器解压/识别阶段（那时没有上传进度可言）
      uploadPercent.value = Math.min(percent, 99)
    })
    uploadId.value = analysis.upload_id
    groups.value = analysis.groups
    skipped.value = analysis.skipped
    for (const group of analysis.groups) {
      groupNames[group.key] = group.name
      groupStatus[group.key] = group.importable ? 'pending' : 'blocked'
      delete groupError[group.key]
      delete groupLangs[group.key]
      delete groupWordCounts[group.key]
    }
    selectedGroupKeys.value = analysis.groups
      .filter((group) => group.importable)
      .map((group) => group.key)
    if (analysis.groups.length === 0) {
      ElMessage.warning('没有识别出可导入的词典（支持 .mdx/.mdd、StarDict、ECDICT CSV 与 .zip 打包）')
    }
    uploadPercent.value = 100
  } finally {
    analyzing.value = false
  }
}

function toggleAllGroups(checked: string | number | boolean) {
  selectedGroupKeys.value = checked
    ? importableGroups.value.map((group) => group.key)
    : []
}

function groupStatusLabel(group: DictsDirGroup) {
  switch (groupStatus[group.key]) {
    case 'blocked':
      return group.reason ?? '文件不完整'
    case 'importing':
      return '导入中…'
    case 'success':
      return '成功'
    case 'error':
      return groupError[group.key] ?? '失败'
    default:
      return '待导入'
  }
}

function groupStatusTagType(group: DictsDirGroup) {
  switch (groupStatus[group.key]) {
    case 'success':
      return 'success'
    case 'error':
      return 'danger'
    case 'importing':
    case 'blocked':
      return 'warning'
    default:
      return 'info'
  }
}

async function submit() {
  if (!uploadId.value) {
    ElMessage.warning('请先选择要上传的文件或文件夹')
    return
  }
  const selected = groups.value.filter((group) => selectedGroupKeys.value.includes(group.key))
  if (selected.length === 0) {
    ElMessage.warning('请选择要导入的词典')
    return
  }

  batchRunning.value = true
  batchCancelled.value = false
  batchSummary.value = ''
  let succeeded = 0
  let failed = 0
  try {
    for (const group of selected) {
      if (batchCancelled.value) break
      const name = (groupNames[group.key] ?? '').trim()
      if (!name) {
        groupStatus[group.key] = 'error'
        groupError[group.key] = '请填写词典名称'
        failed += 1
        continue
      }
      groupStatus[group.key] = 'importing'
      delete groupError[group.key]
      try {
        // 语言方向不传：服务端按词头/释义的文字种类自动识别
        const { task_id: taskId } = await dictApi.importUploaded({
          upload_id: uploadId.value,
          name,
          format: group.format,
          mode: importMode.value,
          files: group.files.map((file) => file.relpath),
        })
        const task = await waitForImportTask(taskId)
        groupStatus[group.key] = 'success'
        const from = resultString(task, 'lang_from')
        const to = resultString(task, 'lang_to')
        groupLangs[group.key] = from && to ? `${langLabel(from)} → ${langLabel(to)}` : ''
        groupWordCounts[group.key] = resultNumber(task, 'word_count') ?? 0
        succeeded += 1
        selectedGroupKeys.value = selectedGroupKeys.value.filter((key) => key !== group.key)
      } catch (err) {
        if (err instanceof PollAbortedError) break
        groupStatus[group.key] = 'error'
        groupError[group.key] = errorMessage(err)
        failed += 1
      }
    }
    if (isUnmounted()) return
    emit('imported')
    batchSummary.value = `本次导入：成功 ${succeeded} 部，失败 ${failed} 部`
    if (failed === 0 && succeeded > 0) {
      ElMessage.success(`批量导入完成，共导入 ${succeeded} 部词典`)
    }
  } finally {
    batchRunning.value = false
  }
}

function cancel() {
  batchCancelled.value = true
}

function reset() {
  uploadId.value = ''
  groups.value = []
  skipped.value = []
  selectedGroupKeys.value = []
  batchSummary.value = ''
}

defineExpose({ submit, cancel, running: batchRunning, analyzing, reset })
</script>

<template>
  <div class="upload-panel">
    <p v-if="!groups.length" class="hint">
      支持多选文件、整个文件夹（保留目录结构）、.zip 压缩包；上传后自动识别格式与词典分组，
      语言方向在导入时自动识别。ECDICT 是 CSV 单文件。
    </p>

    <div
      v-if="!groups.length"
      class="drop-zone"
      :class="{ dragging, busy: analyzing }"
      @dragover.prevent="dragging = true"
      @dragleave="dragging = false"
      @drop.prevent="onDrop"
    >
      <el-progress
        v-if="analyzing && uploadPercent > 0"
        :percentage="uploadPercent"
        :stroke-width="8"
        :show-text="uploadPercent < 100"
        class="upload-progress"
      />
      <p v-if="analyzing && uploadPercent >= 99" class="hint">服务器解压与识别中…</p>
      <el-icon :size="36"><UploadFilled /></el-icon>
      <p class="drop-title">把词典文件 / 文件夹 / .zip 拖到这里</p>
      <div class="drop-actions">
        <el-button :disabled="analyzing" @click="pickFiles">选择文件</el-button>
        <el-button :disabled="analyzing" @click="pickFolder">
          <el-icon class="btn-icon"><Folder /></el-icon>选择文件夹
        </el-button>
      </div>
      <input ref="fileInput" type="file" multiple hidden @change="onInputChange" />
      <!-- webkitdirectory：非标准属性，Vue 原样渲染到 DOM 即可 -->
      <input ref="folderInput" type="file" webkitdirectory directory multiple hidden @change="onInputChange" />
    </div>

    <template v-else>
      <div class="group-toolbar">
        <el-checkbox
          :model-value="allSelected"
          :indeterminate="someSelected"
          :disabled="batchRunning"
          @change="toggleAllGroups"
          >全选</el-checkbox
        >
        <span class="hint"
          >识别到 {{ groups.length }} 部，已选 {{ selectedGroupKeys.length }} 部 ·
          {{ formatSize(selectedTotalSize) }}</span
        >
        <el-button text size="small" :disabled="batchRunning" @click="reset">重新选择</el-button>
      </div>

      <el-form-item label="导入模式">
        <el-radio-group v-model="importMode" :disabled="batchRunning">
          <el-radio value="lite">
            轻量（默认）
            <span class="mode-hint">只入库词头，释义按需读源文件——磁盘约 1 倍、导入秒级</span>
          </el-radio>
          <el-radio value="full">
            全量
            <span class="mode-hint">释义入库——全文检索等高级功能的前提，导入较慢</span>
          </el-radio>
        </el-radio-group>
      </el-form-item>

      <div class="group-scroll app-scrollbar">
        <div v-for="group in groups" :key="group.key" class="group-row">
          <el-checkbox
            :model-value="selectedGroupKeys.includes(group.key)"
            :disabled="!group.importable || batchRunning"
            :title="group.reason ?? ''"
            @change="(checked: string | number | boolean) => checked ? selectedGroupKeys.push(group.key) : selectedGroupKeys = selectedGroupKeys.filter((key) => key !== group.key)"
          />
          <div class="group-main">
            <div class="group-line">
              <el-input
                v-model="groupNames[group.key]"
                size="small"
                :disabled="batchRunning || groupStatus[group.key] === 'success'"
                class="name-input"
              />
              <el-tag size="small">{{ FORMAT_LABELS[group.format] }}</el-tag>
              <el-tag size="small" type="info">{{ group.files.length }} 个文件</el-tag>
              <el-tag v-if="groupLangs[group.key]" size="small" type="success">
                {{ groupLangs[group.key] }}
              </el-tag>
              <el-tag v-if="groupWordCounts[group.key]" size="small" type="success">
                {{ groupWordCounts[group.key].toLocaleString() }} 条
              </el-tag>
              <el-tag size="small" :type="groupStatusTagType(group)">{{ groupStatusLabel(group) }}</el-tag>
            </div>
            <div class="group-files">
              <span v-for="file in group.files" :key="file.relpath" class="file-chip" :title="formatSize(file.size)">
                {{ file.name }}
              </span>
            </div>
          </div>
        </div>
        <p v-if="skipped.length" class="hint skipped">
          忽略了 {{ skipped.length }} 个无关文件（不参与词典导入）
        </p>
      </div>

      <p v-if="batchSummary" class="hint summary">{{ batchSummary }}</p>
    </template>
  </div>
</template>

<style scoped>
.upload-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}

.hint {
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
}

.drop-zone {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-6) var(--space-4);
  border: 1px dashed var(--color-border);
  border-radius: var(--radius-md);
  color: var(--color-text-secondary);
  transition: border-color 0.15s ease;
}

.drop-zone.dragging {
  border-color: var(--color-brand-500);
  background: var(--color-hover-tint);
}

.upload-progress {
  width: min(360px, 80%);
}

.drop-zone.busy {
  pointer-events: none;
}

.drop-title {
  margin: 0;
  font-size: var(--text-sm);
}

.drop-actions {
  display: flex;
  gap: var(--space-2);
}

.btn-icon {
  margin-right: var(--space-1);
}

.group-toolbar {
  display: flex;
  align-items: center;
  gap: var(--space-3);
}

.mode-hint {
  margin-left: var(--space-1);
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
}

.group-scroll {
  max-height: var(--size-scroll-lg, 320px);
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.group-row {
  display: flex;
  align-items: flex-start;
  gap: var(--space-2);
  padding: var(--space-2);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
}

.group-main {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}

.group-line {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  flex-wrap: wrap;
}

.name-input {
  max-width: 220px;
}

.group-files {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-1);
}

.file-chip {
  padding: 0 var(--space-2);
  border-radius: var(--radius-full);
  background: var(--color-hover-tint);
  color: var(--color-text-secondary);
  font-size: var(--text-xs);
  line-height: 20px;
}

.skipped {
  margin: 0;
}

.summary {
  margin: 0;
  color: var(--color-text-secondary);
}
</style>
