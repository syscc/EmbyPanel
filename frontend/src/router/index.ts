import {
  createRouter,
  createWebHashHistory,
  type RouteRecordRaw,
} from 'vue-router'

export const pageRoutes: RouteRecordRaw[] = [
  {
    path: '/',
    name: 'home',
    component: () => import('@/pages/OverviewPage.vue'),
  },
  {
    path: '/servers',
    name: 'server',
    component: () => import('@/pages/ServersPage.vue'),
  },
  {
    path: '/clients',
    name: 'clients',
    component: () => import('@/pages/ClientsPage.vue'),
  },
  {
    path: '/notifications',
    name: 'notifications',
    component: () => import('@/pages/NotificationsPage.vue'),
  },
  {
    path: '/backups',
    name: 'backup',
    component: () => import('@/pages/BackupPage.vue'),
  },
  {
    path: '/logs',
    name: 'logs',
    component: () => import('@/pages/LogsPage.vue'),
  },
  {
    path: '/account',
    name: 'account',
    component: () => import('@/pages/AccountPage.vue'),
  },
]

const router = createRouter({
  history: createWebHashHistory(import.meta.env.BASE_URL),
  routes: [...pageRoutes, { path: '/:pathMatch(.*)*', redirect: '/' }],
  scrollBehavior: () => ({ top: 0 }),
})

const chunkRecoveryKey = 'embypanel_chunk_recovery'
const chunkRecoveryQuery = 'ui-reload'

function isChunkLoadError(error: unknown) {
  const message = error instanceof Error ? error.message : String(error)
  return /Failed to fetch dynamically imported module|Importing a module script failed|error loading dynamically imported module|Load failed/i.test(message)
}

function readRecoveryPath() {
  try {
    return sessionStorage.getItem(chunkRecoveryKey) || ''
  } catch {
    return ''
  }
}

function writeRecoveryPath(path: string) {
  try {
    if (path) sessionStorage.setItem(chunkRecoveryKey, path)
    else sessionStorage.removeItem(chunkRecoveryKey)
  } catch {
    // A normal navigation error remains visible when storage is unavailable.
  }
}

router.onError((error, to) => {
  if (!isChunkLoadError(error)) return

  if (readRecoveryPath() === to.fullPath) {
    writeRecoveryPath('')
    console.error('Unable to recover the page chunk after reloading.', error)
    return
  }

  writeRecoveryPath(to.fullPath)
  const reloadUrl = new URL(window.location.href)
  reloadUrl.hash = `#${to.fullPath}`
  reloadUrl.searchParams.set(chunkRecoveryQuery, Date.now().toString(36))
  window.location.replace(reloadUrl.toString())
})

router.afterEach((to, _from, failure) => {
  if (failure) return
  if (readRecoveryPath() === to.fullPath) writeRecoveryPath('')

  const currentUrl = new URL(window.location.href)
  if (!currentUrl.searchParams.has(chunkRecoveryQuery)) return
  currentUrl.searchParams.delete(chunkRecoveryQuery)
  window.history.replaceState(window.history.state, '', currentUrl)
})

export default router
