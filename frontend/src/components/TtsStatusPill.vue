<script setup lang="ts">
import { useTtsPlayer } from '../utils/ttsPlayer'

/**
 * 朗读状态胶囊（右下角，ScrollButtons 上方）：
 * 合成中显示转圈，播放中可点击停止。朗读的触发点在词条 iframe 或父页面选区气泡。
 */
const { state, stop } = useTtsPlayer()
</script>

<template>
  <transition name="pill">
    <div v-if="state.status !== 'idle'" class="tts-pill" role="status">
      <span v-if="state.status === 'loading'" class="spinner" aria-hidden="true" />
      <span class="pill-text">
        {{ state.status === 'loading' ? '合成中…' : '朗读中' }}
      </span>
      <span
        v-if="state.status === 'playing'"
        class="pill-stop"
        role="button"
        tabindex="0"
        title="停止朗读"
        @click="stop"
        @keydown.enter.stop="stop"
      >
        ■
      </span>
    </div>
  </transition>
</template>

<style scoped>
.tts-pill {
  position: fixed;
  right: var(--space-4);
  /* 挂在回到顶部按钮上方（ScrollButtons 高度 + 间距） */
  bottom: calc(var(--space-4) + 96px);
  z-index: 1200;
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border-radius: var(--radius-full);
  background: var(--color-bg-surface);
  border: 1px solid var(--color-border);
  box-shadow: var(--shadow-elevation-2, 0 4px 14px rgba(0, 0, 0, 0.18));
  font-size: var(--text-sm);
  color: var(--color-text-primary);
}

.spinner {
  width: 12px;
  height: 12px;
  border-radius: 50%;
  border: 2px solid var(--color-border);
  border-top-color: var(--color-brand-500);
  animation: spin 0.8s linear infinite;
}

@keyframes spin {
  to {
    transform: rotate(360deg);
  }
}

.pill-stop {
  cursor: pointer;
  color: var(--color-danger);
  padding: 0 2px;
  user-select: none;
}

.pill-enter-active,
.pill-leave-active {
  transition: opacity 0.15s ease, transform 0.15s ease;
}

.pill-enter-from,
.pill-leave-to {
  opacity: 0;
  transform: translateY(6px);
}
</style>
