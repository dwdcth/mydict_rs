<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { fetchWordAudio } from '../api/dict'
import { useTtsPlayer } from '../utils/ttsPlayer'

/**
 * 词条头部发音按钮：
 * - 词典里有语音 → 普通喇叭，点击播放词典音频（sound:// 已由服务端解析成 /dict-res
 *   地址）；播放失败（资源缺失等）自动回退 TTS
 * - 没有语音 → 喇叭 + 右上角「T」角标，点击用 kokoro TTS 合成播放
 *
 * 播放统一走全局 ttsPlayer（与选区朗读共用）：串号去重保证任何双击/双触发
 * 只会有一路声音——此前按钮私有 Audio 与全局播放器互不去重，手机快速双击时
 * 会出现两个 Audio 先后起播（听感「豫…豫章」发了两个音）。
 */
const props = defineProps<{
  word: string
  dictionaryId: number
}>()

type Mode = 'loading' | 'dict' | 'tts'

const mode = ref<Mode>('loading')
const audioUrl = ref<string | null>(null)
const player = useTtsPlayer()
const playingThisWord = ref(false)

/** 播放器状态与本词条对上时显示进行中（避免按钮在别的文本朗读时也转圈） */
const busy = computed(
  () => player.state.status !== 'idle' && player.state.text.startsWith(props.word.trim()),
)

onMounted(async () => {
  try {
    const result = await fetchWordAudio(props.word, props.dictionaryId)
    audioUrl.value = result.audio_url
    mode.value = result.audio_url ? 'dict' : 'tts'
  } catch {
    // 查不到（词条变化/限流等）→ 静默按 TTS 处理
    mode.value = 'tts'
  }
})

let dictAudio: HTMLAudioElement | null = null

function stopDictAudio() {
  if (dictAudio) {
    dictAudio.pause()
    dictAudio = null
  }
  playingThisWord.value = false
}

function fallbackToTts() {
  // 词典音频失效 → 本词条后续点击直接走 TTS
  mode.value = 'tts'
  player.play(props.word)
}

function onClick() {
  if (busy.value) {
    // 再点一次 = 停止（全局播放器统一停）
    player.stop()
    return
  }
  if (mode.value === 'dict' && audioUrl.value) {
    stopDictAudio()
    playingThisWord.value = true
    dictAudio = new Audio(audioUrl.value)
    dictAudio.onended = () => stopDictAudio()
    dictAudio.onerror = () => {
      stopDictAudio()
      fallbackToTts()
    }
    dictAudio.play().catch(() => {
      stopDictAudio()
      fallbackToTts()
    })
    return
  }
  player.play(props.word)
}
</script>

<template>
  <button
    type="button"
    class="audio-btn"
    :class="{ tts: mode === 'tts' }"
    :disabled="mode === 'loading'"
    :aria-label="mode === 'tts' ? 'TTS 发音' : '播放词典发音'"
    :title="mode === 'tts' ? 'TTS 发音（词典未提供语音）' : '播放词典发音'"
    @click.stop="onClick"
  >
    <svg
      viewBox="0 0 24 24"
      width="20"
      height="20"
      fill="none"
      stroke="currentColor"
      stroke-width="1.6"
    >
      <path
        stroke-linecap="round"
        stroke-linejoin="round"
        d="M11 5 6.5 8.5H3.5v7h3L11 19V5Z"
      />
      <path
        v-if="!busy"
        stroke-linecap="round"
        stroke-linejoin="round"
        d="M15 9.5a4 4 0 0 1 0 5M17.5 7a7.5 7.5 0 0 1 0 10"
      />
    </svg>
    <!-- TTS 角标：喇叭右上角一个 T -->
    <span v-if="mode === 'tts'" class="tts-badge">T</span>
    <span v-if="busy" class="busy-dot" />
  </button>
</template>

<style scoped>
.audio-btn {
  position: relative;
  border: 1px solid var(--color-border);
  background: transparent;
  color: var(--color-text-secondary);
  width: 32px;
  height: 32px;
  border-radius: var(--radius-full);
  display: inline-flex;
  align-items: center;
  justify-content: center;
  cursor: pointer;
  margin: 3px;
}

.audio-btn:hover {
  background: var(--color-hover-tint);
}

.audio-btn:disabled {
  cursor: default;
}

.audio-btn.tts {
  color: var(--color-brand-500);
}

.tts-badge {
  position: absolute;
  top: -4px;
  right: -4px;
  width: 14px;
  height: 14px;
  border-radius: var(--radius-full);
  background: var(--color-brand-500);
  color: #fff;
  font-size: 9px;
  line-height: 14px;
  text-align: center;
  font-weight: var(--font-weight-semibold);
}

.busy-dot {
  position: absolute;
  bottom: 2px;
  left: 50%;
  transform: translateX(-50%);
  width: 10px;
  height: 3px;
  border-radius: 2px;
  background: var(--color-brand-500);
  animation: pulse 1s ease-in-out infinite;
}

@keyframes pulse {
  0%,
  100% {
    opacity: 0.3;
  }
  50% {
    opacity: 1;
  }
}
</style>
