export interface PublicSettings {
  open_access: boolean
  allow_registration: boolean
  /** 在线词典总开关（管理后台默认禁用）；前台据此决定是否渲染【在线】标签 */
  online_dict_enabled: boolean
  /** 随机浏览开关（管理后台默认禁用）；前台据此决定是否渲染【随机】标签 */
  random_browse_enabled: boolean
  site_name: string
  initialized: boolean
  search_hint_text: string
}

/** TTS 注音提取规则（生僻字读音兜底）：正则第一个捕获组 = 拼音 */
export interface TtsPinyinRule {
  name: string
  pattern: string
  enabled: boolean
}

export interface SystemSettings {
  open_access: boolean
  allow_registration: boolean
  online_dict_enabled: boolean
  random_browse_enabled: boolean
  token_default_daily_limit: number
  anonymous_ip_rate_limit_per_min: number
  user_ip_rate_limit_per_min: number
  vocab_max_items_per_owner: number | null
  site_name: string
  search_hint_text: string
  /** 在线词典出站代理；空串表示直连 */
  online_dict_proxy: string
  /** 启用的在线词典源 CSV；空串表示全部启用 */
  online_dict_sources: string
  tts_enabled?: boolean
  tts_voice_zh?: string
  tts_voice_en?: string
  tts_pinyin_rules?: TtsPinyinRule[]
}

export type SystemSettingsUpdate = Partial<SystemSettings>
