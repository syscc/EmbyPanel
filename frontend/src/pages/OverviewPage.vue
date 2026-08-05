<script setup lang="ts">
import {
  Activity,
  Database,
  Gauge,
  MapPin,
  Monitor,
  PlayCircle,
  RefreshCw,
  Server,
  ShieldCheck,
  UserRound,
  Waypoints,
} from '@lucide/vue'
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { usePanelContext } from '@/composables/panel-context'
import type { PlaybackSession } from '@/types/panel'

const {
  mediaOverviews,
  serverHealth,
  detailedHealth,
  healthError,
  overviewError,
  rateLimitWindows,
  playbackSessions,
  playbackLoading,
  playbackError,
  mediaOverviewTotals,
  requestStatsTotals,
  operationalServerRows,
  rateLimitOverview,
  activePlayCount,
  t,
  formatUptime,
  formatBytes,
  refreshOperationalData,
  formatTimestampMs,
  proxyStatusClass,
  proxyStatusLabel,
  connectivityStatusLabel,
  healthPartLabel,
  failedDuration,
  refreshRateLimitStatus,
  playbackRateActionLabel,
  formatServerName,
  formatIpLocation,
  unblockRateLimitWindow,
  refreshPlaybackSessions,
  fetchPlaybackArtwork,
  ipWithLocation,
  formatTicks,
} = usePanelContext()

const serverList = ref<HTMLElement | null>(null)
const artworkUrls = ref<Record<string, string>>({})
const artworkRequests = new Map<string, AbortController>()
const missingArtworkKeys = new Set<string>()
let serverListObserver: ResizeObserver | undefined

function artworkKey(session: PlaybackSession) {
  return `${session.server_id}:${session.item_id}`
}

function activeArtworkKeys() {
  return new Set(
    playbackSessions.value
      .filter((session) => session.server_id && session.item_id)
      .map(artworkKey),
  )
}

function releaseArtwork(key: string) {
  const url = artworkUrls.value[key]
  if (!url) return
  URL.revokeObjectURL(url)
  delete artworkUrls.value[key]
}

async function loadArtwork(session: PlaybackSession, key: string) {
  const controller = new AbortController()
  artworkRequests.set(key, controller)

  try {
    let blob = await fetchPlaybackArtwork(
      session.server_id,
      session.item_id,
      controller.signal,
    )
    if (
      !blob
      && session.series_id
      && session.series_id !== session.item_id
      && !controller.signal.aborted
    ) {
      blob = await fetchPlaybackArtwork(
        session.server_id,
        session.series_id,
        controller.signal,
      )
    }
    if (controller.signal.aborted || artworkRequests.get(key) !== controller) {
      return
    }
    if (!blob) {
      missingArtworkKeys.add(key)
      return
    }
    artworkUrls.value[key] = URL.createObjectURL(blob)
  } catch {
    // Keep transient failures retryable on the next playback refresh.
  } finally {
    if (artworkRequests.get(key) === controller) {
      artworkRequests.delete(key)
    }
  }
}

function reconcileArtwork() {
  const activeKeys = activeArtworkKeys()

  Object.keys(artworkUrls.value).forEach((key) => {
    if (!activeKeys.has(key)) releaseArtwork(key)
  })
  artworkRequests.forEach((controller, key) => {
    if (!activeKeys.has(key)) {
      controller.abort()
      artworkRequests.delete(key)
    }
  })
  missingArtworkKeys.forEach((key) => {
    if (!activeKeys.has(key)) missingArtworkKeys.delete(key)
  })

  playbackSessions.value.forEach((session) => {
    if (!session.server_id || !session.item_id) return
    const key = artworkKey(session)
    if (
      artworkUrls.value[key]
      || artworkRequests.has(key)
      || missingArtworkKeys.has(key)
    ) return
    void loadArtwork(session, key)
  })
}

function handleArtworkError(session: PlaybackSession) {
  const key = artworkKey(session)
  releaseArtwork(key)
  missingArtworkKeys.add(key)
}

function playbackTitle(session: PlaybackSession) {
  const isEpisode = session.item_type?.toLowerCase() === 'episode'
    || session.season_number != null
    || session.episode_number != null
  if (!isEpisode) return session.item_name
  const episodeNumber = [
    session.season_number != null ? `S${session.season_number}` : '',
    session.episode_number != null ? `E${session.episode_number}` : '',
  ].filter(Boolean).join(':')
  return episodeNumber ? `${episodeNumber} - ${session.item_name}` : session.item_name
}

function playbackPercent(session: PlaybackSession) {
  return Math.min(100, Math.max(0, session.percent ?? 0))
}

function playbackModeLabel(mode: PlaybackSession['playback_mode']) {
  if (mode === 'direct_link') return t('直链播放')
  if (mode === 'server_proxy') return t('服务器代理播放')
  if (mode === 'transcode') return t('服务器转码')
  if (mode === 'emby_direct_play') return t('Emby 直接播放')
  if (mode === 'emby_direct_stream') return t('Emby 直接串流')
  return t('播放路径待确认')
}

function measureServerList() {
  const list = serverList.value
  if (!list || mediaOverviews.value.length <= 2) {
    list?.style.removeProperty('--overview-server-list-height')
    return
  }

  const rows = Array.from(list.children).slice(0, 2) as HTMLElement[]
  if (rows.length < 2) return
  const gap = Number.parseFloat(getComputedStyle(list).rowGap) || 0
  const visibleHeight = rows.reduce((height, row) => height + row.getBoundingClientRect().height, gap)
  const nextHeight = `${Math.ceil(visibleHeight)}px`
  if (list.style.getPropertyValue('--overview-server-list-height') !== nextHeight) {
    list.style.setProperty('--overview-server-list-height', nextHeight)
  }
}

async function observeServerList() {
  await nextTick()
  serverListObserver?.disconnect()
  if (!serverList.value) return
  if (typeof ResizeObserver === 'undefined') {
    measureServerList()
    return
  }
  serverListObserver = new ResizeObserver(measureServerList)
  serverListObserver.observe(serverList.value)
  Array.from(serverList.value.children).forEach((row) => serverListObserver?.observe(row))
  measureServerList()
}

watch(mediaOverviews, observeServerList, { flush: 'post' })
watch(playbackSessions, reconcileArtwork, { immediate: true })
onMounted(observeServerList)
onBeforeUnmount(() => {
  serverListObserver?.disconnect()
  artworkRequests.forEach((controller) => controller.abort())
  artworkRequests.clear()
  Object.keys(artworkUrls.value).forEach(releaseArtwork)
  missingArtworkKeys.clear()
})
</script>

<template>
  <section class="dashboard">
    <section class="panel media-overview">
      <div class="panel-head">
        <div class="panel-title-line">
          <Database :size="18" />
          <h2>{{ t('媒体库总览') }}</h2>
        </div>
        <span class="status-dot"
          >{{ t('在线') }} {{ mediaOverviews.length }}</span
        >
      </div>
      <div v-if="mediaOverviews.length" class="stat-grid four">
        <div class="stat-card">
          <span>{{ t('电影') }}</span>
          <strong>{{
            mediaOverviewTotals.movie_count.toLocaleString()
          }}</strong>
        </div>
        <div class="stat-card">
          <span>{{ t('剧集') }}</span>
          <strong>{{
            mediaOverviewTotals.series_count.toLocaleString()
          }}</strong>
        </div>
        <div class="stat-card">
          <span>{{ t('总集数') }}</span>
          <strong>{{
            mediaOverviewTotals.episode_count.toLocaleString()
          }}</strong>
        </div>
        <div class="stat-card">
          <span>{{ t('用户') }}</span>
          <strong>{{ mediaOverviewTotals.user_count.toLocaleString() }}</strong>
        </div>
      </div>
      <div v-else class="empty-state">
        {{ overviewError || t('正在读取媒体库总览') }}
      </div>
      <div v-if="mediaOverviews.length" class="overview-server-frame">
        <div
          ref="serverList"
          class="overview-server-list"
          :class="{
            'has-multiple-servers': mediaOverviews.length > 1,
            'is-scrollable': mediaOverviews.length > 2,
          }"
          role="list"
        >
          <div
            v-for="overview in mediaOverviews"
            :key="overview.server_id"
            class="overview-server-row"
            role="listitem"
          >
            <strong>{{ overview.server_name }}</strong>
            <span
              >{{ t('电影') }} {{ overview.movie_count.toLocaleString() }}</span
            >
            <span
              >{{ t('剧集') }} {{ overview.series_count.toLocaleString() }}</span
            >
            <span
              >{{ t('集数') }} {{ overview.episode_count.toLocaleString() }}</span
            >
            <span
              >{{ t('用户') }} {{ overview.user_count.toLocaleString() }}</span
            >
            <small
              >Emby {{ overview.version }} · {{ overview.operating_system }} ·
              {{ overview.library_count }} {{ t('个媒体库') }}</small
            >
          </div>
        </div>
      </div>
    </section>

    <section class="panel health-panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Gauge :size="18" />
            <h2>{{ t('服务器状态') }}</h2>
          </div>
          <small class="health-subtitle"
            >{{ t('运行') }}
            {{ formatUptime(serverHealth?.uptime_seconds) }}</small
          >
        </div>
      </div>
      <div v-if="serverHealth" class="health-lines">
        <div class="health-line">
          <div>
            <strong>CPU</strong>
            <span
              >{{ serverHealth.cpu_name }} · {{ serverHealth.cpu_cores }}
              {{ t('核') }}</span
            >
          </div>
          <b>{{ serverHealth.cpu_percent }}%</b>
          <div class="health-track">
            <div
              class="health-bar cpu"
              :style="{ width: `${serverHealth.cpu_percent}%` }"
            />
          </div>
        </div>
        <div class="health-line">
          <div>
            <strong>{{ t('内存') }}</strong>
            <span
              >{{ formatBytes(serverHealth.memory_used_bytes) }} /
              {{ formatBytes(serverHealth.memory_total_bytes) }}</span
            >
          </div>
          <b>{{ serverHealth.memory_percent }}%</b>
          <div class="health-track">
            <div
              class="health-bar memory"
              :style="{ width: `${serverHealth.memory_percent}%` }"
            />
          </div>
        </div>
        <div class="health-line">
          <div>
            <strong>{{ t('磁盘') }}</strong>
            <span
              >{{ formatBytes(serverHealth.disk_used_bytes) }} /
              {{ formatBytes(serverHealth.disk_total_bytes) }}</span
            >
          </div>
          <b>{{ serverHealth.disk_percent }}%</b>
          <div class="health-track">
            <div
              class="health-bar disk"
              :style="{ width: `${serverHealth.disk_percent}%` }"
            />
          </div>
        </div>
      </div>
      <div v-else class="empty-state">
        {{ healthError || t('正在读取服务器状态') }}
      </div>
    </section>

    <section class="panel operations-panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Activity :size="18" />
            <h2>{{ t('运维概览') }}</h2>
          </div>
          <p class="muted">{{ t('健康检查、反代监听和今日请求统计。') }}</p>
        </div>
        <button class="secondary" @click="refreshOperationalData">
          <RefreshCw :size="15" />{{ t('刷新') }}
        </button>
      </div>
      <div class="stat-grid four">
        <div class="stat-card">
          <span>{{ t('今日请求') }}</span>
          <strong>{{ requestStatsTotals.requests.toLocaleString() }}</strong>
          <small>{{
            detailedHealth?.status === 'ok' ? t('健康') : t('待检查')
          }}</small>
        </div>
        <div class="stat-card">
          <span>{{ t('重定向') }}</span>
          <strong>{{ requestStatsTotals.redirects.toLocaleString() }}</strong>
          <small>{{ t('STRM 直链') }}</small>
        </div>
        <div class="stat-card">
          <span>{{ t('缓存命中') }}</span>
          <strong>{{ requestStatsTotals.cache_hits.toLocaleString() }}</strong>
          <small>{{ t('内存直链缓存') }}</small>
        </div>
        <div class="stat-card">
          <span>{{ t('拦截 / 错误') }}</span>
          <strong
            >{{ requestStatsTotals.blocks.toLocaleString() }} /
            {{ requestStatsTotals.errors.toLocaleString() }}</strong
          >
          <small>{{ t('今日累计') }}</small>
        </div>
      </div>
      <div class="connectivity-list">
        <div
          v-for="row in operationalServerRows"
          :key="`operation-${row.server.id}`"
          class="connectivity-row"
        >
          <div>
            <strong>{{ row.server.name }}</strong>
            <small>
              {{ t('端口') }} :{{ row.proxy?.port || row.server.port }} ·
              {{ t('最近请求') }}
              {{ formatTimestampMs(row.proxy?.last_request_ms) }} ·
              {{ t('最近巡检') }}
              {{ formatTimestampMs(row.connectivity?.checked_at_ms || 0) }}
              <template v-if="row.connectivity">
                · {{ t('耗时') }} {{ row.connectivity.duration_ms }}ms</template
              >
            </small>
          </div>
          <span :class="['client-badge', proxyStatusClass(row.proxy)]">
            {{ proxyStatusLabel(row.proxy) }}
          </span>
          <span
            :class="[
              'client-badge',
              row.connectivity?.ok ? 'allowed' : 'blocked',
            ]"
          >
            {{
              row.connectivity
                ? connectivityStatusLabel(row.connectivity)
                : t('未巡检')
            }}
          </span>
          <span
            >Emby {{ healthPartLabel(row.connectivity?.emby_ok ?? null) }}</span
          >
          <span
            >OpenList
            {{ healthPartLabel(row.connectivity?.openlist_ok ?? null) }}</span
          >
          <span
            >{{ t('反代') }}
            {{ healthPartLabel(row.connectivity?.proxy_ok ?? null) }}</span
          >
          <span>{{
            row.connectivity ? failedDuration(row.connectivity) : t('无失败')
          }}</span>
          <small v-if="row.connectivity?.auto_restarted_at_ms">
            {{ t('最近自动重启') }}
            {{ formatTimestampMs(row.connectivity.auto_restarted_at_ms) }}
          </small>
          <small
            v-if="row.connectivity?.last_error"
            class="server-status-error"
            >{{ row.connectivity.last_error }}</small
          >
        </div>
      </div>
    </section>

    <section class="panel rate-limit-overview">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <ShieldCheck :size="18" />
            <h2>{{ t('播放频率限制') }}</h2>
          </div>
          <p class="muted">{{ t('首页直接查看当前窗口命中和封禁情况。') }}</p>
        </div>
        <button class="secondary" @click="refreshRateLimitStatus">
          <RefreshCw :size="15" />{{ t('刷新') }}
        </button>
      </div>
      <div class="stat-grid three">
        <div class="stat-card">
          <span>{{ t('活跃窗口') }}</span>
          <strong>{{
            rateLimitOverview.active_windows.toLocaleString()
          }}</strong>
          <small>{{ t('当前监控中的 IP') }}</small>
        </div>
        <div class="stat-card">
          <span>{{ t('已封禁') }}</span>
          <strong>{{
            rateLimitOverview.blocked_windows.toLocaleString()
          }}</strong>
          <small>{{ t('屏蔽 IP / 禁用用户') }}</small>
        </div>
        <div class="stat-card">
          <span>{{ t('最高命中') }}</span>
          <strong>{{
            rateLimitOverview.highest_count.toLocaleString()
          }}</strong>
          <small>{{ t('当前窗口最大次数') }}</small>
        </div>
      </div>
      <div
        v-if="rateLimitWindows.length"
        class="rate-block-table-wrap home-rate-table"
      >
        <table class="rate-block-table">
          <thead>
            <tr>
              <th>{{ t('封禁方式') }}</th>
              <th>{{ t('服务器') }}</th>
              <th>IP</th>
              <th>{{ t('用户') }}</th>
              <th>{{ t('命中') }}</th>
              <th>{{ t('窗口') }}</th>
              <th>{{ t('状态') }}</th>
              <th>{{ rateLimitWindows.length }} {{ t('条') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr
              v-for="row in rateLimitWindows.slice(0, 5)"
              :key="`home-${row.server_id}-${row.ip}`"
            >
              <td>{{ playbackRateActionLabel(row.block_action) }}</td>
              <td>{{ formatServerName(row.server_id) }}</td>
              <td>
                <strong>{{ row.ip }}</strong>
                <small
                  v-if="formatIpLocation(row.ip_location)"
                  class="ip-location"
                  >{{ formatIpLocation(row.ip_location) }}</small
                >
              </td>
              <td>{{ row.user_name || '--' }}</td>
              <td>{{ row.current_count }}/{{ row.threshold }}</td>
              <td>{{ row.window_seconds }}s</td>
              <td>
                <span
                  :class="['client-badge', row.blocked ? 'blocked' : 'allowed']"
                >
                  {{ row.blocked ? t('已封禁') : t('观察中') }}
                </span>
              </td>
              <td>
                <button
                  v-if="row.blocked && row.block_id"
                  type="button"
                  class="secondary"
                  @click="unblockRateLimitWindow(row)"
                >
                  {{ t('解除封禁') }}
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-else class="empty-state compact">
        {{ t('当前没有播放频率窗口数据。') }}
      </div>
    </section>

    <section class="panel playing-panel">
      <div class="panel-head">
        <div class="panel-title-line">
          <PlayCircle :size="18" />
          <h2>{{ t('实时播放') }}</h2>
        </div>
        <div class="panel-actions">
          <span class="status-dot">{{ t('在线') }} {{ activePlayCount }}</span>
          <button
            class="secondary"
            :disabled="playbackLoading"
            @click="refreshPlaybackSessions"
          >
            <RefreshCw :size="15" />{{
              playbackLoading ? t('刷新中') : t('刷新')
            }}
          </button>
        </div>
      </div>
      <div v-if="playbackSessions.length" class="playback-strip">
        <article
          v-for="session in playbackSessions"
          :key="`${session.server_id}:${session.id}:${session.item_id}`"
          class="play-card"
        >
          <div class="play-artwork">
            <img
              v-if="artworkUrls[artworkKey(session)]"
              :src="artworkUrls[artworkKey(session)]"
              :alt="session.item_name"
              @error="handleArtworkError(session)"
            />
            <div v-else class="poster-fallback" aria-hidden="true">
              {{ session.item_name.trim().slice(0, 1) || 'M' }}
            </div>
          </div>
          <div class="play-info">
            <div class="play-heading">
              <div class="play-heading-copy">
                <div v-if="session.series_name" class="play-series">
                  {{ session.series_name }}
                </div>
                <div class="play-title" :title="playbackTitle(session)">
                  {{ playbackTitle(session) }}
                </div>
              </div>
              <span
                class="play-mode-badge"
                :class="`is-${session.playback_mode}`"
              >
                <Waypoints :size="13" aria-hidden="true" />
                {{ playbackModeLabel(session.playback_mode) }}
              </span>
            </div>
            <div class="play-session-meta">
              <span class="play-detail" :title="session.server_name">
                <Server :size="13" aria-hidden="true" />
                <span>{{ session.server_name || session.server_id }}</span>
              </span>
              <span class="play-detail" :title="session.user_name">
                <UserRound :size="13" aria-hidden="true" />
                <span>{{ session.user_name || '--' }}</span>
              </span>
              <span
                class="play-detail play-detail-wide"
                :title="`${session.client} · ${session.device_name}`"
              >
                <Monitor :size="13" aria-hidden="true" />
                <span>
                  {{ session.client || '--' }} ·
                  {{ session.device_name || '--' }}
                </span>
              </span>
              <span
                class="play-detail play-detail-wide play-ip"
                :title="
                  session.playback_ip
                    ? ipWithLocation(session.playback_ip, session.ip_location)
                    : 'IP --'
                "
              >
                <MapPin :size="13" aria-hidden="true" />
                <span>
                  IP
                  {{
                    session.playback_ip
                      ? ipWithLocation(
                          session.playback_ip,
                          session.ip_location,
                        )
                      : '--'
                  }}
                </span>
              </span>
            </div>
            <div class="play-progress-meta">
              <span>
                {{ formatTicks(session.position_ticks) }} /
                {{ formatTicks(session.runtime_ticks) }}
              </span>
              <strong>{{ Math.round(playbackPercent(session)) }}%</strong>
            </div>
            <div
              class="progress-track"
              role="progressbar"
              aria-valuemin="0"
              aria-valuemax="100"
              :aria-valuenow="Math.round(playbackPercent(session))"
            >
              <div
                class="progress-bar"
                :style="{ width: `${playbackPercent(session)}%` }"
              />
            </div>
          </div>
        </article>
      </div>
      <div v-else class="empty-state">
        {{
          playbackLoading
            ? t('正在读取 Emby 播放会话')
            : playbackError || t('当前没有正在播放的媒体')
        }}
      </div>
    </section>
  </section>
</template>
