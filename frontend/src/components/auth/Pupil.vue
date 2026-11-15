<!-- Character animation ported from https://github.com/guohaolian/animated-characters-login-page (MIT); see THIRD_PARTY_NOTICES.md. -->
<script setup lang="ts">
import { computed, type CSSProperties } from 'vue'

const props = withDefaults(defineProps<{
  size?: number
  maxDistance?: number
  pupilColor?: string
  lookX?: number
  lookY?: number
  isBlinking?: boolean
}>(), {
  size: 12,
  maxDistance: 5,
  pupilColor: 'black',
  lookX: 0,
  lookY: 0,
  isBlinking: false,
})

// The pointer is tracked once in AnimatedCharacters; children only clamp the
// offset they receive so no extra listener or layout read is needed here.
const pupilStyle = computed<CSSProperties>(() => {
  const distance = Math.min(Math.hypot(props.lookX, props.lookY), props.maxDistance)
  const angle = Math.atan2(props.lookY, props.lookX)
  // Round to two decimals so SSR output stays readable.
  const offsetX = Math.round(Math.cos(angle) * distance * 100) / 100
  const offsetY = Math.round(Math.sin(angle) * distance * 100) / 100
  return {
    width: `${props.size}px`,
    height: props.isBlinking ? '2px' : `${props.size}px`,
    backgroundColor: props.pupilColor,
    transform: `translate(${offsetX}px, ${offsetY}px)`,
  }
})
</script>

<template>
  <div class="auth-pupil" :style="pupilStyle" />
</template>
