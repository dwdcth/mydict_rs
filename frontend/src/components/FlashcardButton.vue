<script setup lang="ts">
defineProps<{
  /** 已在复习（加过卡） */
  active: boolean
  loading?: boolean
}>()

const emit = defineEmits<{ add: [] }>()
</script>

<template>
  <button
    type="button"
    class="flashcard-btn"
    :class="{ active }"
    :disabled="loading"
    :aria-label="active ? '已在复习计划中' : '加入复习（FSRS 间隔重复）'"
    :title="active ? '已在复习计划中' : '加入复习（FSRS 间隔重复）'"
    @click="emit('add')"
  >
    <!-- 卡片叠层图标 -->
    <svg viewBox="0 0 24 24" width="20" height="20" fill="none" stroke="currentColor" stroke-width="1.6">
      <rect x="7" y="3.5" width="13" height="13" rx="2" opacity="0.45" />
      <rect x="4" y="7.5" width="13" height="13" rx="2" fill="var(--color-bg-surface)" />
    </svg>
  </button>
</template>

<style scoped>
.flashcard-btn {
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
  transition: transform 100ms ease;
  margin: 3px;
}

.flashcard-btn:hover {
  background: var(--color-hover-tint);
}

.flashcard-btn.active {
  color: var(--color-brand-500);
  border-color: var(--color-brand-500);
  transform: scale(1.08);
}
</style>
