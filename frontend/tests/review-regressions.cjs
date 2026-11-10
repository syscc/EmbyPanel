// Execute actual TypeScript controllers in memory; all HTTP/encryption/timers are mocked.
// This is not browser or live Emby evidence.
const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const path = require('node:path')
const { readFileSync } = require('node:fs')
const ts = require('typescript')

function load(relative, overrides = {}) {
  const file = path.resolve(__dirname, '../src', relative)
  const source = ts.transpileModule(fs.readFileSync(file, 'utf8').replaceAll('import.meta.env.BASE_URL', JSON.stringify('/ui/')), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const module = { exports: {} }
  new Function('require', 'module', 'exports', source)((id) => {
    if (id in overrides) return overrides[id]
    if (id.startsWith('@/')) return load(id.slice(2) + '.ts', overrides)
    return require(id)
  }, module, module.exports)
  return module.exports
}
function deferred() {
  let resolve, reject
  const promise = new Promise((yes, no) => { resolve = yes; reject = no })
  return { promise, resolve, reject }
}
const tick = () => new Promise((resolve) => setImmediate(resolve))
const t = (value) => value

test('API preserves auth on 403, expires on 401, and exposes response headers without changing JSON', async (context) => {
  const { createApiClient } = load('lib/api-client.ts')
  let expired = 0
  const api = createApiClient({ getToken: () => 'mock-token', onUnauthorized: () => expired++ })
  context.mock.method(globalThis, 'fetch', async () => new Response('forbidden', { status: 403 }))
  await assert.rejects(api('/api/history'), /forbidden/)
  assert.equal(expired, 0)
  globalThis.fetch = async () => new Response('expired', { status: 401 })
  await assert.rejects(api('/api/history'), /expired/)
  assert.equal(expired, 1)
  let metadata
  globalThis.fetch = async (_, init) => {
    assert.equal(init.headers.get('Authorization'), 'Bearer mock-token')
    assert.equal('onResponseHeaders' in init, false)
    return new Response('[]', { headers: { 'x-embypanel-server-errors': '{"failed_count":1,"server_ids":["safe-id"]}' } })
  }
  assert.deepEqual(await api('/api/monitoring/plays', {
    onResponseHeaders: (headers) => { metadata = JSON.parse(headers.get('x-embypanel-server-errors')) },
  }), [])
  assert.deepEqual(metadata.server_ids, ['safe-id'])
})

test('delete requires the exact supplied name and encrypts that input', async () => {
  const { useUsersController } = load('composables/useUsersController.ts')
  const payloads = [], requests = []
  const controller = useUsersController({
    api: async (...args) => { requests.push(args); return { success: true } },
    encryptPayload: async (name, value) => { payloads.push({ name, value }); return value },
  })
  const user = { server_id: 's', user_id: 'u', name: 'Alice完整' }
  for (const wrong of [null, '', 'Alice', 'alice完整', 'Alice完整 ']) await controller.deleteUser(user, wrong)
  assert.equal(requests.length, 0)
  assert.equal(payloads.length, 0)
  await controller.deleteUser(user, user.name)
  assert.deepEqual(payloads, [{ name: 'user_delete', value: { confirm_name: user.name } }])
  assert.equal(requests[0][1].method, 'DELETE')
})

test('Users page cancel does not delete and confirmation deletes the selected user', async () => {
  const source = fs.readFileSync(path.resolve(__dirname, '../src/pages/UsersPage.vue'), 'utf8')
  const body = source.slice(source.indexOf('async function askDelete('), source.indexOf('async function askDeleteTemplate('))
  const compiled = ts.transpileModule(body + '\nexports.askDelete = askDelete', {
    compilerOptions: { module: ts.ModuleKind.CommonJS },
  }).outputText
  let answer = false, count = 0, dialogOptions
  const exports = {}
  new Function('exports', 'saving', 'error', 'clearNotice', 'confirmAction', 't', 'deleteUser', 'dialogOpen', 'selectedUser', 'showNotice', compiled)(
    exports, { value: false }, { value: '' }, () => {},
    async (options) => { dialogOptions = options; return answer }, t,
    async (selected, confirmName) => { assert.equal(confirmName, selected.name); count++ }, { value: true }, { value: {} }, () => {},
  )
  const user = { name: 'Alice' }
  await exports.askDelete(user)
  assert.equal(count, 0)
  assert.equal(dialogOptions.description, 'Alice：删除操作不可撤销')
  assert.equal(dialogOptions.tone, 'danger')
  answer = true
  await exports.askDelete(user)
  assert.equal(count, 1)
})

test('user delete from the policy dialog uses a globally higher action-dialog layer', () => {
  const css = readFileSync(path.resolve(__dirname, '../src/style.css'), 'utf8')
  const overlay = css.match(/\.action-dialog-overlay\s*\{[^}]*z-index:\s*(\d+)/)
  const content = css.match(/\.action-dialog-content\s*\{[^}]*z-index:\s*(\d+)/)
  const settings = css.match(/\.settings-dialog-content\s*\{[^}]*z-index:\s*(\d+)/)
  assert.ok(overlay && content && settings)
  assert.ok(Number(overlay[1]) > Number(settings[1]))
  assert.ok(Number(content[1]) > Number(overlay[1]))
  const usersPage = readFileSync(path.resolve(__dirname, '../src/pages/UsersPage.vue'), 'utf8')
  const editorDelete = usersPage.slice(usersPage.indexOf('content-class="users-dialog-content"'), usersPage.indexOf('    </SettingsDialogShell>', usersPage.indexOf('content-class="users-dialog-content"')))
  assert.match(editorDelete, /@click="askDelete\(selectedUser\)"/)
})

test('saving user policy closes the dialog only after success and keeps errors open', async () => {
  const source = fs.readFileSync(path.resolve(__dirname, '../src/pages/UsersPage.vue'), 'utf8')
  const start = source.indexOf('async function savePolicy()')
  const end = source.indexOf('\nfunction openCreate()', start)
  const body = source.slice(start, end)
  const compiled = ts.transpileModule(body + '\nexports.savePolicy = savePolicy', {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  async function run(updatePolicy) {
    const dialogOpen = { value: true }
    const selectedUser = { value: { server_id: 'synthetic', user_id: 'u1', name: 'fixture-user' } }
    const notices = [], errors = { value: '' }, clears = []
    const exports = {}
    new Function('exports', 'selectedUser', 'clearNotice', 'updatePolicy', 'policyPayload', 'draft', 'showNotice', 't', 'dialogOpen', 'error', compiled)(
      exports, selectedUser, () => clears.push(true), updatePolicy, () => ({ policy: true }), {},
      (notice) => notices.push(notice), t, dialogOpen, errors,
    )
    await exports.savePolicy()
    return { dialogOpen, selectedUser, notices, errors, clears }
  }

  const succeeded = await run(async (user) => ({ ...user, policy: { changed: true } }))
  assert.equal(succeeded.dialogOpen.value, false)
  assert.equal(succeeded.selectedUser.value.policy.changed, true)
  assert.deepEqual(succeeded.notices, ['用户策略已保存'])
  assert.equal(succeeded.clears.length, 1)

  const failed = await run(async () => { throw new Error('mock policy save failed') })
  assert.equal(failed.dialogOpen.value, true)
  assert.deepEqual(failed.notices, [])
  assert.equal(failed.errors.value, '') // The shared controller owns the visible error.
})

function history(points) {
  const { useHistoryController } = load('composables/useHistoryController.ts')
  return useHistoryController({ api: async () => points, translate: t })
}
const point = (date, value, extra = {}) => ({ date, value, dimension_key: 'a', ...extra })
test('count summary sums dimensions by date before daily average and peak', async () => {
  const c = history([point('2026-11-01', 100), point('2026-11-01', 100, { dimension_key: 'b' }), point('2026-11-02', 10)])
  await c.loadHistory()
  assert.deepEqual(c.metricItems.value.map((item) => item.value), ['210', '105', '200', '2'])
  assert.match(c.metricItems.value[1].detail, /未采样日期不计入/)
})
test('ratios and bandwidth use denominators; missing weights and P95 cannot masquerade as precise global values', async () => {
  const c = history([point('2026-11-01', 100, { numerator: 1, denominator: 1 }), point('2026-11-01', 0, { numerator: 0, denominator: 9 })])
  c.setMetric('redirect_success_rate')
  await c.loadHistory()
  assert.equal(c.metricItems.value[0].value, '10.0%')
  assert.equal(c.metricItems.value[1].value, '10.0%')
  c.setMetric('bandwidth')
  assert.equal(c.metricItems.value[0].value, '0 bps')
  c.points.value = [point('2026-11-01', 100, { numerator: 100, denominator: 1 }), point('2026-11-01', 10, { numerator: 90, denominator: 9 })]
  assert.equal(c.metricItems.value[0].value, '19 bps')
  c.setMetric('latency_p95')
  assert.equal(c.metricItems.value[0].value, '—')
  assert.equal(c.metricItems.value[1].value, '100 ms')
  c.setMetric('transcode_rate')
  c.points.value = [point('2026-11-01', 50)]
  assert.equal(c.metricItems.value[0].value, '—')
})

test('visible session polling is single-flight, pauses while hidden, cancels on dispose and reports partial failure', async (context) => {
  const { useSessionsController } = load('composables/useSessionsController.ts')
  const doc = new EventTarget()
  doc.hidden = false
  const oldDocument = globalThis.document
  globalThis.document = doc
  context.after(() => { globalThis.document = oldDocument })
  let intervalCallback, cleared = false
  context.mock.method(globalThis, 'setInterval', (fn, milliseconds) => { assert.equal(milliseconds, 10000); intervalCallback = fn; return 123 })
  context.mock.method(globalThis, 'clearInterval', (id) => { assert.equal(id, 123); cleared = true })
  const requests = []
  const c = useSessionsController({
    servers: () => [{ id: 'server', name: 'Fixture' }], canControlSessions: () => true,
    api: (url, init) => { const pending = deferred(); requests.push({ init, ...pending }); return pending.promise },
    encryptPayload: async () => ({}), t, confirmAction: async () => true, promptAction: async () => null, showNotice: () => {},
  })
  c.startAutoRefresh()
  intervalCallback()
  assert.equal(requests.length, 1)
  requests[0].init.onResponseHeaders(new Headers({ 'x-embypanel-server-errors': '{"failed_count":1,"server_ids":["safe-id"]}' }))
  requests[0].resolve([])
  await tick()
  assert.match(c.partialError.value, /safe-id/)
  intervalCallback()
  assert.equal(requests.length, 2)
  doc.hidden = true
  doc.dispatchEvent(new Event('visibilitychange'))
  assert.equal(requests[1].init.signal.aborted, true)
  intervalCallback()
  assert.equal(requests.length, 2)
  doc.hidden = false
  doc.dispatchEvent(new Event('visibilitychange'))
  assert.equal(requests.length, 3)
  c.dispose()
  assert.equal(requests[2].init.signal.aborted, true)
  assert.equal(cleared, true)
  requests[1].resolve([{ id: 'late' }])
  requests[2].resolve([{ id: 'late' }])
  await tick()
  assert.deepEqual(c.sessions.value, [])
  intervalCallback()
  assert.equal(requests.length, 3)
})

test('session action success notice survives its refresh clear and is suppressed after dispose', async () => {
  const { useSessionsController } = load('composables/useSessionsController.ts')
  const previousDocument = globalThis.document
  globalThis.document = new EventTarget()
  for (const action of ['stopPlayback', 'sendMessage']) {
    const notices = [], refresh = deferred()
    let refreshStarted = false
    const c = useSessionsController({
    servers: () => [{ id: 'server', name: 'Fixture' }], canControlSessions: () => true,
      api: async (url) => {
        if (url === '/api/monitoring/plays') {
          refreshStarted = true
          return refresh.promise
        }
        return {}
      },
      encryptPayload: async () => ({}), t, confirmAction: async () => true,
      promptAction: async () => 'hello', showNotice: (message) => notices.push(message),
      clearNotice: () => notices.push('cleared'),
    })
    const pending = c[action]('session', 'server')
    await tick()
    assert.equal(refreshStarted, true)
    assert.deepEqual(notices, ['cleared'])
    refresh.resolve([])
    await pending
    assert.equal(notices.at(-1), action === 'stopPlayback' ? '播放会话已停止' : '消息已发送')

    const lateRefresh = deferred()
    const lateNotices = []
    const disposed = useSessionsController({
      servers: () => [{ id: 'server', name: 'Fixture' }], canControlSessions: () => true,
      api: async (url) => url === '/api/monitoring/plays' ? lateRefresh.promise : {},
      encryptPayload: async () => ({}), t, confirmAction: async () => true,
      promptAction: async () => 'hello', showNotice: (message) => lateNotices.push(message),
      clearNotice: () => {},
    })
    const late = disposed[action]('session', 'server')
    await tick()
    disposed.dispose()
    lateRefresh.resolve([])
    await late
    assert.deepEqual(lateNotices, [])
  }
  globalThis.document = previousDocument
})

function alerts(encrypt, api, additions = {}) {
  const { useAlertsController } = load('composables/useAlertsController.ts', {
    '@/composables/usePayloadEncryption': { usePayloadEncryption: () => ({ encryptPayload: encrypt }) },
  })
  return useAlertsController({
    api, translate: t, showNotice: () => { throw new Error('unexpected late notice') },
    confirmAction: async () => true, promptAction: async () => '1', ...additions,
  })
}
test('create and update abort while encryption is pending: no HTTP, notice or post-dispose state mutation', async () => {
  for (const id of ['', 'existing']) {
    const encryption = deferred(), requests = []
    const c = alerts(() => encryption.promise, async (...args) => { requests.push(args); return [] })
    c.draft.value.id = id
    const saving = c.saveDraft()
    c.dispose()
    const snapshot = JSON.stringify([c.actionError.value, c.saving.value, c.dialogOpen.value])
    encryption.resolve({ encrypted: true })
    await saving
    assert.equal(requests.length, 0)
    assert.equal(JSON.stringify([c.actionError.value, c.saving.value, c.dialogOpen.value]), snapshot)
  }
})
test('all alert management requests carry abort signals and ignore late success/failure after dispose', async () => {
  const rule = { id: 'rule' }
  for (const action of ['toggleRule', 'deleteRule', 'testRule', 'promptSilence', 'clearSilence']) {
    for (const failure of [false, true]) {
      const response = deferred(), requests = []
      const c = alerts(async () => ({}), (url, init) => { requests.push({ url, init }); return response.promise })
      const pending = c[action](rule, true)
      await tick()
      assert.equal(requests.length, 1, action)
      assert.ok(requests[0].init.signal, action)
      c.dispose()
      assert.equal(requests[0].init.signal.aborted, true)
      const snapshot = JSON.stringify([c.actionError.value, c.togglingIds.value, c.deletingIds.value, c.testingIds.value, c.silencingIds.value])
      if (failure) response.reject(new Error('late failure'))
      else response.resolve({})
      await pending
      assert.equal(requests.length, 1, action + ' must not reload')
      assert.equal(JSON.stringify([c.actionError.value, c.togglingIds.value, c.deletingIds.value, c.testingIds.value, c.silencingIds.value]), snapshot)
    }
  }
})
test('alert prompts resolving after dispose never send writes', async () => {
  for (const [action, result, name] of [['deleteRule', true, 'confirmAction'], ['promptSilence', '1', 'promptAction']]) {
    const prompt = deferred(), requests = []
    const c = alerts(async () => ({}), async (...args) => { requests.push(args) }, { [name]: () => prompt.promise })
    const pending = c[action]({ id: 'r' })
    c.dispose()
    prompt.resolve(result)
    await pending
    assert.equal(requests.length, 0)
  }
})
test('alert reads ignore late responses and abort all requests on dispose', async () => {
  const requests = []
  const c = alerts(async () => ({}), (url, init) => { const pending = deferred(); requests.push({ init, ...pending }); return pending.promise })
  const reads = c.refreshAll()
  c.openRuleHistory('r')
  assert.equal(requests.length, 3)
  c.dispose()
  for (const request of requests) { assert.equal(request.init.signal.aborted, true); request.resolve([{ id: 'late' }]) }
  await reads
  await tick()
  assert.deepEqual(c.rules.value, [])
  assert.deepEqual(c.deliveries.value, [])
  assert.deepEqual(c.historyByRule.value, {})
})

// Compile the actual SFC and render it in memory; child dialogs and HTTP are mocked.
async function adminSecurityPage(admin, dialogs = {}) {
  const vue = require('vue')
  const { parse, compileScript, compileTemplate } = require('@vue/compiler-sfc')
  const filename = path.resolve(__dirname, '../src/pages/AdminSecurityPage.vue')
  const { descriptor } = parse(readFileSync(filename, 'utf8'), { filename })
  const script = compileScript(descriptor, { id: 'admin-security-test' })
  const template = compileTemplate({
    source: descriptor.template.content, filename, id: 'admin-security-test',
    compilerOptions: { bindingMetadata: script.bindings },
  })
  assert.deepEqual(template.errors, [])
  const compiled = ts.transpileModule(script.content + '\n' + template.code, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const calls = [], prompts = [], reads = []
  const { useAdminSecurityController } = load('composables/useAdminSecurityController.ts')
  let controller, state
  const responses = {
    '/api/admins': { current_user_id: 7, admins: admin ? [{
      id: 7, username: 'fixture-admin', enabled: true, server_grants: [], ...admin,
    }] : [], roles: [] },
    '/api/admin/tokens': { tokens: [{ id: 1, name: 'fixture-token', token_prefix: 'prefix', permissions: [] }] },
    '/api/admin/sessions': { sessions: [{ id: 2, device_label: 'fixture-device', auth_method: 'password' }] },
    '/api/admin/2fa/status': { enabled: true, pending: true, available: true, login_challenge_available: true, recovery_codes_remaining: 5 },
  }
  const mocks = {
    '@/composables/panel-context': { usePanelContext: () => ({
      api: async (url, init) => {
        if (init?.method) { calls.push(url); return {} }
        reads.push(url)
        return responses[url]
      },
      encryptPayload: async () => ({}), t, showNotice: () => {},
      serverOptions: vue.ref([{ id: 'authorized', name: 'Authorized server' }]), settings: { servers: [] }, locale: vue.ref('zh'),
    }) },
    '@/composables/useAdminSecurityController': { useAdminSecurityController: (options) => {
      controller = useAdminSecurityController(options)
      return controller
    } },
    '@/composables/useActionDialog': { useActionDialog: () => ({
      confirmAction: async () => { prompts.push('confirm'); return dialogs.confirm ? dialogs.confirm() : true },
      promptAction: async () => { prompts.push('prompt'); return '123456' },
    }) },
    '@/lib/panel-formatters': { formatTimestamp: () => 'fixture-time' },
    '@/components/ui/SettingsDialogShell.vue': { props: ['open'], setup: (props, { slots }) =>
      () => props.open ? vue.h('dialog', [...(slots.default?.() ?? []), ...(slots.footer?.() ?? [])]) : null },
    '@/components/ui/UiSwitch.vue': { props: ['label'], setup: (props) => () => vue.h('button', props.label) },
    '@/components/ui/CheckboxField.vue': { render: () => null },
    '@/components/ui/RefreshButton.vue': loadPageComponent('../components/ui/RefreshButton.vue', {}),
  }
  const module = { exports: {} }
  new Function('require', 'module', 'exports', compiled)((id) => mocks[id] ?? require(id), module, module.exports)
  const component = module.exports.default
  const setup = component.setup
  component.render = module.exports.render
  const { renderToString } = require('@vue/server-renderer')
  // SSR skips onMounted, so explicitly load the same controller reads first.
  component.setup = (props, context) => {
    const result = setup(props, context)
    state = result
    vue.onServerPrefetch(() => controller.load())
    return result
  }
  const html = await renderToString(vue.createSSRApp(component))
  return { html, state, controller, calls, prompts, reads }
}

test('admin security page hides read-only writes, shows writer/super-admin controls and retains reads', async () => {
  for (const [admin, writable, superAdmin] of [
    [{ role_keys: ['reader'], permission_keys: ['admin.security.read'] }, false, false],
    [{ role_keys: ['writer'], permission_keys: ['admin.security.read', 'admin.security.write'] }, true, false],
    [{ role_keys: ['super_admin'], permission_keys: [] }, true, true],
    [null, false, false],
  ]) {
    const page = await adminSecurityPage(admin)
    assert.equal(page.state.canWriteSecurity.value, writable)
    assert.equal(page.state.canManageAdmins.value, superAdmin)
    for (const label of ['创建 Token', '撤销', '启用 2FA', '禁用 2FA', '重新生成恢复码', '回滚']) {
      assert.equal(page.html.includes(`>${label}</button>`), writable, label)
    }
    assert.equal(page.html.includes('>创建管理员</button>'), superAdmin)
    const refreshButton = page.html.match(/<button\b[^>]*class="secondary refresh-button"[^>]*>/)?.[0]
    assert.ok(refreshButton, 'real RefreshButton renders in every role state')
    assert.match(refreshButton, /aria-label="刷新"/)
    assert.match(refreshButton, /aria-busy="false"/)
    assert.doesNotMatch(refreshButton, /\sdisabled(?:[\s=>])/)
    for (const label of ['fixture-token', 'fixture-device', '登录会话', 'API Token', '剩余恢复码']) {
      assert.ok(page.html.includes(label), label)
    }
    assert.deepEqual(page.reads, ['/api/admins', '/api/admin/tokens', '/api/admin/sessions', '/api/admin/2fa/status'])
    assert.deepEqual(page.calls, [])
  }
})

test('admin security page rejects direct read-only handlers and rechecks permission after confirmation', async () => {
  const page = await adminSecurityPage({ role_keys: ['reader'], permission_keys: ['admin.security.read'] })
  for (const action of [
    'openTokenCreateDialog', 'createToken', 'revokeToken', 'revokeSession',
    'startTwoFactor', 'confirmTwoFactor', 'disableTwoFactor', 'regenerateRecoveryCodes', 'rollbackTwoFactor',
    'openAdminCreateDialog', 'createAdmin', 'openRoleEditor', 'saveRoles', 'openGrantEditor',
    'saveServerGrants', 'setAdminEnabled', 'resetAdminPassword', 'deleteAdmin',
  ]) await page.state[action]({ id: 8 }, false)
  assert.deepEqual(page.calls, [])
  assert.deepEqual(page.prompts, [])
  for (const dialog of ['showTokenCreateDialog', 'showTwoFactorDialog', 'showAdminCreateDialog', 'showRoleDialog', 'showGrantDialog']) {
    assert.equal(page.state[dialog].value, false, dialog)
  }

  for (const action of ['revokeToken', 'revokeSession', 'rollbackTwoFactor', 'disableTwoFactor']) {
    const confirmation = deferred()
    const writer = await adminSecurityPage({ role_keys: ['writer'], permission_keys: ['admin.security.read', 'admin.security.write'] }, {
      confirm: () => confirmation.promise,
    })
    const pending = writer.state[action](1)
    writer.controller.admins.value[0].permission_keys = ['admin.security.read']
    confirmation.resolve(true)
    await pending
    assert.deepEqual(writer.calls, [], action)
  }
})

test('rules refresh after toggle wins over an older query even when cancellation is ignored', async () => {
  const reads = []
  const c = alerts(async () => ({}), async (_, init) => {
    if (init?.method === 'PATCH') return {}
    const pending = deferred()
    reads.push({ ...pending, signal: init.signal })
    return pending.promise
  }, { showNotice: () => {} })
  const rule = { id: 'r', name: 'Fixture', enabled: true }
  const stale = c.loadRules()
  const toggling = c.toggleRule(rule, false)
  await tick()
  assert.equal(reads.length, 2)
  assert.equal(reads[0].signal.aborted, true)
  reads[1].resolve([{ ...rule, enabled: false }])
  await toggling
  reads[0].resolve([rule])
  await stale
  assert.equal(c.rules.value[0].enabled, false)
  assert.equal(c.loading.value, false)
  assert.equal(c.error.value, '')
  c.dispose()
})

test('stale rule failure/finally cannot clear current loading or overwrite current error', async () => {
  const reads = []
  const c = alerts(async () => ({}), (_, init) => {
    const pending = deferred(); reads.push({ ...pending, signal: init.signal }); return pending.promise
  })
  const stale = c.loadRules(), latest = c.loadRules()
  reads[0].reject(new Error('stale failure'))
  await stale
  assert.equal(c.loading.value, true)
  assert.equal(c.error.value, '')
  reads[1].reject(new Error('latest failure'))
  await latest
  assert.equal(c.error.value, 'latest failure')
  assert.equal(c.loading.value, false)
  c.dispose()
})

test('diagnostics manual page submit stays on page 3; only filters reset pagination', async () => {
  const paths = []
  const { useDiagnosticsController } = load('composables/useDiagnosticsController.ts')
  const c = useDiagnosticsController({
    api: async (url) => { paths.push(url); return { records: [], can_global_cleanup: false } },
    translate: t, showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  })
  c.query.value.page = 3
  await c.applyFilters()
  assert.equal(c.query.value.page, 3)
  assert.equal(new URL(paths.at(-1), 'http://localhost').searchParams.get('page'), '3')
  for (const [key, value] of [['request_id', 'request'], ['server_id', 'server'], ['media_id', 'media'], ['limit', 20]]) {
    c.query.value.page = 3
    c.query.value[key] = value
    assert.equal(c.query.value.page, 1, key)
    await c.applyFilters()
    assert.equal(new URL(paths.at(-1), 'http://localhost').searchParams.get('page'), '1', key)
  }
  await c.goToPage(3)
  assert.equal(c.query.value.page, 3)
  await c.prevPage()
  assert.equal(c.query.value.page, 2)
  await c.resetFilters()
  assert.equal(c.query.value.page, 1)
  c.dispose()
})

test('viewer session handlers deny writes and recheck permission after dialogs/encryption', async (context) => {
  const oldDocument = globalThis.document
  context.after(() => { globalThis.document = oldDocument })
  globalThis.document = new EventTarget()
  const { useSessionsController } = load('composables/useSessionsController.ts')
  for (const action of ['stopPlayback', 'sendMessage']) {
    for (const revokeAt of ['initial', 'dialog', 'encryption']) {
      let allowed = revokeAt !== 'initial'
      const calls = [], prompts = []
      const dialog = async () => { prompts.push(true); if (revokeAt === 'dialog') allowed = false; return action === 'stopPlayback' ? true : 'hello' }
      const c = useSessionsController({
        servers: () => [{ id: 'empty-server', name: 'Authorized empty server' }], canControlSessions: () => allowed,
        api: async (url) => { calls.push(url); return [] },
        encryptPayload: async () => { if (revokeAt === 'encryption') allowed = false; return {} },
        t, showNotice: () => {}, confirmAction: dialog, promptAction: dialog,
      })
      assert.equal(c.serverOptions.value[1].value, 'empty-server')
      await c[action]('session', 'empty-server')
      assert.deepEqual(calls, [])
      assert.equal(prompts.length, revokeAt === 'initial' ? 0 : 1)
      c.dispose()
    }
  }
})

// Run actual panel lifecycle with delayed profile/options; no live network or storage.
function panelHarness(context, initialPage, api, options = {}) {
  const vue = require('vue')
  const mounted = [], unmounted = [], intervals = new Map()
  let nextInterval = 0
  const route = vue.reactive({ name: initialPage })
  const storage = () => {
    const values = new Map([['embypanel_token', 'fixture-token']])
    return { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) }
  }
  for (const [key, value] of Object.entries({
    localStorage: storage(), sessionStorage: storage(),
    window: {
      location: { hash: `#/${initialPage}` },
      setInterval: (fn, ms) => { const id = ++nextInterval; intervals.set(id, { fn, ms }); return id },
      clearInterval: (id) => intervals.delete(id), setTimeout: () => 1, clearTimeout: () => {},
    },
    document: { documentElement: { classList: { toggle: () => {} } } },
  })) {
    const previous = Object.getOwnPropertyDescriptor(globalThis, key)
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value })
    context.after(() => previous ? Object.defineProperty(globalThis, key, previous) : delete globalThis[key])
  }
  const { usePanelController } = load('composables/usePanelController.ts', {
    vue: { ...vue, onMounted: (fn) => mounted.push(fn), onBeforeUnmount: (fn) => unmounted.push(fn) },
    '@lucide/vue': {},
    'vue-router': { useRoute: () => route, useRouter: () => ({
      replace: async ({ name }) => { route.name = name },
      push: async ({ name }) => { route.name = name },
    }) },
    '@/lib/api-client': options.realApiClient ? load('lib/api-client.ts') : { createApiClient: () => api },
    '@/composables/usePayloadEncryption': { usePayloadEncryption: () => ({ encryptPayload: options.encryptPayload || (async () => ({})), warmPublicKey: async () => {}, randomId: () => 'fixture-id' }) },
    '@/composables/useActionDialog': { useActionDialog: () => ({ confirmAction: async () => true, promptAction: async () => null }) },
  })
  const scope = vue.effectScope()
  const panel = scope.run(() => usePanelController())
  context.after(() => { unmounted.forEach((fn) => fn()); scope.stop() })
  return { panel, route, intervals, start: () => Promise.all(mounted.map((fn) => fn())) }
}

test('direct history/security/sessions/library-tasks/cache-management/logs entry awaits authorized options without settings.read or home', async (context) => {
  for (const page of ['history', 'admin-security', 'sessions', 'library-tasks', 'cache-management', 'logs']) {
    const profile = deferred(), options = deferred(), reads = []
    const harness = panelHarness(context, page, async (url) => {
      reads.push(url)
      if (url === '/api/setup-status') return { initialized: true }
      if (url === '/api/profile') return profile.promise
      if (url === '/api/servers/options') return options.promise
      if (url === '/api/settings') throw new Error('settings.read forbidden')
      return {}
    })
    const boot = harness.start()
    await tick()
    assert.equal(reads.includes('/api/servers/options'), false)
    profile.resolve({ username: 'viewer' })
    await tick()
    assert.equal(harness.panel.pageReady.value, false)
    assert.equal(reads.filter((url) => url === '/api/servers/options').length, 1)
    const concurrent = harness.panel.ensureServerOptions()
    options.resolve({ servers: [{ id: 'authorized', name: 'Authorized server' }], can_control_sessions: false })
    await Promise.all([boot, concurrent])
    assert.equal(harness.panel.pageReady.value, true)
    assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'authorized', name: 'Authorized server' }])
    assert.equal(harness.panel.canControlSessions.value, false)
    await harness.panel.ensureServerOptions()
    assert.equal(reads.filter((url) => url === '/api/servers/options').length, 1)
    assert.equal(reads.includes('/api/settings'), false)
    assert.deepEqual(harness.panel.settings.servers, [])
  }
})

test('server options failure is retryable and a previous login response cannot refill a new identity cache', async (context) => {
  let attempt = 0
  const pending = deferred(), nextIdentity = deferred()
  const harness = panelHarness(context, 'sessions', async (url) => {
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'fixture' }
    if (url === '/api/servers/options') {
      attempt++
      if (attempt === 1) throw new Error('options unavailable')
      if (attempt === 2) return { servers: [{ id: 's', name: 'S' }], can_control_sessions: true }
      return attempt === 3 ? pending.promise : nextIdentity.promise
    }
    if (url === '/api/login') return { token: 'new-fixture-token' }
    return {}
  })
  await harness.start()
  assert.equal(harness.panel.pageReady.value, false)
  assert.equal(harness.panel.error.value, 'options unavailable')
  harness.panel.retryPage()
  await tick()
  assert.equal(harness.panel.pageReady.value, true)
  assert.equal(harness.panel.canControlSessions.value, true)
  const oldLoad = harness.panel.loadServerOptions()
  // Login resets dataSession before its new page activation.
  const login = harness.panel.login()
  await tick()
  assert.deepEqual(harness.panel.serverOptions.value, [])
  assert.equal(harness.panel.canControlSessions.value, false)
  pending.resolve({ servers: [{ id: 'stale', name: 'Stale identity' }], can_control_sessions: true })
  await oldLoad
  assert.deepEqual(harness.panel.serverOptions.value, [])
  assert.equal(harness.panel.canControlSessions.value, false)
  nextIdentity.resolve({ servers: [], can_control_sessions: false })
  await login
  assert.deepEqual(harness.panel.serverOptions.value, [])
})

function loadPageComponent(filename, panel, overrides = {}) {
  const { parse, compileScript, compileTemplate } = require('@vue/compiler-sfc')
  const absolute = path.resolve(__dirname, '../src/pages', filename)
  const { descriptor } = parse(readFileSync(absolute, 'utf8'), { filename: absolute })
  const script = compileScript(descriptor, { id: 'review-page-test' })
  const template = compileTemplate({ source: descriptor.template.content, filename: absolute, id: 'review-page-test', compilerOptions: { bindingMetadata: script.bindings } })
  assert.deepEqual(template.errors, [])
  const compiled = ts.transpileModule(script.content + '\n' + template.code, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText
  const module = { exports: {} }
  new Function('require', 'module', 'exports', compiled)((id) => {
    if (id in overrides) return overrides[id]
    if (id === '@/composables/panel-context') return { usePanelContext: () => panel }
    if (id.endsWith('.vue') || id === '@lucide/vue') return new Proxy({}, { get: () => ({ render: () => null }) })
    if (id.startsWith('@/')) return load(id.slice(2) + '.ts')
    return require(id)
  }, module, module.exports)
  const component = module.exports.default
  component.render = module.exports.render
  return component
}

async function renderPage(filename, panel, onSetup, overrides = {}, props = {}) {
  const vue = require('vue')
  const component = loadPageComponent(filename, panel, overrides), setup = component.setup
  component.setup = (props, context) => {
    const state = setup(props, context)
    onSetup?.(state)
    return state
  }
  return require('@vue/server-renderer').renderToString(vue.createSSRApp(component, props))
}

test('server validation SSR names every check in order while preserving scopes, statuses and localized results', async (context) => {
  const vue = require('vue')
  const { translations } = load('lib/translations.ts')
  // Synthetic display-only fixtures: no validation action, HTTP or persisted settings are used.
  const fixtures = [
    ['本地配置校验通过', '本地配置', 'Local configuration', true, 'Local configuration passed'],
    ['本地配置校验失败', '本地配置', 'Local configuration', false, 'Local configuration failed'],
    ['反代端口重复', '反代端口', 'Proxy port', false, 'Duplicate proxy port'],
    ['反代端口正在由当前服务监听', '反代端口', 'Proxy port', true, 'The current service is listening on this proxy port'],
    ['反代端口可用', '反代端口', 'Proxy port', true, 'Proxy port is available'],
    ['反代端口已被占用', '反代端口', 'Proxy port', false, 'Proxy port is already in use'],
    ['Emby API Key 可用', 'Emby API', 'Emby API', true, 'Emby API key is valid'],
    ['Emby 连接失败', 'Emby API', 'Emby API', false, 'Emby connection failed'],
    ['OpenList 连接可用', 'OpenList 连接', 'OpenList connection', true, 'OpenList connection is available'],
    ['OpenList 连接失败', 'OpenList 连接', 'OpenList connection', false, 'OpenList connection failed'],
    ['未配置，已跳过', 'OpenList 连接', 'OpenList connection', true, 'Not configured; skipped'],
    ['fixture unknown validation message', '配置检查', 'Configuration check', false, 'fixture unknown validation message'],
  ]
  context.mock.method(globalThis, 'fetch', () => assert.fail('Display-only SSR must not make HTTP requests'))
  for (const locale of ['zh-CN', 'en-US']) {
    const translate = (source) => locale === 'en-US' ? translations[source] || source : source
    const localized = []
    const results = fixtures.map(([message, , , ok], index) => ({
      message, ok,
      scope: index === 0 ? '配置' : index === 10 ? 'OpenList' : index === 11 ? '' : '用户',
      detail: index === 0 ? '配置' : index === 10 ? '' : `fixture detail ${index}`,
    }))
    const panel = {
      t: translate,
      settings: {
        servers: [], openlist_addr: null, openlist_token: null,
        cache_domain_filter_mode: 'off', cache_domain_whitelist: '',
        enable_internal_redirect: false, internal_redirect_timeout_seconds: 15,
        strm_url_mappings: '', strm_url_mapping_enabled: true,
        cache_ttl_seconds: 180, cache_max_capacity: 10000, cache_enabled: true,
        connectivity_check_enabled: true, connectivity_check_interval_seconds: 60,
        connectivity_check_timeout_seconds: 5, connectivity_auto_restart_seconds: 180,
      },
      saving: vue.ref(false), error: vue.ref(''), restartingServerId: vue.ref(''),
      validationResults: vue.ref(results), defaultCdnHeaders: [], realIpModeOptions: [],
      proxyStatusById: vue.ref({}), strmMappingPlaceholder: vue.ref('fixture mappings'),
      validateSettings: () => assert.fail('Display-only SSR must not run validateSettings'),
      api: () => assert.fail('Display-only SSR must not fetch settings'),
      validationClass: (result) => result.ok ? 'success' : 'warn',
      localizeValidationText: (source) => { localized.push(source); return translate(source) },
    }
    const html = await renderPage('ServersPage.vue', panel, (state) => {
      state.validationDialogOpen.value = true
    }, {
      '@/components/ui/SettingsDialogShell.vue': {
        props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null,
      },
    })
    const dialog = html.match(/<dialog\b[^>]*>([\s\S]*?)<\/dialog>/)?.[1]
    assert.ok(dialog, `${locale}: validation dialog renders`)
    assert.match(dialog, /class="validation-list" role="list"/)
    const rows = [...dialog.matchAll(/<div class="validation-row ([^"]+)" role="listitem">\s*<div class="validation-check">\s*<strong>([^<]+)<\/strong>([\s\S]*?)<\/div>\s*<span>([^<]*)<\/span>\s*<small>([^<]*)<\/small>\s*<\/div>/g)]
    assert.equal(rows.length, fixtures.length, `${locale}: all eleven messages and the unknown fixture remain visible`)
    assert.deepEqual(rows.map((row) => row[2]), fixtures.map((fixture) => fixture[locale === 'en-US' ? 2 : 1]))
    for (const [index, row] of rows.entries()) {
      const result = results[index]
      assert.equal(row[1], result.ok ? 'success' : 'warn', `${locale}: ${result.message} status`)
      assert.equal(row[3].match(/<small>([^<]*)<\/small>/)?.[1] ?? '', result.scope, `${locale}: scope is not translated`)
      assert.equal(row[4], locale === 'en-US' ? fixtures[index][4] : result.message, `${locale}: original message stays in its row`)
      assert.equal(row[5], index === 0 && locale === 'en-US' ? 'Configuration' : result.detail || '--', `${locale}: detail or empty placeholder stays in its row`)
    }
    assert.equal(rows[10][1], 'success', `${locale}: skipped OpenList remains successful`)
    assert.equal(rows[10][5], '--', `${locale}: skipped OpenList keeps its empty detail placeholder`)
    assert.deepEqual(localized, results.flatMap((result) => result.detail ? [result.message, result.detail] : [result.message]), `${locale}: messages and nonempty details still use localizeValidationText in order`)
    assert.ok(!html.includes('undefined'), `${locale}: synthetic settings provide all rendered defaults`)
  }
})

test('server policy SSR preserves all fifteen field heads, statuses, help and clear controls with scoped CSS', async (context) => {
  const vue = require('vue')
  const { parse } = require('@vue/compiler-sfc')
  const filename = path.resolve(__dirname, '../src/pages/ServersPage.vue')
  const { descriptor, errors } = parse(readFileSync(filename, 'utf8'), { filename })
  assert.deepEqual(errors, [])
  const requiredRules = {
    '.server-policy-grid': { 'align-items': 'start' },
    '.server-policy-field': { display: 'grid', 'align-content': 'start', gap: '7px' },
    '.server-policy-field-head': { display: 'flex', 'flex-wrap': 'wrap' },
    '.server-policy-field-head > span:first-child': { 'font-size': '11px', 'font-weight': '700' },
    '.server-policy-field-head > .muted': { 'font-size': '11px', 'font-weight': '400' },
    '.server-policy-field-head > button': { 'font-size': 'var(--font-size-control)', 'font-weight': '600' },
    '.server-policy-field > .field-help': { 'font-size': '12px', 'line-height': '1.6' },
    '.server-policy-toggle-field > .settings-card-switch': { 'justify-self': 'start' },
  }
  const rules = new Map()
  for (const style of descriptor.styles) {
    for (const [, selectorText, body] of style.content.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
      const selector = selectorText.trim()
      if (!selector.includes('.server-policy-')) continue
      assert.equal(style.scoped, true, `${selector}: policy styles remain scoped`)
      assert.ok(Object.hasOwn(requiredRules, selector), `${selector}: only targeted policy selectors`)
      assert.equal(rules.has(selector), false, `${selector}: no conflicting duplicate rule`)
      assert.doesNotMatch(selector, /\b(?:input|textarea|select)\b/, 'policy CSS does not target input typography')
      const declarations = new Map([...body.matchAll(/([\w-]+)\s*:\s*([^;]+);?/g)].map((match) => [match[1], match[2].trim()]))
      if ('font-size' in requiredRules[selector]) {
        assert.equal(declarations.has('font'), false, `${selector}: no overriding font shorthand`)
      } else {
        assert.equal(declarations.has('font'), false, `${selector}: no inherited input font shorthand`)
        assert.equal(declarations.has('font-size'), false, `${selector}: no inherited input font size`)
      }
      rules.set(selector, declarations)
    }
  }
  for (const [selector, declarations] of Object.entries(requiredRules)) {
    assert.ok(rules.has(selector), `${selector}: policy rule exists`)
    for (const [property, value] of Object.entries(declarations)) {
      assert.equal(rules.get(selector).get(property), value, `${selector}: ${property}`)
    }
  }

  // Synthetic display-only drafts: no API, validation, save or clear action is executed.
  const fields = [
    ['OpenList 地址', 'openlist_addr_override', 'https://openlist.invalid'],
    ['OpenList Token', 'openlist_token_override', null],
    ['STRM URL 映射', 'strm_url_mappings_override', 'fixture mapping'],
    ['启用 STRM URL 映射', 'strm_url_mapping_enabled_override', false],
    ['启用缓存', 'cache_enabled_override', false],
    ['缓存秒数', 'cache_ttl_seconds_override', 0],
    ['缓存最大条数', 'cache_max_capacity_override', 200],
    ['缓存过滤模式', 'cache_domain_filter_mode_override', 'off'],
    ['缓存过滤域名', 'cache_domain_whitelist_override', 'fixture.invalid'],
    ['开启内部重定向 HEAD 解析', 'enable_internal_redirect_override', false],
    ['HEAD 超时秒数', 'internal_redirect_timeout_seconds_override', 10],
    ['启用服务器连通性巡检', 'connectivity_check_enabled_override', false],
    ['巡检间隔秒数', 'connectivity_check_interval_seconds_override', 60],
    ['单项超时秒数', 'connectivity_check_timeout_seconds_override', 5],
    ['反代无响应自动重启秒数', 'connectivity_auto_restart_seconds_override', 0],
  ]
  context.mock.method(globalThis, 'fetch', () => assert.fail('Display-only SSR must not make HTTP requests'))
  for (const overridden of [false, true]) {
    const panel = {
      t,
      settings: {
        servers: [], openlist_addr: null, openlist_token: null,
        cache_domain_filter_mode: 'off', cache_domain_whitelist: '',
        enable_internal_redirect: false, internal_redirect_timeout_seconds: 15,
        strm_url_mappings: '', strm_url_mapping_enabled: true,
        cache_ttl_seconds: 180, cache_max_capacity: 10000, cache_enabled: true,
        connectivity_check_enabled: true, connectivity_check_interval_seconds: 60,
        connectivity_check_timeout_seconds: 5, connectivity_auto_restart_seconds: 180,
      },
      saving: vue.ref(false), error: vue.ref(''), restartingServerId: vue.ref(''),
      validationResults: vue.ref([]), defaultCdnHeaders: [], realIpModeOptions: [],
      proxyStatusById: vue.ref({}), strmMappingPlaceholder: vue.ref('fixture mappings'),
      apiKeyInputValue: () => '', apiKeyPlaceholder: () => 'fixture placeholder',
      isApiKeyVisible: () => false, isApiKeyRevealLoading: () => false,
      needsRealIpHeader: () => false,
      api: () => assert.fail('Display-only SSR must not fetch settings'),
      saveSettings: () => assert.fail('Display-only SSR must not save settings'),
      validateSettings: () => assert.fail('Display-only SSR must not run validation'),
    }
    const html = await renderPage('ServersPage.vue', panel, (state) => {
      state.serverDraft.value = {
        id: 'policy-fixture', name: 'Synthetic policy server', emby_host: 'https://emby.invalid',
        emby_api_key: '', port: 18096, block_web_ui: false, real_ip_mode: 'system', trusted_proxy_cidrs: '',
        ...Object.fromEntries(fields.map(([, key, value]) => [key, overridden ? value : null])),
        openlist_token_override_configured: overridden,
      }
      state.serverEditorOpen.value = true
    }, {
      '@/components/ui/SettingsDialogShell.vue': {
        props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null,
      },
      '@/components/ui/UiSwitch.vue': loadPageComponent('../components/ui/UiSwitch.vue', {}),
      'reka-ui': { DialogClose: { inheritAttrs: false, setup: (_, { slots }) => () => slots.default?.() } },
    })
    const section = html.match(/<section class="server-policy-overrides"[^>]*>([\s\S]*?)<\/section>/)?.[1]
    assert.ok(section, 'open server editor renders the policy section')
    const rows = section.split(/<div class="server-policy-field(?: server-policy-toggle-field)?">/).slice(1)
    assert.equal(rows.length, 15, 'all fifteen policy fields remain visible')
    for (const [index, row] of rows.entries()) {
      const head = row.match(/<div class="server-policy-field-head">([\s\S]*?)<\/div>/)?.[1]
      assert.ok(head, `${fields[index][0]}: field head exists`)
      assert.equal(head.match(/<span>([^<]+)<\/span>/)?.[1], fields[index][0], 'field labels stay ordered')
      const status = head.match(/<span class="muted">([^<]+)<\/span>/)?.[1]
      // The Token field shares the paired OpenList status and restore control in the address head.
      assert.equal(status, index === 1 ? undefined : overridden ? index === 0 ? '地址与 Token 均覆盖' : '已设置覆盖' : '继承全局', `${fields[index][0]}: status`)
      const clearButtons = [...head.matchAll(/<button\b[^>]*type="button"[^>]*>([^<]+)<\/button>/g)]
      assert.deepEqual(clearButtons.map((match) => match[1]), overridden && index !== 1 ? [index === 0 ? '恢复全局' : '清除覆盖'] : [], `${fields[index][0]}: clear control`)
    }
    assert.deepEqual([...section.matchAll(/<small class="field-help">([^<]+)<\/small>/g)].map((match) => match[1]), [
      '地址与 Token 按最终有效配置成对使用；允许只覆盖其中一项并继承另一项。恢复全局会同时清除两项覆盖。',
      'Token 只显示为密码框；留空且不清除时保持已有安全值。',
    ])
    assert.equal([...section.matchAll(/class="settings-card-switch[^\"]*"[^>]*role="switch"/g)].length, 4, 'four policy switches retain their root class')
    assert.ok(!section.includes('undefined'), 'synthetic draft supplies every displayed policy value')
  }
})

// Check selected typography declarations, not a snapshot or a browser cascade simulator.
function assertTypographyRule(css, selector, expected) {
  const declarations = new Map()
  css.walkRules((rule) => {
    if (!rule.selectors.includes(selector)) return
    rule.walkDecls((declaration) => {
      assert.notEqual(declaration.prop, 'font', `${selector}: no font shorthand resets the role`)
      if (!Object.hasOwn(expected, declaration.prop)) return
      assert.equal(declaration.important, undefined, `${selector}: no important typography override`)
      assert.equal(declaration.value, expected[declaration.prop], `${selector}: ${declaration.prop}`)
      declarations.set(declaration.prop, declaration.value)
    })
  })
  assert.deepEqual(Object.fromEntries(declarations), expected, `${selector}: required typography declarations`)
}

test('typography shared roles preserve body, labels, checkbox, dialogs, navigation and mobile inputs', () => {
  const { parse } = require('postcss')
  const css = parse(readFileSync(path.resolve(__dirname, '../src/style.css'), 'utf8'))
  const roles = { page: 22, panel: 17, dialog: 16, section: 14, body: 13, control: 12, label: 11, meta: 11, help: 12 }
  for (const [role, size] of Object.entries(roles)) {
    const declarations = []
    css.walkDecls(`--font-size-${role}`, (declaration) => {
      assert.equal(declaration.parent.selector, ':root', `${role}: shared root role`)
      declarations.push(declaration.value)
    })
    assert.deepEqual(declarations, [`${size}px`], `${role}: one stable role value`)
  }
  css.walkRules((rule) => {
    if (!rule.selectors.some((selector) => /^(?:html|:root)(?:$|[.:\s])/.test(selector))) return
    rule.walkDecls((declaration) => {
      assert.ok(!['font', 'font-size'].includes(declaration.prop), 'html/root keep the default rem scale')
    })
  })
  assertTypographyRule(css, 'body', { 'font-size': 'var(--font-size-body)', 'font-weight': '400', 'line-height': '1.6' })
  for (const selector of ['.check', '.check > span']) {
    assertTypographyRule(css, selector, { 'font-size': 'var(--font-size-body)', 'font-weight': '400' })
  }
  assertTypographyRule(css, 'label:not(.check)', { 'font-size': 'var(--font-size-label)', 'font-weight': '700' })
  assertTypographyRule(css, '.settings-dialog-title', { 'font-size': 'var(--font-size-dialog)', 'font-weight': '700', 'line-height': '1.4' })
  assertTypographyRule(css, '.panel-head h2', { 'font-size': 'var(--font-size-panel)', 'font-weight': '700', 'line-height': '1.4' })
  assertTypographyRule(css, '.field-help', { 'font-size': 'var(--font-size-help)', 'font-weight': '400', 'line-height': '1.6' })
  for (const selector of ['.nav-item', '.language-trigger', '.language-menu-item', '.profile-trigger', '.profile-menu-item']) {
    assertTypographyRule(css, selector, { 'font-size': 'var(--font-size-body)', 'font-weight': '600' })
  }
  // The final global form rules must retain the <=700px 16px protection.
  for (const selector of ['input', 'select', 'textarea']) {
    const sizes = []
    css.walkRules((rule) => {
      if (!rule.selectors.includes(selector)) return
      rule.walkDecls((declaration) => {
        if (declaration.prop === 'font') {
          assert.equal(sizes.length, 0, `${selector}: no shorthand after explicit sizes`)
          assert.equal(declaration.value, 'inherit')
        }
        if (declaration.prop !== 'font-size') return
        assert.equal(declaration.important, undefined)
        sizes.push(declaration.value)
        if (sizes.length === 1) assert.equal(rule.parent.type, 'root', `${selector}: desktop form rule`)
        else {
          assert.equal(rule.parent.name, 'media')
          assert.equal(rule.parent.params, '(max-width: 700px)')
        }
      })
    })
    assert.deepEqual(sizes, ['var(--font-size-body)', '16px'], `${selector}: mobile form text remains 16px`)
  }
  const shell = readFileSync(path.resolve(__dirname, '../src/components/ui/SettingsDialogShell.vue'), 'utf8')
  const checkbox = readFileSync(path.resolve(__dirname, '../src/components/ui/CheckboxField.vue'), 'utf8')
  assert.match(shell, /<DialogTitle class="settings-dialog-title"/)
  assert.match(checkbox, /<label class="check">[\s\S]*<span>{{ label }}<\/span>/)
  const { parse: parseSfc } = require('@vue/compiler-sfc')
  const policy = parseSfc(readFileSync(path.resolve(__dirname, '../src/components/UserPolicyFields.vue'), 'utf8')).descriptor
  const policyCss = parse(policy.styles.map((style) => style.content).join('\n'))
  assertTypographyRule(policyCss, '.user-policy-subsection > span:not(.muted)', {
    'font-size': 'var(--font-size-section)', 'font-weight': '700', 'line-height': '1.4',
  })
  policyCss.walkRules((rule) => {
    assert.ok(!rule.selectors.includes('.user-policy-subsection > span'), 'subsection titles must exclude muted helper text')
  })
  assertTypographyRule(css, '.muted', { 'font-size': 'var(--font-size-help)', 'font-weight': '400', 'line-height': '1.6' })
})

test('typography fifteen-route CSS contracts and synthetic AdminSecurity SSR retain page and secondary roles', async (context) => {
  const vue = require('vue')
  const { parse: parseSfc } = require('@vue/compiler-sfc')
  const { parse } = require('postcss')
  const globalCss = parse(readFileSync(path.resolve(__dirname, '../src/style.css'), 'utf8'))
  // One representative secondary role per route, plus the existing standalone headers.
  const contracts = {
    'OverviewPage.vue': [/<h2>/, '.health-subtitle', 'meta'],
    'ServersPage.vue': [/<h2>/, '.config-section-head h3', 'section'],
    'ClientsPage.vue': [/<h2>/, '.client-table th', 'label'],
    'UsersPage.vue': [/<h2>/, '.users-section-title', 'section'],
    'NotificationsPage.vue': [/<h2>/, '.muted', 'help'],
    'BackupPage.vue': [/<h2>/, '.backup-card h3', 'section'],
    'LogsPage.vue': [/<h2>/, '.log-title strong', 'body'],
    'AccountPage.vue': [/<h2>/, '.audit-row small', 'meta'],
    'SessionsPage.vue': [/class="sessions-page p-6"/, '.sessions-detail dt', 'label'],
    'LibraryTasksPage.vue': [/<h2>/, '.library-card-title h4', 'section', '.library-page-heading h2'],
    'DiagnosticsPage.vue': [/<h1>/, '.detail-summary dt', 'label', '.page-heading h1'],
    'HistoryPage.vue': [/<h1>/, '.filter-field > span', 'label', '.history-head h1'],
    'CacheManagementPage.vue': [/<h2>/, '.cache-table th', 'label'],
    'AlertsPage.vue': [/<h2>/, '.alert-rule-head h3', 'section'],
    'AdminSecurityPage.vue': [/<h1>/, '.row-meta', 'meta', '.page-head h1'],
  }
  const router = readFileSync(path.resolve(__dirname, '../src/router/index.ts'), 'utf8')
  assert.deepEqual([...router.matchAll(/import\('@\/pages\/([^']+)'\)/g)].map((match) => match[1]).sort(), Object.keys(contracts).sort())
  assert.equal(Object.keys(contracts).length, 15)
  for (const [filename, [heading, selector, role, pageHeading]] of Object.entries(contracts)) {
    const { descriptor, errors } = parseSfc(readFileSync(path.resolve(__dirname, '../src/pages', filename), 'utf8'))
    assert.deepEqual(errors, [], filename)
    assert.match(descriptor.template.content, heading, `${filename}: existing heading structure`)
    const scopedCss = parse(descriptor.styles.map((style) => {
      assert.equal(style.scoped, true, `${filename}: local styles remain scoped`)
      return style.content
    }).join('\n'))
    const css = scopedCss.clone()
    css.prepend(globalCss.nodes.map((node) => node.clone()))
    assertTypographyRule(css, selector, { 'font-size': `var(--font-size-${role})` })
    if (pageHeading) {
      assertTypographyRule(scopedCss, pageHeading, { 'font-size': 'var(--font-size-page)', 'font-weight': '700', 'line-height': '1.3' })
    }
    if (filename === 'HistoryPage.vue' || filename === 'DiagnosticsPage.vue') {
      const inputSelector = filename === 'HistoryPage.vue' ? '.filter-field select' : '.diagnostics-filters input'
      const sizes = []
      scopedCss.walkRules((rule) => {
        if (!rule.selectors.includes(inputSelector)) return
        rule.walkDecls((declaration) => {
          if (declaration.prop === 'font') {
            assert.equal(sizes.length, 0, `${filename}: no shorthand after explicit sizes`)
            assert.equal(declaration.value, 'inherit', `${filename}: no small font shorthand`)
          }
          if (declaration.prop !== 'font-size') return
          assert.equal(declaration.important, undefined, `${filename}: mobile size cannot be blocked`)
          sizes.push(declaration.value)
          if (sizes.length === 1) assert.equal(rule.parent.type, 'root', `${filename}: desktop input rule`)
          else {
            assert.equal(rule.parent.name, 'media')
            assert.equal(rule.parent.params, '(max-width: 700px)')
          }
        })
      })
      assert.deepEqual(sizes, ['var(--font-size-body)', '16px'], `${filename}: scoped mobile input wins with no later size override`)
    }
    if (filename === 'AdminSecurityPage.vue') {
      for (const selector of ['.admin-security-page', '.dialog-form', '.secret-result', '.recovery-codes']) {
        assertTypographyRule(scopedCss, selector, { 'font-size': 'var(--font-size-body)', 'line-height': '1.6' })
      }
      assertTypographyRule(scopedCss, '.section-head h2', { 'font-size': 'var(--font-size-section)', 'font-weight': '700', 'line-height': '1.4' })
      assertTypographyRule(scopedCss, '.dialog-form > label:not(.check)', { 'font-size': 'var(--font-size-label)', 'font-weight': '700' })
      assertTypographyRule(scopedCss, '.dialog-form input', { 'font-weight': '400' })
      assertTypographyRule(scopedCss, '.security-row small', { 'font-size': '12px', 'line-height': '1.6' })
      assertTypographyRule(scopedCss, '.primary-button', { 'font-size': '12px', 'font-weight': '600', 'line-height': '1.4' })
      assertTypographyRule(scopedCss, '.secret-result code', { 'font-size': '12px', 'font-weight': '400', 'overflow-wrap': 'anywhere', 'user-select': 'all', 'white-space': 'pre-wrap' })
    }
  }

  // Render normal rows and a portal-shell stub using only synthetic setup state.
  // Never load the controller, click an action, submit a form or create a credential.
  context.mock.method(globalThis, 'fetch', () => assert.fail('Typography SSR must not make HTTP requests'))
  const noAction = () => assert.fail('Typography SSR must not execute actions')
  const admin = {
    id: 701, username: 'typography-fixture', display_name: 'Synthetic administrator', enabled: true,
    role_keys: ['super_admin'], permission_keys: [], server_grants: [], totp_enabled: false,
  }
  const panel = { t, api: noAction, encryptPayload: noAction, showNotice: noAction, serverOptions: vue.ref([]), locale: vue.ref('zh-CN') }
  const html = await renderPage('AdminSecurityPage.vue', panel, (state) => {
    state.admins.value = [admin]
    state.currentUserId.value = admin.id
    state.roles.value = [{ id: 801, key: 'fixture-role', name: 'Synthetic role', permissions: [] }]
    state.sessions.value = [{ id: 901, device_label: 'Synthetic device', auth_method: 'fixture', last_seen_at: '2025-01-01T00:00:00Z', expires_at: '2025-01-02T00:00:00Z' }]
    state.showRoleDialog.value = true
    state.selectedAdmin.value = admin
    state.roleDraft.value = ['fixture-role']
  }, {
    '@/components/ui/CheckboxField.vue': loadPageComponent('../components/ui/CheckboxField.vue', {}),
    '@/components/ui/SettingsDialogShell.vue': {
      props: ['open', 'title', 'description', 'closeLabel', 'showDescription'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', { class: 'settings-dialog-content' }, [
        vue.h('h2', { class: 'settings-dialog-title' }, props.title), ...(slots.default?.() ?? []), ...(slots.footer?.() ?? []),
      ]) : null,
    },
    '@/composables/useActionDialog': { useActionDialog: () => ({ confirmAction: noAction, promptAction: noAction }) },
  })
  assert.match(html, /<h1>管理员安全<\/h1>/)
  assert.equal([...html.matchAll(/class="security-section(?: two-factor-section)?"/g)].length, 4)
  assert.match(html, /<strong>Synthetic administrator<\/strong>/)
  assert.match(html, /<strong>Synthetic device<\/strong>/)
  assert.match(html, /class="row-meta"/)
  assert.match(html, /<dialog class="settings-dialog-content"><h2 class="settings-dialog-title">编辑角色<\/h2>/)
  assert.match(html, /class="dialog-form choice-list"[\s\S]*<label class="check"><input[^>]*checked[^>]*><span>Synthetic role \(fixture-role\)<\/span>/)
  assert.doesNotMatch(html, /undefined/)
})

test('refresh button renders shared labels and guards single-flight actions through success and failure', async (context) => {
  const vue = require('vue')
  const filename = '../components/ui/RefreshButton.vue'
  const labels = { label: t('刷新'), loadingLabel: t('刷新中') }
  // Only the icon is stubbed; the actual button template owns its decorative attributes.
  const overrides = { '@lucide/vue': { RefreshCw: { render: () => vue.h('svg') } } }
  for (const flags of [{}, { loading: true }, { disabled: true }, { loading: true, disabled: true }]) {
    const busy = Boolean(flags.loading)
    const html = await renderPage(filename, {}, undefined, overrides, {
      ...labels, ...flags, action: () => assert.fail('SSR must not invoke the refresh action'),
    })
    const button = html.match(/<button\b[^>]*>/)?.[0]
    assert.ok(button, JSON.stringify(flags))
    assert.match(button, /\btype="button"/)
    assert.equal(/\sdisabled(?:[\s=>])/.test(button), busy || Boolean(flags.disabled))
    assert.ok(button.includes(`aria-busy="${busy}"`))
    assert.ok(button.includes(`aria-label="${busy ? labels.loadingLabel : labels.label}"`))
    assert.ok(button.includes(`title="${labels.label}"`))
    assert.ok(html.includes(`<span>${busy ? labels.loadingLabel : labels.label}</span>`))
    const icon = html.match(/<svg\b[^>]*>/)?.[0]
    assert.ok(icon, 'the icon stub retains the template attributes, not real icon geometry')
    assert.match(icon, /aria-hidden="true"/)
    assert.match(icon, /class="refresh-button-icon(?: is-spinning)?"/)
    assert.equal(icon.includes('is-spinning'), busy)
  }

  const calls = []
  let completion = deferred()
  const action = (...args) => { calls.push(args); return completion.promise }
  const props = vue.reactive({ ...labels, loading: false, disabled: false, action })
  const component = loadPageComponent(filename, {}, overrides)
  const scope = vue.effectScope()
  context.after(() => scope.stop())
  const state = scope.run(() => component.setup(props, { expose: () => {} }))
  assert.equal(state.pending.value, false)
  assert.equal(state.busy.value, false)

  const succeeded = state.refresh({ type: 'click' })
  assert.equal(state.pending.value, true)
  assert.equal(state.busy.value, true)
  assert.deepEqual(calls, [[]], 'the action receives no click event or other arguments')
  await state.refresh()
  assert.equal(calls.length, 1, 'pending refresh is single-flight')
  completion.resolve('done')
  await succeeded
  assert.equal(state.pending.value, false)
  assert.equal(state.busy.value, false)

  completion = deferred()
  const failure = new Error('synthetic refresh failure')
  const failed = state.refresh()
  const rejected = assert.rejects(failed, (error) => error === failure)
  assert.equal(state.pending.value, true)
  assert.equal(state.busy.value, true)
  await state.refresh()
  assert.equal(calls.length, 2, 'a failing in-flight action also excludes duplicates')
  completion.reject(failure)
  await rejected
  assert.equal(state.pending.value, false, 'finally clears pending after rejection')
  assert.equal(state.busy.value, false)

  const synchronousFailure = new Error('synthetic synchronous refresh failure')
  props.action = () => { throw synchronousFailure }
  await assert.rejects(state.refresh(), (error) => error === synchronousFailure)
  assert.equal(state.pending.value, false)
  assert.equal(state.busy.value, false)
  props.action = action
  for (const flags of [
    { loading: true, disabled: false },
    { loading: false, disabled: true },
    { loading: true, disabled: true },
  ]) {
    Object.assign(props, flags)
    assert.equal(state.busy.value, flags.loading)
    await state.refresh()
    assert.equal(calls.length, 2, 'external loading or disabled skips the action')
    assert.equal(state.pending.value, false)
  }
  Object.assign(props, { loading: false, disabled: false })
  completion = deferred()
  const recovered = state.refresh()
  assert.equal(state.pending.value, true)
  assert.deepEqual(calls, [[], [], []], 'refresh can run again after failures and external guards clear')
  completion.resolve()
  await recovered
  assert.equal(state.pending.value, false)
  assert.equal(state.busy.value, false)
})

test('refresh button contracts stay unified across twelve pages and seventeen manual refresh controls', () => {
  const { parse } = require('@vue/compiler-sfc')
  const contracts = {
    'AccountPage.vue': [{ action: 'refreshAuditLogs' }],
    'AdminSecurityPage.vue': [{ action: 'refreshPage', loading: 'loading || refreshing', disabled: 'loading || refreshing' }],
    'AlertsPage.vue': [
      { action: 'refreshAll', loading: 'loading', disabled: 'loading' },
      { action: '() => loadDeliveries()', loading: 'deliveriesLoading', disabled: 'deliveriesLoading' },
    ],
    'CacheManagementPage.vue': [{ action: 'refreshPage', loading: 'loading || refreshing', disabled: 'loading || refreshing || cleaning' }],
    'ClientsPage.vue': [{ action: 'refreshClientControl' }, { action: 'refreshRateLimitStatus' }],
    'DiagnosticsPage.vue': [{ action: '() => loadDiagnostics()', loading: 'loading', disabled: 'loading || cleaning' }],
    'HistoryPage.vue': [{ action: 'refreshPage', loading: 'loading || refreshing', disabled: 'loading || refreshing' }],
    'LibraryTasksPage.vue': [{ action: 'refreshPage', loading: 'loading || refreshing', disabled: 'loading || refreshing' }],
    'LogsPage.vue': [
      { action: 'refreshActivityLogs', loading: 'logsLoading', disabled: 'logsLoading' },
      { action: 'refreshLogConfig', loading: 'logConfigLoading', disabled: 'logConfigLoading || logConfigSaving' },
    ],
    'OverviewPage.vue': [
      { action: 'refreshOperationalData' },
      { action: 'refreshRateLimitStatus' },
      { action: 'refreshPlaybackSessions', loading: 'playbackLoading', disabled: 'playbackLoading' },
    ],
    'SessionsPage.vue': [{ action: 'refreshPage', loading: 'loading || refreshing', disabled: 'loading || refreshing' }],
    'UsersPage.vue': [{ action: 'refresh', loading: 'loading', disabled: 'loading' }],
  }
  const names = new Set(['action', 'loading', 'disabled', 'label', 'loading-label'])
  let count = 0
  assert.equal(Object.keys(contracts).length, 12)
  for (const [filename, expected] of Object.entries(contracts)) {
    const absolute = path.resolve(__dirname, '../src/pages', filename)
    const { descriptor, errors } = parse(readFileSync(absolute, 'utf8'), { filename: absolute })
    assert.deepEqual(errors, [], filename)
    const buttons = []
    function visit(node) {
      if (node.tag === 'RefreshButton') buttons.push(node)
      node.children?.forEach(visit)
    }
    visit(descriptor.template.ast)
    assert.equal(buttons.length, expected.length, `${filename}: refresh control count`)
    buttons.forEach((button, index) => {
      const bindings = {}
      for (const prop of button.props) {
        const name = prop.type === 7 ? prop.arg?.content : prop.name
        if (!names.has(name)) continue
        assert.equal(prop.type, 7, `${filename}: ${name} must be bound`)
        assert.equal(prop.name, 'bind', `${filename}: ${name} binding`)
        assert.equal(prop.arg.isStatic, true, `${filename}: explicit ${name} argument`)
        assert.equal(Object.hasOwn(bindings, name), false, `${filename}: no duplicate ${name}`)
        bindings[name] = prop.exp?.content.trim().replace(/\s+/g, ' ')
      }
      assert.deepEqual(bindings, {
        label: "t('刷新')", 'loading-label': "t('刷新中')", ...expected[index],
      }, `${filename}: refresh control ${index + 1} preserves its action and original busy/disabled conditions`)
    })
    count += buttons.length
  }
  assert.equal(count, 17, 'all manual refresh controls are covered without inventing external loading flags')
})

function chartHarness(context, input) {
  const vue = require('vue')
  const mounted = [], unmounted = []
  const component = loadPageComponent('../components/ui/P2LineChart.vue', {}, {
    vue: { ...vue, onMounted: (fn) => mounted.push(fn), onBeforeUnmount: (fn) => unmounted.push(fn) },
  })
  const props = vue.reactive({ labels: [], series: [], height: 320, valuePrefix: '', valueSuffix: '', ...input })
  const scope = vue.effectScope()
  const state = scope.run(() => component.setup(props, { expose: () => {} }))
  context.after(() => { unmounted.forEach((fn) => fn()); scope.stop() })
  return { state, props, mounted, unmounted }
}

test('P2LineChart renders isolated zero and nonzero samples beyond 31 dates without bridging NaN gaps', async () => {
  const labels = Array.from({ length: 90 }, (_, i) => `day-${i + 1}`)
  const sparse = Array(90).fill(Number.NaN)
  sparse[0] = 0
  sparse[34] = 180
  sparse[60] = 20
  sparse[61] = 21
  sparse[89] = 0
  let state
  const html = await renderPage('../components/ui/P2LineChart.vue', {}, (value) => { state = value }, {}, {
    labels, series: [{ name: 'continuous', values: labels.map(() => 100) }, { name: 'sparse', values: sparse }],
  })
  assert.equal(state.showPoints.value, false)
  assert.deepEqual(state.markersFor(state.normalizedSeries.value[0].values), [])
  assert.deepEqual(state.markersFor(state.normalizedSeries.value[1].values), [
    { val: 0, idx: 0 }, { val: 180, idx: 34 }, { val: 0, idx: 89 },
  ])
  assert.equal((html.match(/class="data-point"/g) || []).length, 3, 'actual SFC output must retain isolated markers')
  const paths = [...html.matchAll(/class="series-path" d="([^"]*)"/g)].map((match) => match[1])
  assert.equal(paths.length, 2)
  assert.equal((paths[0].match(/ L /g) || []).length, 89)
  assert.equal((paths[1].match(/M /g) || []).length, 4)
  assert.equal((paths[1].match(/ L /g) || []).length, 1, 'only the adjacent pair forms a line')
  assert.equal(state.getAreaPath(sparse, state.chartScale.value), '')
})

test('P2LineChart keeps short-series zero markers and renders empty data without a phantom header', async () => {
  let state
  const html = await renderPage('../components/ui/P2LineChart.vue', {}, (value) => { state = value }, {}, {
    labels: ['2026-11-01', '2026-11-02', '2026-11-03', '2026-11-04'],
    series: [{ name: 'short', values: [0, 2, Number.NaN, 4] }, { name: 'invalid', values: [-1, Infinity] }],
  })
  assert.equal(state.normalizedSeries.value.length, 1)
  assert.equal((html.match(/class="data-point"/g) || []).length, 3)
  assert.equal(state.getPath(state.normalizedSeries.value[0].values, 4), 'M 0,100 L 33.33,50M 100,0')
  assert.equal(state.getAreaPath(state.normalizedSeries.value[0].values, 4), '')
  const empty = await renderPage('../components/ui/P2LineChart.vue', {}, undefined, {}, {
    labels: [], series: [], emptyLabel: 'fixture-empty',
  })
  assert.match(empty, /role="status">fixture-empty/)
  assert.doesNotMatch(empty, /class="chart-header"|class="axis-x"|class="chart-tooltip"/)
})

test('P2LineChart clamps measured tooltips and flips below a top sample without changing its date index', async (context) => {
  const { state } = chartHarness(context, {
    labels: Array.from({ length: 90 }, (_, i) => `day-${i + 1}`),
    series: [{ name: 'fixture', values: Array(90).fill(180) }],
  })
  state.chartElement.value = {
    clientWidth: 373, clientHeight: 318, clientLeft: 1, clientTop: 1,
    getBoundingClientRect: () => ({ left: 100, top: 200, width: 375, height: 320 }),
  }
  const plot = { getBoundingClientRect: () => ({ left: 180, top: 280, width: 270, height: 200 }) }
  state.plotElement.value = plot
  state.tooltipElement.value = { getBoundingClientRect: () => ({ width: 210, height: 125 }) }
  state.handleMouseMove({ currentTarget: plot, clientX: 450, clientY: 280 })
  await require('vue').nextTick()
  assert.equal(state.tooltip.value.index, 89)
  assert.equal(state.tooltip.value.x, 100)
  assert.deepEqual(state.tooltipPosition.value, { left: 155, top: 89 }, 'top sample opens downward and stays inside the right edge')
  state.handleMouseMove({ currentTarget: plot, clientX: 450, clientY: 300 })
  assert.equal(state.tooltip.value.y, 0, 'moving toward the tooltip must not chase the pointer')
  state.handleMouseMove({ currentTarget: plot, clientX: 180, clientY: 480 })
  await require('vue').nextTick()
  assert.equal(state.tooltip.value.index, 0)
  assert.deepEqual(state.tooltipPosition.value, { left: 8, top: 144 }, 'bottom sample opens upward and stays inside the left edge')
  state.tooltipElement.value = { getBoundingClientRect: () => ({ width: 357, height: 302 }) }
  await require('vue').nextTick()
  assert.deepEqual(state.tooltipPosition.value, { left: 8, top: 8 }, 'a tooltip capped to card size still fits')
})

test('P2LineChart falls back to window resize when ResizeObserver is unavailable and removes the listener on unmount', (context) => {
  const previousObserver = globalThis.ResizeObserver
  const previousWindow = globalThis.window
  delete globalThis.ResizeObserver
  const listeners = new Map()
  const added = [], removed = []
  globalThis.window = {
    addEventListener(type, listener) {
      added.push([type, listener])
      listeners.set(type, listener)
    },
    removeEventListener(type, listener) {
      removed.push([type, listener])
      if (listeners.get(type) === listener) listeners.delete(type)
    },
  }
  context.after(() => {
    if (previousObserver === undefined) delete globalThis.ResizeObserver
    else globalThis.ResizeObserver = previousObserver
    if (previousWindow === undefined) delete globalThis.window
    else globalThis.window = previousWindow
  })

  const { state, mounted, unmounted } = chartHarness(context, {
    labels: ['first', 'last'], series: [{ name: 'fixture', values: [0, 10] }],
  })
  let measured = 0
  const plot = { getBoundingClientRect: () => { measured += 1; return { width: 240, height: 120 } } }
  state.plotElement.value = plot
  state.xAxisLabelElement.value = { getBoundingClientRect: () => ({ width: 30, height: 0 }) }

  assert.doesNotThrow(() => mounted.forEach((fn) => fn()))
  assert.equal(added.length, 1)
  assert.equal(added[0][0], 'resize')
  assert.ok(measured > 0, 'mount performs initial chart measurement')
  const measuredOnMount = measured
  listeners.get('resize')()
  assert.ok(measured > measuredOnMount, 'window resize remeasures the chart')

  globalThis.window = { addEventListener() {}, removeEventListener() { throw new Error('wrong window') } }
  unmounted.forEach((fn) => fn())
  assert.deepEqual(removed, added, 'unmount removes the listener from the original window')
  assert.equal(listeners.has('resize'), false)

  globalThis.window = { addEventListener() {} }
  const noRemove = chartHarness(context, { labels: [], series: [] })
  assert.doesNotThrow(() => noRemove.mounted.forEach((fn) => fn()))
  globalThis.window = { addEventListener() {}, removeEventListener() { throw new Error('replacement window') } }
  assert.doesNotThrow(() => noRemove.unmounted.forEach((fn) => fn()))

  delete globalThis.window
  const withoutWindow = chartHarness(context, { labels: [], series: [] })
  assert.doesNotThrow(() => withoutWindow.mounted.forEach((fn) => fn()))
  withoutWindow.unmounted.forEach((fn) => fn())
})


test('P2LineChart hides stale tooltips on data changes, repositions on resize, and disconnects its observer', async (context) => {
  const vue = require('vue')
  let callback, observer
  const previousObserver = globalThis.ResizeObserver
  globalThis.ResizeObserver = class {
    targets = []
    disconnected = 0
    constructor(fn) { callback = fn; observer = this }
    observe(element) { this.targets.push(element) }
    disconnect() { this.targets = []; this.disconnected++ }
  }
  context.after(() => {
    if (previousObserver === undefined) delete globalThis.ResizeObserver
    else globalThis.ResizeObserver = previousObserver
  })
  const { state, props, mounted, unmounted } = chartHarness(context, {
    labels: ['first', 'last'], series: [{ name: 'fixture', values: [0, 500] }],
  })
  let width = 600
  state.chartElement.value = {
    get clientWidth() { return width }, clientHeight: 318, clientLeft: 0, clientTop: 0,
    getBoundingClientRect: () => ({ left: 0, top: 0, width, height: 318 }),
  }
  const plot = { getBoundingClientRect: () => ({ left: 80, top: 70, width: width - 100, height: 200 }) }
  state.plotElement.value = plot
  state.tooltipElement.value = { getBoundingClientRect: () => ({ width: 180, height: 100 }) }
  mounted.forEach((fn) => fn())
  state.handleMouseMove({ currentTarget: plot, clientX: 580, clientY: 70 })
  await vue.nextTick()
  assert.equal(observer.targets.length, 3)
  assert.equal(state.tooltipPosition.value.left, 412)
  width = 375
  callback()
  assert.equal(state.tooltipPosition.value.left, 187)
  props.series[0].values[1] = 25
  await vue.nextTick()
  assert.equal(state.tooltip.value.visible, false)
  assert.equal(state.tooltip.value.index, -1)
  state.handleMouseMove({ currentTarget: plot, clientX: 355, clientY: 70 })
  props.labels = ['replacement', 'date']
  await vue.nextTick()
  assert.equal(state.tooltip.value.visible, false)
  assert.equal(state.tooltip.value.index, -1)
  const before = observer.disconnected
  unmounted.forEach((fn) => fn())
  assert.equal(observer.disconnected, before + 1)
  assert.deepEqual(observer.targets, [])
})

test('P2LineChart starts with endpoint dates in SSR before axis measurements exist', async () => {
  const labels = Array.from({ length: 7 }, (_, i) => `2026-11-${String(i + 1).padStart(2, '0')}`)
  const html = await renderPage('../components/ui/P2LineChart.vue', {}, (state) => {
    assert.deepEqual(state.xAxisTicks.value, [0, 6])
    assert.equal(state.xAxisLabelText.value, labels.map((label) => label.slice(5).replace('-', '/')).join('\n'))
  }, {}, { labels, series: [{ name: 'fixture', values: [0, 1, 2, 3, 4, 5, 6] }] })
  assert.match(html, /left:0%;transform:translateX\(0\);">11\/01<\/span>/)
  assert.match(html, /left:100%;transform:translateX\(-100%\);">11\/07<\/span>/)
})

test('P2LineChart adapts date ticks to measured plot and label widths without changing samples or tooltip indexes', async (context) => {
  const vue = require('vue')
  let callback, observer
  const previousObserver = globalThis.ResizeObserver
  globalThis.ResizeObserver = class {
    targets = []
    constructor(fn) { callback = fn; observer = this }
    observe(element) { this.targets.push(element) }
    disconnect() { this.targets = [] }
  }
  context.after(() => {
    if (previousObserver === undefined) delete globalThis.ResizeObserver
    else globalThis.ResizeObserver = previousObserver
  })
  const labels = Array.from({ length: 7 }, (_, i) => `2026-11-${String(i + 1).padStart(2, '0')}`)
  const series = [{ name: 'fixture', values: [0, 1, 2, 3, 4, 5, 6] }]
  const { state, props, mounted } = chartHarness(context, { labels, series })
  const originalPath = state.getPath(state.normalizedSeries.value[0].values, state.chartScale.value)
  assert.deepEqual(state.xAxisTicks.value, [0, 6], 'before mounting, only endpoint dates are shown')
  let plotWidth = 147, labelWidth = 30.28
  const plot = { getBoundingClientRect: () => ({ left: 100, top: 70, width: plotWidth, height: 200 }) }
  state.plotElement.value = plot
  state.xAxisLabelElement.value = { getBoundingClientRect: () => ({ width: labelWidth, height: 0 }) }
  mounted.forEach((fn) => fn())
  await vue.nextTick()
  assert.equal(observer.targets.length, 2, 'plot and the independent label measure are observed')
  assert.deepEqual(state.xAxisTicks.value, [0, 3, 6], '375px scenario reduces crowded endpoint neighbors')
  plotWidth = 162
  callback()
  assert.deepEqual(state.xAxisTicks.value, [0, 2, 4, 6], '390px plot admits four separated dates')
  plotWidth = 640
  callback()
  assert.deepEqual(state.xAxisTicks.value, [0, 1, 2, 3, 4, 5, 6], 'wide plots restore all seven dates')
  plotWidth = 320
  callback()
  assert.equal(state.xAxisTicks.value.length, 7)
  labelWidth = 60
  callback()
  assert.deepEqual(state.xAxisTicks.value, [0, 2, 4, 6], 'a wider measured label reduces tick density too')
  const layout = state.xAxisLayout.value
  callback()
  assert.equal(state.xAxisLayout.value, layout, 'unchanged observations must not invalidate layout again')
  plotWidth = 147
  labelWidth = 30.28
  callback()
  for (const index of [0, 1, 6]) {
    state.handleMouseMove({ currentTarget: plot, clientX: 100 + plotWidth * index / 6, clientY: 70 })
    assert.equal(state.tooltip.value.index, index, 'hidden date ticks remain selectable by their original sample index')
    assert.equal(state.tooltip.value.x, state.safeX(index, labels.length))
  }
  assert.deepEqual(props.labels, labels)
  assert.deepEqual(props.series, series)
  assert.equal(state.getPath(state.normalizedSeries.value[0].values, state.chartScale.value), originalPath)
  assert.deepEqual(state.markersFor(state.normalizedSeries.value[0].values).map((point) => point.idx), [0, 1, 2, 3, 4, 5, 6])
})

test('P2LineChart renders all tooltip series with complete metric values and units', async () => {
  const { formatMetric } = load('composables/useHistoryController.ts')
  const series = Array.from({ length: 20 }, (_, index) => ({ name: `server-${String(index + 1).padStart(2, '0')}`, values: [index === 0 ? Number.NaN : 500_000_000] }))
  series[0].values = [0]
  const html = await renderPage('../components/ui/P2LineChart.vue', {}, (state) => {
    state.tooltip.value = { visible: true, index: 0, x: 50, y: 0 }
    assert.equal(state.formatAxisValue(500_000_000), '500.00 Mbps')
    assert.equal(state.formatAxisValue(20_000_000), '20.00 Mbps')
  }, {}, { labels: ['2026-11-01'], series, valueFormatter: (value) => formatMetric('bandwidth', value) })
  assert.equal((html.match(/class="tooltip-series"/g) || []).length, 20)
  assert.match(html, /<div class="tooltip-list">[\s\S]*server-20[\s\S]*<strong>500\.00 Mbps<\/strong>/)
  assert.match(html, /<strong>0 bps<\/strong>/)
  assert.match(html, /class="tooltip-date">11\/01/)
  const counts = await renderPage('../components/ui/P2LineChart.vue', {}, (state) => {
    assert.equal(state.formatAxisValue(1_000_000), '1,000,000')
  }, {}, { labels: ['day'], series: [{ name: 'requests', values: [1_000_000] }], valueFormatter: (value) => formatMetric('requests', value) })
  assert.match(counts, />1,000,000<\/span>/)
})

test('library task names react to authorized configuration while filters keep server IDs and empty/failed options', async (context) => {
  const vue = require('vue')
  const servers = vue.ref([
    { id: 'server-cn', name: '  家庭影音  ' },
    { id: 'empty-server', name: '空媒体库服务器' },
    { id: 'failed-server', name: '暂时离线服务器' },
    { id: 'blank-server', name: ' \t ' },
  ])
  const requests = []
  let rejectQueries = false
  const { useLibraryTasksController } = load('composables/useLibraryTasksController.ts')
  const scope = vue.effectScope()
  const controller = scope.run(() => useLibraryTasksController({
    servers: () => servers.value,
    api: async (url) => {
      requests.push(url)
      if (rejectQueries) throw new Error('mock library endpoint unavailable')
      return {
        items: [{ server_id: 'server-cn', id: 'scan', name: '电影扫描' }],
        failures: [{ server_id: 'failed-server', error: 'mock server unavailable' }],
      }
    },
    encryptPayload: async () => ({}), t, confirmAction: async () => false,
    promptAction: async () => null, showNotice: () => {},
  }))
  context.after(() => { controller.dispose(); scope.stop() })
  const expected = [
    { value: 'all', label: '全部服务器' },
    { value: 'server-cn', label: '家庭影音' },
    { value: 'empty-server', label: '空媒体库服务器' },
    { value: 'failed-server', label: '暂时离线服务器' },
    { value: 'blank-server', label: 'blank-server' },
  ]
  assert.deepEqual(controller.serverOptions.value, expected)
  const name = vue.computed(() => controller.serverName('server-cn'))
  assert.equal(name.value, '家庭影音')
  assert.equal(controller.serverName('blank-server'), 'blank-server')
  assert.equal(controller.serverName('unknown-server'), 'unknown-server')
  await controller.refresh()
  assert.match(controller.partialError.value, /mock server unavailable/)
  assert.deepEqual(controller.serverOptions.value, expected)

  servers.value[0].name = '  客厅影视  '
  assert.equal(name.value, '客厅影视')
  assert.equal(controller.serverOptions.value[1].label, '客厅影视')
  servers.value = servers.value.map((server) => server.id === 'server-cn' ? { ...server, name: '家庭影院新名称' } : server)
  assert.equal(name.value, '家庭影院新名称')
  assert.equal(controller.serverOptions.value[1].value, 'server-cn')

  requests.length = 0
  controller.selectedServer.value = 'server-cn'
  await tick()
  assert.deepEqual(requests.sort(), [
    '/api/library/libraries?server_id=server-cn',
    '/api/library/tasks?server_id=server-cn',
  ])
  rejectQueries = true
  await controller.refresh()
  assert.match(controller.error.value, /mock library endpoint unavailable/)
  assert.deepEqual(controller.tasks.value, [])
  assert.deepEqual(controller.libraries.value, [])
  assert.deepEqual(controller.serverOptions.value.map((option) => option.value), expected.map((option) => option.value))
})

test('library tasks SSR maps configured names in library cards, task rows and failure details', async () => {
  const vue = require('vue')
  const panel = {
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    encryptPayload: async () => ({}), showNotice: () => {}, clearNotice: () => {},
    confirmAction: async () => false, promptAction: async () => null, formatLogTime: () => 'fixture-time',
    serverOptions: vue.ref([{ id: 'server-cn', name: '  家庭影音  ' }]),
  }
  const task = {
    id: 'scan', server_id: 'server-cn', name: '电影扫描', state: 'Failed',
    type: null, path: null, progress: null, last_run: null,
    capabilities: { path: { available: false }, progress: { available: false }, last_run: { available: false } },
  }
  const html = await renderPage('LibraryTasksPage.vue', panel, (state) => {
    state.libraries.value = [{ server_id: 'server-cn', name: '电影媒体库', path: '/media/movies', capabilities: { path: { available: true } } }]
    state.tasks.value = [task]
    state.selectedTask.value = task
    state.dialogOpen.value = true
    state.failureDetail.value = { summary: '模拟失败摘要', history: [] }
  }, {
    '@/components/ui/SettingsDialogShell.vue': {
      props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null,
    },
  })
  for (const [label, pattern, content] of [
    ['article', /<article\b[^>]*>([\s\S]*?)<\/article>/, '电影媒体库'],
    ['library-task-list', /<ul\b[^>]*class="library-task-list"[^>]*>([\s\S]*?)<\/ul>/, '电影扫描'],
    ['dialog', /<dialog\b[^>]*>([\s\S]*?)<\/dialog>/, '模拟失败摘要'],
  ]) {
    const section = html.match(pattern)?.[1]
    assert.ok(section, `${label} must render`)
    assert.ok(section.includes(content), `${label} contains its fixture`)
    assert.ok(section.includes('家庭影音'), `${label} displays the configured name`)
    assert.equal(section.includes('server-cn'), false, `${label} must not display the raw server ID`)
  }
  const taskList = html.match(/<ul\b[^>]*class="library-task-list"[^>]*>/)?.[0]
  assert.ok(taskList && /\brole="list"/.test(taskList), 'task list preserves explicit list semantics when markers are hidden')
  assert.ok(html.includes('class="library-path-list"'), 'library paths also render a separate list')
  assert.ok(html.includes('value="server-cn"'))
})

test('library task metrics render complete counts or unknown placeholders in valid definition lists', async () => {
  const vue = require('vue')
  const panel = {
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    encryptPayload: async () => ({}), showNotice: () => {}, clearNotice: () => {},
    confirmAction: async () => false, promptAction: async () => null, formatLogTime: () => 'fixture-time',
    serverOptions: vue.ref([{ id: 'server-cn', name: '家庭影音' }]),
  }
  const labels = ['媒体库', '运行中', '空闲', '异常任务']
  for (const status of ['complete', 'loading', 'error', 'partialError']) {
    for (const populated of [true, false]) {
      const html = await renderPage('LibraryTasksPage.vue', panel, (state) => {
        if (populated) {
          state.libraries.value = ['电影', '剧集'].map((name) => ({
            server_id: 'server-cn', name, path: null, capabilities: { path: { available: false } },
          }))
          state.tasks.value = ['Running', 'Idle', 'Idle', 'Failed', 'failed', 'Completed'].map((taskState, index) => ({
            id: `scan-${index}`, server_id: 'server-cn', name: `扫描-${index}`, state: taskState,
            type: null, path: null, progress: null, last_run: null,
            error: taskState === 'Completed' ? '模拟任务错误' : null,
            capabilities: { path: { available: false }, progress: { available: false }, last_run: { available: false } },
          }))
        }
        if (status === 'loading') state.loading.value = true
        if (status === 'error') state.error.value = '模拟加载失败'
        if (status === 'partialError') state.partialError.value = '模拟部分服务器失败'
      })
      const metrics = html.match(/<section\b[^>]*class="library-metrics"[^>]*aria-label="任务概览"[^>]*>([\s\S]*?)<\/section>/)?.[1]
      assert.ok(metrics, `${status}/${populated}: metrics use a labeled section`)
      const groups = [...metrics.matchAll(/<div class="library-metric">([\s\S]*?)<\/div>/g)]
      assert.equal(groups.length, 4)
      const expected = status !== 'complete' ? ['—', '—', '—', '—'] : populated ? ['2', '1', '2', '3'] : ['0', '0', '0', '0']
      groups.forEach((group, index) => {
        assert.match(group[1], /^<span\b[^>]*aria-hidden="true"[^>]*>[\s\S]*?<\/span>\s*<dl><dt>[^<]+<\/dt><dd>[^<]+<\/dd><\/dl>$/,
          'each metric keeps its decorative icon outside a definition list containing one term/value pair')
        assert.deepEqual([...group[1].matchAll(/<d[td]>([^<]+)<\/d[td]>/g)].map((match) => match[1]),
          [labels[index], expected[index]], `${status}/${populated}: ${labels[index]}`)
      })
    }
  }
})

test('library cards prefer all locations, fall back to path and label unavailable capabilities', async () => {
  const vue = require('vue')
  const panel = {
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    encryptPayload: async () => ({}), showNotice: () => {}, clearNotice: () => {},
    confirmAction: async () => false, promptAction: async () => null, formatLogTime: () => 'fixture-time',
    serverOptions: vue.ref([{ id: 'server-cn', name: '家庭影音' }]),
  }
  const longPath = `/media/电影/${'完整目录/'.repeat(16)}最终目录`
  const fixtures = [
    { name: '多路径优先', path: '/legacy/ignored', locations: [longPath, '/media/剧集 第二路径'], expected: [longPath, '/media/剧集 第二路径'] },
    { name: '仅多路径', path: null, locations: ['/media/纪录片', '/media/动画'], expected: ['/media/纪录片', '/media/动画'] },
    { name: '空列表回退', path: longPath, locations: [], expected: [longPath] },
    { name: '缺失列表回退', path: '/media/音乐', expected: ['/media/音乐'] },
    { name: '空列表无路径', path: null, locations: [], expected: [], unavailable: '不可用' },
    { name: '缺失列表无路径', path: null, expected: [], unavailable: 'Emby 未提供', reason: 'emby_not_provided' },
  ]
  const html = await renderPage('LibraryTasksPage.vue', panel, (state) => {
    state.libraries.value = fixtures.map(({ name, path, locations, reason, expected }) => ({
      server_id: 'server-cn', name, path,
      ...(locations === undefined ? {} : { locations }),
      capabilities: { path: { available: expected.length > 0, unavailable_reason: reason } },
    }))
  })
  const cards = [...html.matchAll(/<article\b[^>]*class="library-card"[^>]*>([\s\S]*?)<\/article>/g)]
  assert.equal(cards.length, fixtures.length)
  cards.forEach((card, index) => {
    const fixture = fixtures[index], content = card[1]
    assert.ok(content.includes(`<h4>${fixture.name}</h4>`), fixture.name)
    assert.deepEqual([...content.matchAll(/<code>([^<]*)<\/code>/g)].map((match) => match[1]), fixture.expected,
      `${fixture.name}: full paths render in order without the legacy fallback`)
    const rows = [...content.matchAll(/<li\b[^>]*class="library-path-item"[^>]*>([\s\S]*?)<\/li>/g)]
    const buttons = [...content.matchAll(/<button\b[^>]*class="library-validate"[^>]*>/g)]
    assert.equal(rows.length, fixture.expected.length, `${fixture.name}: one row per path`)
    assert.equal(buttons.length, fixture.expected.length, `${fixture.name}: one validation button per path`)
    fixture.expected.forEach((path, pathIndex) => {
      assert.ok(rows[pathIndex][1].includes(`<code>${path}</code>`))
      assert.ok(buttons[pathIndex][0].includes(`aria-label="验证路径: ${path}"`), `${fixture.name}: path-specific accessible name`)
      assert.ok(buttons[pathIndex][0].includes(`title="验证路径: ${path}"`), `${fixture.name}: full path in button title`)
    })
    if (fixture.expected.length) {
      assert.match(content, /<ul\b[^>]*class="library-path-list"[^>]*aria-label="路径"/)
      assert.equal(content.includes('class="library-unavailable"'), false)
    } else {
      assert.ok(content.includes(`<p class="library-unavailable">${fixture.unavailable}</p>`), fixture.name)
      assert.equal(content.includes('class="library-path-list"'), false)
    }
    assert.equal(content.includes('/legacy/ignored'), false, `${fixture.name}: no fallback duplication`)
  })
})

test('library cards render validation status summaries and disable every path button while busy', async () => {
  const vue = require('vue')
  const panel = {
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    encryptPayload: async () => ({}), showNotice: () => {}, clearNotice: () => {},
    confirmAction: async () => false, promptAction: async () => null, formatLogTime: () => 'fixture-time',
    serverOptions: vue.ref([{ id: 'server-cn', name: '家庭影音' }]),
  }
  const fixtures = [
    { path: '/media/缺失状态', label: '未校验', title: '需要显式验证', className: 'is-neutral' },
    { path: '/media/尚未校验', status: 'unvalidated', label: '未校验', title: '需要显式验证', className: 'is-neutral' },
    { path: '/media/可用目录', status: 'valid', label: '可用', title: '可用', className: 'is-success' },
    { path: '/media/失败目录', status: 'invalid', label: '校验失败', title: '校验失败', className: 'is-danger', error: '模拟目录暂不可用' },
  ]
  for (const actionTaskId of [null, '', 'server-cn:another-task']) {
    const html = await renderPage('LibraryTasksPage.vue', panel, (state) => {
      state.libraries.value = [{
        server_id: 'server-cn', name: '校验状态矩阵', path: null,
        locations: fixtures.map((fixture) => fixture.path),
        location_statuses: fixtures.filter((fixture) => fixture.status).map(({ path, status, error }) => ({ path, status, error })).reverse(),
        capabilities: { path: { available: true } },
      }]
      state.actionTaskId.value = actionTaskId
    })
    const card = html.match(/<article\b[^>]*class="library-card"[^>]*>([\s\S]*?)<\/article>/)?.[1]
    assert.ok(card, 'validation matrix renders a library card')
    const rows = [...card.matchAll(/<li\b[^>]*class="library-path-item"[^>]*>([\s\S]*?)<\/li>/g)]
    assert.equal(rows.length, fixtures.length)
    rows.forEach((row, index) => {
      const fixture = fixtures[index], content = row[1]
      const label = `${fixture.path}/${actionTaskId ?? 'null'}`
      assert.ok(content.includes(`<code>${fixture.path}</code>`), label)
      const status = content.match(/<span class="library-status ([^"]+)" title="([^"]+)"><span\b[^>]*><\/span>\s*([^<]*?)<\/span>/)
      assert.ok(status, `${label}: status badge renders`)
      assert.deepEqual([status[1], status[2], status[3].trim()], [fixture.className, fixture.title, fixture.label],
        `${label}: class, descriptive title and compact visible label match the path's status`)
      const errors = [...content.matchAll(/<p class="library-error-text">([^<]*)<\/p>/g)].map((match) => match[1].trim())
      assert.deepEqual(errors, fixture.error ? [`错误摘要: ${fixture.error}`] : [], `${label}: error summary belongs only to the failed path`)
      const buttons = [...content.matchAll(/<button\b[^>]*class="library-validate"[^>]*>/g)]
      assert.equal(buttons.length, 1, label)
      assert.ok(buttons[0][0].includes(`aria-label="验证路径: ${fixture.path}"`), label)
      assert.equal(/\sdisabled(?:\s|=|>)/.test(buttons[0][0]), Boolean(actionTaskId), `${label}: validation disabled only while busy`)
    })
  }
})

test('diagnostics summary keeps long step details out of the list and marks only actual failed or bypassed steps', async () => {
  const panel = {
    locale: require('vue').ref('zh-CN'),
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  }
  const records = [
    {
      request_id: 'compact-hit', server_id: 'server-a', media_id: 'media-1', ua: 'Fixture player',
      cache_hit: true, failure_node: null, sanitized_errors: [],
      steps: [
        { name: 'Emby Path', duration_ms: 0, details: 'bypassed' },
        { name: '缓存', duration_ms: 0, details: 'hit' },
        { name: '302', duration_ms: 0, details: 'redirect location=https://fixture.invalid/very-long-path/episode.mkv' },
      ],
    },
    {
      request_id: 'compact-failure', server_id: 'server-a', media_id: 'media-2', ua: 'Fixture player',
      cache_hit: false, failure_node: 'OpenList', sanitized_errors: ['OpenList: fixture failure with path=https://fixture.invalid/failure-long-path/episode.mkv'],
      steps: [{ name: 'OpenList', duration_ms: 0, details: 'fixture failure with path=https://fixture.invalid/failure-long-path/episode.mkv' }],
    },
  ]
  const html = await renderPage('DiagnosticsPage.vue', panel, (state) => { state.diagnostics.value = records })
  assert.ok(!html.includes('https://fixture.invalid/very-long-path/episode.mkv'))
  const articles = [...html.matchAll(/<article\b[^>]*>([\s\S]*?)<\/article>/g)].map((match) => match[1])
  assert.equal(articles.length, 2)
  assert.match(articles[0], /class="chain-step skipped"/)
  assert.match(articles[0], /class="chain-step recorded"/)
  assert.match(articles[0], /缓存命中/)
  assert.match(articles[1], /失败节点: OpenList/)
  assert.ok(!articles[1].includes('https://fixture.invalid/failure-long-path/episode.mkv'))
  assert.ok(!articles[1].includes('302'))
  assert.match(html, /诊断总耗时: <strong>—<\/strong>/)
  assert.doesNotMatch(html, /Invalid Date|NaN/)
  assert.ok(!html.includes('清空诊断记录'))
})

test('diagnostics distinguish cancellations from failed steps without inventing completed nodes', async () => {
  const panel = {
    locale: require('vue').ref('zh-CN'),
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  }
  for (const details of ['cancelled: fs_get path=/fixture.mkv', 'cancelled:fs_get path=/fixture.mkv', 'cancelled before step started']) {
    const record = {
      request_id: 'canceled-fixture', server_id: 'server-a', media_id: 'media-1', ua: 'Fixture player',
      cache_hit: false, failure_node: 'OpenList', sanitized_errors: [`OpenList: ${details}`],
      steps: [{ name: 'OpenList', duration_ms: 0, details }],
    }
    const html = await renderPage('DiagnosticsPage.vue', panel, (state) => { state.diagnostics.value = [record] })
    assert.match(html, /class="chain-step cancelled"/)
    assert.match(html, /已取消: OpenList/)
    assert.doesNotMatch(html, /失败节点:/)
    assert.doesNotMatch(html, /class="chain-step failed"/)
    assert.doesNotMatch(html, /raw_url|302/)
  }
})

test('diagnostics details preserve full text and close clears the selected record', async () => {
  const vue = require('vue')
  const panel = {
    locale: vue.ref('zh-CN'),
    t, api: async () => { assert.fail('SSR must not fetch page data') },
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  }
  const record = {
    request_id: 'detail-record', server_id: 'server-a', media_id: 'media-1', ua: 'Fixture player',
    cache_hit: false, failure_node: null, sanitized_errors: [],
    steps: [{ name: 'Emby Path', duration_ms: 1, details: 'resolved path=https://fixture.invalid/完整路径/episode.mkv\nsecond line' }],
  }
  let state
  const html = await renderPage('DiagnosticsPage.vue', panel, (result) => {
    state = result
    state.diagnostics.value = [record]
    state.openDetail(record)
  }, {
    '@/components/ui/SettingsDialogShell.vue': {
      props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null,
    },
  })
  const dialog = html.match(/<dialog\b[^>]*>([\s\S]*?)<\/dialog>/)?.[1]
  assert.ok(dialog)
  assert.ok(dialog.includes('https://fixture.invalid/完整路径/episode.mkv\nsecond line'))
  assert.ok(dialog.includes('Fixture player'))
  state.onDetailOpenChange(false)
  assert.equal(state.detailOpen.value, false)
  assert.equal(state.selectedRecord.value, null)
})

test('millisecond timestamp formatting supports precision and timezone without changing default callers', () => {
  const { formatTimestampMs } = load('lib/panel-formatters.ts')
  const timestamp = Date.UTC(2026, 0, 2, 3, 4, 5, 123)
  const options = {
    year: 'numeric', month: '2-digit', day: '2-digit',
    hour: '2-digit', minute: '2-digit', second: '2-digit',
    fractionalSecondDigits: 3, hour12: false, timeZoneName: 'shortOffset', timeZone: 'UTC',
  }
  for (const locale of ['zh-CN', 'en-US']) {
    assert.equal(formatTimestampMs(timestamp, locale), new Date(timestamp).toLocaleString(locale))
    assert.equal(formatTimestampMs(timestamp, locale, options), new Date(timestamp).toLocaleString(locale, options))
    assert.match(formatTimestampMs(timestamp, locale, options), /05\.123/)
    assert.match(formatTimestampMs(timestamp, locale, options), /GMT/)
    for (const missing of [undefined, null, 0, -1, NaN, Infinity, 1e20]) {
      assert.equal(formatTimestampMs(missing, locale, options), '--')
    }
  }
})

test('diagnostics retain API timing and render precise local times, raw timestamps and measured totals in both locales', async () => {
  const vue = require('vue')
  const { useDiagnosticsController } = load('composables/useDiagnosticsController.ts')
  const { translations } = load('lib/translations.ts')
  const started = Date.UTC(2026, 0, 2, 3, 4, 5, 123)
  const record = {
    request_id: 'timed-record', server_id: 'server-a', media_id: 'media-1', ua: 'Fixture player',
    started_at_ms: started, finished_at_ms: started + 50, duration_ms: 50,
    cache_hit: false, failure_node: null, sanitized_errors: [],
    steps: [
      { name: 'Emby Path', started_at_ms: started + 4, finished_at_ms: started + 12, duration_ms: 8, details: 'resolved fixture path' },
      { name: '缓存', started_at_ms: started + 1, finished_at_ms: started + 40, duration_ms: 2, details: 'miss' },
    ],
  }
  const legacy = {
    request_id: 'legacy-record', server_id: 'server-a', media_id: 'media-2', ua: 'Fixture player',
    cache_hit: false, failure_node: null, sanitized_errors: [],
    steps: [{ name: 'Emby Path', duration_ms: 99, details: 'legacy' }],
  }
  const zeroDuration = { ...record, request_id: 'zero-duration', duration_ms: 0, finished_at_ms: started, steps: [] }
  const c = useDiagnosticsController({
    api: async () => ({ records: [record, legacy, zeroDuration], can_global_cleanup: false }),
    translate: t, showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  })
  await c.loadDiagnostics()
  assert.deepEqual(JSON.parse(JSON.stringify(c.diagnostics.value)), [record, legacy, zeroDuration])
  c.dispose()

  for (const locale of ['zh-CN', 'en-US']) {
    const translate = (source) => locale === 'en-US' ? translations[source] || source : source
    const panel = {
      locale: vue.ref(locale), t: translate, api: async () => { assert.fail('SSR must not fetch page data') },
      showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
    }
    let state
    const html = await renderPage('DiagnosticsPage.vue', panel, (result) => {
      state = result
      state.diagnostics.value = c.diagnostics.value
      state.openDetail(record)
    }, {
      '@/components/ui/SettingsDialogShell.vue': {
        props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null,
      },
    })
    const articles = [...html.matchAll(/<article\b[^>]*>([\s\S]*?)<\/article>/g)].map((match) => match[1])
    assert.equal(articles.length, 3)
    assert.ok(articles[0].includes(state.diagnosisTime(started)))
    assert.match(articles[0], /50 ms/)
    assert.match(articles[1], /<strong>--<\/strong>/)
    assert.match(articles[1], /<strong>—<\/strong>/)
    assert.match(articles[2], /<strong>0 ms<\/strong>/)
    const dialog = html.match(/<dialog\b[^>]*>([\s\S]*?)<\/dialog>/)?.[1]
    assert.ok(dialog.includes(state.diagnosisTime(started)))
    assert.ok(dialog.includes(state.diagnosisTime(started + 50)))
    assert.ok(dialog.includes(String(started)))
    assert.ok(dialog.includes(String(started + 50)))
    assert.ok(dialog.includes(translate('开始时间戳（毫秒）')))
    assert.ok(dialog.includes(translate('结束时间戳（毫秒）')))
    assert.ok(dialog.includes(translate('诊断总耗时')))
    assert.ok(dialog.includes(translate('缓存节点可能合并多次操作；起止时间覆盖这些操作，耗时仅累计缓存操作本身。')))
    const steps = [...dialog.matchAll(/<dl class="step-timing"[^>]*>([\s\S]*?)<\/dl>/g)].map((match) => match[1])
    assert.equal(steps.length, 2)
    assert.ok(steps[0].includes(state.diagnosisTime(started + 4)))
    assert.ok(steps[0].includes(state.diagnosisTime(started + 12)))
    assert.ok(steps[1].includes(state.diagnosisTime(started + 1)))
    assert.ok(steps[1].includes(state.diagnosisTime(started + 40)))
    assert.doesNotMatch(html, /Invalid Date|NaN/)
  }
})

test('history SSR uses independent authorized options and session SSR hides viewer control buttons', async () => {
  const vue = require('vue')
  const panel = {
    t, api: async () => [], encryptPayload: async () => ({}), showNotice: () => {}, clearNotice: () => {},
    confirmAction: async () => true, promptAction: async () => null,
    settings: { servers: [] }, serverOptions: vue.ref([{ id: 'authorized', name: 'Authorized server' }]),
    canControlSessions: vue.ref(false),
  }
  const historyHtml = await renderPage('HistoryPage.vue', panel)
  assert.ok(historyHtml.includes('Authorized server'))
  const session = { id: 'fixture', server_id: 'authorized', server_name: 'Authorized server', item_name: 'Fixture movie' }
  for (const allowed of [false, true]) {
    panel.canControlSessions.value = allowed
    const html = await renderPage('SessionsPage.vue', panel, (state) => { state.sessions.value = [session] })
    assert.ok(html.includes('Fixture movie'))
    assert.equal(html.includes('aria-label="停止播放"'), allowed)
    assert.equal(html.includes('aria-label="发送消息"'), allowed)
    assert.ok(html.includes('aria-label="查看详情"'))
  }
  const admin = await adminSecurityPage({ role_keys: ['super_admin'], permission_keys: [] })
  assert.deepEqual(admin.state.serverOptions.value, [{ id: 'authorized', name: 'Authorized server' }])
})

test('saving settings invalidates server options and ignores a query from the previous configuration', async (context) => {
  const stale = deferred()
  let optionsReads = 0
  const server = (id) => ({ id, name: id, emby_host: 'http://synthetic.invalid', emby_api_key: 'fixture-key', port: 18096, enabled: true, real_ip_mode: 'auto', real_ip_header: '', trusted_proxy_cidrs: '' })
  const harness = panelHarness(context, 'server', async (url, init) => {
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'owner' }
    if (url === '/api/settings') return { servers: [server(init?.method === 'PUT' ? 'new-server' : 'old-server')], openlist_addr: null, openlist_token: null }
    if (url === '/api/servers/options') {
      optionsReads++
      if (optionsReads === 2) return stale.promise
      const id = optionsReads === 1 ? 'old-server' : 'new-server'
      return { servers: [{ id, name: id }], can_control_sessions: true }
    }
    if (url === '/api/monitoring/proxy-status') return []
    return {}
  })
  await harness.start()
  await harness.panel.ensureServerOptions()
  assert.equal(harness.panel.serverOptions.value[0].id, 'old-server')
  const pendingOldQuery = harness.panel.loadServerOptions()
  harness.panel.settings.servers = [server('new-server')]
  await harness.panel.saveSettings()
  assert.equal(harness.panel.error.value, '')
  assert.deepEqual(harness.panel.serverOptions.value, [])
  await harness.panel.ensureServerOptions()
  assert.equal(harness.panel.serverOptions.value[0].id, 'new-server')
  stale.resolve({ servers: [{ id: 'old-server', name: 'old-server' }], can_control_sessions: false })
  await pendingOldQuery
  assert.equal(harness.panel.serverOptions.value[0].id, 'new-server')
  assert.equal(harness.panel.canControlSessions.value, true)
  assert.equal(optionsReads, 3)
})

test('a late settings GET cannot erase the active sessions options', async (context) => {
  const settings = deferred()
  let optionsReads = 0
  const harness = panelHarness(context, 'server', async (url) => {
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'owner' }
    if (url === '/api/settings') return settings.promise
    if (url === '/api/servers/options') { optionsReads++; return { servers: [{ id: 'a', name: 'A' }], can_control_sessions: true } }
    if (url === '/api/monitoring/proxy-status') return []
    return {}
  })
  const boot = harness.start()
  await tick()
  harness.route.name = 'sessions'
  await tick()
  assert.equal(harness.panel.pageReady.value, true)
  assert.equal(harness.panel.canControlSessions.value, true)
  settings.resolve({ servers: [], openlist_addr: null, openlist_token: null })
  await boot
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'a', name: 'A' }])
  assert.equal(harness.panel.canControlSessions.value, true)
  assert.equal(optionsReads, 1)
})

test('a settings write that completes after navigation reloads the active page options', async (context) => {
  for (const page of ['sessions', 'library-tasks', 'cache-management']) {
    const saved = deferred()
    let optionsReads = 0
    const server = (id) => ({ id, name: id, emby_host: 'http://synthetic.invalid', emby_api_key: 'fixture', port: 18096, enabled: true, real_ip_mode: 'auto', real_ip_header: '', trusted_proxy_cidrs: '' })
    const harness = panelHarness(context, 'server', async (url, init) => {
      if (url === '/api/setup-status') return { initialized: true }
      if (url === '/api/profile') return { username: 'owner' }
      if (url === '/api/settings') return init?.method === 'PUT' ? saved.promise : { servers: [server('a')], openlist_addr: null, openlist_token: null }
      if (url === '/api/servers/options') { optionsReads++; const id = optionsReads === 1 ? 'a' : 'b'; return { servers: [{ id, name: id }], can_control_sessions: true } }
      if (url === '/api/monitoring/proxy-status') return []
      return {}
    })
    await harness.start()
    const save = harness.panel.saveSettings()
    await tick()
    harness.route.name = page
    await tick()
    assert.equal(harness.panel.serverOptions.value[0].id, 'a')
    saved.resolve({ servers: [server('b')], openlist_addr: null, openlist_token: null })
    await save
    await tick()
    assert.equal(harness.panel.serverOptions.value[0].id, 'b')
    assert.equal(harness.panel.canControlSessions.value, true)
    assert.equal(optionsReads, 2)
  }
})

test('authorized options reload on each protected page activation while sessions polling stays data-only', async (context) => {
  let optionsVersion = 0
  const harness = panelHarness(context, 'sessions', async (url) => {
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'fixture' }
    if (url === '/api/servers/options') {
      optionsVersion++
      return { servers: [{ id: `grant-${optionsVersion}`, name: `Grant ${optionsVersion}` }], can_control_sessions: optionsVersion % 2 === 1 }
    }
    if (url === '/api/monitoring/plays') return []
    return []
  })
  await harness.start()
  assert.equal(optionsVersion, 1)
  assert.equal(harness.panel.canControlSessions.value, true)
  for (const page of ['history', 'admin-security', 'sessions', 'library-tasks', 'cache-management', 'history', 'admin-security', 'sessions', 'library-tasks', 'cache-management']) {
    harness.route.name = page
    await tick()
    assert.equal(optionsVersion >= 1, true)
  }
  assert.equal(optionsVersion, 11)
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'grant-11', name: 'Grant 11' }])
  assert.equal(harness.panel.canControlSessions.value, true)

  const { useSessionsController } = load('composables/useSessionsController.ts')
  let pollingReads = 0
  const doc = new EventTarget()
  doc.hidden = false
  const oldDocument = globalThis.document
  globalThis.document = doc
  context.after(() => { globalThis.document = oldDocument })
  const sessions = useSessionsController({
    servers: () => [], canControlSessions: () => false,
    api: async () => { pollingReads++; return [] },
    encryptPayload: async () => ({}), t, confirmAction: async () => true, promptAction: async () => null, showNotice: () => {},
  })
  sessions.startAutoRefresh()
  await tick()
  assert.equal(pollingReads, 1)
  sessions.dispose()
})

test('manual refresh on protected pages awaits options before page data and reports failure', async (context) => {
  const vue = require('vue')
  const results = [
    ['HistoryPage.vue', '/api/history?metric=requests&dimension=server&days=7'],
    ['SessionsPage.vue', '/api/monitoring/plays'],
    ['AdminSecurityPage.vue', '/api/admins'],
    ['LibraryTasksPage.vue', '/api/library/tasks?server_id=all'],
    ['CacheManagementPage.vue', '/api/cache/entries?page=1&limit=100'],
  ]
  for (const [filename, dataPath] of results) {
    const calls = []
    let failOptions = false
    let cancelNextRefresh = false
    const pendingOptions = deferred()
    const panel = {
      t, api: async (url) => {
        calls.push(url)
        if (url === '/api/servers/options') {
          if (failOptions) throw new Error('mock options failure')
          return { servers: [{ id: 'fresh-grant', name: 'Fresh grant' }], can_control_sessions: false }
        }
        if (url === '/api/cache/stats') return {
          canViewGlobalStats: true, canCleanupAll: false,
          stats: { capacity: 0, maxCapacity: 100, unit: 'entries', hits: 0, misses: 0, hitRate: 0 },
        }
        if (url === '/api/admins') return { admins: [], roles: [], current_user_id: null }
        if (url === '/api/admin/tokens') return { tokens: [] }
        if (url === '/api/admin/sessions') return { sessions: [] }
        if (url === '/api/admin/2fa/status') return { enabled: false, pending: false, available: false, login_challenge_available: false, recovery_codes_remaining: 0 }
        return []
      },
      refreshServerOptions: async () => {
        calls.push('/api/servers/options')
        await pendingOptions.promise
        if (cancelNextRefresh) return false
        if (failOptions) throw new Error('mock options failure')
        panel.serverOptions.value = [{ id: 'fresh-grant', name: 'Fresh grant' }]
        panel.canControlSessions.value = false
        return true
      },
      encryptPayload: async () => ({}), showNotice: () => {}, clearNotice: () => {},
      confirmAction: async () => true, promptAction: async () => null,
      settings: { servers: [] }, serverOptions: vue.ref([]), canControlSessions: vue.ref(false), locale: vue.ref('zh-CN'),
    }
    let state
    await renderPage(filename, panel, (value) => { state = value })
    calls.length = 0
    const refreshing = state.refreshPage()
    await tick()
    assert.deepEqual(calls, ['/api/servers/options'], filename)
    assert.equal(state.refreshing.value, true, filename)
    if (filename === 'CacheManagementPage.vue') {
      await state.refreshPage()
      assert.deepEqual(calls, ['/api/servers/options'], 'cache refresh is single-flight while options are pending')
    }
    pendingOptions.resolve()
    await refreshing
    assert.equal(calls[0], '/api/servers/options', filename)
    assert.equal(calls.includes(dataPath), true, filename)
    assert.equal(state.refreshing.value, false)
    if (filename === 'CacheManagementPage.vue') {
      assert.deepEqual(calls, ['/api/servers/options', '/api/cache/stats', dataPath])
      assert.equal(state.canViewGlobalStats.value, true)
      assert.equal(state.stats.value.maxCapacity, 100)
      for (const busy of ['loading', 'cleaning']) {
        calls.length = 0
        state[busy].value = true
        await state.refreshPage()
        assert.deepEqual(calls, [], `cache refresh skips requests during ${busy}`)
        assert.equal(state.refreshing.value, false)
        state[busy].value = false
      }
    }

    cancelNextRefresh = true
    calls.length = 0
    await state.refreshPage()
    assert.deepEqual(calls, ['/api/servers/options'], filename)
    assert.equal(state.refreshing.value, false, filename)
    cancelNextRefresh = false

    failOptions = true
    calls.length = 0
    await state.refreshPage()
    assert.deepEqual(calls, ['/api/servers/options'], filename)
    assert.equal(state.error.value, 'mock options failure', filename)
    assert.equal(panel.canControlSessions.value, false, filename)
    assert.equal(state.refreshing.value, false)
  }
})

test('forced options refresh starts a new generation while an older options request is pending', async (context) => {
  const oldOptions = deferred(), refreshedOptions = deferred()
  let calls = 0
  const harness = panelHarness(context, 'sessions', async (url) => {
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'fixture' }
    if (url === '/api/servers/options') {
      calls++
      if (calls === 1) return { servers: [{ id: 'initial', name: 'Initial' }], can_control_sessions: true }
      if (calls === 2) return oldOptions.promise
      if (calls === 3) return refreshedOptions.promise
    }
    return []
  })
  await harness.start()
  const oldCaller = harness.panel.refreshServerOptions()
  await tick()
  let refreshFinished = false
  const refresh = harness.panel.refreshServerOptions()
  void refresh.then(() => { refreshFinished = true })
  await tick()
  assert.equal(calls, 3)
  assert.equal(refreshFinished, false)

  oldOptions.resolve({ servers: [{ id: 'stale', name: 'Stale' }], can_control_sessions: true })
  assert.equal(await oldCaller, false)
  await tick()
  assert.equal(refreshFinished, false)
  assert.deepEqual(harness.panel.serverOptions.value, [])
  assert.equal(harness.panel.canControlSessions.value, false)

  refreshedOptions.resolve({ servers: [{ id: 'fresh', name: 'Fresh' }], can_control_sessions: false })
  assert.equal(await refresh, true)
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'fresh', name: 'Fresh' }])
  assert.equal(harness.panel.canControlSessions.value, false)
})

test('current page activation joins options refresh superseded by a settings save response', async (context) => {
  const firstOptions = deferred(), saveResponse = deferred()
  const calls = []
  let optionsCalls = 0
  const harness = panelHarness(context, 'home', async (url, init) => {
    calls.push([url, init?.method])
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'fixture' }
    if (url === '/api/settings' && init?.method === 'PUT') return saveResponse.promise
    if (url === '/api/settings') return { servers: [] }
    if (url === '/api/servers/options') {
      optionsCalls++
      return optionsCalls === 1 ? firstOptions.promise : { servers: [{ id: 'latest', name: 'Latest' }], can_control_sessions: false }
    }
    if (url === '/api/monitoring/proxy-status') return []
    return []
  })
  await harness.start()
  calls.length = 0
  harness.route.name = 'sessions'
  await tick()
  await tick()
  assert.equal(optionsCalls, 1)
  assert.equal(harness.panel.pageReady.value, false)

  const saving = harness.panel.saveSettings()
  await tick()
  saveResponse.resolve({ servers: [] })
  await saving
  await tick()
  assert.equal(optionsCalls, 2)
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'latest', name: 'Latest' }])

  firstOptions.resolve({ servers: [{ id: 'stale', name: 'Stale' }], can_control_sessions: true })
  await tick()
  await tick()
  assert.equal(harness.panel.pageReady.value, true)
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'latest', name: 'Latest' }])
  assert.equal(calls.some(([url]) => url === '/api/monitoring/plays'), false)
})

test('forced options refresh clears stale control permission, retries failures, and rejects previous login responses', async (context) => {
  const oldOptions = deferred()
  let calls = 0
  const harness = panelHarness(context, 'sessions', async (url) => {
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return { username: 'fixture' }
    if (url === '/api/servers/options') {
      calls++
      if (calls === 1) return { servers: [{ id: 'old', name: 'Old' }], can_control_sessions: true }
      if (calls === 2) return oldOptions.promise
      if (calls === 3) return { servers: [{ id: 'new-login', name: 'New login' }], can_control_sessions: false }
      if (calls === 4) throw new Error('mock refresh failure')
      return { servers: [{ id: 'new', name: 'New' }], can_control_sessions: false }
    }
    if (url === '/api/login') return { token: 'new-fixture-token' }
    return []
  })
  await harness.start()
  const stale = harness.panel.refreshServerOptions()
  await tick()
  assert.equal(harness.panel.canControlSessions.value, false)
  await harness.panel.login()
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'new-login', name: 'New login' }])
  oldOptions.resolve({ servers: [{ id: 'stale', name: 'Stale' }], can_control_sessions: true })
  await stale
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'new-login', name: 'New login' }])
  assert.equal(harness.panel.canControlSessions.value, false)
  await assert.rejects(harness.panel.refreshServerOptions(), /mock refresh failure/)
  assert.deepEqual(harness.panel.serverOptions.value, [])
  await harness.panel.refreshServerOptions()
  assert.deepEqual(harness.panel.serverOptions.value, [{ id: 'new', name: 'New' }])
  assert.equal(harness.panel.canControlSessions.value, false)
})

test('cache SSR shows authorized names and original entry IDs; renames stay reactive and cleanup keeps server IDs', async () => {
  const vue = require('vue')
  const entries = [
    { id: 'a1'.repeat(32), serverId: 'server-cn', mediaId: 'media-original', sourceId: 'source-original', legacy: false },
    { id: 'blank-name-hash', serverId: 'blank-server', mediaId: 'media-blank', sourceId: 'source-blank', legacy: false },
    { id: 'empty-name-hash', serverId: 'empty-server', mediaId: 'media-empty', sourceId: 'source-empty', legacy: false },
    { id: 'unknown-hash', serverId: 'unknown-server', mediaId: 'media-unknown', sourceId: 'source-unknown', legacy: false },
    { id: 'empty-id-hash', serverId: '', mediaId: '', sourceId: '', legacy: true },
  ]
  const originalEntries = structuredClone(entries)
  const requests = []
  const panel = {
    t, showNotice: () => {}, confirmAction: async () => true,
    settings: { servers: [{ id: 'server-cn', name: '不应读取全局配置名称' }] },
    serverOptions: vue.ref([
      { id: 'server-cn', name: '  家庭影音  ' },
      { id: 'blank-server', name: ' \t ' },
      { id: 'empty-server', name: '' },
    ]),
    api: async (url, init) => {
      requests.push({ url, init })
      if (url === '/api/cache/stats') return { canViewGlobalStats: false, canCleanupAll: false }
      if (url === '/api/cache/entries?page=1&limit=100') return entries
      if (url === '/api/cache/cleanup') return { ok: true, deleted: 1, scope: 'server' }
      assert.fail(`unexpected request: ${url}`)
    },
  }
  let state
  const html = await renderPage('CacheManagementPage.vue', panel, (value) => {
    state = value
    state.entries.value = structuredClone(entries)
  })
  assert.deepEqual(requests, [], 'SSR must not fetch options or cache data')
  assert.match(html, /<th[^>]*title="用于区分缓存记录，媒体请查看媒体 ID。"[^>]*>缓存条目 ID<\/th>/)
  const rows = [...html.matchAll(/<tbody[^>]*>([\s\S]*?)<\/tbody>/g)][0]?.[1]
  assert.ok(rows, 'cache table body must render')
  const renderedRows = [...rows.matchAll(/<tr[^>]*>([\s\S]*?)<\/tr>/g)]
  assert.equal(renderedRows.length, entries.length)
  const names = ['家庭影音', 'blank-server', 'empty-server', 'unknown-server', '—']
  for (const [index, entry] of entries.entries()) {
    const cells = [...renderedRows[index][1].matchAll(/<td([^>]*)>([\s\S]*?)<\/td>/g)]
    assert.equal(cells.length, 5)
    assert.equal(cells[1][2].trim(), names[index])
    if (entry.serverId) assert.ok(cells[1][1].includes(`title="${entry.serverId}"`))
    assert.ok(cells[0][2].includes(`<code title="${entry.id}">${entry.id}</code>`))
    assert.equal(cells[2][2].trim(), entry.mediaId || '—')
    assert.equal(cells[3][2].trim(), entry.sourceId || '—')
  }
  assert.equal(html.includes('不应读取全局配置名称'), false)

  const displayedName = vue.computed(() => state.serverName('server-cn'))
  assert.equal(displayedName.value, '家庭影音')
  panel.serverOptions.value[0].name = '  客厅影视  '
  assert.equal(displayedName.value, '客厅影视')
  panel.serverOptions.value = [{ id: 'server-cn', name: '家庭影院新名称' }]
  assert.equal(displayedName.value, '家庭影院新名称')
  assert.equal(state.serverName('unknown-server'), 'unknown-server')
  assert.equal(state.serverName(''), '—')
  assert.deepEqual(state.entries.value, originalEntries)

  state.serverId.value = 'server-cn'
  await state.cleanupByServer()
  const cleanup = requests.filter(({ url }) => url === '/api/cache/cleanup')
  assert.equal(cleanup.length, 1)
  assert.equal(cleanup[0].init.method, 'POST')
  assert.deepEqual(JSON.parse(cleanup[0].init.body), { scope: 'server', serverId: 'server-cn' })
  assert.deepEqual(state.entries.value, originalEntries)
  assert.deepEqual(entries, originalEntries)
  assert.equal(displayedName.value, '家庭影院新名称')
  state.dispose()
})

test('cache mount refreshes only data and disposal during options refresh prevents later data requests', async () => {
  const vue = require('vue')
  const mounted = [], unmounted = [], calls = []
  const options = deferred()
  const panel = {
    t, showNotice: () => {}, confirmAction: async () => true, serverOptions: vue.ref([]),
    refreshServerOptions: async () => { calls.push('/api/servers/options'); return options.promise },
    api: async (url) => {
      calls.push(url)
      if (url === '/api/cache/stats') return { canViewGlobalStats: false, canCleanupAll: false }
      if (url === '/api/cache/entries?page=1&limit=100') return []
      assert.fail(`unexpected request: ${url}`)
    },
  }
  let state
  await renderPage('CacheManagementPage.vue', panel, (value) => { state = value }, {
    vue: { ...vue, onMounted: (fn) => mounted.push(fn), onBeforeUnmount: (fn) => unmounted.push(fn) },
  })
  assert.equal(mounted.length, 1)
  mounted[0]()
  await tick()
  assert.deepEqual(calls, ['/api/cache/stats', '/api/cache/entries?page=1&limit=100'])
  calls.length = 0
  const refreshing = state.refreshPage()
  assert.deepEqual(calls, ['/api/servers/options'])
  assert.equal(state.refreshing.value, true)
  assert.equal(unmounted.length, 1)
  unmounted[0]()
  options.resolve(true)
  await refreshing
  assert.deepEqual(calls, ['/api/servers/options'])
  assert.equal(state.refreshing.value, false)
  await state.refreshPage()
  assert.deepEqual(calls, ['/api/servers/options'])
})

test('cache all cleanup follows its independent capability, not global stats visibility', async () => {
  const page = readFileSync(path.resolve(__dirname, '../src/pages/CacheManagementPage.vue'), 'utf8')
  const allCleanupClick = page.indexOf('@click="cleanupAll"')
  const allButton = page.slice(page.lastIndexOf('<button', allCleanupClick), page.indexOf('</button>', allCleanupClick))
  assert.match(allButton, /v-if="canCleanupAll"/)

  const { useCacheManagementController } = load('composables/useCacheManagementController.ts')
  const requests = []
  const c = useCacheManagementController({
    api: async (url, init) => {
      requests.push({ url, init })
      if (url === '/api/cache/stats') return {
        canViewGlobalStats: true,
        canCleanupAll: false,
        stats: { capacity: 1, maxCapacity: 2, unit: 'entries', hits: 1, misses: 0, hitRate: 1 },
      }
      if (url.startsWith('/api/cache/entries?')) return []
      return { ok: true, deleted: 1 }
    },
    translate: t,
    showNotice: () => {},
    confirmAction: async () => { throw new Error('all cleanup must not open confirmation') },
  })
  await c.refresh()
  await c.cleanupAll()
  assert.equal(c.canViewGlobalStats.value, true)
  assert.equal(c.canCleanupAll.value, false)
  assert.equal(c.error.value, '无权限')
  assert.equal(requests.some(({ url }) => url === '/api/cache/cleanup'), false)
})

test('global API-token cleanup capability works while global stats remain hidden', async () => {
  const { useCacheManagementController } = load('composables/useCacheManagementController.ts')
  const requests = []
  const c = useCacheManagementController({
    api: async (url, init) => {
      requests.push({ url, init })
      if (url === '/api/cache/stats') {
        return {
          canViewGlobalStats: false,
          canCleanupAll: true,
        }
      }
      if (url.startsWith('/api/cache/entries?')) return []
      return { ok: true, deleted: 1, scope: 'all' }
    },
    translate: t,
    showNotice: () => {},
    confirmAction: async () => true,
  })
  await c.refresh()
  assert.equal(c.canViewGlobalStats.value, false)
  assert.equal(c.canCleanupAll.value, true)
  await c.cleanupAll()
  const cleanupRequest = requests.find(({ url }) => url === '/api/cache/cleanup')
  assert.ok(cleanupRequest)
  assert.equal(cleanupRequest.init.method, 'POST')
  assert.deepEqual(JSON.parse(cleanupRequest.init.body), { scope: 'all' })
})

test('cache entries failure clears previously loaded entries and missing cleanup capability defaults false', async () => {
  let failEntries = false
  const { useCacheManagementController } = load('composables/useCacheManagementController.ts')
  const c = useCacheManagementController({
    api: async (url) => {
      if (url === '/api/cache/stats') return {
        canViewGlobalStats: true,
        stats: { capacity: 1, maxCapacity: 2, unit: 'entries', hits: 1, misses: 0, hitRate: 1 },
      }
      if (url.startsWith('/api/cache/entries?')) {
        if (failEntries) throw new Error('mock entries failure')
        return [{ id: 'entry-1', serverId: 's1', mediaId: 'm1', sourceId: 'src' }]
      }
      return []
    },
    translate: t,
    showNotice: () => {},
    confirmAction: async () => true,
  })
  await c.refresh()
  assert.equal(c.entries.value.length, 1)
  assert.equal(c.canCleanupAll.value, false)
  failEntries = true
  await c.refresh()
  assert.deepEqual(c.entries.value, [])
  assert.equal(c.hasNextPage.value, false)
  assert.equal(c.canViewGlobalStats.value, true)
})

// Diagnostics extension: only synthetic display data; no live requests or stored records.
function diagnosticsHarness(api, extra = {}) {
  return load('composables/useDiagnosticsController.ts').useDiagnosticsController({
    api, translate: t, showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false, ...extra,
  })
}

function diagnosticFixture(extra = {}) {
  return {
    request_id: 'diag-synthetic-correlation', server_id: 's-fixture', media_id: 'm-fixture',
    ua: 'synthetic-agent', cache_hit: false, failure_node: null, sanitized_errors: [], steps: [], ...extra,
  }
}

test('diagnostics serialize all extended filters, local times and zero minimum while retaining manual pagination', async () => {
  const requests = []
  const c = diagnosticsHarness(async (url) => { requests.push(url); return { records: [], can_global_cleanup: false } })
  const filters = {
    outcome: 'proxy', user: ' fixture user ', keyword: ' safe keyword ',
    since: '2026-01-02T03:04:05.123', until: '2026-01-03T04:05:06.456', min_duration_ms: 0,
  }
  for (const [key, value] of Object.entries(filters)) {
    c.query.value.page = 7
    c.query.value[key] = value
    assert.equal(c.query.value.page, 1, `${key} resets the page`)
  }
  c.query.value.page = 3
  c.query.value.limit = 100
  c.query.value.page = 3
  await c.applyFilters()
  const params = new URL(requests.at(-1), 'http://fixture.invalid').searchParams
  assert.equal(params.get('page'), '3')
  assert.equal(params.get('limit'), '100')
  assert.equal(params.get('outcome'), 'proxy')
  assert.equal(params.get('user'), 'fixture user')
  assert.equal(params.get('keyword'), 'safe keyword')
  assert.equal(params.get('min_duration_ms'), '0')
  assert.equal(params.get('since_ms'), String(new Date(filters.since).getTime()))
  assert.equal(params.get('until_ms'), String(new Date(filters.until).getTime()))
  assert.equal(params.has('since'), false)
  await c.resetFilters()
  const reset = new URL(requests.at(-1), 'http://fixture.invalid').searchParams
  assert.deepEqual([...reset], [['limit', '50'], ['page', '1']])
  for (const limit of [1000, 1.8, -1, NaN]) {
    c.query.value.limit = limit
    await c.loadDiagnostics()
    assert.equal(c.query.value.limit, Number.isNaN(limit) ? 50 : Math.max(1, Math.min(100, Math.trunc(limit))))
  }
  c.dispose()
})

test('diagnostics reject invalid date ranges and minimum durations before issuing any request', async () => {
  let requests = 0
  const { translations } = load('lib/translations.ts')
  const c = diagnosticsHarness(async () => { requests++; return {} }, { translate: (s) => translations[s] || s })
  for (const [filters, expected] of [
    [{ since: 'not a date' }, 'Invalid diagnostic filter time'],
    [{ since: '2026-01-03T00:00', until: '2026-01-02T00:00' }, 'Diagnostic start time cannot be after end time'],
    [{ min_duration_ms: -1 }, 'Minimum duration must be a nonnegative integer'],
    [{ min_duration_ms: 0.5 }, 'Minimum duration must be a nonnegative integer'],
  ]) {
    Object.assign(c.query.value, { since: '', until: '', min_duration_ms: '', ...filters })
    await c.loadDiagnostics()
    assert.equal(c.error.value, expected)
    assert.equal(c.loading.value, false)
    assert.equal(c.total.value, null)
    assert.equal(c.storage.value, null)
  }
  assert.equal(requests, 0)
  c.dispose()
})

test('diagnostics respect authoritative totals, has_more and storage without fabricating metadata for legacy responses', async () => {
  let response = {
    records: [diagnosticFixture()], total: 123, has_more: false, can_global_cleanup: true,
    storage: { persistent: false, retention_days: 7, max_records: 10000, memory_capacity: 1000 },
  }
  const c = diagnosticsHarness(async () => response)
  c.query.value.limit = 1
  await c.loadDiagnostics()
  assert.equal(c.total.value, 123)
  assert.equal(c.hasNextPage.value, false, 'not inferred from a full page when authoritative false is present')
  assert.equal(c.storage.value.persistent, false)
  assert.equal(c.canGlobalCleanup.value, true)
  assert.equal(c.loadedPage.value, 1)
  c.query.value.page = 9
  assert.equal(c.loadedPage.value, 1, 'unsubmitted page input cannot relabel loaded exports')
  response = { records: [diagnosticFixture()], can_global_cleanup: false }
  await c.loadDiagnostics()
  assert.equal(c.loadedPage.value, 9)
  assert.equal(c.total.value, null)
  assert.equal(c.storage.value, null)
  assert.equal(c.hasNextPage.value, true, 'legacy full-page fallback only')
  response = { records: [], total: -1, has_more: true, storage: { persistent: true }, can_global_cleanup: 'true' }
  await c.loadDiagnostics()
  assert.equal(c.total.value, null)
  assert.equal(c.storage.value, null)
  assert.equal(c.canGlobalCleanup.value, false)
  assert.equal(c.hasNextPage.value, true)
  response = []
  await c.loadDiagnostics()
  assert.deepEqual(c.diagnostics.value, [])
  assert.equal(c.total.value, null)
  assert.equal(c.storage.value, null)
  c.dispose()
})

test('diagnostics cancel stale loads and dispose without stale records, metadata, errors or notices', async () => {
  const requests = [], notices = []
  const c = diagnosticsHarness((url, init) => {
    const pending = deferred(); requests.push({ ...pending, signal: init.signal }); return pending.promise
  }, { showNotice: (s) => notices.push(s) })
  const stale = c.loadDiagnostics(), latest = c.loadDiagnostics()
  assert.equal(requests[0].signal.aborted, true)
  requests[0].reject(new Error('stale failure'))
  await stale
  assert.equal(c.loading.value, true)
  assert.equal(c.error.value, '')
  requests[1].resolve({ records: [diagnosticFixture()], total: 1, has_more: false, can_global_cleanup: true })
  await latest
  const late = c.loadDiagnostics()
  c.dispose()
  assert.equal(requests[2].signal.aborted, true)
  requests[2].resolve({ records: [diagnosticFixture({ request_id: 'late' })], total: 900, can_global_cleanup: true })
  await late
  assert.equal(c.diagnostics.value[0].request_id, 'diag-synthetic-correlation')
  assert.equal(c.total.value, 1)
  assert.equal(c.canGlobalCleanup.value, false)
  await c.loadDiagnostics()
  assert.equal(requests.length, 3)
  assert.deepEqual(notices, [])
})

test('diagnostics cleanup preserves success notice, rechecks capability, and cancels post-dispose continuation', async () => {
  const notices = [], clears = [], calls = []
  let confirm = true
  const c = diagnosticsHarness(async (url, init) => {
    calls.push({ url, init })
    return url.endsWith('/clean') ? null : { records: [], total: 0, has_more: false, can_global_cleanup: true }
  }, { confirmAction: async () => confirm, showNotice: (s) => notices.push(s), clearNotice: () => clears.push(true) })
  await c.cleanDiagnostics()
  assert.equal(calls.length, 0)
  await c.loadDiagnostics()
  confirm = false
  await c.cleanDiagnostics()
  assert.equal(calls.length, 1)
  confirm = true
  await c.cleanDiagnostics()
  assert.equal(calls[1].init.method, 'POST')
  assert.deepEqual(JSON.parse(calls[1].init.body), { confirm: 'yes' })
  assert.deepEqual(notices, ['诊断记录已清空'])
  assert.equal(clears.length, 1, 'clean refresh must preserve success notice')
  c.dispose()

  const confirmation = deferred(), cleanup = deferred()
  const d = diagnosticsHarness(async (url) => url.endsWith('/clean') ? cleanup.promise : { records: [], can_global_cleanup: true }, {
    confirmAction: () => confirmation.promise, showNotice: () => assert.fail('no post-dispose success notice'),
  })
  await d.loadDiagnostics()
  const clean = d.cleanDiagnostics()
  d.canGlobalCleanup.value = false
  confirmation.resolve(true)
  await clean
  assert.equal(d.cleaning.value, false, 'capability rechecked after dialog')
  d.canGlobalCleanup.value = true
  const pending = d.cleanDiagnostics()
  await tick()
  d.dispose()
  cleanup.resolve(null)
  await pending
})

test('diagnostics share whitelist removes personal data, URLs, arbitrary free text and filter context', () => {
  const { diagnosisShareRecord } = load('composables/useDiagnosticsController.ts')
  const record = diagnosticFixture({
    ua: 'private-user private-device 192.0.2.42', server_id: 'private-server', media_id: 'private-media',
    context: { user_name: 'private-user', device_id: 'private-device', device_name: 'private-name', source_ip: '192.0.2.42', request_path: '/private-user/file' },
    result: { outcome: 'redirect', status_code: 302, target_host: 'private-host.invalid', reason: 'private-user', error_kind: 'private-error' },
    cache: { status: 'hit', stored: false, ttl_seconds: 100, remaining_ttl_ms: 0, reason: 'private-device', target_host: 'private-host.invalid' },
    paths: { original: '/private-user', mapped: '/private-device', final_url: 'https://private-host.invalid/file' },
    timing: { slowest_step: 'private-user', slowest_step_ms: 0, unaccounted_ms: 0, name_resolution_ms: 0, connection_setup_ms: 0 },
    sanitized_errors: ['private-user 192.0.2.42'],
    steps: [{ name: 'OpenList', state: 'recorded', details: 'private-user', reason: 'private-device', duration_ms: 0, response_headers_ms: 0 },
      { name: 'private-step', state: 'private-state', details: 'private-detail', duration_ms: 3 }],
  })
  const safe = diagnosisShareRecord(record), json = JSON.stringify(safe)
  assert.doesNotMatch(json, /private-|192\.0\.2\.42|https:|context|paths|sanitized_errors|server_id|media_id|error_kind|target_host/)
  assert.equal(safe.request_id, record.request_id)
  assert.ok(!('dns_ms' in safe.timing) && !('tcp_ms' in safe.timing) && !('tls_ms' in safe.timing))
  assert.equal(diagnosisShareRecord({ ...record, request_id: 'arbitrary identifier' }).request_id, null)
  assert.equal(safe.result.status_code, 302)
  assert.equal(safe.cache.stored, false)
  assert.equal(safe.cache.remaining_ttl_ms, 0)
  assert.equal(safe.timing.name_resolution_ms, 0)
  assert.equal(safe.steps[0].response_headers_ms, 0)
  assert.equal(safe.steps[1].name, null)
  assert.equal(safe.steps[1].state, null)
  const observedNames = ['请求上下文', '播放策略', 'Emby Proxy']
  assert.deepEqual(diagnosisShareRecord({ ...record, steps: observedNames.map((name) => ({ name, duration_ms: 0, details: '' })) }).steps.map((step) => step.name), observedNames)
  assert.equal(record.context.user_name, 'private-user', 'sharing never mutates admin source data')
})

test('diagnostics network share fields accept only numeric metrics and fixed observation states without mutating details', () => {
  const { diagnosisShareRecord } = load('composables/useDiagnosticsController.ts')
  const metricKeys = ['name_resolution_ms', 'name_resolution_calls', 'connection_setup_ms', 'connection_attempts']
  for (const value of [0, 7, null, undefined, -1, 0.5, NaN, Infinity, Number.MAX_SAFE_INTEGER + 1, 'private-value', {}]) {
    const network = Object.fromEntries(metricKeys.map((key) => [key, value]))
    network.connection_observation = 'private-observation'
    network.host = 'private-host.invalid'
    network.headers = { token: 'private-token' }
    network.query = 'private-query'
    const record = diagnosticFixture({ timing: network, context: { user_name: 'private-user' },
      steps: [{ name: 'OpenList', state: 'recorded', duration_ms: 0, details: 'private-detail', ...network }] })
    const safe = diagnosisShareRecord(record)
    for (const target of [safe.timing, safe.steps[0]]) {
      for (const key of metricKeys) assert.equal(target[key], value === 0 || value === 7 ? value : null, key)
      assert.equal(target.connection_observation, null)
    }
    assert.doesNotMatch(JSON.stringify(safe), /private-|"headers"|"query"|"user_name"/)
    assert.equal(record.timing.host, 'private-host.invalid')
    assert.equal(record.steps[0].details, 'private-detail', 'authorized source text is not changed by export')
  }
  for (const observation of ['not_observed', 'observed', 'partial', null, undefined, 'unknown', 'private-host.invalid']) {
    const network = { connection_observation: observation }
    const safe = diagnosisShareRecord(diagnosticFixture({ timing: network,
      steps: [{ name: 'OpenList', duration_ms: 0, details: '', ...network }] }))
    const expected = ['not_observed', 'observed', 'partial'].includes(observation) ? observation : null
    assert.equal(safe.timing.connection_observation, expected)
    assert.equal(safe.steps[0].connection_observation, expected)
  }
  const legacy = diagnosisShareRecord(diagnosticFixture({ steps: [{ name: 'OpenList', duration_ms: 0, details: '' }] }))
  for (const target of [legacy.timing, legacy.steps[0]]) {
    for (const key of [...metricKeys, 'connection_observation']) assert.equal(target[key], null, `legacy ${key}`)
  }
})

test('diagnostics network observations render independent totals, attempts, nulls and legacy unknowns in both locales', async (context) => {
  const vue = require('vue'), { translations } = load('lib/translations.ts')
  const labels = ['名称解析调用耗时', '名称解析调用次数', '连接建立尝试合计耗时', '连接建立尝试次数', '连接观测状态']
  const help = [
    '名称解析调用耗时是系统名称解析 API 的等待时间，包含排队和操作系统 resolver 等开销，不是线上 DNS 报文耗时。',
    '连接建立尝试合计耗时可能包含代理、CONNECT 和 TLS，不是纯 TCP/TLS 耗时；受 hyper 连接池归因限制，观测到的 attempt 可能不能归因于当前 HTTP 响应，也不能证明该响应使用了该连接。',
    '名称解析与连接建立尝试耗时可能重叠，不可相加，也不额外计入诊断总耗时或未归属耗时。未归属耗时是未被操作区间覆盖的诊断时间，不等于网络耗时。无 resolver 或连接事件不代表连接复用成功；0 是实际观测值，空值为未采集，缺失观测状态为未知。',
  ]
  const fixtures = [
    [{ name_resolution_ms: 0, name_resolution_calls: 1, connection_setup_ms: 0, connection_attempts: 1, connection_observation: 'observed' },
      ['0 ms', '1', '0 ms', '1', '已观测到连接尝试']],
    [{ name_resolution_ms: 7, name_resolution_calls: 2, connection_setup_ms: 30, connection_attempts: 3, connection_observation: 'partial' },
      ['7 ms', '2', '30 ms', '3', '连接尝试观测不完整']],
    [{ name_resolution_ms: null, name_resolution_calls: 0, connection_setup_ms: null, connection_attempts: 0, connection_observation: 'not_observed' },
      ['未采集', '0', '未采集', '0', '未观测到连接尝试']],
    [{ name_resolution_ms: null, name_resolution_calls: null, connection_setup_ms: null, connection_attempts: null, connection_observation: null },
      ['未采集', '未采集', '未采集', '未采集', '未知（旧数据或未提供）']],
    [{}, ['未采集', '未采集', '未采集', '未采集', '未知（旧数据或未提供）']],
  ]
  context.mock.method(globalThis, 'fetch', () => assert.fail('Synthetic network observations must not make HTTP requests'))
  for (const locale of ['zh-CN', 'en-US']) {
    const translate = (key) => locale === 'en-US' ? translations[key] || key : key
    for (const [index, [network, expected]] of fixtures.entries()) {
      // Step data differs from aggregate data to catch accidental use of the other scope.
      const [stepNetwork, stepExpected] = fixtures[(index + 1) % fixtures.length]
      const record = diagnosticFixture({ duration_ms: 20, timing: { slowest_step_ms: 10, unaccounted_ms: 4, ...network },
        context: { server_name: 'Synthetic server', media_name: 'Synthetic media', user_name: 'fixture-private-user' },
        steps: [{ name: 'OpenList', duration_ms: 10, state: 'recorded', details: 'fixture-private-detail', ...stepNetwork }] })
      const before = JSON.stringify(record)
      const c = diagnosticsHarness(async () => ({ records: [record], can_global_cleanup: false }))
      await c.loadDiagnostics()
      assert.deepEqual(JSON.parse(JSON.stringify(c.diagnostics.value[0])), record, 'controller retains DTO observations as received')
      let state
      const html = await renderPage('DiagnosticsPage.vue', {
        locale: vue.ref(locale), t: translate, api: async () => assert.fail('display only'),
        showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
      }, (s) => { state = s; s.diagnostics.value = c.diagnostics.value; s.openDetail(c.diagnostics.value[0]) }, {
        '@/components/ui/SettingsDialogShell.vue': { props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null },
      })
      const dialogStart = html.indexOf('<dialog')
      const dialogEnd = html.indexOf('</dialog>', dialogStart)
      const dialog = dialogStart >= 0 && dialogEnd >= 0
        ? html.slice(html.indexOf('>', dialogStart) + 1, dialogEnd)
        : undefined
      assert.ok(dialog, `${locale}: detail dialog`)
      const summary = dialog.split('<ol class="detail-timeline">')[0]
      const observationsStart = dialog.indexOf('<dl class="step-observations">')
      const observationsEnd = dialog.indexOf('</dl>', observationsStart)
      const observations = observationsStart >= 0 && observationsEnd >= 0
        ? dialog.slice(dialog.indexOf('>', observationsStart) + 1, observationsEnd)
        : undefined
      assert.ok(observations, `${locale}: step observations`)
      for (const [position, label] of labels.entries()) {
        assert.ok(translations[label], `${label}: English label exists`)
        const expectedRow = `<dt>${translate(label)}</dt><dd>${translate(expected[position])}</dd>`
        assert.ok(summary.includes(expectedRow), `${locale}/${index}: aggregate ${expectedRow}`)
        const stepRow = `<dt>${translate(label)}</dt><dd>${translate(stepExpected[position])}</dd>`
        assert.ok(observations.includes(stepRow), `${locale}/${index}: step ${stepRow}`)
      }
      for (const key of help) {
        assert.ok(translations[key], 'network measurement caveat has English translation')
        assert.ok(summary.includes(translate(key)), `${locale}: measurement caveat remains adjacent to aggregate metrics`)
      }
      for (const label of ['DNS 耗时', 'TCP 耗时', 'TLS 耗时']) {
        assert.ok(!summary.includes(`<dt>${translate(label)}</dt>`))
        assert.ok(!observations.includes(`<dt>${translate(label)}</dt>`))
      }
      assert.ok(summary.includes(`<dt>${translate('未归属耗时')}</dt><dd>4 ms</dd>`), 'overlapping observations never change unaccounted time')
      assert.ok(summary.includes(`<dt>${translate('诊断总耗时')}</dt><dd>20 ms</dd>`), 'overlapping observations never change trace duration')
      assert.ok(summary.includes('Synthetic server') && summary.includes('Synthetic media') && summary.includes('fixture-private-user'))
      assert.equal(state.connectionObservationLabel('unrecognized'), translate('未知（旧数据或未提供）'))
      assert.equal(JSON.stringify(record), before, 'rendering does not mutate the source observations')
      assert.doesNotMatch(html, /NaN|undefined|Invalid Date/)
      c.dispose()
    }
  }
})

test('diagnostics modern summary and authorized details show real context, outcomes and unknown network timing in both locales', async () => {
  const vue = require('vue'), { translations } = load('lib/translations.ts')
  const record = diagnosticFixture({
    started_at_ms: Date.UTC(2026, 0, 2), finished_at_ms: Date.UTC(2026, 0, 2) + 10, duration_ms: 10,
    context: { server_name: 'Synthetic server label', media_name: 'Synthetic media label', series_name: 'Synthetic series', season_number: 0, episode_number: 1,
      user_name: 'fixture-private-user', client_name: 'Synthetic client', device_name: 'fixture-private-name', device_id: 'fixture-private-device', source_ip: '192.0.2.42',
      method: 'GET', request_path: '/fixture-private-user/path', range: 'bytes=0-' },
    result: { outcome: 'redirect', status_code: 302, reason: 'Synthetic decision', upstream_status: 200 },
    cache: { status: 'disabled', stored: false, remaining_ttl_ms: 0 },
    paths: { original: '/fixture-private-user/input', mapped: '/safe/output', matched_rules: [0, 2], final_url: 'https://fixture.invalid/safe' },
    timing: { unaccounted_ms: 0, slowest_step: 'OpenList', slowest_step_ms: 10, redirect_count: 0 },
    steps: [{ name: 'OpenList', state: 'skipped', details: 'fixture-private-user fixture-private-device 192.0.2.42', reason: 'Synthetic skip reason', duration_ms: 0,
      response_headers_ms: 0, response_body_ms: null, upstream_status: 200, error_kind: 'Synthetic error kind' }],
  })
  for (const locale of ['zh-CN', 'en-US']) {
    const translate = (s) => locale === 'en-US' ? translations[s] || s : s
    let state
    const html = await renderPage('DiagnosticsPage.vue', {
      locale: vue.ref(locale), t: translate, api: async () => assert.fail('display only'), copyText: async () => {},
      showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
    }, (s) => { state = s; s.diagnostics.value = [record]; s.openDetail(record) }, {
      '@/components/ui/SettingsDialogShell.vue': { props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null },
    })
    const article = html.match(/<article\b[^>]*>([\s\S]*?)<\/article>/)[1]
    assert.match(article, /Synthetic server label/)
    assert.match(article, /Synthetic media label/)
    assert.match(article, /fixture-private-user/)
    assert.match(article, /Synthetic client/)
    assert.ok(article.includes(translate('已返回跳转')))
    assert.ok(article.includes(translate('缓存已禁用')))
    assert.doesNotMatch(article, /Synthetic decision|Synthetic skip reason|fixture-private-device|192\.0\.2\.42/)
    const dialog = html.match(/<dialog\b[^>]*>([\s\S]*?)<\/dialog>/)[1]
    for (const value of ['fixture-private-user', 'fixture-private-device', 'fixture-private-name', '192.0.2.42']) {
      assert.ok(dialog.includes(value), 'authorized details retain troubleshooting context')
    }
    for (const key of ['基本请求与用户', '处理结果与选择原因', '路径变换', '耗时分布', '未采集']) {
      assert.ok(translations[key], `${key} has English mapping`)
      assert.ok(dialog.includes(translate(key)), key)
    }
    assert.doesNotMatch(dialog, /DNS duration|TCP duration|TLS duration/)
    assert.match(dialog, /0, 2/)
    assert.match(dialog, /Synthetic decision/)
    assert.match(dialog, /Synthetic skip reason/)
    assert.match(dialog, /0 ms/)
    assert.equal(state.stepState(record, record.steps[0]), 'skipped')
    assert.equal(state.cacheLabel(diagnosticFixture()), translate('未采集'), 'legacy false does not prove a cache miss')
    assert.equal(state.outcomeLabel(diagnosticFixture()), translate('未采集'), 'legacy success must not be invented')
  }
})

test('diagnostics copy uses shared clipboard and current-page download revokes URLs even when click fails', async (context) => {
  const vue = require('vue'), copies = []
  const network = { name_resolution_ms: 0, name_resolution_calls: 1, connection_setup_ms: 12,
    connection_attempts: 2, connection_observation: 'partial' }
  const record = diagnosticFixture({ context: { user_name: 'fixture-private-user' }, timing: network,
    steps: [{ name: 'OpenList', duration_ms: 20, details: 'fixture-private-details', ...network }] })
  let state
  await renderPage('DiagnosticsPage.vue', {
    locale: vue.ref('zh-CN'), t, api: async () => assert.fail('no network'), copyText: async (text) => copies.push(text),
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  }, (s) => { state = s; s.diagnostics.value = [record]; s.loadedPage.value = 2 })
  await state.copyRecord(record)
  const copied = JSON.parse(copies[0])
  assert.equal(copied.request_id, record.request_id)
  for (const [key, value] of Object.entries(network)) {
    assert.equal(copied.timing[key], value, `copy timing ${key}`)
    assert.equal(copied.steps[0][key], value, `copy step ${key}`)
  }
  assert.doesNotMatch(copies[0], /fixture-private-|context/)
  state.query.value.page = 99
  const payload = JSON.parse(state.sharePayload([record]))
  assert.equal(payload.page, 2)
  assert.equal(payload.scope, 'current_page')
  assert.equal(payload.records.length, 1)
  assert.equal(payload.redaction, 'share_allowlist_v1')
  assert.deepEqual(payload.records[0], copied, 'page export and single-record copy use the same network allowlist')
  const oldDocument = globalThis.document
  context.after(() => { globalThis.document = oldDocument })
  let blob, removed = 0, appended = 0, clicks = 0, shouldFail = false
  const revoked = [], link = {
    href: '', download: '', remove: () => removed++, click: () => { clicks++; if (shouldFail) throw new Error('synthetic click failure') },
  }
  globalThis.document = { createElement: (tag) => { assert.equal(tag, 'a'); return link }, body: { appendChild: () => appended++ } }
  context.mock.method(URL, 'createObjectURL', (value) => { blob = value; return 'blob:fixture' })
  context.mock.method(URL, 'revokeObjectURL', (url) => revoked.push(url))
  state.exportCurrentPage()
  assert.equal(link.download, 'embypanel-diagnostics-page-2.json')
  assert.deepEqual(JSON.parse(await blob.text()), payload)
  assert.deepEqual(revoked, ['blob:fixture'])
  assert.equal(removed, 1)
  assert.equal(appended, 1)
  shouldFail = true
  assert.throws(() => state.exportCurrentPage(), /synthetic click failure/)
  assert.equal(removed, 2)
  assert.equal(revoked.length, 2)
  state.loading.value = true
  state.exportCurrentPage()
  assert.equal(clicks, 2, 'export blocked while replacing the page')
})

test('diagnostics new UI copy is translated, retains shared dialog/refresh, and mobile selects use 16px', () => {
  const { translations } = load('lib/translations.ts')
  const source = fs.readFileSync(path.resolve(__dirname, '../src/pages/DiagnosticsPage.vue'), 'utf8')
  for (const [, key] of source.matchAll(/\bt\('([^']+)'\)/g)) {
    assert.ok(translations[key], `${key} has English mapping`)
  }
  assert.match(source, /SettingsDialogShell/)
  assert.match(source, /RefreshButton/)
  assert.match(source, /copyText\(selectedRecord\.request_id\)/)
  assert.match(source, /max-width: 700px[\s\S]*?\.diagnostics-filters input, \.diagnostics-filters select \{ font-size: 16px; \}/)
  assert.doesNotMatch(source, /window\.confirm|window\.prompt|DialogRoot|navigator\.clipboard|fetch\(/)
})

test('diagnostics explicit states outrank legacy inference and modern result labels never claim playback success', async () => {
  const vue = require('vue')
  let state
  await renderPage('DiagnosticsPage.vue', {
    locale: vue.ref('zh-CN'), t, api: async () => assert.fail('display only'),
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  }, (s) => { state = s })
  for (const [outcome, label] of [['redirect', '已返回跳转'], ['proxy', '代理响应'], ['failed', '失败'], ['cancelled', '已取消'], ['blocked', '已拦截']]) {
    const record = diagnosticFixture({ result: { outcome } })
    assert.equal(state.outcomeLabel(record), label)
    assert.equal(state.isCancelled(record), outcome === 'cancelled')
    assert.equal(state.hasIssue(record), ['failed', 'cancelled', 'blocked'].includes(outcome))
  }
  const record = diagnosticFixture({ failure_node: 'OpenList', steps: [{ name: 'OpenList', details: 'cancelled: legacy text', duration_ms: 0 }] })
  for (const explicit of ['recorded', 'failed', 'skipped', 'cancelled']) {
    assert.equal(state.stepState(record, { ...record.steps[0], state: explicit }), explicit)
  }
  for (const [status, expected] of [['hit', '缓存命中'], ['miss', '未命中'], ['disabled', '缓存已禁用'], ['not_checked', '未检查']]) {
    assert.equal(state.cacheLabel(diagnosticFixture({ cache: { status } })), expected)
  }
  assert.equal(state.cacheLabel(diagnosticFixture({ cache_hit: true })), '缓存命中')
})

test('diagnostics authorized text preserves identifiers and never rewrites numeric timing', async () => {
  const vue = require('vue')
  let state
  await renderPage('DiagnosticsPage.vue', {
    locale: vue.ref('zh-CN'), t, api: async () => assert.fail('display only'),
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
  }, (s) => { state = s; s.openDetail(diagnosticFixture({ context: { user_name: 'a.b[0]+', device_id: '0', source_ip: '192.0.2.42' } })) })
  assert.equal(state.detailText('a.b[0]+ 192.0.2.42'), 'a.b[0]+ 192.0.2.42')
  assert.equal(state.detailText('aXb'), 'aXb')
  assert.equal(state.detailText(0), '0')
  assert.equal(state.detailValue('连接建立尝试合计耗时', 0), '0 ms')
  assert.equal(state.detailValue('未归属耗时', null), '未采集')
})

test('diagnostics disposal during an unresolved cleanup aborts the write and suppresses refresh and notice', async () => {
  const cleanup = deferred(), requests = [], notices = []
  const c = diagnosticsHarness(async (url, init) => {
    requests.push({ url, init })
    return url.endsWith('/clean') ? cleanup.promise : { records: [], can_global_cleanup: true }
  }, { confirmAction: async () => true, showNotice: (s) => notices.push(s) })
  await c.loadDiagnostics()
  const clean = c.cleanDiagnostics()
  await tick()
  assert.equal(requests.length, 2)
  assert.equal(requests[1].init.signal.aborted, false)
  c.dispose()
  assert.equal(requests[1].init.signal.aborted, true)
  cleanup.resolve(null)
  await clean
  assert.equal(requests.length, 2, 'disposed cleanup must not refresh')
  assert.deepEqual(notices, [])
})


test('diagnostics log links close details and preserve the selected request scope', async () => {
  const vue = require('vue'), navigations = []
  const record = diagnosticFixture({ request_id: 'diag-synthetic-123', server_id: 'server-a' })
  let state
  await renderPage('DiagnosticsPage.vue', {
    locale: vue.ref('zh-CN'), t, api: async () => assert.fail('display only'),
    showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
    openDiagnosticLogs: (...args) => {
      assert.equal(state.detailOpen.value, false)
      assert.equal(state.selectedRecord.value, null)
      navigations.push(args)
    },
  }, (s) => { state = s })
  for (const view of ['proxy', 'playback', 'general']) {
    state.openDetail(record)
    state.viewLogs(view)
  }
  assert.deepEqual(navigations, [
    ['diag-synthetic-123', 'server-a', 'proxy'],
    ['diag-synthetic-123', 'server-a', 'playback'],
    ['diag-synthetic-123', 'server-a', 'general'],
  ])
  state.viewLogs('proxy')
  assert.equal(navigations.length, 3, 'closed dialog cannot navigate a stale record')
})

test('cancelled request logs with no HTTP response never display HTTP 0 or success', async (context) => {
  const { requestOutcomeClass } = load('lib/panel-formatters.ts')
  const row = { id: 1, server_id: 'server-a', server_name: 'Synthetic', status_code: 0,
    event_type: 'request', method: 'GET', path_type: 'video_stream', path: '/videos/fixture/stream',
    outcome: '请求取消', cache_hit: true, blocked: false, detail: 'request_id=diag-synthetic-123',
    timestamp_ms: 1767323045123, duration_ms: 5 }
  assert.equal(requestOutcomeClass(row), 'warn')
  const harness = panelHarness(context, 'home', async () => ({}))
  assert.equal(harness.panel.requestSeverity(row), 'warn')
  assert.equal(harness.panel.requestSeverityLabel(row), '未返回响应')
  const html = await renderPage('LogsPage.vue', {
    t, filteredRequestDetailRows: [row], isRequestDetailLogView: true,
    requestOutcomeClass, requestSeverity: harness.panel.requestSeverity,
    requestSeverityLabel: harness.panel.requestSeverityLabel,
    requestRowTitle: () => 'Synthetic request', requestPathTypeLabel: () => '视频流',
    formatLogTime: () => 'Synthetic time', ipWithLocation: () => '—',
    visibleActivityLogRows: [], logServers: [], logLevelOptions: [],
    requestDetailDisplayMax: 100, requestDetailPersistDays: 7, requestDetailPersistMax: 10000,
    visibleLogCount: 1, logConfig: { level: 'info', max_size_mb: 20, max_backups: 5, format: '' },
  })
  assert.match(html, /未返回响应/)
  assert.doesNotMatch(html, /HTTP 0|level-pill success/)
})


test('diagnostics localized reasons, measured percentages and recovered step failures remain visible', async () => {
  const vue = require('vue'), { translations } = load('lib/translations.ts')
  for (const locale of ['zh-CN', 'en-US']) {
    const translate = (key) => locale === 'en-US' ? translations[key] || key : key
    let state
    await renderPage('DiagnosticsPage.vue', {
      locale: vue.ref(locale), t: translate, api: async () => assert.fail('display only'),
      showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
    }, (s) => { state = s })
    for (const name of ['请求上下文', '播放策略', '内部跳转']) {
      assert.ok(translations[name], 'every collected fixed node has an English mapping')
    }
    assert.equal(state.detailValue('最慢步骤', '缓存'), translate('缓存'))
    assert.equal(state.detailValue('选择原因', 'cache policy disabled'), translate('缓存策略未启用'))
    assert.equal(state.diagnosticMessage('timeout'), translate('请求超时'))
    const recovered = diagnosticFixture({ result: { outcome: 'redirect', status_code: 302 },
      steps: [{ name: '内部跳转', state: 'failed', duration_ms: 10, details: 'probe failed, original URL used' }] })
    assert.equal(state.hasIssue(recovered), true, 'a recovered step failure still contributes to issue counts')
    assert.equal(state.outcomeLabel(recovered), translate('已返回跳转'))
    state.openDetail({ ...recovered, duration_ms: 20, timing: { slowest_step: '内部跳转', slowest_step_ms: 10 } })
    assert.ok(state.detailSections.value.find((section) => section.title === '耗时分布').rows.some((row) => row[0] === '最慢步骤占比' && row[1] === '50.0%'))
  }
})

// Actual panel navigation: the router mutates the reactive route, so the real page
// watcher, activation, server-options request, log query and polling all execute.
test('diagnostic navigation reaches ready logs for Viewer, Operator and monitoring-only profiles without settings/config reads', async (context) => {
  for (const profile of [
    { username: 'Viewer', can_manage_log_config: false },
    { username: 'Operator', can_manage_log_config: false },
    { username: 'monitoring.read only' },
    { username: 'non-boolean legacy', can_manage_log_config: 'true' },
  ]) {
    const reads = []
    const harness = panelHarness(context, 'diagnostics', async (url) => {
      reads.push(url)
      if (url === '/api/setup-status') return { initialized: true }
      if (url === '/api/profile') return profile
      if (url === '/api/servers/options') return { servers: [{ id: 'server-a', name: 'Authorized server' }], can_control_sessions: profile.username === 'Operator' }
      if (url.startsWith('/api/monitoring/')) return []
      if (url.startsWith('/api/settings')) assert.fail(`unexpected privileged read: ${url}`)
      return {}
    })
    await harness.start()
    assert.equal(harness.panel.pageReady.value, true)
    for (const view of ['proxy', 'playback', 'general']) {
      harness.panel.setPage('diagnostics')
      await tick()
      harness.panel.openDiagnosticLogs('diag-synthetic-123', 'server-a', view)
      await tick()
      assert.equal(harness.route.name, 'logs')
      assert.equal(harness.panel.pageReady.value, true)
      assert.equal(harness.panel.pageLoading.value, false)
      assert.equal(harness.panel.error.value, '')
      assert.equal(harness.panel.canManageLogConfig.value, false)
      assert.deepEqual(harness.panel.logServers.value, [{ id: 'server-a', name: 'Authorized server' }])
      const query = new URL(reads.filter((url) => url.startsWith('/api/monitoring/')).at(-1), 'http://fixture.invalid')
      assert.equal(query.pathname, view === 'proxy' ? '/api/monitoring/request-details' : '/api/monitoring/logs')
      assert.equal(query.searchParams.get('keyword'), 'diag-synthetic-123')
      assert.equal(query.searchParams.get('server_id'), 'server-a')
      assert.equal(query.searchParams.get(view === 'proxy' ? 'path_type' : 'kind'), view === 'proxy' ? 'video_stream' : view === 'playback' ? 'playback' : 'general')
      assert.equal(harness.intervals.size, 1)
      const interval = [...harness.intervals.values()][0]
      assert.equal(interval.ms, 3000)
      interval.fn()
      await tick()
      await harness.panel.saveLogConfig()
      await harness.panel.refreshLogConfig()
      const html = await renderPage('LogsPage.vue', harness.panel)
      assert.ok(html.includes('Authorized server'))
      assert.ok(!html.includes('日志文件配置'))
      assert.ok(!html.includes('undefined'))
    }
    assert.equal(reads.some((url) => url.startsWith('/api/settings')), false)
    assert.deepEqual(harness.panel.settings.servers, [])
  }
})

const syntheticLogConfig = (level = 'info') => ({ debug_mode: false, level, max_size_mb: 5, max_backups: 10, format: 'synthetic-format' })
function logsApi(profile, configRequest, requests = []) {
  return async (url, init) => {
    requests.push({ url, init })
    if (url === '/api/setup-status') return { initialized: true }
    if (url === '/api/profile') return typeof profile === 'function' ? profile(init) : profile
    if (url === '/api/login') return { token: 'fixture-next-login-token' }
    if (url === '/api/servers/options') return { servers: [{ id: 'server-a', name: 'Authorized server' }], can_control_sessions: false }
    if (url === '/api/settings/log-config') return configRequest(init)
    if (url.startsWith('/api/monitoring/')) return []
    if (url === '/api/settings') assert.fail('logs must not depend on settings.read')
    return {}
  }
}

test('super-admin config is independent of log readiness, refreshes, saves encrypted payloads and preserves visible errors', async (context) => {
  const pendingConfig = deferred(), requests = [], encrypted = []
  let configRead = 0, failRead = false, failSave = false
  const harness = panelHarness(context, 'logs', logsApi({ username: 'SuperAdmin', can_manage_log_config: true }, async (init) => {
    if (init?.method === 'PUT') {
      assert.deepEqual(JSON.parse(init.body), { encrypted: 'synthetic' })
      if (failSave) throw new Error('synthetic save failure')
      return syntheticLogConfig('debug')
    }
    configRead++
    if (configRead === 1) return pendingConfig.promise
    if (failRead) throw new Error('synthetic config unavailable')
    return syntheticLogConfig('warning')
  }, requests), { encryptPayload: async (name, value) => { encrypted.push({ name, value }); return { encrypted: 'synthetic' } } })
  await harness.start()
  assert.equal(harness.panel.pageReady.value, true, 'even a pending config must not block logs')
  assert.equal(harness.panel.logConfigLoading.value, true)
  pendingConfig.reject(new Error('synthetic config forbidden'))
  await tick()
  assert.equal(harness.panel.error.value, '')
  assert.equal(harness.panel.logsError.value, '')
  assert.equal(harness.panel.logConfigError.value, 'synthetic config forbidden')
  let html = await renderPage('LogsPage.vue', harness.panel)
  assert.ok(html.includes('日志文件配置') && html.includes('synthetic config forbidden'))
  assert.match(html, /role="alert"/)
  const before = requests.length
  await harness.panel.saveLogConfig()
  assert.equal(requests.length, before, 'unloaded config cannot be saved')
  for (let i = 0; i < 2; i++) {
    ;[...harness.intervals.values()][0].fn()
    await tick()
  }
  assert.equal(configRead, 1, 'polling must not retry forbidden config reads')
  await harness.panel.refreshLogConfig()
  assert.equal(harness.panel.logConfig.level, 'warning')
  assert.equal(harness.panel.logConfigError.value, '')
  await harness.panel.saveLogConfig()
  assert.equal(encrypted[0].name, 'log_config')
  assert.equal(encrypted[0].value.level, 'warning')
  assert.equal(harness.panel.logConfig.level, 'debug')
  assert.equal(harness.panel.notice.value, '日志配置已保存')
  failSave = true
  await harness.panel.saveLogConfig()
  assert.equal(harness.panel.logConfigError.value, 'synthetic save failure')
  await harness.panel.refreshActivityLogs()
  html = await renderPage('LogsPage.vue', harness.panel)
  assert.ok(html.includes('synthetic save failure'), 'log refresh cannot clear config errors')
  const saveButton = html.match(/<button\b[^>]*>[^<]*<!---->[^<]*保存日志配置[^<]*<\/button>/)?.[0]
    || html.match(/<button\b[^>]*>\s*保存日志配置\s*<\/button>/)?.[0]
  assert.ok(saveButton, 'loaded config keeps its save button after a failed write')
  assert.doesNotMatch(saveButton, /\sdisabled(?:[\s=>])/, 'save errors remain retryable')
  failSave = false
  await harness.panel.saveLogConfig()
  assert.equal(harness.panel.logConfigError.value, '')
  assert.equal(harness.panel.pageReady.value, true)
  failRead = true
  await harness.panel.refreshLogConfig()
  assert.equal(harness.panel.logConfigError.value, 'synthetic config unavailable')
  assert.equal(harness.panel.logConfigLoading.value, false)
  assert.equal(harness.panel.logConfigSaving.value, false)
})

test('config read generations ignore late success, error and finally while a replacement is pending', async (context) => {
  const reads = []
  const harness = panelHarness(context, 'logs', logsApi({ username: 'SuperAdmin', can_manage_log_config: true }, () => {
    const response = deferred(); reads.push(response); return response.promise
  }))
  await harness.start()
  const replacement = harness.panel.refreshLogConfig()
  reads[0].reject(new Error('stale read failure'))
  await tick()
  assert.equal(harness.panel.logConfigLoading.value, true)
  assert.equal(harness.panel.logConfigError.value, '')
  reads[1].resolve(syntheticLogConfig('warning'))
  await replacement
  const stale = harness.panel.refreshLogConfig(), latest = harness.panel.refreshLogConfig()
  reads[3].resolve(syntheticLogConfig('error'))
  await latest
  reads[2].resolve(syntheticLogConfig('debug'))
  await stale
  assert.equal(harness.panel.logConfig.level, 'error')
  assert.equal(harness.panel.logConfigLoading.value, false)
})

test('new login clears stale true and ignores config read/write successes and failures from the prior identity', async (context) => {
  for (const action of ['read', 'save', 'encryption']) {
    for (const failed of [false, true]) {
      const delayed = deferred(), encryption = deferred(), nextProfile = deferred(), requests = []
      let nextIdentity = false, delayEncryption = false
      const harness = panelHarness(context, 'logs', logsApi(() => nextIdentity ? nextProfile.promise : { username: 'SuperAdmin', can_manage_log_config: true },
        (init) => (action === 'read' || init?.method === 'PUT') ? delayed.promise : syntheticLogConfig(), requests), {
        encryptPayload: async (name) => name === 'log_config' && delayEncryption ? encryption.promise : {},
      })
      await harness.start()
      let pending
      if (action !== 'read') {
        delayEncryption = action === 'encryption'
        pending = harness.panel.saveLogConfig()
        await tick()
      }
      nextIdentity = true
      const login = harness.panel.login()
      await tick()
      assert.equal(harness.panel.profile.can_manage_log_config, false, 'pending profile never inherits true')
      nextProfile.resolve({ username: 'legacy monitoring user' })
      await login
      assert.equal(harness.panel.pageReady.value, true)
      assert.equal(harness.panel.canManageLogConfig.value, false)
      const writes = requests.filter(({ init }) => init?.method === 'PUT').length
      const snapshot = JSON.stringify([harness.panel.logConfig, harness.panel.logConfigError.value, harness.panel.logConfigLoading.value, harness.panel.logConfigSaving.value, harness.panel.notice.value])
      if (action === 'encryption') encryption.resolve({ encrypted: 'late' })
      else if (failed) delayed.reject(new Error('late prior identity failure'))
      else delayed.resolve(syntheticLogConfig('critical'))
      await pending
      await tick()
      assert.equal(JSON.stringify([harness.panel.logConfig, harness.panel.logConfigError.value, harness.panel.logConfigLoading.value, harness.panel.logConfigSaving.value, harness.panel.notice.value]), snapshot)
      await harness.panel.saveLogConfig()
      assert.equal(requests.filter(({ init }) => init?.method === 'PUT').length, writes)
    }
  }
})

test('profile from a previous login cannot restore log management or overwrite the new profile', async (context) => {
  const oldProfile = deferred(), requests = []
  let first = true
  const harness = panelHarness(context, 'logs', logsApi(() => {
    if (first) { first = false; return oldProfile.promise }
    return { username: 'current monitoring user' }
  }, () => assert.fail('stale profile must not enable config reads'), requests))
  const boot = harness.start()
  await tick()
  await harness.panel.login()
  oldProfile.resolve({ username: 'stale SuperAdmin', can_manage_log_config: true })
  await boot
  assert.equal(harness.panel.profile.username, 'current monitoring user')
  assert.equal(harness.panel.canManageLogConfig.value, false)
  assert.equal(harness.panel.pageReady.value, true)
  assert.equal(requests.some(({ url }) => url === '/api/settings/log-config'), false)
})

test('profile update without optional capability fails closed and direct save sends no write', async (context) => {
  const requests = []
  const harness = panelHarness(context, 'logs', logsApi((init) => init?.method === 'PUT' ? { username: 'renamed' } : { username: 'SuperAdmin', can_manage_log_config: true }, () => syntheticLogConfig(), requests))
  await harness.start()
  harness.panel.profileForm.username = 'renamed'
  await harness.panel.saveAccount()
  assert.equal(harness.panel.profile.can_manage_log_config, false)
  await harness.panel.saveLogConfig()
  assert.equal(requests.some(({ url, init }) => url === '/api/settings/log-config' && init?.method === 'PUT'), false)
})

test('log config 401 still expires the session through the real API helper and clears management capability', async (context) => {
  const calls = []
  context.mock.method(globalThis, 'fetch', async (url) => {
    calls.push(url)
    if (url === '/api/settings/log-config') return new Response('synthetic expired', { status: 401 })
    const response = url === '/api/setup-status' ? { initialized: true }
      : url === '/api/profile' ? { username: 'SuperAdmin', can_manage_log_config: true }
        : url === '/api/servers/options' ? { servers: [], can_control_sessions: false }
          : url.startsWith('/api/monitoring/') ? [] : {}
    return new Response(JSON.stringify(response))
  })
  const harness = panelHarness(context, 'logs', null, { realApiClient: true })
  await harness.start()
  await tick()
  assert.equal(harness.panel.mode.value, 'login')
  assert.equal(harness.panel.profile.can_manage_log_config, false)
  assert.equal(harness.panel.canManageLogConfig.value, false)
  assert.equal(harness.panel.error.value, '登录已过期，请重新登录')
  assert.equal(harness.panel.pageReady.value, false)
  assert.equal(harness.intervals.size, 0)
  assert.ok(calls.includes('/api/settings/log-config'))
})

test('diagnostic cache-hit and legacy rows label IDs explicitly in both locales without upstream lookups', async (context) => {
  const vue = require('vue'), { translations } = load('lib/translations.ts')
  context.mock.method(globalThis, 'fetch', () => assert.fail('SSR must not query upstream names'))
  for (const locale of ['zh-CN', 'en-US']) {
    const translate = (key) => locale === 'en-US' ? translations[key] || key : key
    const records = [
      diagnosticFixture({ request_id: 'diag-named', cache_hit: true, context: { media_name: 'Synthetic named media' } }),
      diagnosticFixture({ request_id: 'diag-hit-id', cache_hit: true, cache: { status: 'hit' }, context: { media_name: null } }),
      diagnosticFixture({ request_id: 'diag-legacy-id', context: undefined }),
      diagnosticFixture({ request_id: 'diag-no-media', media_id: '' }),
    ]
    const html = await renderPage('DiagnosticsPage.vue', {
      locale: vue.ref(locale), t: translate, api: () => assert.fail('SSR must not query names'),
      showNotice: () => {}, clearNotice: () => {}, confirmAction: async () => false,
    }, (state) => { state.diagnostics.value = records })
    const rows = [...html.matchAll(/<article\b[^>]*>([\s\S]*?)<\/article>/g)].map((match) => match[1])
    assert.equal(rows.length, 4)
    const media = rows.map((row) => row.match(/class="meta-media"[^>]*>([\s\S]*?)<\/code>/)?.[1])
    assert.ok(media[0].includes(`${translate('媒体')}: <code>Synthetic named media`))
    assert.ok(media[1].includes(`${translate('媒体 ID')}: <code>m-fixture`))
    assert.ok(media[2].includes(`${translate('媒体 ID')}: <code>m-fixture`))
    assert.ok(media[3].includes(`${translate('媒体 ID')}: <code>—`))
    assert.ok(rows[1].includes(translate('缓存命中')))
    assert.doesNotMatch(html, /undefined|NaN|Invalid Date/)
  }
})

test('logout resets the profile capability and late config results cannot restore it', async (context) => {
  const lateConfig = deferred()
  context.mock.method(globalThis, 'fetch', async (url) => {
    assert.equal(url, '/api/logout')
    return new Response('{}')
  })
  const harness = panelHarness(context, 'logs', logsApi({ username: 'SuperAdmin', can_manage_log_config: true }, () => lateConfig.promise))
  await harness.start()
  assert.equal(harness.panel.canManageLogConfig.value, true)
  await harness.panel.logout()
  assert.equal(harness.panel.profile.can_manage_log_config, false)
  assert.equal(harness.panel.profile.username, '')
  assert.equal(harness.panel.mode.value, 'login')
  lateConfig.resolve(syntheticLogConfig('critical'))
  await tick()
  assert.equal(harness.panel.canManageLogConfig.value, false)
  assert.equal(harness.panel.logConfig.level, 'info')
  assert.equal(harness.panel.logConfigError.value, '')
  assert.equal(harness.panel.logConfigLoading.value, false)
  assert.equal(harness.intervals.size, 0)
})
