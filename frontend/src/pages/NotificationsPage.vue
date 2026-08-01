<script setup lang="ts">
import { Bell, Check, Plus, Trash2, Webhook } from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'

const {
  clientControl,
  clientControlError,
  savingClientControl,
  testingWebhook,
  t,
  addWebhook,
  removeWebhook,
  saveClientControl,
  testWebhook,
} = usePanelContext()
</script>

<template>
  <section class="client-page notification-page">
    <form
      class="panel webhook-panel"
      autocomplete="off"
      @submit.prevent="saveClientControl"
    >
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Bell :size="18" />
            <h2>{{ t('通知配置') }}</h2>
          </div>
          <p class="muted">
            {{
              t(
                'Webhook 使用 POST JSON 发送：{ title, text }。命中通知包含播放频率屏蔽和 UA 拦截命中。',
              )
            }}
          </p>
        </div>
        <div class="panel-actions">
          <label class="check compact-check">
            <input v-model="clientControl.notify_enabled" type="checkbox" />
            <span>{{ t('命中通知') }}</span>
          </label>
          <button class="secondary" type="button" @click="addWebhook">
            <Plus :size="15" />{{ t('添加 Webhook') }}
          </button>
          <button
            class="primary"
            type="submit"
            :disabled="savingClientControl"
          >
            <Check :size="15" />{{
              savingClientControl ? t('保存中') : t('保存通知')
            }}
          </button>
        </div>
      </div>
      <div v-if="clientControlError" class="notice error" role="alert">
        {{ clientControlError }}
      </div>
      <div class="webhook-list">
        <article
          v-for="(webhook, index) in clientControl.webhooks"
          :key="webhook.id"
          class="webhook-item"
        >
          <div class="webhook-item-head">
            <label class="check compact-check">
              <input v-model="webhook.enabled" type="checkbox" />
              <span>{{ t('启用') }}</span>
            </label>
            <div class="rule-actions">
              <button
                class="secondary"
                type="button"
                :disabled="testingWebhook"
                @click="testWebhook(webhook)"
              >
                <Webhook :size="15" />{{
                  testingWebhook ? t('测试中') : t('测试连接')
                }}
              </button>
              <button
                class="danger-button"
                type="button"
                @click="removeWebhook(index)"
              >
                <Trash2 :size="15" />{{ t('删除') }}
              </button>
            </div>
          </div>
          <div class="grid webhook-grid">
            <label>
              <span>{{ t('名称') }}</span>
              <input
                v-model="webhook.name"
                :placeholder="t('例如：企业微信通知')"
              />
            </label>
            <label>
              <span>Webhook URL</span>
              <input
                v-model="webhook.url"
                placeholder="https://example.com/webhook"
              />
            </label>
            <label>
              <span>{{ t('密钥（可选）') }}</span>
              <input
                v-model="webhook.secret"
                type="password"
                :placeholder="t('可选密钥')"
              />
            </label>
          </div>
        </article>
      </div>
      <p class="muted rate-limit-help">
        {{
          t(
            '请求体固定为 {"title":"${title}","text":"${text}"}，密钥会通过 `X-Webhook-Secret` 头发送。',
          )
        }}
      </p>
    </form>
  </section>
</template>
