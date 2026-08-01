<script setup lang="ts">
import { Check, ChevronDown, Languages } from '@lucide/vue'
import {
  DropdownMenuContent,
  DropdownMenuPortal,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuRoot,
  DropdownMenuTrigger,
} from 'reka-ui'

type Locale = 'zh-CN' | 'en-US'

defineProps<{
  locale: Locale
  label: string
}>()

const emit = defineEmits<{
  select: [locale: Locale]
}>()

function selectLocale(value: unknown) {
  if (value === 'zh-CN' || value === 'en-US') emit('select', value)
}
</script>

<template>
  <DropdownMenuRoot>
    <DropdownMenuTrigger as-child>
      <button class="language-trigger" type="button" :aria-label="label">
        <Languages :size="16" />
        <span>{{ locale === 'zh-CN' ? '简体中文' : 'English' }}</span>
        <ChevronDown :size="14" />
      </button>
    </DropdownMenuTrigger>
    <DropdownMenuPortal>
      <DropdownMenuContent
        align="end"
        :side-offset="8"
        class="language-menu reka-language-menu z-[80] outline-none"
      >
        <DropdownMenuRadioGroup :model-value="locale" @update:model-value="selectLocale">
          <DropdownMenuRadioItem
            value="zh-CN"
            class="language-menu-item"
            :class="{ selected: locale === 'zh-CN' }"
          >
            <span>简体中文</span>
            <Check v-if="locale === 'zh-CN'" :size="15" />
          </DropdownMenuRadioItem>
          <DropdownMenuRadioItem
            value="en-US"
            class="language-menu-item"
            :class="{ selected: locale === 'en-US' }"
          >
            <span>English</span>
            <Check v-if="locale === 'en-US'" :size="15" />
          </DropdownMenuRadioItem>
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenuPortal>
  </DropdownMenuRoot>
</template>
