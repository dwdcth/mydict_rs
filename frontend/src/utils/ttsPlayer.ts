import { reactive } from 'vue'
import { ElMessage } from 'element-plus'
import { fetchTtsBlob } from '../api/dict'

/**
 * 全局 TTS 播放器（单例）：
 * - 词条 iframe 里【朗读】按钮（postMessage mydict:tts）与父页面选区气泡共用
 * - state 供 TtsStatusPill 显示「合成中…/朗读中（可停止）」
 * - 同一时间只播一段，新的播放请求会先停掉旧的
 */
const state = reactive({
  status: 'idle' as 'idle' | 'loading' | 'playing',
  text: '',
})

let audio: HTMLAudioElement | null = null
let objectUrl: string | null = null
/** 递增的播放请求序号：慢请求回来时若已被新请求取代则丢弃 */
let playSeq = 0

function stop() {
  playSeq += 1 // 让在途的合成结果作废
  if (audio) {
    audio.pause()
    audio = null
  }
  state.status = 'idle'
  state.text = ''
}

async function play(text: string) {
  const trimmed = text.replace(/\s+/g, ' ').trim()
  if (!trimmed || trimmed.length > 500) {
    if (trimmed.length > 500) ElMessage.warning('选区太长（超过 500 字），朗读不了')
    return
  }
  playSeq += 1
  const seq = playSeq
  if (audio) {
    audio.pause()
    audio = null
  }
  state.status = 'loading'
  state.text = trimmed
  try {
    const blob = new Blob([await fetchTtsBlob(trimmed)], { type: 'audio/wav' })
    if (seq !== playSeq) return // 已被停止/取代
    if (objectUrl) URL.revokeObjectURL(objectUrl)
    objectUrl = URL.createObjectURL(blob)
    const element = new Audio(objectUrl)
    element.onended = () => {
      if (seq === playSeq) stop()
    }
    element.onerror = () => {
      if (seq === playSeq) {
        ElMessage.error('朗读播放失败')
        stop()
      }
    }
    audio = element
    await element.play()
    if (seq !== playSeq) return
    state.status = 'playing'
  } catch {
    if (seq === playSeq) {
      // 未启用/限流等错误已由响应拦截器提示
      stop()
    }
  }
}

export function useTtsPlayer() {
  return { state, play, stop }
}
