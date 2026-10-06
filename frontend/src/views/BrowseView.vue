<script setup lang="ts">
import { onMounted, ref } from 'vue'
import { useRouter } from 'vue-router'
import NavBar from '../components/NavBar.vue'
import { browseWords } from '../api/learning'
import { listDictionaries } from '../api/dict'
import type { PublicDictionary } from '../types/query'

/**
 * 词条浏览（A-Z 翻阅模式，吸收自 PythonMDict / 论坛用户的「像纸质词典从头翻到尾」需求）：
 * 选词典 → 干净词头按字母序滚动加载 → 点词即查。
 */
const router = useRouter()

const dictionaries = ref<PublicDictionary[]>([])
const dictionaryId = ref<number | null>(null)
const words = ref<string[]>([])
const cursor = ref('')
const nextCursor = ref<string | null>(null)
const loading = ref(false)
const prefix = ref('')

async function loadDicts() {
  try {
    dictionaries.value = await listDictionaries('all')
    if (dictionaries.value.length > 0) {
      dictionaryId.value = dictionaries.value[0].id
      await reload()
    }
  } catch {
    /* 拦截器已提示 */
  }
}

async function reload() {
  words.value = []
  cursor.value = ''
  nextCursor.value = null
  await loadMore()
}

async function loadMore() {
  if (dictionaryId.value === null || loading.value) return
  loading.value = true
  try {
    const resp = await browseWords(dictionaryId.value, cursor.value, 200)
    words.value = [...words.value, ...resp.words]
    nextCursor.value = resp.next_cursor
    if (resp.next_cursor) cursor.value = resp.next_cursor
  } finally {
    loading.value = false
  }
}

function onDictChange() {
  reload()
}

function queryWord(word: string) {
  router.push({ path: '/', query: { q: word, d: dictionaryId.value?.toString() } })
}

function matchesPrefix(word: string) {
  if (!prefix.value.trim()) return true
  return word.toLowerCase().startsWith(prefix.value.trim().toLowerCase())
}

onMounted(loadDicts)
</script>

<template>
  <div class="page">
    <NavBar />
    <div class="browse">
      <div class="page-header">
        <h1>词条浏览</h1>
        <p class="hint">像翻纸质词典一样从头读到最后：选一部词典，按字母序滚动浏览，点词即查。</p>
      </div>

      <div class="controls">
        <el-select
          :model-value="dictionaryId"
          filterable
          placeholder="选择词典"
          style="max-width: 280px"
          @update:model-value="((v: number) => { dictionaryId = v; onDictChange() })"
        >
          <el-option
            v-for="d in dictionaries"
            :key="d.id"
            :label="d.name"
            :value="d.id"
          />
        </el-select>
        <el-input
          v-model="prefix"
          placeholder="按前缀过滤（如 ab）"
          clearable
          style="max-width: 200px"
        />
        <span class="hint">已载 {{ words.length }} 个词头</span>
      </div>

      <div v-loading="loading && words.length === 0" class="word-grid app-scrollbar">
        <button
          v-for="(word, index) in words.filter(matchesPrefix)"
          :key="`${word}-${index}`"
          type="button"
          class="word-item"
          :title="`查询「${word}」`"
          @click="queryWord(word)"
        >
          {{ word }}
        </button>
        <p v-if="!loading && words.length === 0" class="hint empty">这部词典还没有可浏览的词头</p>
      </div>

      <div v-if="nextCursor" class="load-more">
        <el-button :loading="loading" @click="loadMore">加载更多</el-button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.browse {
  max-width: var(--size-content-lg, 1000px);
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
}

.controls {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  flex-wrap: wrap;
}

.word-grid {
  background: var(--color-bg-surface);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-elevation-1);
  padding: var(--space-3);
  min-height: 300px;
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-1);
  align-content: flex-start;
  max-height: 65vh;
  overflow-y: auto;
}

.word-item {
  border: none;
  background: transparent;
  color: var(--color-text-primary);
  font-size: var(--text-sm);
  padding: var(--space-1) var(--space-2);
  border-radius: var(--radius-md);
  cursor: pointer;
}

.word-item:hover {
  background: var(--color-hover-tint);
  color: var(--color-brand-500);
}

.empty {
  width: 100%;
  text-align: center;
  padding: var(--space-6);
}

.load-more {
  text-align: center;
}
</style>
