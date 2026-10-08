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
