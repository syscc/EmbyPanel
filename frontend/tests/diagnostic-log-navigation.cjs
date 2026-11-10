// Isolated navigation-state fixtures; no HTTP, authentication or real logs are used.
const { test } = require('node:test')
const assert = require('node:assert/strict')
const { readFileSync } = require('node:fs')
const path = require('node:path')
const ts = require('typescript')

function navigationFixture() {
  const source = readFileSync(path.resolve(__dirname, '../src/composables/usePanelController.ts'), 'utf8')
  const file = ts.createSourceFile('usePanelController.ts', source, ts.ScriptTarget.Latest, true)
  let declaration
  function find(node) {
    if (ts.isFunctionDeclaration(node) && node.name?.text === 'openDiagnosticLogs') declaration = node
    ts.forEachChild(node, find)
  }
  find(file)
  assert.ok(declaration)
  const compiled = ts.transpileModule(declaration.getText(file) + '\nexports.openDiagnosticLogs = openDiagnosticLogs', {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
  const state = {
    token: { value: 'isolated-fixture' }, mode: { value: 'app' },
    selectedLogView: { value: 'general' }, selectedLogServer: { value: 'all' },
    selectedLogLevel: { value: 'error' }, selectedRequestPathType: { value: 'all' },
    logKeywordFilter: { value: 'old' }, logSince: { value: '2026-01-01' }, logUntil: { value: '2026-01-02' },
  }
  const pages = [], resets = []
  const exported = {}
  new Function('exports', ...Object.keys(state), 'resetLogLimits', 'setPage', compiled)(
    exported, ...Object.values(state), () => resets.push(true), (page) => pages.push(page),
  )
  return { state, pages, resets, open: exported.openDiagnosticLogs }
}

test('diagnostic log navigation preserves correlation and server scope across request, playback and runtime views', () => {
  for (const view of ['proxy', 'playback', 'general']) {
    const fixture = navigationFixture()
    fixture.open('diag-synthetic-123', 'server-a', view)
    assert.equal(fixture.state.selectedLogView.value, view)
    assert.equal(fixture.state.selectedLogServer.value, 'server-a')
    assert.equal(fixture.state.selectedLogLevel.value, 'all')
    assert.equal(fixture.state.selectedRequestPathType.value, view === 'proxy' ? 'video_stream' : 'all')
    assert.equal(fixture.state.logKeywordFilter.value, 'diag-synthetic-123')
    assert.equal(fixture.state.logSince.value, '')
    assert.equal(fixture.state.logUntil.value, '')
    assert.deepEqual(fixture.pages, ['logs'])
    assert.equal(fixture.resets.length, 1)
  }
})

test('diagnostic log navigation does nothing outside an active login or without a valid record identity', () => {
  for (const [token, mode, id, server] of [
    ['', 'app', 'diag-valid', 'server-a'],
    ['isolated-fixture', 'login', 'diag-valid', 'server-a'],
    ['isolated-fixture', 'app', '', 'server-a'],
    ['isolated-fixture', 'app', 'other-id', 'server-a'],
    ['isolated-fixture', 'app', 'diag-valid', ''],
  ]) {
    const fixture = navigationFixture()
    fixture.state.token.value = token
    fixture.state.mode.value = mode
    const before = JSON.stringify(fixture.state)
    fixture.open(id, server, 'proxy')
    assert.equal(JSON.stringify(fixture.state), before)
    assert.equal(fixture.pages.length, 0)
    assert.equal(fixture.resets.length, 0)
  }
})
