<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { ElMessage, ElMessageBox } from 'element-plus'
import { RefreshRight, Setting } from '@element-plus/icons-vue'
import EntryFrame from '../components/EntryFrame.vue'
import NavBar from '../components/NavBar.vue'
import {
  deleteFlashcard,
  fetchFlashcardStats,
  fetchReviewQueue,
  getFlashcardSettings,
  listFlashcards,
  reviewFlashcard,
  updateFlashcardSettings,
  type FlashcardQueueItem,
  type FlashcardStats,
} from '../api/flashcards'
import { getVocabEntryHtml } from '../api/dict'
import { useUserAuthStore } from '../stores/userAuth'

/**
 * 间隔重复复习页（FSRS）：正面词头 → 空格/点击翻面（背面 = 收藏时的释义快照）→
 * 1-4 评分（Again/Hard/Good/Easy），按钮上直接显示预测间隔。
 * 键盘：空格=翻面，1-4=评分，与 Anki 同肌肉记忆。
 */

const authStore = useUserAuthStore()

const stats = ref<FlashcardStats | null>(null)
const queue = ref<FlashcardQueueItem[]>([])
const current = computed(() => queue.value[0] ?? null)
const revealed = ref(false)
const reviewing = ref(false)
const loading = ref(false)
const sessionDone = ref(0)
const sessionTotal = ref(0)

// ── 设置弹窗 ──
const settingsVisible = ref(false)
const retentionInput = ref('0.9')
const weightsInput = ref('')
const savingSettings = ref(false)

// ── 卡片管理 ──
const manageVisible = ref(false)
const manageItems = ref<Awaited<ReturnType<typeof listFlashcards>>['items']>([])
const manageLoading = ref(false)

async function loadStats() {
  try {
    stats.value = await fetchFlashcardStats()
  } catch {
    /* 徽章类数据，静默 */
  }
}

async function loadQueue() {
  loading.value = true
  try {
    const { queue: items } = await fetchReviewQueue(50)
    queue.value = items
    sessionTotal.value = items.length
    sessionDone.value = 0
    revealed.value = false
  } finally {
    loading.value = false
  }
}

function reveal() {
  revealed.value = true
}

const RATING_KEYS: Record<string, 1 | 2 | 3 | 4> = {
  '1': 1,
  '2': 2,
  '3': 3,
  '4': 4,
}

/** 评分按钮（顺序即键盘 1-4） */
const GRADE_BUTTONS: Array<{ rating: 1 | 2 | 3 | 4; key: 'again' | 'hard' | 'good' | 'easy'; label: string }> = [
  { rating: 1, key: 'again', label: '想不起来' },
  { rating: 2, key: 'hard', label: '困难' },
  { rating: 3, key: 'good', label: '良好' },
  { rating: 4, key: 'easy', label: '简单' },
]

async function grade(rating: 1 | 2 | 3 | 4) {
  const card = current.value
  if (!card || reviewing.value) return
  reviewing.value = true
  try {
    const result = await reviewFlashcard(card.vocab_item_id, rating)
    // 出队 + 计数（新卡 Good 间隔 ≥1 天，下次到期自然不在本队列）
    queue.value = queue.value.slice(1)
    sessionDone.value += 1
    revealed.value = false
    loadStats().catch(() => undefined)
    if (queue.value.length === 0) {
      ElMessage.success(
        `本轮完成：${result.interval_label}后见（今日已复习 ${stats.value?.today_reviewed ?? sessionDone.value} 次）`,
      )
    }
  } finally {
    reviewing.value = false
  }
}

function onKeydown(event: KeyboardEvent) {
  // 输入控件里不抢键
  const target = event.target as HTMLElement | null
  if (target && ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName)) return
  if (event.code === 'Space' || event.key === 'Enter') {
    if (current.value && !revealed.value) {
      event.preventDefault()
      reveal()
    }
    return
  }
  const rating = RATING_KEYS[event.key]
  if (rating && current.value && revealed.value) {
    event.preventDefault()
    grade(rating)
  }
}

onMounted(() => {
  window.addEventListener('keydown', onKeydown)
  if (authStore.isLoggedIn) {
    loadStats()
    loadQueue()
  }
})
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))

// ── 设置 ──
async function openSettings() {
  try {
    const s = await getFlashcardSettings()
    retentionInput.value = String(s.retention ?? 0.9)
    weightsInput.value = s.weights ?? ''
  } catch {
    /* 拦截器已提示 */
  }
  settingsVisible.value = true
}

async function saveSettings() {
  const retention = Number(retentionInput)
  if (!Number.isFinite(retention) || retention < 0.7 || retention > 0.99) {
    ElMessage.warning('目标记忆率须在 0.7-0.99 之间（越高复习越勤、间隔越短）')
    return
  }
  const weights = weightsInput.value.trim()
  if (weights) {
    try {
      const parsed = JSON.parse(weights)
      if (!Array.isArray(parsed) || parsed.length !== 19 || !parsed.every((v) => typeof v === 'number' && Number.isFinite(v))) {
        throw new Error('bad')
      }
    } catch {
      ElMessage.warning('权重须是 19 个数值的 JSON 数组（FSRS-4.5 格式，ts-fsrs / Anki 导出）')
      return
    }
  }
  savingSettings.value = true
  try {
    await updateFlashcardSettings({ retention, weights: weights || '' })
    ElMessage.success('已保存（下一次评分即生效）')
    settingsVisible.value = false
    loadQueue().catch(() => undefined)
  } finally {
    savingSettings.value = false
  }
}

// ── 卡片管理 ──
async function openManage() {
  manageVisible.value = true
  manageLoading.value = true
  try {
    const result = await listFlashcards('all', 1, 200)
    manageItems.value = result.items
  } finally {
    manageLoading.value = false
  }
}

async function removeCard(itemId: number, word: string) {
  try {
    await ElMessageBox.confirm(`把「${word}」移出复习？（生词本条目保留）`, '移出复习', {
      type: 'warning',
      confirmButtonText: '移出',
      confirmButtonClass: 'el-button--danger',
    })
  } catch {
    return
  }
  await deleteFlashcard(itemId).catch(() => undefined)
  manageItems.value = manageItems.value.filter((item) => item.vocab_item_id !== itemId)
  loadStats().catch(() => undefined)
  loadQueue().catch(() => undefined)
}

function stateLabel(state: number) {
  switch (state) {
    case 0:
      return '新卡'
    case 1:
      return '学习中'
    case 2:
      return '复习中'
    default:
      return '重学中'
  }
}

function dueLabel(dueAt: number) {
  const diffDays = Math.ceil((dueAt * 1000 - Date.now()) / 86400000)
  if (diffDays <= 0) return '已到期'
  return `${diffDays} 天后`
}
</script>

<template>
  <div class="page">
    <NavBar />
    <div class="review-page">
      <div class="review-header">
        <h1>复习</h1>
        <div class="review-actions">
          <el-button text :loading="loading" @click="loadQueue()">
            <el-icon><RefreshRight /></el-icon>刷新队列
          </el-button>
          <el-button text @click="openManage">管理卡片</el-button>
          <el-button text @click="openSettings">
            <el-icon><Setting /></el-icon>参数
          </el-button>
        </div>
      </div>

      <div v-if="stats" class="progress-row">
        <span>到期 {{ stats.due_count }}（含新卡 {{ stats.new_count }}）</span>
        <span>共 {{ stats.total }} 张</span>
        <span>今日已复习 {{ stats.today_reviewed }} 次</span>
        <span v-if="sessionTotal > 0">本轮 {{ sessionDone }}/{{ sessionTotal }}</span>
      </div>

      <div v-if="!authStore.isLoggedIn" class="gate">登录后开始复习</div>

      <template v-else>
        <div v-if="loading" class="gate">载入中…</div>

        <div v-else-if="!current" class="done-card">
          <p class="done-title">{{ stats && stats.total > 0 ? '当前没有到期的卡片' : '还没有复习卡片' }}</p>
          <p class="done-hint">
            {{
              stats && stats.total > 0
                ? '都复习完了，休息一下吧。'
                : '查询后点词条标题旁的卡片图标即可加入复习。'
            }}
          </p>
        </div>

        <div v-else class="card" :class="{ revealed }">
          <!-- 正面：词头 -->
          <div class="card-front" @click="!revealed && reveal()">
            <div class="front-word">{{ current.word }}</div>
            <div v-if="current.phonetic" class="front-phonetic">[{{ current.phonetic }}]</div>
            <div v-if="current.dictionary_name" class="front-dict">{{ current.dictionary_name }}</div>
            <button v-if="!revealed" type="button" class="reveal-btn" @click.stop="reveal">
              显示答案（空格）
            </button>
          </div>

          <!-- 背面：释义快照 + 评分 -->
          <template v-if="revealed">
            <div class="card-back app-scrollbar">
              <EntryFrame :loader="() => getVocabEntryHtml(current!.vocab_item_id)" />
            </div>
            <div class="grade-row">
              <button
                v-for="btn in GRADE_BUTTONS"
                :key="btn.rating"
                type="button"
                class="grade-btn"
                :class="`grade-${btn.rating}`"
                :disabled="reviewing"
                @click="grade(btn.rating)"
              >
                <span class="grade-key">{{ btn.rating }}</span>
                <span class="grade-label">{{ btn.label }}</span>
                <span class="grade-interval">{{ current.previews[btn.key]?.label ?? '' }}</span>
              </button>
            </div>
          </template>
        </div>
      </template>
    </div>

    <!-- 参数设置 -->
    <el-dialog v-model="settingsVisible" title="复习参数" width="var(--size-dialog-sm)">
      <el-form label-position="top">
        <el-form-item label="目标记忆率（0.7-0.99）">
          <el-input v-model="retentionInput" placeholder="0.9" />
          <p class="hint">越高记得越牢、复习越勤（间隔越短）；默认 0.9。</p>
        </el-form-item>
        <el-form-item label="自定义权重（可选，19 位 FSRS-4.5 JSON 数组）">
          <el-input
            v-model="weightsInput"
            type="textarea"
            :rows="3"
            placeholder='[0.4072, 1.1829, …] 共 19 个数值；留空用默认'
          />
          <p class="hint">
            从 ts-fsrs / Anki（FSRS-4.5）导出的权重串粘贴进来即生效；清空恢复默认。
          </p>
        </el-form-item>
      </el-form>
      <template #footer>
        <el-button @click="settingsVisible = false">取消</el-button>
        <el-button type="primary" :loading="savingSettings" @click="saveSettings">保存</el-button>
      </template>
    </el-dialog>

    <!-- 卡片管理 -->
    <el-dialog v-model="manageVisible" title="复习卡片" width="var(--size-dialog-md)">
      <div v-loading="manageLoading" class="manage-list app-scrollbar">
        <div v-for="item in manageItems" :key="item.vocab_item_id" class="manage-row">
          <span class="manage-word">{{ item.word }}</span>
          <span class="manage-dict">{{ item.dictionary_name ?? '—' }}</span>
          <el-tag size="small">{{ stateLabel(item.state) }}</el-tag>
          <span class="manage-due">{{ dueLabel(item.due_at) }}</span>
          <span class="manage-reps" :title="`复习 ${item.reps} 次，忘记 ${item.lapses} 次`">
            {{ item.reps }} 次
          </span>
          <el-button text type="danger" size="small" @click="removeCard(item.vocab_item_id, item.word)">
            移出
          </el-button>
        </div>
        <p v-if="!manageLoading && manageItems.length === 0" class="hint">暂无卡片</p>
      </div>
    </el-dialog>
  </div>
</template>

<style scoped>
.review-page {
  max-width: var(--size-content-md, 720px);
  margin: var(--space-6) auto;
  padding: 0 var(--space-4);
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.review-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.review-header h1 {
  font-size: var(--text-xl);
  margin: 0;
}

.review-actions {
  display: flex;
  align-items: center;
  gap: var(--space-1);
}

.progress-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-4);
  color: var(--color-text-secondary);
  font-size: var(--text-sm);
}

.gate,
.done-card {
  background: var(--color-bg-surface);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-elevation-1);
  padding: var(--space-8);
  text-align: center;
  color: var(--color-text-secondary);
}

.done-title {
  font-size: var(--text-lg);
  color: var(--color-text-primary);
  margin: 0 0 var(--space-2);
}

.done-hint {
  margin: 0;
  font-size: var(--text-sm);
}

.card {
  background: var(--color-bg-surface);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-elevation-1);
  overflow: hidden;
  display: flex;
  flex-direction: column;
}

.card-front {
  padding: var(--space-8) var(--space-4);
  text-align: center;
  cursor: pointer;
  min-height: 200px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-2);
}

.front-word {
  font-size: 2.2rem;
  font-weight: var(--font-weight-semibold);
  color: var(--color-text-primary);
}

.front-phonetic {
  color: var(--color-text-secondary);
}

.front-dict {
  font-size: var(--text-sm);
  color: var(--color-text-tertiary);
}

.reveal-btn {
  margin-top: var(--space-4);
  padding: var(--space-2) var(--space-5);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-full);
  background: transparent;
  color: var(--color-text-secondary);
  cursor: pointer;
}

.reveal-btn:hover {
  background: var(--color-hover-tint);
}

.card-back {
  border-top: 1px solid var(--color-border);
  max-height: 55vh;
  overflow-y: auto;
  padding: var(--space-3) var(--space-4);
}

.grade-row {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: var(--space-2);
  padding: var(--space-3) var(--space-4) var(--space-4);
}

.grade-btn {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  padding: var(--space-2);
  border: 1px solid var(--color-border);
  border-radius: var(--radius-md);
  background: transparent;
  cursor: pointer;
}

.grade-btn:hover:not(:disabled) {
  background: var(--color-hover-tint);
}

.grade-btn:disabled {
  opacity: 0.6;
  cursor: wait;
}

.grade-key {
  font-size: var(--text-xs);
  color: var(--color-text-tertiary);
}

.grade-label {
  font-size: var(--text-sm);
  color: var(--color-text-primary);
}

.grade-interval {
  font-size: var(--text-xs);
  color: var(--color-text-secondary);
}

.grade-btn.grade-1 .grade-label {
  color: var(--color-danger);
}

.grade-btn.grade-4 .grade-label {
  color: var(--color-brand-500);
}

.hint {
  color: var(--color-text-tertiary);
  font-size: var(--text-sm);
  margin: var(--space-1) 0 0;
}

.manage-list {
  max-height: 60vh;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
}

.manage-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-2) 0;
  border-bottom: 1px solid var(--color-border);
  font-size: var(--text-sm);
}

.manage-word {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-weight: var(--font-weight-medium);
}

.manage-dict {
  color: var(--color-text-tertiary);
  max-width: 140px;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.manage-due {
  color: var(--color-text-secondary);
  min-width: 64px;
  text-align: right;
}

.manage-reps {
  color: var(--color-text-tertiary);
  min-width: 44px;
  text-align: right;
}

@media (max-width: 640px) {
  .grade-row {
    grid-template-columns: repeat(2, 1fr);
  }

  .front-word {
    font-size: 1.7rem;
  }
}
</style>
