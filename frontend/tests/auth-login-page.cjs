// Static SFC/SSR plus in-memory host checks for the sign-in page.
// No browser, HTTP, live Emby data or Vite dev server is used.
const { test } = require('node:test')
const assert = require('node:assert/strict')
const fs = require('node:fs')
const path = require('node:path')
const ts = require('typescript')

const SRC = path.resolve(__dirname, '../src')
const STYLE = path.join(SRC, 'style.css')
const APP = path.join(SRC, 'App.vue')
const CHARACTERS = path.join(SRC, 'components/auth/AnimatedCharacters.vue')

function transpile(code) {
  return ts.transpileModule(code, {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText
}

function compileSfc(file) {
  const { parse, compileScript, compileTemplate } = require('@vue/compiler-sfc')
  const source = fs.readFileSync(file, 'utf8')
  const { descriptor } = parse(source, { filename: file })
  assert.ok(descriptor.template, `${path.basename(file)} has a template`)
  const script = compileScript(descriptor, { id: 'auth-login-test' })
  const template = compileTemplate({
    source: descriptor.template.content,
    filename: file,
    id: 'auth-login-test',
    compilerOptions: { bindingMetadata: script.bindings },
  })
  assert.deepEqual(template.errors, [])
  return transpile(`${script.content}\n${template.code}`)
}

function createLoader(stubs) {
  const cache = new Map()
  function loadFile(file) {
    if (cache.has(file)) return cache.get(file)
    const module = { exports: {} }
    cache.set(file, module.exports)
    const code = file.endsWith('.vue') ? compileSfc(file) : transpile(fs.readFileSync(file, 'utf8'))
    const dir = path.dirname(file)
    new Function('require', 'module', 'exports', code)((id) => {
      if (Object.prototype.hasOwnProperty.call(stubs, id)) return stubs[id]
      if (id.startsWith('@/')) return loadFile(path.join(SRC, id.slice(2)))
      if (id.startsWith('.')) return loadFile(path.resolve(dir, id))
      return require(id)
    }, module, module.exports)
    // compileTemplate emits render as a sibling export of the SFC default.
    if (module.exports.default && typeof module.exports.render === 'function') {
      module.exports.default.render = module.exports.render
    }
    cache.set(file, module.exports)
    return module.exports
  }
  return loadFile
}

function loadComponent(file, stubs = {}) {
  const exports = createLoader(stubs)(file)
  const component = exports.default
  component.render = component.render ?? exports.render
  return component
}

function panelFixture(vue, overrides = {}) {
  return {
    mode: vue.ref('login'),
    page: vue.ref('overview'),
    darkMode: vue.ref(false),
    locale: vue.ref('zh-CN'),
    mobileNavOpen: vue.ref(false),
    mobileMenuButton: vue.ref(null),
    mobileNavCloseButton: vue.ref(null),
    mobileSidebar: vue.ref(null),
    saving: vue.ref(false),
    secondFactorRequired: vue.ref(false),
    secondFactorMode: vue.ref('totp'),
    pageLoading: vue.ref(false),
    pageReady: vue.ref(false),
    error: vue.ref(''),
    notice: vue.ref(''),
    credentials: vue.reactive({ username: '', password: '', totp_code: '', recovery_code: '' }),
    logoUrl: '/ui/logo.svg',
    profile: vue.reactive({ username: 'admin', can_manage_log_config: false }),
    appInfo: vue.reactive({ name: 'EmbyPanel', version: '0.3.8' }),
    menu: [],
    updateCheck: vue.ref(null),
    updateChecking: vue.ref(false),
    updateCheckError: vue.ref(''),
    t: (value) => value,
    setLocale() {},
    closeMobileNav() {},
    toggleMobileNav() {},
    trapMobileNavFocus() {},
    toggleTheme() {},
    setupAdmin() {},
    login() {},
    selectSecondFactorMode() {},
    refreshUpdateCheck() {},
    setPage() {},
    retryPage() {},
    logout() {},
    ...overrides,
  }
}

function appStubs(panel) {
  return {
    'vue-router': { RouterView: { render: () => null } },
    '@/composables/panel-context': { providePanelContext() {} },
    '@/composables/usePanelController': { usePanelController: () => panel },
    '@/components/ParticleBackdrop.vue': { render: () => null },
    '@/components/ActionDialogHost.vue': { render: () => null },
    '@/components/ui/PreferenceControls.vue': {
      props: ['locale', 'darkMode', 'languageLabel', 'themeLabel', 'themeTitle'],
      render: () => null,
    },
    '@lucide/vue': new Proxy({}, {
      get: (_target, key) => (typeof key === 'string' ? { render: () => null } : undefined),
    }),
  }
}

async function renderAuthPage(panel) {
  const vue = require('vue')
  const component = loadComponent(APP, appStubs(panel))
  return require('@vue/server-renderer').renderToString(vue.createSSRApp(component))
}

function characterPanelTag(html) {
  return html.match(/<div[^>]*auth-characters[^>]*>/)?.[0] ?? ''
}

function styleBlock(css, selector) {
  const index = css.search(new RegExp(`(^|\\n)${selector.replace(/[.$*+?^()[\]{}|\\]/g, '\\$&')}\\s*\\{`))
  if (index < 0) return ''
  const open = css.indexOf('{', index)
  const close = css.indexOf('}', open)
  return css.slice(open + 1, close)
}

function mediaBlock(css, query) {
  const start = css.indexOf(`@media ${query}`)
  if (start < 0) return ''
  const open = css.indexOf('{', start)
  let depth = 0
  for (let index = open; index < css.length; index += 1) {
    if (css[index] === '{') depth += 1
    else if (css[index] === '}') {
      depth -= 1
      if (depth === 0) return css.slice(open + 1, index)
    }
  }
  return ''
}

test('sign-in and first-time setup render the character stage, both inputs and the password toggle', async () => {
  const vue = require('vue')
  for (const mode of ['login', 'setup']) {
    const html = await renderAuthPage(panelFixture(vue, { mode: vue.ref(mode) }))
    const tag = characterPanelTag(html)
    assert.ok(tag, `${mode} renders the character container`)
    assert.ok(tag.includes('aria-hidden="true"'), `${mode} keeps the characters decorative`)
    assert.match(html, /class="auth-characters-stage"/)
    assert.match(html, /class="auth-character auth-character--purple/)
    assert.match(html, /class="auth-character auth-character--black/)
    assert.match(html, /class="auth-character auth-character--orange/)
    assert.match(html, /class="auth-character auth-character--yellow/)
    assert.match(html, /name="username"/)
    assert.match(html, /name="password"[^>]*type="password"/)
    assert.match(html, /class="icon-button auth-password-toggle"[^>]*aria-label="显示密码"/)
    assert.match(html, /aria-pressed="false"/)
    assert.doesNotMatch(html, /name="totp_code"/)
  }
})

test('two-factor step keeps the characters and renders the TOTP or recovery input', async () => {
  const vue = require('vue')
  const totp = await renderAuthPage(panelFixture(vue, { secondFactorRequired: vue.ref(true) }))
  assert.ok(characterPanelTag(totp).includes('aria-hidden="true"'))
  assert.match(totp, /name="totp_code"/)
  assert.match(totp, /使用恢复码/)
  const recovery = await renderAuthPage(panelFixture(vue, {
    secondFactorRequired: vue.ref(true),
    secondFactorMode: vue.ref('recovery'),
  }))
  assert.match(recovery, /name="recovery_code"/)
  assert.match(recovery, /使用 TOTP 验证码/)
  assert.doesNotMatch(recovery, /name="totp_code"/)
})

test('character panel is decorative and never captures pointer input', async () => {
  const css = fs.readFileSync(STYLE, 'utf8')
  assert.match(styleBlock(css, '.auth-characters'), /pointer-events:\s*none/)
  assert.match(styleBlock(css, '.auth-characters-stage'), /pointer-events:\s*none/)
  const vue = require('vue')
  const html = await renderAuthPage(panelFixture(vue))
  assert.ok(characterPanelTag(html).includes('aria-hidden="true"'))
  assert.doesNotMatch(characterPanelTag(html), /tabindex/)
})

test('characters are hidden at 900px and the single-column sign-in card is kept', () => {
  const css = fs.readFileSync(STYLE, 'utf8')
  const narrow = mediaBlock(css, '(max-width: 900px)')
  assert.ok(narrow, 'a max-width: 900px breakpoint exists')
  assert.match(narrow, /\.auth-characters\s*\{[^}]*display:\s*none/)
  assert.match(narrow, /\.auth-login\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)/)
})

test('short desktop viewports anchor the sign-in card below the language and theme controls', () => {
  const css = fs.readFileSync(STYLE, 'utf8')
  const short = mediaBlock(css, '(min-width: 901px) and (max-height: 640px)')
  assert.ok(short, 'a short desktop breakpoint exists')
  assert.match(short, /\.auth-shell\s*\{[^}]*place-items:\s*start center/)
  assert.match(short, /\.auth-shell\s*\{[^}]*overflow-y:\s*auto/)
  const paddingTop = Number(short.match(/padding-top:\s*calc\((\d+)px/)?.[1])
  // .auth-utility sits at top: 22px with a 36px control row (bottom edge 58px).
  const utilityBottom = 22 + 36
  assert.ok(paddingTop >= utilityBottom + 12, `card top ${paddingTop}px must clear the controls (${utilityBottom}px) by 12px`)
  // The taller-viewport centring and the <=900px single column must stay untouched.
  const wide = mediaBlock(css, '(max-width: 900px)')
  assert.match(wide, /\.auth-login\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)/)
})

function createMemoryWindow(options = {}) {
  const query = {
    matches: Boolean(options.reducedMotion),
    media: '(prefers-reduced-motion: reduce)',
    listeners: [],
    addEventListener(type, handler) {
      this.listeners.push({ type, handler })
    },
    removeEventListener(type, handler) {
      const index = this.listeners.findIndex((entry) => entry.type === type && entry.handler === handler)
      if (index >= 0) this.listeners.splice(index, 1)
    },
  }
  const timeouts = new Map()
  let nextTimer = 1
  return {
    innerWidth: 1280,
    innerHeight: 800,
    added: [],
    removed: [],
    frames: [],
    timeouts,
    query,
    addEventListener(type, handler, listenerOptions) {
      this.added.push({ type, handler, listenerOptions })
    },
    removeEventListener(type, handler) {
      this.removed.push({ type, handler })
    },
    matchMedia: () => query,
    requestAnimationFrame(callback) {
      this.frames.push(callback)
      return this.frames.length
    },
    cancelAnimationFrame(id) {
      this.frames[id - 1] = null
    },
    setTimeout(callback, delay) {
      const id = nextTimer
      nextTimer += 1
      timeouts.set(id, { callback, delay })
      return id
    },
    clearTimeout(id) {
      timeouts.delete(id)
    },
  }
}

function createMemoryRenderer() {
  const vue = require('vue')
  // Host elements expose what the v-model directive needs (event listeners and a
  // value slot) so the real App.vue form can mount without a browser.
  const element = (tag) => {
    const listeners = new Map()
    return {
      tag,
      props: {},
      children: [],
      parent: null,
      value: '',
      addEventListener(type, handler) {
        const handlers = listeners.get(type) ?? []
        handlers.push(handler)
        listeners.set(type, handlers)
      },
      removeEventListener(type, handler) {
        const handlers = listeners.get(type)
        if (!handlers) return
        const index = handlers.indexOf(handler)
        if (index >= 0) handlers.splice(index, 1)
      },
      getBoundingClientRect: () => ({ left: 0, top: 0, width: 10, height: 10 }),
    }
  }
  const nodeOps = {
    createElement: element,
    createText: (text) => ({ tag: '#text', text, children: [], parent: null }),
    createComment: (text) => ({ tag: '#comment', text, children: [], parent: null }),
    setText: (node, text) => {
      node.text = text
    },
    setElementText: (node, text) => {
      node.text = text
      node.children = []
    },
    insert: (child, parent, anchor) => {
      child.parent = parent
      const index = anchor ? parent.children.indexOf(anchor) : -1
      if (index >= 0) parent.children.splice(index, 0, child)
      else parent.children.push(child)
    },
    remove: (child) => {
      const parent = child.parent
      if (!parent) return
      const index = parent.children.indexOf(child)
      if (index >= 0) parent.children.splice(index, 1)
      child.parent = null
    },
    parentNode: (node) => node.parent,
    nextSibling: (node) => {
      const parent = node.parent
      if (!parent) return null
      return parent.children[parent.children.indexOf(node) + 1] ?? null
    },
    patchProp: (el, key, _previous, next) => {
      el.props[key] = next
    },
    querySelector: () => null,
    setScopeId: () => {},
    insertStaticContent: () => [element('div'), element('div')],
  }
  return vue.createRenderer(nodeOps)
}

function mountCharacters(win, props = {}) {
  const previousWindow = globalThis.window
  globalThis.window = win
  const renderer = createMemoryRenderer()
  const component = loadComponent(CHARACTERS)
  const app = renderer.createApp(component, props)
  const container = { tag: 'root', props: {}, children: [], parent: null }
  app.mount(container)
  return {
    container,
    unmount() {
      app.unmount()
      globalThis.window = previousWindow
    },
  }
}

test('reduced motion keeps the characters static without pointer tracking or timers', () => {
  const win = createMemoryWindow({ reducedMotion: true })
  const mounted = mountCharacters(win, { passwordLength: 4 })
  try {
    assert.equal(win.query.matches, true)
    assert.equal(win.added.filter((entry) => entry.type === 'pointermove').length, 0)
    assert.equal(win.added.filter((entry) => entry.type === 'resize').length, 0)
    assert.equal(win.timeouts.size, 0)
    assert.equal(win.frames.length, 0)
    assert.equal(mounted.container.children.length, 1)
  } finally {
    mounted.unmount()
  }
})

test('one throttled pointer listener drives every character and is removed on unmount', () => {
  const win = createMemoryWindow()
  const mounted = mountCharacters(win, { passwordLength: 3 })
  try {
    const pointerListeners = win.added.filter((entry) => entry.type === 'pointermove')
    assert.equal(pointerListeners.length, 1, 'a single pointermove listener covers all four characters')
    assert.deepEqual(pointerListeners[0].listenerOptions, { passive: true })
    assert.ok(win.timeouts.size > 0, 'blink and entrance timers are scheduled')
    const handler = pointerListeners[0].handler
    handler({ clientX: 20, clientY: 40, pointerType: 'mouse' })
    handler({ clientX: 60, clientY: 80, pointerType: 'mouse' })
    assert.equal(win.frames.length, 1, 'pointer moves are coalesced into one animation frame')
    win.frames[0]()
  } finally {
    mounted.unmount()
  }
  assert.equal(win.timeouts.size, 0, 'every timer is cleared on unmount')
  assert.equal(win.added.length, win.removed.length, 'each listener is removed again')
  assert.ok(win.removed.some((entry) => entry.type === 'pointermove'))
  assert.ok(win.removed.some((entry) => entry.type === 'resize'))
})

function findAll(node, predicate, results = []) {
  if (!node) return results
  if (predicate(node)) results.push(node)
  for (const child of node.children ?? []) findAll(child, predicate, results)
  return results
}

function createMemoryDocument() {
  const listeners = []
  return {
    listeners,
    addEventListener(type, handler) {
      listeners.push({ type, handler })
    },
    removeEventListener(type, handler) {
      const index = listeners.findIndex((entry) => entry.type === type && entry.handler === handler)
      if (index >= 0) listeners.splice(index, 1)
    },
  }
}

// Mounts the real AnimatedCharacters behind a reactive prop holder so a test can
// flip password visibility the way the sign-in form does.
function mountCharactersWithState(win, initial) {
  const vue = require('vue')
  const previousWindow = globalThis.window
  globalThis.window = win
  const renderer = createMemoryRenderer()
  const component = loadComponent(CHARACTERS)
  const state = vue.reactive({ ...initial })
  const app = renderer.createApp({ setup: () => () => vue.h(component, { ...state }) })
  const container = { tag: 'root', props: {}, children: [], parent: null }
  app.mount(container)
  return {
    state,
    container,
    unmount() {
      app.unmount()
      globalThis.window = previousWindow
    },
  }
}

// Mounts the real App.vue sign-in branch in memory (no browser, no HTTP).
function mountAuthApp(panel) {
  const vue = require('vue')
  const previousWindow = globalThis.window
  const previousDocument = globalThis.document
  const win = createMemoryWindow()
  const doc = createMemoryDocument()
  globalThis.window = win
  globalThis.document = doc
  const renderer = createMemoryRenderer()
  const app = renderer.createApp(loadComponent(APP, appStubs(panel)))
  const container = { tag: 'root', props: {}, children: [], parent: null }
  app.mount(container)
  return {
    win,
    doc,
    container,
    unmount() {
      app.unmount()
      globalThis.window = previousWindow
      globalThis.document = previousDocument
    },
  }
}

test('motion preference changes start and stop the characters and release the media listener', () => {
  const win = createMemoryWindow({ reducedMotion: true })
  const mounted = mountCharacters(win, { passwordLength: 4, showPassword: true })
  const fireChange = () => {
    for (const entry of [...win.query.listeners]) entry.handler({ matches: win.query.matches, media: win.query.media })
  }
  try {
    assert.equal(win.query.listeners.length, 1, 'the component observes the media query')
    assert.equal(win.timeouts.size, 0, 'reduced motion starts with no timers')
    assert.equal(win.added.length, 0, 'reduced motion starts with no window listeners')

    win.query.matches = false
    fireChange()
    assert.equal(win.added.filter((entry) => entry.type === 'pointermove').length, 1)
    assert.equal(win.added.filter((entry) => entry.type === 'resize').length, 1)
    assert.ok(win.timeouts.size > 0, 'animation timers start when motion is allowed again')

    win.query.matches = true
    fireChange()
    assert.equal(win.timeouts.size, 0, 'flipping back to reduced motion cancels every timer')
    assert.ok(win.removed.some((entry) => entry.type === 'pointermove'))
  } finally {
    mounted.unmount()
  }
  assert.equal(win.timeouts.size, 0)
  assert.equal(win.query.listeners.length, 0, 'the media listener is released on unmount')
})

test('hiding the password cancels its peek chain instead of leaking one per reveal', async () => {
  const vue = require('vue')
  const win = createMemoryWindow()
  const mounted = mountCharactersWithState(win, { passwordLength: 4, showPassword: true })
  const peekEndDelays = () => [...win.timeouts.values()].filter((timer) => timer.delay === 800).length
  try {
    // four blinks + entrance + exactly one peek chain
    assert.equal(win.timeouts.size, 6)
    assert.equal(peekEndDelays(), 0, 'no peek is running yet')
    for (let round = 0; round < 20; round += 1) {
      mounted.state.showPassword = false
      await vue.nextTick()
      assert.equal(win.timeouts.size, 5, `round ${round}: hiding the password drops the peek timer`)
      assert.equal(peekEndDelays(), 0, `round ${round}: no orphaned peek timer`)
      mounted.state.showPassword = true
      await vue.nextTick()
      assert.equal(win.timeouts.size, 6, `round ${round}: re-revealing adds exactly one peek chain`)
    }
    mounted.state.showPassword = false
    await vue.nextTick()
    assert.equal(win.timeouts.size, 5)
    assert.equal(peekEndDelays(), 0)
  } finally {
    mounted.unmount()
  }
  assert.equal(win.timeouts.size, 0)
})

test('the password toggle swaps the input type and keeps aria-pressed in sync', async () => {
  const vue = require('vue')
  const mounted = mountAuthApp(panelFixture(vue))
  const passwordInput = () => findAll(mounted.container, (node) => node.props?.name === 'password')[0]
  const toggle = () => findAll(mounted.container,
    (node) => typeof node.props?.class === 'string' && node.props.class.includes('auth-password-toggle'))[0]
  try {
    assert.equal(passwordInput().props.type, 'password')
    assert.equal(toggle().props['aria-pressed'], false)
    assert.equal(toggle().props['aria-label'], '显示密码')

    let prevented = 0
    toggle().props.onClick({ preventDefault() { prevented += 1 } })
    await vue.nextTick()
    assert.equal(prevented, 1, 'the toggle never submits the sign-in form')
    assert.equal(passwordInput().props.type, 'text')
    assert.equal(toggle().props['aria-pressed'], true)
    assert.equal(toggle().props['aria-label'], '隐藏密码')
    assert.equal(toggle().props.title, '隐藏密码')

    toggle().props.onClick({ preventDefault() {} })
    await vue.nextTick()
    assert.equal(passwordInput().props.type, 'password')
    assert.equal(toggle().props['aria-pressed'], false)
    assert.equal(toggle().props['aria-label'], '显示密码')
  } finally {
    mounted.unmount()
  }
})

test('sign-in markup never leaks the password outside the password field itself', async () => {
  const vue = require('vue')
  const secret = 'Synthetic-Pass-1234'
  const panel = panelFixture(vue)
  panel.credentials.username = 'synthetic-admin'
  panel.credentials.password = secret
  const html = await renderAuthPage(panel)
  const characters = html.slice(html.indexOf('<div class="auth-characters"'), html.indexOf('<form'))
  assert.ok(characters.includes('auth-characters-stage'), 'character subtree was found')
  assert.equal(characters.includes(secret), false, 'the characters never receive the password')
  assert.equal(characters.includes('synthetic-admin'), false)
  const inputTags = [...html.matchAll(/<input\b[^>]*>/g)].map((match) => match[0])
  const passwordInput = inputTags.find((tag) => tag.includes('name="password"'))
  assert.ok(passwordInput, 'the password field is rendered')
  // Vue SSR serialises the controlled v-model value; production is a client-side
  // SPA render, so the invariant asserted here is that nothing else carries it.
  assert.ok(passwordInput.includes(secret))
  assert.equal(html.replace(passwordInput, '').includes(secret), false)
  const labellingAttributes = [...html.matchAll(/\s(aria-[a-z-]+|title|alt|placeholder|data-[a-z-]+)="[^"]*"/g)]
  assert.equal(labellingAttributes.some((match) => match[0].includes(secret)), false)
})
