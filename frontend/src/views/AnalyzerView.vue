<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { ElMessage } from 'element-plus'
import { DocumentAdd, UploadFilled } from '@element-plus/icons-vue'
import NavBar from '../components/NavBar.vue'
import { analyzeFile, analyzeText, type FreqResult } from '../api/learning'
import { addFlashcard } from '../api/flashcards'
import { addVocab } from '../api/vocab'
import { useUserAuthStore } from '../stores/userAuth'
import { formatSize } from '../utils/format'

/**
 * 文本词频分析（吸收自 PythonMDict 的 analyzer）：
 * 贴文本或上传 .txt/.epub → 分词去停用词 → 用已导入词典验证「真实单词」
 * → 标记已在生词本/复习 → 勾选批量收藏/进复习。先掌握高频生词。
 */
const authStore = useUserAuthStore()

const text = ref('')
const fileInput = ref<HTMLInputElement | null>(null)
const analyzing = ref(false)
const result = ref<FreqResult | null>(null)
const selected = ref<Set<string>>(new Set())
const batchRunning = ref(false)
const onlyNew = ref(true)

const words = computed(() => {
  if (!result.value) return []
  const list = result.value.words
  return onlyNew.value ? list.filter((w) => !w.in_vocab) : list
})

async function run() {
  if (!text.value.trim()) {
    ElMessage.warning('先粘贴或上传一段英文文本')
    return
  }
  analyzing.value = true
  try {
    result.value = await analyzeText(text.value)
    defaultSelect()
  } finally {
    analyzing.value = false
  }
}

function onFileChange(event: Event) {
  const input = event.target as HTMLInputElement
  const file = input.files?.[0]
  input.value = ''
  if (!file) return
  analyzing.value = true
  analyzeFile(file)
    .then((r) => {
      result.value = r
      defaultSelect()
      ElMessage.success(
        `已分析 ${file.name}（${formatSize(file.size)}）`,
      )
    })
    .catch(() => undefined)
    .finally(() => {
      analyzing.value = false
    })
}

/** 默认勾选：词典收录 + 尚未收藏的（这些都是「值得学的真实生词」） */
function defaultSelect() {
  selected.value = new Set(
    result.value?.words
      .filter((w) => w.in_dict && !w.in_vocab)
      .map((w) => w.word) ?? [],
  )
}

function toggle(word: string) {
  if (selected.value.has(word)) selected.value.delete(word)
  else selected.value.add(word)
  selected.value = new Set(selected.value)
}

const selectedCount = computed(() => selected.value.size)

async function batch(action: 'vocab' | 'review') {
  const words = [...selected.value]
  if (words.length === 0) {
    ElMessage.warning('先勾选要加入的词')
    return
  }
  batchRunning.value = true
  let ok = 0
  try {
    for (const word of words) {
      try {
        if (action === 'review') {
          await addFlashcard(word)
        } else {
          await addVocab(word)
        }
        ok += 1
      } catch {
        // 单词失败（如某词典无此词）跳过，继续后面的
      }
    }
    ElMessage.success(`已加入 ${ok} 个词${action === 'review' ? '到复习计划' : '到生词本'}`)
    // 重算结果里的 in_vocab/in_review 标记（简单起见重新分析）
    if (text.value.trim()) {
      result.value = await analyzeText(text.value)
      defaultSelect()
    }
  } finally {
    batchRunning.value = false
  }
}

onMounted(() => {
  if (!authStore.isLoggedIn) {
    ElMessage.info('登录后才能批量加入生词本（分析本身无需登录）')
  }
})
</script>

<template>
  <div class="page">
    <NavBar />
    <div class="analyzer">
      <div class="page-header">
        <h1>词频分析</h1>
        <p class="hint">
          粘贴英文文本或上传 .txt / .epub：分词 → 去停用词 → 用你的词典验证真实单词
          → 按出现频次排序。先勾高频生词批量收藏/进复习，优先掌握最常遇到的词。
        </p>
      </div>

      <div class="input-row">
        <el-input
          v-model="text"
          type="textarea"
          :rows="6"
          placeholder="粘贴文章 / 电子书文本…"
          class="text-input"
        />
        <div class="input-actions">
          <el-button :loading="analyzing" type="primary" @click="run">分析</el-button>
          <el-button :loading="analyzing" @click="fileInput?.click()">
            <el-icon><UploadFilled /></el-icon>上传 .txt / .epub
          </el-button>
          <input ref="fileInput" type="file" accept=".txt,.epub,text/plain" hidden @change="onFileChange" />
        </div>
      </div>

      <template v-if="result">
        <div class="stats-row">
          <span>总词数 {{ result.total_tokens.toLocaleString() }}</span>
          <span>不同词 {{ result.unique_words.toLocaleString() }}</span>
          <span>词典收录 {{ result.in_dict_count }}</span>
          <span>已在生词本 {{ result.in_vocab_count }}</span>
          <el-checkbox v-model="onlyNew" size="small">只看未收藏</el-checkbox>
        </div>

        <div class="batch-bar">
          <span class="hint">已选 {{ selectedCount }} 个词</span>
          <el-button
            size="small"
            type="primary"
            :loading="batchRunning"
            :disabled="!authStore.isLoggedIn"
            @click="batch('vocab')"
          >
            <el-icon><DocumentAdd /></el-icon>加入生词本
          </el-button>
          <el-button
            size="small"
            type="primary"
            plain
            :loading="batchRunning"
            :disabled="!authStore.isLoggedIn"
            @click="batch('review')"
          >
            加入复习
          </el-button>
        </div>

        <div class="word-table app-scrollbar">
          <div class="word-header">
            <span class="col-check" />
            <span class="col-word">单词</span>
            <span class="col-count">次数</span>
            <span class="col-flags">状态</span>
          </div>
          <label
            v-for="item in words"
            :key="item.word"
            class="word-row"
            :class="{ dim: !item.in_dict }"
          >
            <span class="col-check" @click.prevent="toggle(item.word)">
              <el-checkbox
                :model-value="selected.has(item.word)"
                @change="toggle(item.word)"
              />
            </span>
            <span class="col-word">{{ item.word }}</span>
            <span class="col-count">{{ item.count }}</span>
            <span class="col-flags">
              <el-tag v-if="!item.in_dict" size="small" type="info">词典未收录</el-tag>
              <el-tag v-if="item.in_vocab" size="small" type="success">
                {{ item.in_review ? '复习中' : '已收藏' }}
              </el-tag>
            </span>
          </label>
          <p v-if="words.length === 0" class="hint empty">没有可显示的词</p>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.analyzer {
  max-width: var(--size-content-md, 760px);
  margin: var(--space-6) auto;
  padding: 0 var(--space-4);
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.page-header h1 {
  font-size: var(--text-xl);
  margin: 0 0 var(--space-2);
}

.hint {
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
  margin: 0;
}

.input-row {
  display: flex;
  gap: var(--space-3);
  align-items: flex-start;
}

.text-input {
  flex: 1;
}

.input-actions {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.stats-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-4);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
  align-items: center;
}

.batch-bar {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.word-table {
  background: var(--color-bg-surface);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-elevation-1);
  max-height: 60vh;
  overflow-y: auto;
  padding: var(--space-2) var(--space-4);
}

.word-header,
.word-row {
  display: grid;
  grid-template-columns: 36px minmax(0, 1fr) 64px 200px;
  gap: var(--space-2);
  align-items: center;
  padding: var(--space-1) 0;
}

.word-header {
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
  position: sticky;
  top: 0;
  background: var(--color-bg-surface);
  padding: var(--space-2) 0;
}

.word-row {
  border-top: 1px solid var(--color-border);
  cursor: pointer;
}

.word-row.dim .col-word {
  color: var(--color-text-tertiary);
}

.col-word {
  font-weight: var(--font-weight-medium);
  overflow: hidden;
  text-overflow: ellipsis;
}

.col-count {
  text-align: right;
  color: var(--color-text-secondary);
}

.col-flags {
  display: flex;
  gap: var(--space-1);
  justify-content: flex-end;
}

.empty {
  padding: var(--space-6);
  text-align: center;
}

@media (max-width: 640px) {
  .input-row {
    flex-direction: column;
    align-items: stretch;
  }

  .input-actions {
    flex-direction: row;
  }
}
</style>
