import { reactive } from 'vue'
import { ElMessage } from 'element-plus'
import { fetchTtsBlob } from '../api/dict'

/**
 * 全局 TTS 播放器（单例）：
 * - 词条 iframe 里【朗读】按钮（postMessage mydict:tts）与父页面选区气泡共用
 * - 词条头部发音按钮也统一走这里：词典音频走 playUrl、TTS 走 play——
 *   所有声音同在一个 Audio 实例里，任何双击/双触发都不会叠音
 * - state 供 TtsStatusPill 显示「合成中…/朗读中（可停止）」
 * - 同一段时间只播一段；**同一段加载中/播放中再来一次请求直接忽略**
 *   （此前行为是停掉重播：合成要 1~2 秒，用户没听到声音再点一下，
 *   「yù(被截断) + yù——」听感就是首音节发了两个音）
 *
 * 移动端自动播放策略：合成是异步的，等 1~2 秒后 play() 已脱离用户手势
 * 上下文，iOS/Safari 会直接拒绝（听感「点了没反应」）。对策是全站共用
 * 一个 Audio 实例，并在第一个用户手势时用 50ms 静音音轨把它解锁——解锁
 * 过的实例之后换 src 播放不再需要手势（每次 new Audio 会重新被锁）。
 */
const state = reactive({
  status: 'idle' as 'idle' | 'loading' | 'playing',
  text: '',
})

let playSeq = 0
let objectUrl: string | null = null
let loadingTimer = 0
let playingTimer = 0
let sharedAudio: HTMLAudioElement | null = null

function getAudio(): HTMLAudioElement {
  if (!sharedAudio) {
    sharedAudio = new Audio()
    sharedAudio.preload = 'auto'
  }
  return sharedAudio
}

function clearTimers() {
  if (loadingTimer) {
    window.clearTimeout(loadingTimer)
    loadingTimer = 0
  }
  if (playingTimer) {
    window.clearTimeout(playingTimer)
    playingTimer = 0
  }
}

function stop() {
  playSeq += 1 // 让在途的合成结果作废
  clearTimers()
  if (sharedAudio) sharedAudio.pause()
  state.status = 'idle'
  state.text = ''
}

// --- 首手势解锁（iOS/Safari） --------------------------------------------

const SILENT_WAV =
  'data:audio/wav;base64,UklGRkQDAABXQVZFZm10IBAAAAABAAEAQB8AAIA+AAACABAAZGF0YSADAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=='

let unlockArmed = false

function unlockOnGesture() {
  if (unlockArmed) return
  unlockArmed = true
  const el = getAudio()
  el.onended = null
  el.onerror = null
  el.src = SILENT_WAV
  // 被拦截也不影响主流程：真正的播放请求发生在点击手势内
  el.play().then(
    () => {
      el.pause()
    },
    () => {},
  )
}

if (typeof document !== 'undefined') {
  document.addEventListener('pointerdown', unlockOnGesture, { capture: true })
  document.addEventListener('touchend', unlockOnGesture, { capture: true })
}

// --- 播放 ----------------------------------------------------------------

function armWatchdogs(seq: number, element: HTMLAudioElement) {
  // 合成/加载卡死 → 自动复位（否则按钮永远转圈，状态胶囊永远「合成中」）
  loadingTimer = window.setTimeout(() => {
    if (seq === playSeq) {
      ElMessage.error('语音加载超时')
      stop()
    }
  }, 45_000)
  // onended 丢失（页面切后台被浏览器挂起等）→ 按时长兜底复位
  const dur = Number.isFinite(element.duration) && element.duration > 0 ? element.duration : 30
  playingTimer = window.setTimeout(() => {
    if (seq === playSeq) stop()
  }, dur * 1000 + 3000)
}

function handlePlayRejection(seq: number, onError?: () => void) {
  if (seq !== playSeq) return
  stop()
  onError?.()
}

async function play(text: string) {
  const trimmed = text.replace(/\s+/g, ' ').trim()
  if (!trimmed || trimmed.length > 500) {
    if (trimmed.length > 500) ElMessage.warning('选区太长（超过 500 字），朗读不了')
    return
  }
  // 同一段正在加载/播放：忽略重复触发（双击、多入口同发）
  if (state.text === trimmed && state.status !== 'idle') return
  playSeq += 1
  const seq = playSeq
  clearTimers()
  state.status = 'loading'
  state.text = trimmed
  try {
    // 直接用服务端返回的 blob（edge=audio/mpeg、kokoro=audio/wav）
    const blob = new Blob([await fetchTtsBlob(trimmed)])
    if (seq !== playSeq) return // 已被停止/取代
    if (objectUrl) URL.revokeObjectURL(objectUrl)
    objectUrl = URL.createObjectURL(blob)
    const element = getAudio()
    element.onended = null
    element.onerror = () => {
      if (seq === playSeq) {
        ElMessage.error('朗读播放失败')
        stop()
      }
    }
    element.onended = () => {
      if (seq === playSeq) stop()
    }
    element.src = objectUrl
    try {
      await element.play()
      if (seq !== playSeq) return
      state.status = 'playing'
      armWatchdogs(seq, element)
    } catch (err) {
      if (seq !== playSeq) return
      if ((err as DOMException)?.name === 'NotAllowedError') {
        ElMessage.warning('浏览器拦截了自动播放，请再点一次')
      }
      handlePlayRejection(seq)
    }
  } catch {
    if (seq === playSeq) {
      // 未启用/限流等错误已由响应拦截器提示
      stop()
    }
  }
}

/**
 * 播放现成音频地址（词条的词典语音、管理端嗓音试听）。
 * label 用于与 play() 共用同一套去重/状态（按钮 busy、状态胶囊）；
 * onError 在加载或播放失败且未被新请求取代时回调（调用方回退 TTS）。
 */
async function playUrl(url: string, label: string, onError?: () => void) {
  const trimmed = label.replace(/\s+/g, ' ').trim()
  if (!trimmed) return
  if (state.text === trimmed && state.status !== 'idle') return
  playSeq += 1
  const seq = playSeq
  clearTimers()
  state.status = 'loading'
  state.text = trimmed
  const element = getAudio()
  element.onended = null
  element.onerror = () => handlePlayRejection(seq, onError)
  element.onended = () => {
    if (seq === playSeq) stop()
  }
  element.src = url
  try {
    await element.play()
    if (seq !== playSeq) return
    state.status = 'playing'
    armWatchdogs(seq, element)
  } catch (err) {
    if (seq !== playSeq) return
    if ((err as DOMException)?.name === 'NotAllowedError') {
      ElMessage.warning('浏览器拦截了自动播放，请再点一次')
    }
    handlePlayRejection(seq, onError)
  }
}

export function useTtsPlayer() {
  return { state, play, playUrl, stop }
}
