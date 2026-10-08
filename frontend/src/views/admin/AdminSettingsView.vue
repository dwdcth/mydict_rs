<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue'
import { ElMessage } from 'element-plus'
import * as settingsApi from '../../api/admin/settings'
import { fetchTtsBlob } from '../../api/dict'
import { listEdgeVoices, type EdgeVoice } from '../../api/admin/settings'
import { useTtsPlayer } from '../../utils/ttsPlayer'
import RefreshButton from '../../components/admin/RefreshButton.vue'

const loading = ref(true)
const saving = ref(false)

const form = reactive({
  open_access: false,
  allow_registration: true,
  token_default_daily_limit: 1000,
  anonymous_ip_rate_limit_per_min: 60,
  user_ip_rate_limit_per_min: 120,
  vocab_max_items_per_owner: null as number | null,
  site_name: 'MyDict',
  search_hint_text: '小搜一下, 大进一步',
  online_dict_proxy: '',
  online_dict_sources: '',
  online_dict_enabled: false,
  random_browse_enabled: false,
  tts_enabled: false,
  tts_engine: 'edge' as 'edge' | 'kokoro',
  tts_voice_zh: 'zh-CN-XiaoxiaoNeural',
  tts_voice_en: 'en-US-AriaNeural',
  tts_pinyin_rules: [] as Array<{ name: string; pattern: string; enabled: boolean }>,
})

// 在线词典源开关。后端存 CSV（空 = 全部启用，向后兼容），界面用 checkbox 数组。
// 注意：选中态存独立的 ref，**不能**从 form 字符串反向推导——「空串 = 全部启用」的
// 后端语义会让「取消最后一个勾选」立即被渲染成「全部勾选」（实测踩过的坑）。
// 加载时空 CSV 显示为全选；保存时全选存回空串（将来新增的源自动默认启用）。
const ONLINE_SOURCES = [
  { id: 'wikipedia', label: '维基百科' },
  { id: 'wiktionary', label: '维基词典' },
  { id: 'baike', label: '百度百科' },
  { id: 'google', label: 'Google' },
  { id: 'urban', label: 'Urban Dictionary' },
  { id: 'merriam', label: 'Merriam-Webster' },
  { id: 'goodreads', label: 'Goodreads' },
]

// Kokoro 内建嗓音（嗓音名决定语言）；下拉可手输任意合法名（allow-create）
const ZH_VOICES = [
  { value: 'zf_xiaobei', label: '晓贝（女，最标准）' },
  { value: 'zf_xiaoxiao', label: '晓晓（女）' },
  { value: 'zf_xiaoyi', label: '晓伊（女）' },
  { value: 'zf_xiaoni', label: '晓妮（女，口音偏重）' },
  { value: 'zm_yunjian', label: '云健（男，浑厚）' },
  { value: 'zm_yunxi', label: '云希（男）' },
  { value: 'zm_yunyang', label: '云扬（男，播音）' },
  { value: 'zm_yunxia', label: '云夏（男）' },
]
const EN_VOICES = [
  { value: 'af_heart', label: 'Heart（美音女，默认）' },
  { value: 'af_sky', label: 'Sky（美音女）' },
  { value: 'am_adam', label: 'Adam（美音男）' },
  { value: 'am_michael', label: 'Michael（美音男）' },
  { value: 'bf_emma', label: 'Emma（英音女）' },
  { value: 'bm_george', label: 'George（英音男）' },
]
const onlineSourceSelection = ref<string[]>([])

// edge（微软在线）嗓音：Neural 系，名字即语言+人声
const EDGE_ZH_VOICES = [
  { value: 'zh-CN-XiaoxiaoNeural', label: '晓晓（女，自然，默认）' },
  { value: 'zh-CN-XiaoyiNeural', label: '晓伊（女）' },
  { value: 'zh-CN-YunjianNeural', label: '云健（男，浑厚）' },
  { value: 'zh-CN-YunxiNeural', label: '云希（男）' },
  { value: 'zh-CN-YunyangNeural', label: '云扬（男，播音）' },
  { value: 'zh-CN-YunxiaNeural', label: '云夏（男，少年）' },
  { value: 'zh-CN-liaoning-XiaobeiNeural', label: '晓北（女，东北话）' },
  { value: 'zh-TW-HsiaoChenNeural', label: '曉臻（女，台湾腔）' },
]
const EDGE_EN_VOICES = [
  { value: 'en-US-AriaNeural', label: 'Aria（美音女，默认）' },
  { value: 'en-US-JennyNeural', label: 'Jenny（美音女）' },
  { value: 'en-US-GuyNeural', label: 'Guy（美音男）' },
  { value: 'en-US-EmmaMultilingualNeural', label: 'Emma（多语女）' },
  { value: 'en-GB-SoniaNeural', label: 'Sonia（英音女）' },
  { value: 'en-GB-RyanNeural', label: 'Ryan（英音男）' },
]
// edge 全量音色（远端拉取，失败回退精选表）：中文/英文分组 + 其他语言
const edgeVoices = ref<EdgeVoice[] | null>(null)
const edgeVoicesLoaded = ref(false)

async function loadEdgeVoices() {
  if (edgeVoicesLoaded.value) return
  try {
    const res = await listEdgeVoices()
    edgeVoices.value = res.voices
  } catch {
    edgeVoices.value = null // 离线：回退精选表
  } finally {
    edgeVoicesLoaded.value = true
  }
}

function edgeGroup(localePrefix: string) {
  const list = (edgeVoices.value ?? []).filter((v) => v.locale.startsWith(localePrefix))
  return list.map((v) => ({
    value: v.short_name,
    label: `${v.display} · ${v.locale_name}`,
  }))
}

const zhVoiceOptions = computed(() => {
  if (form.tts_engine !== 'edge') return ZH_VOICES
  if (edgeVoices.value && edgeVoices.value.length) {
    return [
      ...edgeGroup('zh-CN'),
      ...edgeGroup('zh-HK'),
      ...edgeGroup('zh-TW'),
    ]
  }
  return EDGE_ZH_VOICES
})

const enVoiceOptions = computed(() => {
  if (form.tts_engine !== 'edge') return EN_VOICES
  if (edgeVoices.value && edgeVoices.value.length) {
    return [...edgeGroup('en-US'), ...edgeGroup('en-GB'), ...edgeGroup('en-AU'), ...edgeGroup('en-')]
  }
  return EDGE_EN_VOICES
})

/** 其他语言的完整列表（日/韩/法/德/俄/西等，下拉全量可搜索） */
const edgeOtherVoices = computed(() => {
  const list = (edgeVoices.value ?? []).filter(
    (v) => !v.locale.startsWith('zh') && !v.locale.startsWith('en'),
  )
  return list.map((v) => ({
    value: v.short_name,
    label: `${v.display} · ${v.locale_name}（${v.locale}）`,
  }))
})

/** 切引擎时嗓音名互不通用，重置成该引擎的默认 */
function onEngineChange(engine: 'edge' | 'kokoro') {
  form.tts_engine = engine
  if (engine === 'edge') {
    form.tts_voice_zh = 'zh-CN-XiaoxiaoNeural'
    form.tts_voice_en = 'en-US-AriaNeural'
  } else {
    form.tts_voice_zh = 'zm_yunjian'
    form.tts_voice_en = 'af_heart'
  }
}

const previewingZh = ref(false)
const previewingEn = ref(false)

/** 试听：指定嗓音合成一句固定样本并播放（不落设置）。
 * 走全局 ttsPlayer（解锁过的共享 Audio 实例），合成完成后能直接出声 */
async function previewVoice(voice: string) {
  if (!voice.trim()) return
  const isZh = voice.startsWith('z')
  const sample = isZh
    ? '豫章故郡，洪都新府。星分翼轸，地接衡庐。'
    : 'The apple is a sweet fruit that grows on trees.'
  const flag = isZh ? previewingZh : previewingEn
  flag.value = true
  try {
    const blob = new Blob([await fetchTtsBlob(sample, undefined, voice, form.tts_engine)])
    const url = URL.createObjectURL(blob)
    await useTtsPlayer().playUrl(url, `嗓音试听 ${voice}`)
    URL.revokeObjectURL(url)
  } catch {
    /* 拦截器已提示 */
  } finally {
    flag.value = false
  }
}

function syncSourceSelectionFromForm() {
  onlineSourceSelection.value = form.online_dict_sources
    ? form.online_dict_sources.split(',').filter((id) => ONLINE_SOURCES.some((s) => s.id === id))
    : ONLINE_SOURCES.map((s) => s.id)
}

function onSourceSelectionChange(ids: string[]) {
  onlineSourceSelection.value = [...ids]
  form.online_dict_sources =
    ids.length === ONLINE_SOURCES.length
      ? ''
      : ONLINE_SOURCES.filter((s) => ids.includes(s.id))
          .map((s) => s.id)
          .join(',')
}

const vocabUnlimited = computed({
  get: () => form.vocab_max_items_per_owner === null,
  set: (unlimited: boolean) => {
    form.vocab_max_items_per_owner = unlimited ? null : 100
  },
})

async function load() {
  loading.value = true
  try {
    Object.assign(form, await settingsApi.getSettings())
    syncSourceSelectionFromForm()
  } finally {
    loading.value = false
  }
}

onMounted(() => {
  load()
  loadEdgeVoices()
})

async function save() {
  if (!onlineSourceSelection.value.length) {
    ElMessage.warning('至少启用一个在线词典源')
    return
  }
  saving.value = true
  try {
    // 规则先过一遍本地 trim（空正则交给后端校验会报错，这里提前拦）
    form.tts_pinyin_rules = form.tts_pinyin_rules.map((r) => ({
      name: r.name.trim(),
      pattern: r.pattern,
      enabled: r.enabled,
    }))
    const updated = await settingsApi.updateSettings({ ...form })
    Object.assign(form, updated)
    ElMessage.success('设置已保存')
  } finally {
    saving.value = false
  }
}

// --- TTS 注音提取规则 -------------------------------------------------------

function addPinyinRule() {
  form.tts_pinyin_rules.push({ name: '', pattern: '', enabled: true })
}

const ruleTestText = ref('')
const ruleTesting = ref(false)
const ruleTestResults = ref<Array<{ name: string; matched: boolean; value: string | null; note?: string }> | null>(null)

/** 用当前编辑中的规则跑一次提取（后端正则引擎，与朗读路径同口径） */
async function testPinyinRules() {
  ruleTesting.value = true
  try {
    const res = await settingsApi.testPinyinRules(form.tts_pinyin_rules, ruleTestText.value)
    ruleTestResults.value = res.results
  } catch {
    /* 校验错误由拦截器提示 */
  } finally {
    ruleTesting.value = false
  }
}
</script>

<template>
  <div v-loading="loading" class="page">
    <div class="title-row">
      <h1>系统设置</h1>
      <RefreshButton :loading="loading" @refresh="load" />
    </div>

    <el-form label-position="top" class="settings-form">
      <section class="panel">
        <h2>开放使用</h2>
        <el-form-item>
          <div class="switch-row">
            <el-switch v-model="form.open_access" />
            <span>
              {{
                form.open_access
                  ? '已开启：访客可直接查询，匿名 API 调用也放行'
                  : '已关闭：查询需要登录或有效 Token'
              }}
            </span>
          </div>
        </el-form-item>
        <el-form-item>
          <div class="switch-row">
            <el-switch v-model="form.allow_registration" />
            <span>{{ form.allow_registration ? '允许用户自助注册' : '已关闭自助注册' }}</span>
          </div>
        </el-form-item>
      </section>

      <section class="panel">
        <h2>限流设置</h2>
        <el-form-item label="Token 默认每日调用上限">
          <el-input-number v-model="form.token_default_daily_limit" :min="1" style="width: 220px" />
        </el-form-item>
        <el-form-item label="匿名访问单 IP 每分钟请求上限">
          <el-input-number
            v-model="form.anonymous_ip_rate_limit_per_min"
            :min="1"
            style="width: 220px"
          />
        </el-form-item>
        <el-form-item label="登录用户单 IP 每分钟请求上限">
          <el-input-number
            v-model="form.user_ip_rate_limit_per_min"
            :min="1"
            style="width: 220px"
          />
        </el-form-item>
      </section>

      <section class="panel">
        <h2>生词本</h2>
        <el-form-item>
          <el-checkbox v-model="vocabUnlimited">不限容量</el-checkbox>
        </el-form-item>
        <el-form-item v-if="!vocabUnlimited" label="单用户/单 Token 生词本容量上限">
          <el-input-number v-model="form.vocab_max_items_per_owner" :min="1" style="width: 220px" />
        </el-form-item>
      </section>

      <section class="panel">
        <h2>站点信息</h2>
        <el-form-item label="站点名称">
          <el-input v-model="form.site_name" style="width: 320px" />
        </el-form-item>
        <el-form-item label="搜索提示语（登录用户在首页搜索框下方看到，限 100 字以内）">
          <el-input
            v-model="form.search_hint_text"
            maxlength="100"
            show-word-limit
            style="width: 320px"
          />
        </el-form-item>
      </section>

      <section class="panel">
        <h2>在线词典</h2>
        <el-form-item>
          <div class="switch-row">
            <el-switch v-model="form.online_dict_enabled" />
            <span>
              {{
                form.online_dict_enabled
                  ? '已开启：检索范围出现【在线】标签，查询维基百科/维基词典/百度百科等在线源'
                  : '已禁用：前台不显示【在线】标签，在线查询接口一并拒绝'
              }}
            </span>
          </div>
        </el-form-item>
        <el-form-item label="出站代理服务器">
          <el-input
            v-model="form.online_dict_proxy"
            maxlength="300"
            placeholder="留空直连；如 http://127.0.0.1:7890"
          />
        </el-form-item>
        <p class="hint">
          维基百科/维基词典的查询经由该代理发出（百度百科直连即可），只支持 http:// 或 https://
          地址。保存后立即生效，无需重启。默认显示部署环境变量 ONLINE_DICT_PROXY 的值；
          改成其它值（包括清空为直连）才会单独保存。
        </p>
        <el-form-item label="启用的源">
          <el-checkbox-group
            :model-value="onlineSourceSelection"
            class="source-group"
            @change="onSourceSelectionChange"
          >
            <el-checkbox v-for="source in ONLINE_SOURCES" :key="source.id" :value="source.id">
              {{ source.label }}
            </el-checkbox>
          </el-checkbox-group>
        </el-form-item>
        <p class="hint">
          不勾的源不参与在线查询（外链按钮也会隐藏）。全部勾选时保存为「默认」，之后新增的源自动启用。
        </p>
      </section>

      <section class="panel">
        <h2>随机浏览</h2>
        <el-form-item>
          <div class="switch-row">
            <el-switch v-model="form.random_browse_enabled" />
            <span>
              {{
                form.random_browse_enabled
                  ? '已开启：检索范围出现【随机】标签，可随机浏览所选词典范围内的词条'
                  : '已禁用：前台不显示【随机】标签，随机接口一并拒绝'
              }}
            </span>
          </div>
        </el-form-item>
        <p class="hint">
          开启后会在后台预热词典的主键区间缓存（启动时、以及每次从关闭改为开启的那一刻），
          这样第一次点【随机】也是毫秒级响应；关闭时不做任何预热。保存后立即生效，无需重启。
        </p>
      </section>

      <section class="panel">
        <h2>TTS 发音（kokoro）</h2>
        <el-form-item>
          <div class="switch-row">
            <el-switch v-model="form.tts_enabled" />
            <span>
              {{
                form.tts_enabled
                  ? '已开启：词条无词典语音时，发音按钮显示「喇叭+T」并用本地模型合成'
                  : '已禁用：词条只使用词典自带的语音（无语音则无发音按钮兜底）'
              }}
            </span>
          </div>
        </el-form-item>
        <el-form-item label="合成引擎">
          <el-radio-group :model-value="form.tts_engine" @update:model-value="onEngineChange">
            <el-radio-button value="edge">edge 在线（微软，默认）</el-radio-button>
            <el-radio-button value="kokoro">kokoro 本地离线</el-radio-button>
          </el-radio-group>
          <p class="hint">
            edge：微软神经嗓音（官方全量 322 个，下拉可搜索；离线时显示精选表），
            质量最好、不占内存，但需要出网（失败自动回落已加载的 kokoro）；
            kokoro：本地 CPU 合成，完全离线，首次使用需下载 ~337MB 模型。
          </p>
        </el-form-item>
        <div class="lang-row">
          <el-form-item label="中文嗓音">
            <div class="voice-row">
              <el-select
                v-model="form.tts_voice_zh"
                filterable
                allow-create
                :loading="!edgeVoicesLoaded && form.tts_engine === 'edge'"
                placeholder="搜索或输入任意 edge 嗓音名"
              >
                <el-option
                  v-for="v in zhVoiceOptions"
                  :key="v.value"
                  :label="v.label"
                  :value="v.value"
                />
                <el-option-group
                  v-if="form.tts_engine === 'edge' && edgeOtherVoices.length"
                  label="多语种（日韩法德俄西等）"
                >
                  <el-option
                    v-for="v in edgeOtherVoices"
                    :key="v.value"
                    :label="v.label"
                    :value="v.value"
                  />
                </el-option-group>
              </el-select>
              <el-button
                :loading="previewingZh"
                :disabled="!form.tts_enabled"
                title="用这句试听当前选择的嗓音"
                @click="previewVoice(form.tts_voice_zh)"
              >
                试听
              </el-button>
            </div>
          </el-form-item>
          <el-form-item label="英文嗓音">
            <div class="voice-row">
              <el-select
                v-model="form.tts_voice_en"
                filterable
                allow-create
                :loading="!edgeVoicesLoaded && form.tts_engine === 'edge'"
                placeholder="搜索或输入任意 edge 嗓音名"
              >
                <el-option v-for="v in enVoiceOptions" :key="v.value" :label="v.label" :value="v.value" />
              </el-select>
              <el-button
                :loading="previewingEn"
                :disabled="!form.tts_enabled"
                @click="previewVoice(form.tts_voice_en)"
              >
                试听
              </el-button>
            </div>
          </el-form-item>
        </div>
        <p class="hint">
          默认引擎 edge（微软在线神经嗓音，MP3 直出）；kokoro 为本地离线备选
         （内嵌 Kokoro-82M，首次使用自动下载模型 ~337MB 到 ~/.cache/k/）。
          结果按词×嗓音×引擎缓存，重复播放零成本。
        </p>

        <h3 class="sub-title">注音提取规则（生僻字读音兜底）</h3>
        <p class="hint">
          词头是 PUA 私有区字形（说文系古文字）这类读不出音的字时，TTS 会从词条里找拼音：
          先看 phonetic 列，再按下面的规则（按序、<b>第一个捕获组</b> = 拼音，
          支持前后看断言），最后是内置兜底 &lt;py&gt;…&lt;/py&gt; 与 class="py"。
          @@@LINK 链接词条会自动跟随（最多 5 跳）。
        </p>
        <div v-for="(rule, idx) in form.tts_pinyin_rules" :key="idx" class="rule-row">
          <el-switch v-model="rule.enabled" />
          <el-input v-model="rule.name" placeholder="规则名（如：说文 py）" class="rule-name" />
          <el-input
            v-model="rule.pattern"
            :placeholder="'正则，如 <py>([^<]+)</py>'"
            class="rule-pattern"
            spellcheck="false"
          />
          <el-button type="danger" text title="删除这条规则" @click="form.tts_pinyin_rules.splice(idx, 1)">
            删除
          </el-button>
        </div>
        <el-button text type="primary" @click="addPinyinRule">＋ 添加规则</el-button>

        <el-form-item class="rule-test">
          <template #label>规则测试（贴一段释义 HTML）</template>
          <el-input
            v-model="ruleTestText"
            type="textarea"
            :rows="3"
            placeholder='例如：<td py><py>jīn</py></td>'
            spellcheck="false"
          />
          <div class="rule-test-actions">
            <el-button :loading="ruleTesting" :disabled="!ruleTestText.trim()" @click="testPinyinRules">
              用当前编辑中的规则测试
            </el-button>
          </div>
          <ul v-if="ruleTestResults" class="rule-test-results">
            <li v-for="(r, i) in ruleTestResults" :key="i">
              <el-tag :type="r.matched ? 'success' : 'info'" size="small">
                {{ r.matched ? '命中' : '未命中' }}
              </el-tag>
              {{ r.name }}：
              <code v-if="r.value">{{ r.value }}</code>
              <span v-else class="hint">—</span>
              <span v-if="r.note" class="hint">{{ r.note }}</span>
            </li>
          </ul>
        </el-form-item>
      </section>

      <el-button type="primary" :loading="saving" @click="save">保存设置</el-button>
    </el-form>
  </div>
</template>

<style scoped>
.page {
  max-width: 720px;
  margin: var(--space-6) auto;
  padding: 0 var(--space-4);
}

.title-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-5);
}

h1 {
  font-size: var(--text-xl);
  color: var(--color-text-primary);
  margin: 0;
}

.voice-row {
  display: flex;
  gap: var(--space-2);
  width: 100%;
}

.voice-row .el-select {
  flex: 1;
}

.lang-row {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: var(--space-4);
}

.panel {
  background: var(--color-bg-surface);
  border-radius: var(--radius-lg);
  box-shadow: var(--shadow-elevation-1);
  padding: var(--space-5);
  margin-bottom: var(--space-4);
}

.panel h2 {
  font-size: var(--text-md);
  font-weight: var(--font-weight-medium);
  color: var(--color-text-primary);
  margin: 0 0 var(--space-4);
}

.switch-row {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  font-size: var(--text-sm);
  color: var(--color-text-secondary);
}

.hint {
  margin: var(--space-3) 0 0;
  font-size: var(--text-xs);
  line-height: var(--leading-body);
  color: var(--color-text-tertiary);
}

.source-group {
  display: flex;
  flex-wrap: wrap;
}

.sub-title {
  margin: var(--space-5) 0 var(--space-2);
  font-size: var(--text-md);
  font-weight: var(--font-weight-semibold);
}

.rule-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-bottom: var(--space-2);
}

.rule-row .rule-name {
  flex: 0 0 180px;
}

.rule-row .rule-pattern {
  flex: 1;
  font-family: var(--font-family-mono);
}

.rule-test {
  margin-top: var(--space-4);
}

.rule-test-actions {
  margin-top: var(--space-2);
}

.rule-test-results {
  margin: var(--space-2) 0 0;
  padding-left: var(--space-5);
  font-size: var(--text-sm);
  line-height: 1.9;
}

.rule-test-results code {
  font-family: var(--font-family-mono);
  color: var(--color-brand-600, var(--color-brand-500));
}

.hint code {
  font-family: var(--font-family-mono);
  color: var(--color-text-secondary);
}

.intro {
  margin: 0 0 var(--space-4);
  line-height: var(--leading-body);
}

.status-row {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-3);
}

.install-guide {
  margin-top: var(--space-4);
  border-top: 1px solid var(--color-border);
}

.step {
  margin: 0 0 var(--space-2);
  font-size: var(--text-xs);
  line-height: var(--leading-body);
  color: var(--color-text-secondary);
}

.code {
  margin: 0 0 var(--space-3);
  padding: var(--space-3);
  border-radius: var(--radius-sm);
  background: var(--color-bg-base);
  color: var(--color-text-secondary);
  font-family: var(--font-family-mono);
  font-size: var(--text-xs);
  line-height: var(--leading-body);
  overflow-x: auto;
  white-space: pre;
}
</style>
