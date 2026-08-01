<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

const props = withDefaults(defineProps<{
  variant?: 'auth' | 'app'
}>(), {
  variant: 'app',
})

type Particle = {
  x: number
  y: number
  vx: number
  vy: number
  radius: number
  phase: number
}

type Star = {
  x: number
  y: number
  radius: number
  alpha: number
  phase: number
}

const canvas = ref<HTMLCanvasElement | null>(null)

let animationFrame = 0
let resizeObserver: ResizeObserver | undefined
let themeObserver: MutationObserver | undefined
let motionQuery: MediaQueryList | undefined
let particles: Particle[] = []
let stars: Star[] = []
let width = 0
let height = 0
let pixelRatio = 0
let lastFrame = 0
let primaryColor = '#759a25'
let secondaryColor = '#2f9d7e'
let neutralColor = '#8c978f'
let darkTheme = false
const pointer = { x: 0, y: 0, active: false }

function addMotionPreferenceListener(query: MediaQueryList) {
  if (typeof query.addEventListener === 'function') {
    query.addEventListener('change', handleMotionPreference)
  } else {
    query.addListener(handleMotionPreference)
  }
}

function removeMotionPreferenceListener(query: MediaQueryList) {
  if (typeof query.removeEventListener === 'function') {
    query.removeEventListener('change', handleMotionPreference)
  } else {
    query.removeListener(handleMotionPreference)
  }
}

const clusterAnchors = [
  { x: 0.1, y: 0.2 },
  { x: 0.28, y: 0.72 },
  { x: 0.46, y: 0.34 },
  { x: 0.64, y: 0.78 },
  { x: 0.79, y: 0.24 },
  { x: 0.92, y: 0.58 },
]

function randomParticle(): Particle {
  const speed = 0.028 + Math.random() * 0.044
  const angle = Math.random() * Math.PI * 2
  const sizeRoll = Math.random()
  const radius = sizeRoll < 0.08
    ? 2.05 + Math.random() * 0.7
    : sizeRoll < 0.38
      ? 1.35 + Math.random() * 0.9
      : 0.75 + Math.random() * 0.7
  let x = Math.random() * width
  let y = Math.random() * height
  const cluster = clusterAnchors[Math.floor(Math.random() * clusterAnchors.length)]
  if (cluster && Math.random() < 0.64) {
    const compactness = 1 - Math.min(1, Math.max(0, (width - 390) / 570))
    const horizontalSpread = 0.16 + compactness * 0.08
    const verticalSpread = 0.2 - compactness * 0.04
    x = (cluster.x + (Math.random() + Math.random() - 1) * horizontalSpread) * width
    y = (cluster.y + (Math.random() + Math.random() - 1) * verticalSpread) * height
    x = (x + width) % width
    y = (y + height) % height
  }
  return {
    x,
    y,
    vx: Math.cos(angle) * speed,
    vy: Math.sin(angle) * speed,
    radius,
    phase: Math.random() * Math.PI * 2,
  }
}

function randomStar(): Star {
  const sizeRoll = Math.random()
  return {
    x: Math.random() * width,
    y: Math.random() * height,
    radius: sizeRoll < 0.08 ? 1 + Math.random() * 0.55 : 0.3 + Math.random() * 0.65,
    alpha: 0.14 + Math.random() * 0.32,
    phase: Math.random() * Math.PI * 2,
  }
}

function syncParticleCounts() {
  const auth = props.variant === 'auth'
  const area = width * height
  const target = auth
    ? Math.min(152, Math.max(56, Math.round(area / 8500)))
    : Math.min(128, Math.max(48, Math.round(area / 10000)))
  const starTarget = Math.min(340, Math.max(90, Math.round(area / 4000)))
  while (particles.length < target) particles.push(randomParticle())
  if (particles.length > target) particles.length = target
  while (stars.length < starTarget) stars.push(randomStar())
  if (stars.length > starTarget) stars.length = starTarget
}

function readPalette() {
  const styles = getComputedStyle(document.documentElement)
  const appBackdrop = props.variant === 'app'
  primaryColor = styles.getPropertyValue(appBackdrop ? '--particle-primary' : '--brand').trim() || '#759a25'
  secondaryColor = styles.getPropertyValue(appBackdrop ? '--particle-secondary' : '--accent-green').trim() || '#2f9d7e'
  neutralColor = styles.getPropertyValue('--text-soft').trim() || '#8c978f'
  darkTheme = document.documentElement.classList.contains('dark')
}

function resizeCanvas() {
  const element = canvas.value
  if (!element) return

  const nextWidth = window.innerWidth
  const nextHeight = window.innerHeight
  const pixelBudgetRatio = Math.sqrt(6_000_000 / (nextWidth * nextHeight))
  const nextPixelRatio = Math.min(window.devicePixelRatio || 1, 1.5, pixelBudgetRatio)
  if (nextWidth === width && nextHeight === height && nextPixelRatio === pixelRatio) return

  const previousWidth = width
  const previousHeight = height
  width = nextWidth
  height = nextHeight
  pixelRatio = nextPixelRatio
  if (previousWidth > 0 && previousHeight > 0) {
    particles.forEach((particle) => {
      particle.x = (particle.x / previousWidth) * width
      particle.y = (particle.y / previousHeight) * height
    })
    stars.forEach((star) => {
      star.x = (star.x / previousWidth) * width
      star.y = (star.y / previousHeight) * height
    })
  }

  element.width = Math.round(width * pixelRatio)
  element.height = Math.round(height * pixelRatio)
  element.style.width = `${width}px`
  element.style.height = `${height}px`

  const context = element.getContext('2d')
  context?.setTransform(pixelRatio, 0, 0, pixelRatio, 0, 0)
  syncParticleCounts()
  drawFrame(0)
}

function updateParticle(particle: Particle, elapsed: number) {
  const scale = Math.min(elapsed, 48)
  particle.phase += scale * 0.0005
  particle.vx += Math.cos(particle.phase) * 0.00024 * (scale / 16)
  particle.vy += Math.sin(particle.phase) * 0.00024 * (scale / 16)

  if (pointer.active) {
    const dx = particle.x - pointer.x
    const dy = particle.y - pointer.y
    const distanceSquared = dx * dx + dy * dy
    if (distanceSquared > 1 && distanceSquared < 22500) {
      const force = (1 - distanceSquared / 22500) * 0.004 * (scale / 16)
      const distance = Math.sqrt(distanceSquared)
      particle.vx += (dx / distance) * force
      particle.vy += (dy / distance) * force
    }
  }

  const speed = Math.hypot(particle.vx, particle.vy)
  if (speed > 0.15) {
    particle.vx = (particle.vx / speed) * 0.15
    particle.vy = (particle.vy / speed) * 0.15
  }

  particle.x += particle.vx * scale
  particle.y += particle.vy * scale
  if (particle.x < -10) particle.x = width + 10
  if (particle.x > width + 10) particle.x = -10
  if (particle.y < -10) particle.y = height + 10
  if (particle.y > height + 10) particle.y = -10
}

function drawFrame(elapsed: number) {
  const element = canvas.value
  const context = element?.getContext('2d')
  if (!context || !element) return

  context.clearRect(0, 0, width, height)
  const reducedMotion = motionQuery?.matches === true
  const auth = props.variant === 'auth'
  stars.forEach((star, index) => {
    if (!reducedMotion && elapsed > 0) star.phase += elapsed * 0.0005
    const twinkle = 0.72 + Math.sin(star.phase) * 0.28
    const starStrength = auth
      ? darkTheme ? 0.74 : 0.64
      : darkTheme ? 0.58 : 0.48
    context.globalAlpha = star.alpha * twinkle * starStrength
    context.fillStyle = index % 7 === 0 ? secondaryColor : neutralColor
    context.beginPath()
    context.arc(star.x, star.y, star.radius, 0, Math.PI * 2)
    context.fill()
  })

  if (!reducedMotion && elapsed > 0) particles.forEach((particle) => updateParticle(particle, elapsed))

  const viewportProgress = Math.min(1, Math.max(0, (width - 390) / 1050))
  const connectionScale = 0.72 + viewportProgress * 0.28
  const connectionDistance = Math.round((auth ? 176 : 160) * connectionScale)
  const connectionDistanceSquared = connectionDistance * connectionDistance
  const maximumConnections = width <= 700
    ? auth ? 5 : 4
    : auth ? 6 : 5

  for (let leftIndex = 0; leftIndex < particles.length; leftIndex += 1) {
    const left = particles[leftIndex]
    if (!left) continue
    let connections = 0

    for (let rightIndex = leftIndex + 1; rightIndex < particles.length; rightIndex += 1) {
      const right = particles[rightIndex]
      if (!right) continue
      const dx = left.x - right.x
      const dy = left.y - right.y
      const distanceSquared = dx * dx + dy * dy
      if (distanceSquared >= connectionDistanceSquared) continue

      const lineStrength = auth
        ? darkTheme ? 0.5 : 0.54
        : darkTheme ? 0.4 : 0.44
      context.globalAlpha = (1 - distanceSquared / connectionDistanceSquared) * lineStrength
      context.strokeStyle = leftIndex % 5 === 0 ? secondaryColor : primaryColor
      context.lineWidth = left.radius > 2 ? 1.05 : 0.78
      context.beginPath()
      context.moveTo(left.x, left.y)
      context.lineTo(right.x, right.y)
      context.stroke()
      connections += 1
      if (connections >= maximumConnections) break
    }
  }

  particles.forEach((particle, index) => {
    const pulse = 0.9 + Math.sin(particle.phase) * 0.1
    const nodeAlpha = auth
      ? darkTheme
        ? particle.radius > 2 ? 0.94 : 0.82
        : particle.radius > 2 ? 0.94 : 0.88
      : darkTheme
        ? particle.radius > 2 ? 0.9 : 0.76
        : particle.radius > 2 ? 0.9 : 0.8
    context.globalAlpha = pulse * nodeAlpha
    context.fillStyle = index % 5 === 0 ? secondaryColor : primaryColor
    context.beginPath()
    context.arc(particle.x, particle.y, particle.radius, 0, Math.PI * 2)
    context.fill()
    if (particle.radius > 2) {
      context.globalAlpha = 0.2
      context.lineWidth = 0.6
      context.strokeStyle = secondaryColor
      context.beginPath()
      context.arc(particle.x, particle.y, particle.radius + 1.8, 0, Math.PI * 2)
      context.stroke()
    }
  })
  context.globalAlpha = 1
}

function animate(timestamp: number) {
  if (motionQuery?.matches) {
    animationFrame = 0
    return
  }
  animationFrame = window.requestAnimationFrame(animate)
  if (document.hidden || timestamp - lastFrame < 32) return
  const elapsed = lastFrame ? timestamp - lastFrame : 16
  lastFrame = timestamp
  drawFrame(elapsed)
}

function handlePointerMove(event: PointerEvent) {
  pointer.x = event.clientX
  pointer.y = event.clientY
  pointer.active = event.pointerType !== 'touch'
}

function handlePointerLeave() {
  pointer.active = false
}

function handleMotionPreference() {
  window.cancelAnimationFrame(animationFrame)
  animationFrame = 0
  lastFrame = 0
  drawFrame(0)
  if (!motionQuery?.matches) animationFrame = window.requestAnimationFrame(animate)
}

function handleVisibilityChange() {
  window.cancelAnimationFrame(animationFrame)
  animationFrame = 0
  lastFrame = 0
  if (!document.hidden && !motionQuery?.matches) animationFrame = window.requestAnimationFrame(animate)
}

onMounted(() => {
  readPalette()
  motionQuery = window.matchMedia('(prefers-reduced-motion: reduce)')
  addMotionPreferenceListener(motionQuery)
  if (typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(resizeCanvas)
    resizeObserver.observe(document.documentElement)
  }
  window.addEventListener('resize', resizeCanvas)
  themeObserver = new MutationObserver(() => {
    readPalette()
    drawFrame(0)
  })
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class'] })
  window.addEventListener('pointermove', handlePointerMove, { passive: true })
  document.addEventListener('visibilitychange', handleVisibilityChange)
  document.documentElement.addEventListener('pointerleave', handlePointerLeave)
  resizeCanvas()
  animationFrame = window.requestAnimationFrame(animate)
})

onBeforeUnmount(() => {
  window.cancelAnimationFrame(animationFrame)
  resizeObserver?.disconnect()
  themeObserver?.disconnect()
  if (motionQuery) removeMotionPreferenceListener(motionQuery)
  window.removeEventListener('pointermove', handlePointerMove)
  window.removeEventListener('resize', resizeCanvas)
  document.removeEventListener('visibilitychange', handleVisibilityChange)
  document.documentElement.removeEventListener('pointerleave', handlePointerLeave)
})
</script>

<template>
  <canvas ref="canvas" class="particle-backdrop" aria-hidden="true" />
</template>
