import request from './request'

/** 今日一词（日期种子确定性取样，公开） */
export function wordOfTheDay() {
  return request.get<never, {
    word: string
    dictionary_id: number
    dictionary_name: string
    date: string
  }>('/dict/word-of-the-day')
}

/** 生词本导出 Anki TSV（带鉴权头下载 → blob） */
export async function downloadAnkiExport() {
  const resp = await request.get<BlobPart, BlobPart>('/vocab/export', {
    responseType: 'blob',
  })
  const blob = new Blob([resp], { type: 'text/tab-separated-values' })
  const url = URL.createObjectURL(blob)
  const a = document.createElement('a')
  a.href = url
  a.download = 'vocab-anki.tsv'
  a.click()
  URL.revokeObjectURL(url)
}

// ── 词频分析 ─────────────────────────────────────────────────

export interface FreqWord {
  word: string
  count: number
  in_dict: boolean
  in_vocab: boolean
  in_review: boolean
}

export interface FreqResult {
  total_tokens: number
  unique_words: number
  words: FreqWord[]
  in_dict_count: number
  in_vocab_count: number
}

export function analyzeText(text: string) {
  return request.post<never, FreqResult>('/tools/word-frequency', { text })
}

export function analyzeFile(file: File) {
  const form = new FormData()
  form.append('file', file)
  return request.post<never, FreqResult>('/tools/word-frequency-upload', form, {
    headers: { 'Content-Type': 'multipart/form-data' },
    timeout: 0,
  })
}

// ── 词条浏览（A-Z 翻阅）──────────────────────────────────────

export function browseWords(dictionaryId: number, after = '', limit = 200) {
  return request.get<never, {
    dictionary_id: number
    dictionary_name: string
    words: string[]
    next_cursor: string | null
  }>(`/tools/dict/browse/${dictionaryId}`, { params: { after, limit } })
}
