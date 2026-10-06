import { computed, ref } from 'vue'
import { useRouter } from 'vue-router'
import { ElMessage } from 'element-plus'
import { addFlashcard, deleteFlashcard, listFlashcards } from '../api/flashcards'
import { useUserAuthStore } from '../stores/userAuth'
import { favoriteKey } from './useFavorites'

/**
 * 复习态与收藏态同键（词+词典）：查询结果卡片上的卡片图标据此点亮；
 * 再点一次 = 移出复习（生词本条目保留）。
 */
export function useFlashcards() {
  const router = useRouter()
  const authStore = useUserAuthStore()

  // favoriteKey(词+词典) -> 生词本条目 id（= 闪卡主键）
  const flashcardMap = ref<Map<string, number>>(new Map())
  const flashcardLoading = ref<Set<string>>(new Set())

  async function loadFlashcards() {
    if (!authStore.isLoggedIn) return
    try {
      const resp = await listFlashcards('all', 1, 500)
      const map = new Map<string, number>()
      for (const item of resp.items) {
        map.set(favoriteKey(item.word, item.dictionary_id), item.vocab_item_id)
      }
      flashcardMap.value = map
    } catch {
      // 预加载失败不影响主流程（按钮只是不点亮）
    }
  }

  function inReview(word: string, dictionaryId?: number | null) {
    return flashcardMap.value.has(favoriteKey(word, dictionaryId))
  }

  async function toggleFlashcard(word: string, dictionaryId: number | null | undefined) {
    if (!authStore.isLoggedIn) {
      ElMessage.warning('登录后才能加入复习')
      router.push('/login')
      return
    }
    const key = favoriteKey(word, dictionaryId)
    flashcardLoading.value.add(key)
    try {
      if (inReview(word, dictionaryId)) {
        const id = flashcardMap.value.get(key)!
        await deleteFlashcard(id)
        flashcardMap.value.delete(key)
        ElMessage.success(`「${word}」已移出复习（生词本保留）`)
      } else {
        const result = await addFlashcard(word, dictionaryId ?? undefined)
        flashcardMap.value.set(key, result.vocab_item_id)
        ElMessage.success(
          result.already ? `「${word}」已在复习计划中` : `「${word}」已加入复习`,
        )
      }
      flashcardMap.value = new Map(flashcardMap.value)
    } catch {
      // 错误已由响应拦截器统一提示
    } finally {
      flashcardLoading.value = new Set(flashcardLoading.value)
      flashcardLoading.value.delete(key)
      flashcardLoading.value = new Set(flashcardLoading.value)
    }
  }

  const count = computed(() => flashcardMap.value.size)

  return { flashcardMap, flashcardLoading, loadFlashcards, inReview, toggleFlashcard, count }
}
