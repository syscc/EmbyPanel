<script setup lang="ts">
import { Archive, Download, Upload } from '@lucide/vue'
import { usePanelContext } from '@/composables/panel-context'

const {
  backupError,
  backupFileInput,
  t,
  exportBackup,
  importBackup,
  handleBackupFileSelected,
} = usePanelContext()
</script>

<template>
  <section class="backup-page">
    <section class="panel">
      <div class="panel-head">
        <div>
          <div class="panel-title-line">
            <Archive :size="18" />
            <h2>{{ t('配置备份') }}</h2>
          </div>
          <p class="muted">
            {{ t('导出配置文件，或从电脑选择配置文件还原运行配置。') }}
          </p>
        </div>
      </div>
      <input
        ref="backupFileInput"
        hidden
        type="file"
        accept=".json,application/json,text/plain"
        @change="handleBackupFileSelected"
      />
      <div v-if="backupError" class="notice error" role="alert">
        {{ backupError }}
      </div>
      <div class="backup-layout">
        <section class="backup-card">
          <h3>{{ t('备份范围') }}</h3>
          <div class="backup-scope-grid">
            <div>
              <strong>{{ t('服务器配置') }}</strong>
              <span>{{
                t('Emby 地址、API Key、反代端口、真实 IP、缓存和映射规则')
              }}</span>
            </div>
            <div>
              <strong>{{ t('客户端管控') }}</strong>
              <span>{{
                t('UA 拦截、播放频率限制、封禁列表和客户端规则')
              }}</span>
            </div>
            <div>
              <strong>{{ t('通知配置') }}</strong>
              <span>{{ t('Webhook 地址、启用状态和密钥') }}</span>
            </div>
            <div>
              <strong>{{ t('日志配置') }}</strong>
              <span>{{ t('日志级别、文件大小、保留数量和格式') }}</span>
            </div>
          </div>
          <p class="muted backup-note">
            {{
              t(
                '备份文件会使用备份密码加密；不包含面板管理员用户名、密码、登录会话、运行日志文件和请求统计数据。',
              )
            }}
          </p>
        </section>

        <section class="backup-card">
          <h3>{{ t('配置文件备份 / 还原') }}</h3>
          <div class="backup-actions text-actions">
            <button class="secondary" @click="exportBackup">
              <Download :size="15" />{{ t('导出备份') }}
            </button>
            <button class="primary" @click="importBackup">
              <Upload :size="15" />{{ t('还原') }}
            </button>
          </div>
          <div class="backup-drop-hint">
            <strong>{{ t('导出备份') }}</strong>
            <span>{{
              t(
                '输入备份密码后生成加密的 `embypanel-config-时间.json` 并弹出浏览器下载。',
              )
            }}</span>
            <strong>{{ t('还原') }}</strong>
            <span>{{
              t(
                '点击后选择本机配置文件，加密备份需要输入对应密码，读取成功后自动还原并差量同步受影响的反代监听器。',
              )
            }}</span>
          </div>
        </section>
      </div>
    </section>
  </section>
</template>
