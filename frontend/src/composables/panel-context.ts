import { inject, provide, type InjectionKey } from 'vue'
import type { usePanelController } from './usePanelController'

export type PanelContext = ReturnType<typeof usePanelController>

const panelContextKey: InjectionKey<PanelContext> = Symbol('panel-context')

export function providePanelContext(context: PanelContext) {
  provide(panelContextKey, context)
}

export function usePanelContext() {
  const context = inject(panelContextKey)
  if (!context) throw new Error('Panel context is unavailable')
  return context
}
