import request from '../request'
import type {
  SystemSettings,
  SystemSettingsUpdate,
  TtsPinyinRule,
} from '../../types/settings'

export function getSettings() {
  return request.get<never, SystemSettings>('/admin/settings')
}

export function updateSettings(update: SystemSettingsUpdate) {
  return request.put<never, SystemSettings>('/admin/settings', update)
}

export interface PinyinRuleTestResult {
  name: string
  enabled: boolean
  matched: boolean
  value: string | null
  note?: string
}

/** 注音提取规则测试：与朗读路径同引擎（服务端 fancy-regex），所见即所得 */
export function testPinyinRules(rules: TtsPinyinRule[], text: string) {
  return request.post<never, { results: PinyinRuleTestResult[] }>(
    '/admin/settings/tts-pinyin-rules/test',
    { rules, text },
  )
}

export interface EdgeVoice {
  short_name: string
  gender: string
  locale: string
  locale_name: string
  display: string
}

/** edge-tts 官方全量音色（服务端缓存 6h；离线部署会失败，调用方回退精选表） */
export function listEdgeVoices() {
  return request.get<never, { voices: EdgeVoice[] }>('/admin/settings/tts-edge-voices')
}
