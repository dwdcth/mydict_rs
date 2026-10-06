import request from './request'

/**
 * 闪卡复习（FSRS 间隔重复）。
 *
 * 卡 = 生词本条目 + 调度信息；正面词头、背面收藏时的释义快照
 * （渲染复用 getVocabEntryHtml）。评分 1-4 对应 Again/Hard/Good/Easy。
 */

export interface FlashcardQueueItem {
  vocab_item_id: number
  word: string
  phonetic: string | null
  dictionary_id: number | null
  dictionary_name: string | null
  state: number
  reps: number
  lapses: number
  /** 复习前的当前记忆率（0-1；新卡为 null） */
  retrievability: number | null
  days_since_last_review: number | null
  /** 四档评分的预测间隔（按钮上直接展示） */
  previews: Record<'again' | 'hard' | 'good' | 'easy', { interval_days: number; label: string } | null>
}

export interface FlashcardStats {
  total: number
  due_count: number
  new_count: number
  today_reviewed: number
}

export interface FlashcardSettings {
  retention: number | null
  weights: string | null
}

export function addFlashcard(word: string, dictionaryId?: number | null) {
  return request.post<never, { vocab_item_id: number; already: boolean; due_at: number }>(
    '/flashcards',
    { word, dictionary_id: dictionaryId ?? undefined },
  )
}

export function fetchReviewQueue(limit = 50) {
  return request.get<never, { queue: FlashcardQueueItem[]; now: number }>('/flashcards/queue', {
    params: { limit },
  })
}

export function reviewFlashcard(vocabItemId: number, rating: 1 | 2 | 3 | 4) {
  return request.post<never, {
    vocab_item_id: number
    interval_days: number
    interval_label: string
    due_at: number
    state: number
    reps: number
    lapses: number
  }>(`/flashcards/${vocabItemId}/review`, { rating })
}

export function fetchFlashcardStats() {
  return request.get<never, FlashcardStats>('/flashcards/stats')
}

export function listFlashcards(filter: 'due' | 'all' = 'all', page = 1, pageSize = 100) {
  return request.get<never, {
    items: Array<{
      vocab_item_id: number
      word: string
      phonetic: string | null
      dictionary_id: number | null
      dictionary_name: string | null
      state: number
      due_at: number
      reps: number
      lapses: number
    }>
    total: number
    due_count: number
    new_count: number
    today_reviewed: number
  }>('/flashcards', { params: { filter, page, page_size: pageSize } })
}

export function deleteFlashcard(vocabItemId: number) {
  return request.delete<never, { ok: boolean }>(`/flashcards/${vocabItemId}`)
}

export function getFlashcardSettings() {
  return request.get<never, FlashcardSettings>('/flashcards/settings')
}

/**
 * 更新复习参数：retention=目标记忆率（0.7-0.99）；
 * weights=19 位 FSRS-4.5 权重 JSON 数组文本，空串清空回默认，缺省不动。
 */
export function updateFlashcardSettings(payload: { retention?: number; weights?: string }) {
  return request.put<never, FlashcardSettings>('/flashcards/settings', payload)
}

// ── 例句挖空测验 ─────────────────────────────────────────────

export interface QuizQuestion {
  vocab_item_id: number
  sentence: string
  blank_count: number
  options: string[]
  correct_index: number
  dictionary_name: string | null
}

export function fetchQuiz(count = 10) {
  return request.get<never, { questions: QuizQuestion[] }>('/flashcards/quiz', {
    params: { count },
  })
}

/** 答对=Good、答错=Again（走 FSRS，测验即复习） */
export function answerQuiz(vocabItemId: number, correct: boolean) {
  return request.post<never, { correct: boolean; interval_label: string }>(
    `/flashcards/quiz/${vocabItemId}/answer`,
    { correct },
  )
}
