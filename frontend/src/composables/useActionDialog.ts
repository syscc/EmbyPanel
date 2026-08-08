import { computed, ref, shallowRef, type ComputedRef, type Ref } from 'vue'

export type ActionDialogTone = 'default' | 'warning' | 'danger'

export type ActionDialogRequest = {
  kind: 'confirm' | 'prompt'
  title: string
  description?: string
  confirmText?: string
  cancelText?: string
  tone?: ActionDialogTone
  inputLabel?: string
  inputType?: 'text' | 'password'
  inputAutocomplete?: 'off' | 'new-password' | 'current-password'
  inputPlaceholder?: string
  minLength?: number
  initialValue?: string
}

type ActionDialogValue = boolean | string | null

const open = ref(false)
const request = shallowRef<ActionDialogRequest | null>(null)
const inputValue = ref('')
const canConfirm = computed(() => {
  const active = request.value
  if (!active || active.kind !== 'prompt') return true
  return inputValue.value.trim().length >= (active.minLength ?? 0)
})

let resolver: ((value: ActionDialogValue) => void) | null = null
let activeKind: ActionDialogRequest['kind'] | null = null

function settle(value: ActionDialogValue) {
  const resolve = resolver
  resolver = null
  activeKind = null
  open.value = false
  request.value = null
  inputValue.value = ''
  resolve?.(value)
}

function cancelActionDialog() {
  settle(activeKind === 'confirm' ? false : null)
}

function confirmActionDialog() {
  if (!request.value || !canConfirm.value) return
  settle(request.value.kind === 'confirm' ? true : inputValue.value)
}

function begin(nextRequest: ActionDialogRequest): Promise<ActionDialogValue> {
  if (resolver) cancelActionDialog()
  request.value = nextRequest
  activeKind = nextRequest.kind
  inputValue.value = nextRequest.initialValue ?? ''
  open.value = true
  return new Promise<ActionDialogValue>((resolve) => {
    resolver = resolve
  })
}

function confirmAction(options: Omit<ActionDialogRequest, 'kind'>) {
  return begin({ ...options, kind: 'confirm' }).then((value) => value === true)
}

function promptAction(options: Omit<ActionDialogRequest, 'kind'>) {
  return begin({ ...options, kind: 'prompt' }).then((value) => (typeof value === 'string' ? value : null))
}

function handleOpenChange(nextOpen: boolean) {
  if (!nextOpen && open.value) {
    cancelActionDialog()
    return
  }
  open.value = nextOpen
}

export type ActionDialogController = {
  open: Ref<boolean>
  request: Ref<ActionDialogRequest | null>
  inputValue: Ref<string>
  canConfirm: ComputedRef<boolean>
  confirmAction: (options: Omit<ActionDialogRequest, 'kind'>) => Promise<boolean>
  promptAction: (options: Omit<ActionDialogRequest, 'kind'>) => Promise<string | null>
  confirmActionDialog: () => void
  cancelActionDialog: () => void
  handleOpenChange: (nextOpen: boolean) => void
}

export function useActionDialog(): ActionDialogController {
  return {
    open,
    request,
    inputValue,
    canConfirm,
    confirmAction,
    promptAction,
    confirmActionDialog,
    cancelActionDialog,
    handleOpenChange,
  }
}
