<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'
import { useTtsPlayer } from '../utils/ttsPlayer'

/**
 * 父页面选区朗读气泡：在非 iframe 区域（生词本释义、选择题句子、历史列表等）
 * 选中文字后，选区末端弹出「🔊 朗读」。词条 iframe 内的选区由引导脚本的
 * 【查词｜朗读】菜单负责（沙箱 iframe 父页读不到选区），两边共用全局播放器。
 */
const { play } = useTtsPlayer()

const visible = ref(false)
const x = ref(0)
const y = ref(0)
const text = ref('')
const root = ref<HTMLElement | null>(null)

function hide() {
  visible.value = false
  text.value = ''
}

function selectionInsidePopup(target: Node | null): boolean {
  return !!root.value && !!target && root.value.contains(target)
}

function onSelectionChange() {
  const selection = document.getSelection()
  if (!selection || selection.isCollapsed || selection.rangeCount === 0) {
    hide()
    return
  }
  const anchor = selection.anchorNode
  if (selectionInsidePopup(anchor)) return
  const selected = selection.toString().replace(/\s+/g, ' ').trim()
  if (!selected || selected.length > 500) {
    hide()
    return
  }
  const rect = selection.getRangeAt(0).getBoundingClientRect()
  if (rect.width === 0 && rect.height === 0) {
    hide()
    return
  }
  text.value = selected
  // 气泡挂在选区下方居中；快照量宽放在样式里做 clamp（max-width 足够小）
  x.value = rect.left + rect.width / 2
  y.value = rect.bottom + 8
  visible.value = true
}

let mouseUpTimer = 0

function onMouseUp(event: MouseEvent) {
  if (selectionInsidePopup(event.target as Node | null)) return
  // 拖选完成才触发；双击选词也走这里
  window.clearTimeout(mouseUpTimer)
  mouseUpTimer = window.setTimeout(onSelectionChange, 60)
}

function onScrollOrResize() {
  hide()
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && visible.value) {
    hide()
    event.stopPropagation()
  }
}

function onClickPlay() {
  play(text.value)
  hide()
}

onMounted(() => {
  document.addEventListener('mouseup', onMouseUp)
  document.addEventListener('selectionchange', onSelectionChange)
  window.addEventListener('scroll', onScrollOrResize, true)
  window.addEventListener('resize', onScrollOrResize)
  document.addEventListener('keydown', onKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('mouseup', onMouseUp)
  document.removeEventListener('selectionchange', onSelectionChange)
  window.removeEventListener('scroll', onScrollOrResize, true)
  window.removeEventListener('resize', onScrollOrResize)
  document.removeEventListener('keydown', onKeydown)
  window.clearTimeout(mouseUpTimer)
})
</script>

<template>
  <div
    v-if="visible"
    ref="root"
    class="selection-tts"
    :style="{ left: `${x}px`, top: `${y}px` }"
    role="button"
    tabindex="0"
    title="朗读选中内容"
    @mousedown.prevent
    @click="onClickPlay"
    @keydown.enter="onClickPlay"
  >
    🔊 朗读
  </div>
</template>

<style scoped>
.selection-tts {
  position: fixed;
  transform: translateX(-50%);
  z-index: 2147483000;
  padding: 4px 12px;
  border-radius: var(--radius-full);
  border: 1px solid var(--color-border);
  background: var(--color-bg-surface);
  color: var(--color-brand-500);
  box-shadow: 0 4px 14px rgba(0, 0, 0, 0.18);
  font-size: var(--text-sm);
  line-height: 1.7;
  white-space: nowrap;
  cursor: pointer;
  user-select: none;
}

.selection-tts:hover {
  background: var(--color-hover-tint);
}
</style>
