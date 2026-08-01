<script setup lang="ts">
import { Moon, Sun } from '@lucide/vue'
import LanguageMenu from '@/components/ui/LanguageMenu.vue'

type Locale = 'zh-CN' | 'en-US'

defineProps<{
  locale: Locale
  darkMode: boolean
  languageLabel: string
  themeLabel: string
  themeTitle: string
}>()

const emit = defineEmits<{
  selectLocale: [locale: Locale]
  toggleTheme: []
}>()
</script>

<template>
  <div class="preference-controls">
    <div class="language-picker">
      <LanguageMenu :locale="locale" :label="languageLabel" @select="emit('selectLocale', $event)" />
    </div>
    <button
      class="theme-toggle icon-button"
      type="button"
      :aria-label="themeLabel"
      :title="themeTitle"
      @click="emit('toggleTheme')"
    >
      <Sun v-if="!darkMode" :size="16" />
      <Moon v-else :size="16" />
    </button>
  </div>
</template>
