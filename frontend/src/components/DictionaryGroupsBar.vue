<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { Plus, Setting } from '@element-plus/icons-vue'
import {
  createDictionaryGroup,
  deleteDictionaryGroup,
  listDictionaryGroups,
  updateDictionaryGroup,
} from '../api/dict'
import type { DictionaryGroup, PublicDictionary } from '../types/query'

/**
 * 词典组标签行（GoldenDict 式）：点一个组 = 把检索范围一键切到该组的词典集合
 * （走现有的「勾选通道」，查询参数与手动勾选完全一致）。管理弹窗负责建/改/删组；
 * 组存服务端（登录用户专属），换设备也在。
 */

const props = defineProps<{
  dictionaries: PublicDictionary[]
  selectedIds: number[]
}>()

const emit = defineEmits<{ select: [ids: number[]] }>()

const groups = ref<DictionaryGroup[]>([])
const manageVisible = ref(false)

// 可用词典 id 集（组里可能存着已不可用的词典，应用时求交集）
const availableIds = computed(() => new Set(props.dictionaries.map((d) => d.id)))

/** 组在标签行上是否点亮：当前勾选恰好等于该组成员（无视顺序） */
function isGroupActive(group: DictionaryGroup) {
  const current = [...props.selectedIds].sort((a, b) => a - b).join(',')
  const target = [...group.dictionary_ids].sort((a, b) => a - b).join(',')
  return current !== '' && current === target
}

function selectGroup(group: DictionaryGroup) {
  const ids = group.dictionary_ids.filter((id) => availableIds.value.has(id))
  if (ids.length === 0) {
    ElMessage.warning(`组「${group.name}」里已没有可用词典，请先编辑该组`)
    manageVisible.value = true
    return
  }
  emit('select', ids)
}

async function loadGroups() {
  try {
    const { groups: list } = await listDictionaryGroups()
    groups.value = list
  } catch {
    // 未登录等场景由拦截器提示；标签行静默
  }
}

onMounted(loadGroups)

function openManage() {
  manageVisible.value = true
}

defineExpose({ reload: loadGroups })

// ── 管理弹窗 ───────────────────────────────────────────────
const editing = reactive({
  id: 0, // 0 = 新建
  name: '',
  // el-select 多选保留点选顺序 → 组内顺序即点选顺序
  dictionaryIds: [] as number[],
})
const saving = ref(false)

function startCreate() {
  editing.id = 0
  editing.name = ''
  editing.dictionaryIds = []
}

function startEdit(group: DictionaryGroup) {
  editing.id = group.id
  editing.name = group.name
  editing.dictionaryIds = [...group.dictionary_ids]
}

async function save() {
  const name = editing.name.trim()
  if (!name) {
    ElMessage.warning('请填写组名')
    return
  }
  if (editing.dictionaryIds.length === 0) {
    ElMessage.warning('请选择至少一部词典')
    return
  }
  saving.value = true
  try {
    if (editing.id === 0) {
      await createDictionaryGroup(name, editing.dictionaryIds)
      ElMessage.success(`已创建组「${name}」`)
      startCreate()
    } else {
      await updateDictionaryGroup(editing.id, {
        name,
        dictionary_ids: editing.dictionaryIds,
      })
      ElMessage.success('已保存')
    }
    await loadGroups()
  } finally {
    saving.value = false
  }
}

async function remove(group: DictionaryGroup) {
  try {
    await ElMessageBox.confirm(`删除词典组「${group.name}」？`, '删除确认', {
      type: 'warning',
      confirmButtonText: '删除',
      confirmButtonClass: 'el-button--danger',
    })
  } catch {
    return
  }
  try {
    await deleteDictionaryGroup(group.id)
    if (editing.id === group.id) startCreate()
    await loadGroups()
    ElMessage.success('已删除')
  } catch {
    // 拦截器已提示
  }
}
</script>

<template>
  <!-- 无组时也常驻：管理入口本身就是「创建第一个组」的发现路径 -->
  <div class="group-bar">
    <template v-if="groups.length">
      <span class="group-label">组</span>
      <button
        v-for="group in groups"
        :key="group.id"
        type="button"
        class="group-chip"
        :class="{ active: isGroupActive(group) }"
        :title="group.dictionary_ids.length + ' 部词典'"
        @click="selectGroup(group)"
      >
        {{ group.name }}
      </button>
    </template>
    <button type="button" class="group-manage" title="管理词典组" @click="openManage">
      <el-icon><Setting /></el-icon>词典组
    </button>
  </div>

  <el-dialog v-model="manageVisible" title="词典组" width="var(--size-dialog-md)">
    <div class="group-manager">
      <!-- 左：组列表 -->
      <div class="group-list app-scrollbar">
        <button
          type="button"
          class="group-item"
          :class="{ active: editing.id === 0 }"
          @click="startCreate"
        >
          <el-icon><Plus /></el-icon>新建组
        </button>
        <button
          v-for="group in groups"
          :key="group.id"
          type="button"
          class="group-item"
          :class="{ active: editing.id === group.id }"
          @click="startEdit(group)"
        >
          <span class="group-item-name">{{ group.name }}</span>
          <span class="group-item-count">{{ group.dictionary_ids.length }} 部</span>
        </button>
        <p v-if="groups.length === 0" class="hint">还没有词典组。像 GoldenDict 一样把常用词典组合存成一组，查询时一键切换范围。</p>
      </div>

      <!-- 右：编辑表单 -->
      <div class="group-edit">
        <template v-if="editing.id === 0">
          <h4>新建组</h4>
        </template>
        <template v-else>
          <h4>编辑组</h4>
        </template>
        <el-form label-position="top">
          <el-form-item label="组名">
            <el-input v-model="editing.name" placeholder="如：英语精查 / 古汉语" maxlength="50" />
          </el-form-item>
          <el-form-item label="成员词典（点选顺序即组内顺序）">
            <el-select
              v-model="editing.dictionaryIds"
              multiple
              filterable
              placeholder="选择词典"
              style="width: 100%"
            >
              <el-option
                v-for="d in dictionaries"
                :key="d.id"
                :label="d.name"
                :value="d.id"
              />
            </el-select>
          </el-form-item>
        </el-form>
        <div class="group-edit-actions">
          <el-button
            v-if="editing.id !== 0"
            type="danger"
            text
            @click="remove(groups.find((g) => g.id === editing.id)!)"
          >
            删除该组
          </el-button>
          <span class="spacer" />
          <el-button type="primary" :loading="saving" @click="save">
            {{ editing.id === 0 ? '创建' : '保存' }}
          </el-button>
        </div>
      </div>
    </div>
  </el-dialog>
</template>

<style scoped>
.group-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-1);
}

.group-label {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.group-chip {
  display: inline-flex;
  align-items: center;
  padding: var(--space-1) var(--space-3);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-full);
  background: transparent;
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  cursor: pointer;
}

.group-chip:hover {
  background: var(--color-hover-tint);
}

.group-chip.active {
  background: var(--color-brand-500);
  border-color: var(--color-brand-500);
  color: #fff;
}

.group-manage {
  display: inline-flex;
  align-items: center;
  gap: var(--space-1);
  padding: var(--space-1) var(--space-2);
  border: none;
  border-radius: var(--radius-full);
  background: transparent;
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
  cursor: pointer;
}

.group-manage:hover {
  background: var(--color-hover-tint);
  color: var(--color-text-secondary);
}

.group-manager {
  display: grid;
  grid-template-columns: 200px 1fr;
  gap: var(--space-4);
  min-height: 280px;
}

.group-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  max-height: 420px;
  overflow-y: auto;
}

.group-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  background: transparent;
  cursor: pointer;
  font-size: var(--text-sm);
  color: var(--color-text-primary);
  text-align: left;
}

.group-item:hover {
  background: var(--color-hover-tint);
}

.group-item.active {
  border-color: var(--color-brand-500);
  background: var(--color-hover-tint);
}

.group-item-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.group-item-count {
  flex-shrink: 0;
  color: var(--color-text-tertiary);
  font-size: var(--text-xs);
}

.group-edit h4 {
  margin: 0 0 var(--space-3);
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

.group-edit-actions {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.spacer {
  flex: 1;
}

.hint {
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
  padding: var(--space-2);
}

@media (max-width: 640px) {
  .group-manager {
    grid-template-columns: 1fr;
  }
}
</style>
