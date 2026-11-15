<!-- Character animation ported from https://github.com/guohaolian/animated-characters-login-page (MIT); see THIRD_PARTY_NOTICES.md. -->
<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch, type CSSProperties } from 'vue'
import EyeBall from './EyeBall.vue'
import Pupil from './Pupil.vue'

const props = withDefaults(defineProps<{
  isTyping?: boolean
  showPassword?: boolean
  passwordLength?: number
  loginFailed?: boolean
}>(), {
  isTyping: false,
  showPassword: false,
  passwordLength: 0,
  loginFailed: false,
})

type CharacterKey = 'purple' | 'black' | 'orange' | 'yellow'
type FacePosition = { faceX: number; faceY: number; bodySkew: number }
type CharacterRange = { minX: number; maxX: number; minY: number; maxY: number }
type Point = { x: number; y: number }

const CHARACTER_KEYS: CharacterKey[] = ['purple', 'black', 'orange', 'yellow']
const CHARACTER_RANGES: Record<CharacterKey, CharacterRange> = {
  purple: { minX: -46, maxX: 18, minY: -8, maxY: 5 },
  black: { minX: -15, maxX: 15, minY: -10, maxY: 10 },
  orange: { minX: -46, maxX: 20, minY: -18, maxY: 20 },
  yellow: { minX: -15, maxX: 15, minY: -10, maxY: 10 },
}
const IDLE_FACE: FacePosition = { faceX: 0, faceY: 0, bodySkew: 0 }
const POINTER_RANGE = 300
const ENTRANCE_DELAY = 1400
const BLINK_HOLD = 150
const YELLOW_MOUTH_STRAIGHT = 'M0 10 Q10 10, 20 10 Q30 10, 40 10 Q50 10, 60 10 Q70 10, 80 10'
const YELLOW_MOUTH_WAVY = 'M0 10 Q10 2, 20 10 Q30 18, 40 10 Q50 2, 60 10 Q70 18, 80 10'

const purpleRef = ref<HTMLElement | null>(null)
const blackRef = ref<HTMLElement | null>(null)
const orangeRef = ref<HTMLElement | null>(null)
const yellowRef = ref<HTMLElement | null>(null)

const hasEntered = ref(false)
const reducedMotion = ref(false)
const lookingAtEachOther = ref(false)
const purplePeeking = ref(false)
const blinking = reactive<Record<CharacterKey, boolean>>({
  purple: false,
  black: false,
  orange: false,
  yellow: false,
})
const pointer = reactive<Point>({ x: 0, y: 0 })
const centers = reactive<Record<CharacterKey, Point>>({
  purple: { x: 0, y: 0 },
  black: { x: 0, y: 0 },
  orange: { x: 0, y: 0 },
  yellow: { x: 0, y: 0 },
})

const passwordVisible = computed(() => props.passwordLength > 0 && props.showPassword)
const hidingPassword = computed(() => props.passwordLength > 0 && !props.showPassword)
const typingPose = computed(() => props.isTyping || hidingPassword.value)
const mouthModifier = computed(() => {
  if (props.loginFailed) return 'sad'
  return typingPose.value ? 'typing' : ''
})

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value))
}

function faceOf(key: CharacterKey): FacePosition {
  if (reducedMotion.value) return IDLE_FACE
  const range = CHARACTER_RANGES[key]
  const center = centers[key]
  const deltaX = pointer.x - center.x
  const deltaY = pointer.y - center.y
  const scaleX = Math.max(Math.abs(range.minX), Math.abs(range.maxX))
  const scaleY = Math.max(Math.abs(range.minY), Math.abs(range.maxY))
  return {
    faceX: clamp(deltaX / (POINTER_RANGE / scaleX), range.minX, range.maxX),
    faceY: clamp(deltaY / (POINTER_RANGE / scaleY), range.minY, range.maxY),
    bodySkew: clamp(-deltaX / 120, -6, 6),
  }
}

const purpleFace = computed(() => faceOf('purple'))
const blackFace = computed(() => faceOf('black'))
const orangeFace = computed(() => faceOf('orange'))
const yellowFace = computed(() => faceOf('yellow'))

const purpleTransform = computed(() => {
  if (passwordVisible.value) return 'skewX(0deg)'
  if (typingPose.value) return `skewX(${purpleFace.value.bodySkew - 12}deg) translateX(40px)`
  return `skewX(${purpleFace.value.bodySkew}deg)`
})

const blackTransform = computed(() => {
  if (passwordVisible.value) return 'skewX(0deg)'
  if (lookingAtEachOther.value) return `skewX(${blackFace.value.bodySkew * 1.5 + 10}deg) translateX(20px)`
  if (typingPose.value) return `skewX(${blackFace.value.bodySkew * 1.5}deg)`
  return `skewX(${blackFace.value.bodySkew}deg)`
})

const purpleBodyStyle = computed<CSSProperties>(() => ({
  height: typingPose.value ? '440px' : '400px',
  transform: hasEntered.value ? purpleTransform.value : undefined,
}))

const blackBodyStyle = computed<CSSProperties>(() => ({
  transform: hasEntered.value ? blackTransform.value : undefined,
}))

const orangeBodyStyle = computed<CSSProperties>(() => ({
  transform: hasEntered.value
    ? passwordVisible.value
      ? 'skewX(0deg)'
      : `skewX(${orangeFace.value.bodySkew}deg)`
    : undefined,
}))

const yellowBodyStyle = computed<CSSProperties>(() => ({
  transform: hasEntered.value
    ? passwordVisible.value
      ? 'skewX(0deg)'
      : `skewX(${yellowFace.value.bodySkew}deg)`
    : undefined,
}))

const purpleEyesStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 50 : lookingAtEachOther.value ? 85 : 75 + purpleFace.value.faceX}px`,
  top: `${passwordVisible.value ? 20 : lookingAtEachOther.value ? 50 : 25 + purpleFace.value.faceY}px`,
}))

const blackEyesStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 10 : lookingAtEachOther.value ? 32 : 26 + blackFace.value.faceX}px`,
  top: `${passwordVisible.value ? 28 : lookingAtEachOther.value ? 12 : 32 + blackFace.value.faceY}px`,
}))

const orangeEyesStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 80 : 112 + orangeFace.value.faceX}px`,
  top: `${passwordVisible.value ? 55 : 60 + orangeFace.value.faceY}px`,
}))

const yellowEyesStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 20 : 52 + yellowFace.value.faceX}px`,
  top: `${passwordVisible.value ? 35 : 40 + yellowFace.value.faceY}px`,
}))

const purpleMouthStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 72 : lookingAtEachOther.value ? 106 : 97 + purpleFace.value.faceX}px`,
  top: `${passwordVisible.value ? 57 : lookingAtEachOther.value ? 82 : 57 + purpleFace.value.faceY}px`,
  // Counter-skew keeps the typing mouth upright while the body leans aside.
  transform: mouthModifier.value === 'typing'
    ? `translateX(13.5px) translateY(-28px) skewX(${-((purpleFace.value.bodySkew || 0) - 12)}deg)`
    : undefined,
}))

const orangeMouthStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 94 : 126 + orangeFace.value.faceX}px`,
  top: `${passwordVisible.value ? 87 : 92 + orangeFace.value.faceY}px`,
}))

const yellowMouthStyle = computed<CSSProperties>(() => ({
  left: `${passwordVisible.value ? 10 : 40 + yellowFace.value.faceX}px`,
  top: `${passwordVisible.value ? 88 : 88 + yellowFace.value.faceY}px`,
}))

const yellowMouthPath = computed(() => (props.loginFailed ? YELLOW_MOUTH_WAVY : YELLOW_MOUTH_STRAIGHT))

const purpleLook = computed<Point>(() => {
  if (passwordVisible.value) return { x: purplePeeking.value ? 4 : -4, y: purplePeeking.value ? 5 : -4 }
  if (lookingAtEachOther.value) return { x: 3, y: 4 }
  return { x: purpleFace.value.faceX, y: purpleFace.value.faceY }
})

const blackLook = computed<Point>(() => {
  if (passwordVisible.value) return { x: -4, y: -4 }
  if (lookingAtEachOther.value) return { x: 0, y: -4 }
  return { x: blackFace.value.faceX, y: blackFace.value.faceY }
})

const orangeLook = computed<Point>(() => (
  passwordVisible.value
    ? { x: -5, y: -4 }
    : { x: orangeFace.value.faceX, y: orangeFace.value.faceY }
))

const yellowLook = computed<Point>(() => (
  passwordVisible.value
    ? { x: -5, y: -4 }
    : { x: yellowFace.value.faceX, y: yellowFace.value.faceY }
))

const timers = new Set<number>()
let motionQuery: MediaQueryList | undefined
let pointerFrame = 0
let resizeFrame = 0
let peekStartTimer = 0
let peekEndTimer = 0
const pendingPointer: Point = { x: 0, y: 0 }

function later(callback: () => void, delay: number) {
  const id = window.setTimeout(() => {
    timers.delete(id)
    callback()
  }, delay)
  timers.add(id)
  return id
}

function clearTimers() {
  for (const id of timers) window.clearTimeout(id)
  timers.clear()
}

// The peek chain renews itself, so hiding the password must cancel both of its
// own timers. Otherwise every re-reveal leaves one more orphan chain running.
function clearPeekTimers() {
  if (peekStartTimer) window.clearTimeout(peekStartTimer)
  if (peekEndTimer) window.clearTimeout(peekEndTimer)
  timers.delete(peekStartTimer)
  timers.delete(peekEndTimer)
  peekStartTimer = 0
  peekEndTimer = 0
}

function measureCenters() {
  const elements: Array<[CharacterKey, HTMLElement | null]> = [
    ['purple', purpleRef.value],
    ['black', blackRef.value],
    ['orange', orangeRef.value],
    ['yellow', yellowRef.value],
  ]
  for (const [key, element] of elements) {
    if (!element || typeof element.getBoundingClientRect !== 'function') continue
    const rect = element.getBoundingClientRect()
    centers[key] = { x: rect.left + rect.width / 2, y: rect.top + rect.height / 3 }
  }
}

function handlePointerMove(event: PointerEvent) {
  if (event.pointerType === 'touch') return
  pendingPointer.x = event.clientX
  pendingPointer.y = event.clientY
  if (pointerFrame) return
  pointerFrame = window.requestAnimationFrame(() => {
    pointerFrame = 0
    pointer.x = pendingPointer.x
    pointer.y = pendingPointer.y
  })
}

function handleResize() {
  if (resizeFrame) return
  resizeFrame = window.requestAnimationFrame(() => {
    resizeFrame = 0
    measureCenters()
  })
}

function scheduleBlink(key: CharacterKey) {
  if (reducedMotion.value) return
  later(() => {
    blinking[key] = true
    later(() => {
      blinking[key] = false
      scheduleBlink(key)
    }, BLINK_HOLD)
  }, Math.random() * 4000 + 3000)
}

function schedulePeek() {
  if (reducedMotion.value || !passwordVisible.value) return
  // Only ever one chain: a start (or running end) timer means one is active.
  if (peekStartTimer || peekEndTimer) return
  peekStartTimer = later(() => {
    peekStartTimer = 0
    if (reducedMotion.value || !passwordVisible.value) return
    purplePeeking.value = true
    peekEndTimer = later(() => {
      peekEndTimer = 0
      purplePeeking.value = false
      schedulePeek()
    }, 800)
  }, Math.random() * 3000 + 2000)
}

function startAnimations() {
  if (reducedMotion.value) return
  window.addEventListener('pointermove', handlePointerMove, { passive: true })
  window.addEventListener('resize', handleResize, { passive: true })
  for (const key of CHARACTER_KEYS) scheduleBlink(key)
  if (passwordVisible.value) schedulePeek()
  if (!hasEntered.value) {
    later(() => {
      hasEntered.value = true
      measureCenters()
    }, ENTRANCE_DELAY)
  }
}

function stopAnimations() {
  window.removeEventListener('pointermove', handlePointerMove)
  window.removeEventListener('resize', handleResize)
  if (pointerFrame) {
    window.cancelAnimationFrame(pointerFrame)
    pointerFrame = 0
  }
  if (resizeFrame) {
    window.cancelAnimationFrame(resizeFrame)
    resizeFrame = 0
  }
  clearPeekTimers()
  clearTimers()
  for (const key of CHARACTER_KEYS) blinking[key] = false
  purplePeeking.value = false
}

function applyMotionPreference() {
  reducedMotion.value = motionQuery?.matches ?? false
  stopAnimations()
  if (reducedMotion.value) {
    // Static characters: no pointer tracking, no blinking, no timers.
    hasEntered.value = true
    lookingAtEachOther.value = false
    measureCenters()
    return
  }
  startAnimations()
}

watch(() => props.isTyping, (typing) => {
  if (!typing || reducedMotion.value) {
    lookingAtEachOther.value = false
    return
  }
  lookingAtEachOther.value = true
  later(() => {
    lookingAtEachOther.value = false
  }, 800)
})

watch(passwordVisible, (visible) => {
  clearPeekTimers()
  purplePeeking.value = false
  if (visible) schedulePeek()
})

onMounted(() => {
  // Start with the gaze centred so the characters look straight ahead until the
  // pointer actually moves, instead of staring at the viewport origin.
  pointer.x = window.innerWidth / 2
  pointer.y = window.innerHeight / 2
  pendingPointer.x = pointer.x
  pendingPointer.y = pointer.y
  if (typeof window.matchMedia === 'function') {
    motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)')
    if (typeof motionQuery.addEventListener === 'function') {
      motionQuery.addEventListener('change', applyMotionPreference)
    } else {
      motionQuery.addListener(applyMotionPreference)
    }
  }
  measureCenters()
  reducedMotion.value = motionQuery?.matches ?? false
  if (reducedMotion.value) {
    hasEntered.value = true
  } else {
    startAnimations()
  }
})

onBeforeUnmount(() => {
  stopAnimations()
  if (motionQuery) {
    if (typeof motionQuery.removeEventListener === 'function') {
      motionQuery.removeEventListener('change', applyMotionPreference)
    } else {
      motionQuery.removeListener(applyMotionPreference)
    }
    motionQuery = undefined
  }
})
</script>

<template>
  <div class="auth-characters-stage" aria-hidden="true">
    <div
      ref="purpleRef"
      class="auth-character auth-character--purple"
      :class="{ 'is-entered': hasEntered }"
      :style="purpleBodyStyle"
    >
      <div class="auth-character-eyes auth-character-eyes--purple" :style="purpleEyesStyle">
        <EyeBall
          :size="18"
          :pupil-size="7"
          :max-distance="5"
          eye-color="white"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.purple"
          :look-x="purpleLook.x"
          :look-y="purpleLook.y"
        />
        <EyeBall
          :size="18"
          :pupil-size="7"
          :max-distance="5"
          eye-color="white"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.purple"
          :look-x="purpleLook.x"
          :look-y="purpleLook.y"
        />
      </div>
      <div
        class="auth-character-mouth auth-character-mouth--purple"
        :class="mouthModifier ? `auth-character-mouth--${mouthModifier}` : undefined"
        :style="purpleMouthStyle"
      />
    </div>

    <div
      ref="blackRef"
      class="auth-character auth-character--black"
      :class="{ 'is-entered': hasEntered }"
      :style="blackBodyStyle"
    >
      <div class="auth-character-eyes auth-character-eyes--black" :style="blackEyesStyle">
        <EyeBall
          :size="16"
          :pupil-size="6"
          :max-distance="4"
          eye-color="white"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.black"
          :is-sad="loginFailed"
          :sad-rotate="-20"
          :look-x="blackLook.x"
          :look-y="blackLook.y"
        />
        <EyeBall
          :size="16"
          :pupil-size="6"
          :max-distance="4"
          eye-color="white"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.black"
          :is-sad="loginFailed"
          :sad-rotate="20"
          :look-x="blackLook.x"
          :look-y="blackLook.y"
        />
      </div>
    </div>

    <div
      ref="orangeRef"
      class="auth-character auth-character--orange"
      :class="{ 'is-entered': hasEntered }"
      :style="orangeBodyStyle"
    >
      <div class="auth-character-eyes auth-character-eyes--orange" :style="orangeEyesStyle">
        <Pupil
          :size="12"
          :max-distance="5"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.orange"
          :look-x="orangeLook.x"
          :look-y="orangeLook.y"
        />
        <Pupil
          :size="12"
          :max-distance="5"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.orange"
          :look-x="orangeLook.x"
          :look-y="orangeLook.y"
        />
      </div>
      <div
        class="auth-character-mouth auth-character-mouth--orange"
        :class="mouthModifier ? `auth-character-mouth--${mouthModifier}` : undefined"
        :style="orangeMouthStyle"
      />
    </div>

    <div
      ref="yellowRef"
      class="auth-character auth-character--yellow"
      :class="{ 'is-entered': hasEntered }"
      :style="yellowBodyStyle"
    >
      <div class="auth-character-eyes auth-character-eyes--yellow" :style="yellowEyesStyle">
        <Pupil
          :size="12"
          :max-distance="5"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.yellow"
          :look-x="yellowLook.x"
          :look-y="yellowLook.y"
        />
        <Pupil
          :size="12"
          :max-distance="5"
          pupil-color="#2d2d2d"
          :is-blinking="blinking.yellow"
          :look-x="yellowLook.x"
          :look-y="yellowLook.y"
        />
      </div>
      <div class="auth-character-yellow-mouth" :style="yellowMouthStyle">
        <svg width="80" height="20" viewBox="0 0 80 20" aria-hidden="true">
          <path
            class="auth-character-yellow-mouth-path"
            :d="yellowMouthPath"
            stroke="#2d2d2d"
            stroke-width="3"
            fill="none"
            stroke-linecap="round"
          />
        </svg>
      </div>
    </div>
  </div>
</template>
