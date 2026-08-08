<script setup lang="ts">
import {
  Activity,
  ChevronDown,
  ChevronRight,
  CircleCheck,
  KeyRound,
  LogOut,
  Menu as MenuIcon,
  RefreshCw,
  UserRound,
  X,
} from '@lucide/vue'
import { nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { RouterView } from 'vue-router'
import ParticleBackdrop from '@/components/ParticleBackdrop.vue'
import ActionDialogHost from '@/components/ActionDialogHost.vue'
import PreferenceControls from '@/components/ui/PreferenceControls.vue'
import { providePanelContext } from '@/composables/panel-context'
import { usePanelController } from '@/composables/usePanelController'

const panel = usePanelController()
providePanelContext(panel)

const {
  mode,
  page,
  darkMode,
  locale,
  mobileNavOpen,
  mobileMenuButton,
  mobileNavCloseButton,
  mobileSidebar,
  saving,
  pageLoading,
  pageReady,
  error,
  notice,
  credentials,
  logoUrl,
  profile,
  appInfo,
  menu,
  updateCheck,
  updateChecking,
  updateCheckError,
  t,
  setLocale,
  closeMobileNav,
  toggleMobileNav,
  trapMobileNavFocus,
  toggleTheme,
  setupAdmin,
  login,
  refreshUpdateCheck,
  setPage,
  retryPage,
  logout,
} = panel

const profileMenu = ref<HTMLElement | null>(null)
const profileMenuTrigger = ref<HTMLButtonElement | null>(null)
const profileMenuOpen = ref(false)

function closeProfileMenu(restoreFocus = false) {
  profileMenuOpen.value = false
  if (restoreFocus) profileMenuTrigger.value?.focus()
}

function toggleProfileMenu() {
  profileMenuOpen.value = !profileMenuOpen.value
}

function profileMenuItems() {
  return Array.from(profileMenu.value?.querySelectorAll<HTMLButtonElement>('.profile-menu-item') ?? [])
}

function focusProfileMenuItem(position: 'first' | 'last') {
  void nextTick(() => {
    const items = profileMenuItems()
    const item = position === 'first' ? items[0] : items.at(-1)
    item?.focus()
  })
}

function handleProfileTriggerKeydown(event: KeyboardEvent) {
  if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return
  event.preventDefault()
  profileMenuOpen.value = true
  focusProfileMenuItem(event.key === 'ArrowDown' ? 'first' : 'last')
}

function handleProfileMenuNavigation(event: KeyboardEvent) {
  if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
  const items = profileMenuItems()
  if (!items.length) return
  event.preventDefault()
  const activeIndex = items.indexOf(document.activeElement as HTMLButtonElement)
  let nextIndex = 0
  if (event.key === 'End') nextIndex = items.length - 1
  else if (event.key === 'ArrowDown') nextIndex = activeIndex < 0 ? 0 : (activeIndex + 1) % items.length
  else if (event.key === 'ArrowUp') nextIndex = activeIndex < 0 ? items.length - 1 : (activeIndex - 1 + items.length) % items.length
  items[nextIndex]?.focus()
}

function handleProfilePointerDown(event: PointerEvent) {
  const target = event.target
  if (!(target instanceof Node) || profileMenu.value?.contains(target)) return
  closeProfileMenu()
}

function handleProfileKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape' && profileMenuOpen.value) {
    event.preventDefault()
    closeProfileMenu(true)
  }
}

watch([mode, page], () => closeProfileMenu())

onMounted(() => {
  document.addEventListener('pointerdown', handleProfilePointerDown)
  document.addEventListener('keydown', handleProfileKeydown)
})

onBeforeUnmount(() => {
  document.removeEventListener('pointerdown', handleProfilePointerDown)
  document.removeEventListener('keydown', handleProfileKeydown)
})
</script>

<template>
  <main v-if="mode === 'loading'" class="auth-shell" :class="{ dark: darkMode }">
    <ParticleBackdrop variant="auth" />
    <div class="auth-atmosphere" aria-hidden="true" />
    <div class="auth-utility">
      <PreferenceControls
        :locale="locale"
        :dark-mode="darkMode"
        :language-label="t('切换语言')"
        :theme-label="darkMode ? t('切换到浅色模式') : t('切换到暗夜模式')"
        :theme-title="darkMode ? t('浅色模式') : t('暗夜模式')"
        @select-locale="setLocale"
        @toggle-theme="toggleTheme"
      />
    </div>
    <section class="auth-card loading-card">
      <div class="loading-mark"><Activity :size="22" /></div>
      <span>{{ t('加载中') }}</span>
    </section>
  </main>

  <main v-else-if="mode === 'setup' || mode === 'login'" class="auth-shell" :class="{ dark: darkMode }">
    <ParticleBackdrop variant="auth" />
    <div class="auth-atmosphere" aria-hidden="true" />
    <div class="auth-utility">
      <PreferenceControls
        :locale="locale"
        :dark-mode="darkMode"
        :language-label="t('切换语言')"
        :theme-label="darkMode ? t('切换到浅色模式') : t('切换到暗夜模式')"
        :theme-title="darkMode ? t('浅色模式') : t('暗夜模式')"
        @select-locale="setLocale"
        @toggle-theme="toggleTheme"
      />
    </div>
    <form
      class="auth-card"
      @submit.prevent="mode === 'setup' ? setupAdmin() : login()"
    >
      <div class="auth-brand">
        <div class="auth-logo-wrap"><img class="logo-mark" :src="logoUrl" alt="Emby Panel" /></div>
        <div>
          <span class="eyebrow">MEDIA CONTROL / 302</span>
          <h1>Emby Panel</h1>
          <p>{{ mode === 'setup' ? t('首次初始化') : t('管理员登录') }}</p>
        </div>
      </div>
      <div v-if="error" class="notice error" role="alert">{{ error }}</div>
      <label>
        <span>{{ t('用户名') }}</span>
        <input
          v-model="credentials.username"
          name="username"
          autocomplete="username"
          required
        />
      </label>
      <label>
        <span>{{ t('密码') }}</span>
        <input
          v-model="credentials.password"
          name="password"
          type="password"
          :autocomplete="mode === 'setup' ? 'new-password' : 'current-password'"
          required
        />
      </label>
      <button class="primary wide" type="submit" :disabled="saving">
        {{ saving ? t('处理中') : mode === 'setup' ? t('创建并进入') : t('登录') }}
      </button>
      <p class="auth-footnote">EmbyPanel · {{ appInfo.version || '0.2' }} · secure local console</p>
    </form>
  </main>

  <main v-else class="app-shell" :class="{ dark: darkMode, 'mobile-nav-open': mobileNavOpen }" @keydown.esc="closeMobileNav">
    <ParticleBackdrop variant="app" />
    <div v-if="mobileNavOpen" class="nav-backdrop" aria-hidden="true" @click="closeMobileNav" />
    <aside
      id="primary-navigation"
      ref="mobileSidebar"
      class="sidebar"
      :aria-label="t('主导航')"
      @keydown="trapMobileNavFocus"
    >
      <div class="brand-row compact">
        <div class="brand-logo-wrap"><img class="logo-mark" :src="logoUrl" alt="Emby Panel" /></div>
        <button
          class="brand-version"
          :class="{ update: updateCheck?.has_update, error: Boolean(updateCheckError) }"
          :title="
            updateChecking
              ? t('正在检查更新')
              : updateCheck?.has_update
                ? `${t('有新版本')}：${updateCheck.latest_version}`
                : updateCheckError
                  ? `${t('检查失败')}：${updateCheckError}`
                  : t('点击检查更新')
          "
          @click="refreshUpdateCheck(true)"
        >
          <strong>{{ appInfo.name }}</strong>
          <small>
            {{ appInfo.version || t('版本读取中') }}
            <span v-if="updateChecking" class="brand-version-badge">{{ t('检查中') }}</span>
            <span v-else-if="updateCheck?.has_update" class="brand-version-badge update">{{ t('有更新') }}</span>
            <span v-else-if="updateCheck" class="brand-version-badge latest">{{ t('最新') }}</span>
            <span v-else-if="updateCheckError" class="brand-version-badge error">{{ t('失败') }}</span>
          </small>
        </button>
        <button ref="mobileNavCloseButton" class="sidebar-close icon-button" type="button" :aria-label="t('关闭')" @click="closeMobileNav">
          <X :size="18" />
        </button>
      </div>

      <nav>
        <button
          v-for="item in menu"
          :key="item.id"
          class="nav-item"
          :class="{ active: page === item.id }"
          type="button"
          :aria-current="page === item.id ? 'page' : undefined"
          @click="setPage(item.id)"
        >
          <component :is="item.icon" :size="18" :stroke-width="1.8" />
          <span>{{ t(item.label) }}</span>
          <ChevronRight class="nav-chevron" :size="14" />
        </button>
      </nav>

      <div class="sidebar-footer">
        <div class="sidebar-status"><span class="status-orb" /> <span>{{ t('系统在线') }}</span></div>
      </div>
    </aside>

    <section class="workspace">
      <header class="topbar">
        <div class="topbar-leading">
          <button
            ref="mobileMenuButton"
            class="mobile-menu-button icon-button"
            type="button"
            aria-controls="primary-navigation"
            :aria-expanded="mobileNavOpen"
            :aria-label="t('打开导航')"
            @click="toggleMobileNav"
          >
            <MenuIcon v-if="!mobileNavOpen" :size="19" />
            <X v-else :size="19" />
          </button>
          <div class="breadcrumb">
            <span class="breadcrumb-root">EmbyPanel</span>
            <ChevronRight :size="14" />
            <strong>{{ t(menu.find((item) => item.id === page)?.label || '首页') }}</strong>
          </div>
        </div>
        <div class="top-actions">
          <PreferenceControls
            :locale="locale"
            :dark-mode="darkMode"
            :language-label="t('切换语言')"
            :theme-label="darkMode ? t('切换到浅色模式') : t('切换到暗夜模式')"
            :theme-title="darkMode ? t('浅色模式') : t('暗夜模式')"
            @select-locale="setLocale"
            @toggle-theme="toggleTheme"
          />
          <div ref="profileMenu" class="profile-menu">
            <button
              id="profile-menu-trigger"
              ref="profileMenuTrigger"
              class="profile-trigger"
              :class="{ 'is-open': profileMenuOpen }"
              type="button"
              :aria-label="`${t('账户')}：${profile.username || 'admin'}`"
              :aria-expanded="profileMenuOpen"
              aria-controls="profile-menu-popover"
              aria-haspopup="menu"
              @click="toggleProfileMenu"
              @keydown="handleProfileTriggerKeydown"
            >
              <UserRound :size="17" />
              <span class="profile-name">{{ profile.username || 'admin' }}</span>
              <ChevronDown :size="14" />
            </button>
            <Transition name="profile-menu">
              <div
                v-if="profileMenuOpen"
                id="profile-menu-popover"
                class="profile-menu-popover"
                role="menu"
                aria-labelledby="profile-menu-trigger"
                @keydown="handleProfileMenuNavigation"
              >
                <button class="profile-menu-item" type="button" role="menuitem" @click="setPage('account'); closeProfileMenu()">
                  <KeyRound :size="16" />
                  <span>{{ t('修改密码') }}</span>
                </button>
                <button class="profile-menu-item danger" type="button" role="menuitem" @click="logout(); closeProfileMenu()">
                  <LogOut :size="16" />
                  <span>{{ t('登出') }}</span>
                </button>
              </div>
            </Transition>
          </div>
        </div>
      </header>

      <transition name="toast-fade">
        <div v-if="notice" class="toast-notice" role="status" aria-live="polite">
          <span class="toast-icon"><CircleCheck :size="14" /></span>
          <span>{{ notice }}</span>
        </div>
      </transition>

      <div class="content">
        <div v-if="error" class="notice error" role="alert">{{ error }}</div>

        <RouterView v-if="pageReady" v-slot="{ Component, route: currentRoute }">
          <Transition name="page-fade" mode="out-in">
            <component :is="Component" :key="currentRoute.name" />
          </Transition>
        </RouterView>
        <section v-else class="panel page-load-state" :aria-busy="pageLoading">
          <template v-if="pageLoading">
            <div class="loading-mark"><Activity :size="22" /></div>
            <span>{{ t('加载中') }}</span>
          </template>
          <template v-else>
            <p class="muted">{{ t('页面加载失败，请刷新后重试') }}</p>
            <button class="secondary" type="button" @click="retryPage">
              <RefreshCw :size="15" />{{ t('重试') }}
            </button>
          </template>
        </section>
      </div>

      <ActionDialogHost />
    </section>
  </main>
</template>
