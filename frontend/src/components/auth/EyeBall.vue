<!-- Character animation ported from https://github.com/guohaolian/animated-characters-login-page (MIT); see THIRD_PARTY_NOTICES.md. -->
<script setup lang="ts">
import { computed, type CSSProperties } from 'vue'
import Pupil from './Pupil.vue'

const props = withDefaults(defineProps<{
  size?: number
  pupilSize?: number
  maxDistance?: number
  eyeColor?: string
  pupilColor?: string
  isBlinking?: boolean
  isSad?: boolean
  sadRotate?: number
  lookX?: number
  lookY?: number
}>(), {
  size: 48,
  pupilSize: 16,
  maxDistance: 10,
  eyeColor: 'white',
  pupilColor: 'black',
  isBlinking: false,
  isSad: false,
  sadRotate: 0,
  lookX: 0,
  lookY: 0,
})

const eyeStyle = computed<CSSProperties>(() => ({
  width: `${props.size}px`,
  height: props.isBlinking ? '2px' : props.isSad ? `${props.size * 0.5}px` : `${props.size}px`,
  backgroundColor: props.eyeColor,
  borderRadius: props.isSad ? `0 0 ${props.size}px ${props.size}px` : '50%',
  transform: props.isSad ? `rotate(${props.sadRotate}deg)` : 'rotate(0deg)',
}))

const pupilLookY = computed(() => (props.isSad ? -1 : props.lookY))
</script>

<template>
  <div class="auth-eyeball" :style="eyeStyle">
    <Pupil
      v-if="!isBlinking"
      :size="pupilSize"
      :max-distance="maxDistance"
      :pupil-color="pupilColor"
      :look-x="lookX"
      :look-y="pupilLookY"
    />
  </div>
</template>
