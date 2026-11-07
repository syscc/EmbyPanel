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
function panelHarness(context, initialPage, api) {
  const vue = require('vue')
  const mounted = [], unmounted = []
  const route = vue.reactive({ name: initialPage })
  const storage = () => {
    const values = new Map([['embypanel_token', 'fixture-token']])
    return { getItem: (key) => values.get(key) ?? null, setItem: (key, value) => values.set(key, value), removeItem: (key) => values.delete(key) }
  }
  for (const [key, value] of Object.entries({
    localStorage: storage(), sessionStorage: storage(),
    window: { location: { hash: `#/${initialPage}` }, clearInterval: () => {}, setTimeout: () => 1, clearTimeout: () => {} },
    document: { documentElement: { classList: { toggle: () => {} } } },
  })) {
    const previous = Object.getOwnPropertyDescriptor(globalThis, key)
    Object.defineProperty(globalThis, key, { configurable: true, writable: true, value })
    context.after(() => previous ? Object.defineProperty(globalThis, key, previous) : delete globalThis[key])
  }
  const { usePanelController } = load('composables/usePanelController.ts', {
    vue: { ...vue, onMounted: (fn) => mounted.push(fn), onBeforeUnmount: (fn) => unmounted.push(fn) },
    '@lucide/vue': {},
    'vue-router': { useRoute: () => route, useRouter: () => ({ replace: async ({ name }) => { route.name = name } }) },
    '@/lib/api-client': { createApiClient: () => api },
    '@/composables/usePayloadEncryption': { usePayloadEncryption: () => ({ encryptPayload: async () => ({}), warmPublicKey: async () => {}, randomId: () => 'fixture-id' }) },
    '@/composables/useActionDialog': { useActionDialog: () => ({ confirmAction: async () => true, promptAction: async () => null }) },
  })
  const scope = vue.effectScope()
  const panel = scope.run(() => usePanelController())
  context.after(() => { unmounted.forEach((fn) => fn()); scope.stop() })
  return { panel, route, start: () => Promise.all(mounted.map((fn) => fn())) }
}

test('direct history/security/sessions/library-tasks/cache-management entry awaits authorized options without settings.read or home', async (context) => {
  for (const page of ['history', 'admin-security', 'sessions', 'library-tasks', 'cache-management']) {
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

async function renderPage(filename, panel, onSetup, overrides = {}) {
  const vue = require('vue')
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
  const component = module.exports.default, setup = component.setup
  component.render = module.exports.render
  component.setup = (props, context) => {
    const state = setup(props, context)
    onSetup?.(state)
    return state
  }
  return require('@vue/server-renderer').renderToString(vue.createSSRApp(component))
}

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
    state.libraries.value = [{ server_id: 'server-cn', name: '电影媒体库', path: null, capabilities: { path: { available: false } } }]
    state.tasks.value = [task]
    state.selectedTask.value = task
    state.dialogOpen.value = true
    state.failureDetail.value = { summary: '模拟失败摘要', history: [] }
  }, {
    '@/components/ui/SettingsDialogShell.vue': {
      props: ['open'], setup: (props, { slots }) => () => props.open ? vue.h('dialog', slots.default?.()) : null,
    },
  })
  for (const [tag, content] of [['article', '电影媒体库'], ['tbody', '电影扫描'], ['dialog', '模拟失败摘要']]) {
    const section = html.match(new RegExp(`<${tag}\\b[^>]*>([\\s\\S]*?)</${tag}>`))?.[1]
    assert.ok(section, `${tag} must render`)
    assert.ok(section.includes(content), `${tag} contains its fixture`)
    assert.ok(section.includes('家庭影音'), `${tag} displays the configured name`)
    assert.equal(section.includes('server-cn'), false, `${tag} must not display the raw server ID`)
  }
  assert.ok(html.includes('value="server-cn"'))
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
