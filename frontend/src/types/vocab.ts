export interface VocabItem {
  id: number
  word: string
  phonetic: string | null
  definition: string | null
  note: string | null
  dictionary_id: number | null
  /** 来源词典名快照（词典被删后仍有值） */
  dictionary_name: string | null
  created_at: string
  /** 已加入 FSRS 复习计划 */
  in_review?: boolean
}

export interface VocabListResponse {
  items: VocabItem[]
  total: number
  page: number
  page_size: number
}

export type VocabSort = 'word' | 'date'
export type SortOrder = 'asc' | 'desc'
