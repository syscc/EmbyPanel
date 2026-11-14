# 302 直链反代原理

EmbyPanel 的核心逻辑不是代理媒体文件本身，而是在识别到 STRM 直链后返回 HTTP `302 Found`，让播放器直接去访问真实媒体地址。

## 请求流程

```text
Emby 客户端
  ↓
EmbyPanel 反代端口
  ↓
Emby 原始服务器
  ↓
获取媒体源 Path
  ↓
判断是否可以直链
  ↓
返回 302 Location: 真实直链
  ↓
播放器直接访问真实直链
```

客户端仍然把 EmbyPanel 当作 Emby 服务器访问。EmbyPanel 会代理普通 Emby API 请求，只在播放 STRM 媒体时处理直链跳转。

## PlaybackInfo 改写

客户端播放前通常会请求：

```text
/Items/{item_id}/PlaybackInfo
```

EmbyPanel 会转发这个请求到 Emby，并改写返回内容，让 STRM 媒体尽量走 DirectPlay/DirectStream，避免转码。

这一步只负责让客户端后续请求视频流地址，不做播放频率限制计数。

## 视频流请求

播放器随后会请求：

```text
/videos/{item_id}/stream
/videos/{item_id}/original
```

EmbyPanel 在这个阶段读取 Emby 媒体源 Path：

- 如果 Path 是 `http://` 或 `https://`，直接返回 `302`。
- 如果 Path 是 OpenList `/d/...` 地址，并且配置了 OpenList，则调用 OpenList API 获取 `raw_url` 后返回 `302`。
- 如果 Path 不是可直链地址，则回落为普通 Emby 反代。

返回示例：

```http
HTTP/1.1 302 Found
Location: https://example.com/direct/file.mkv
```

## OpenList 处理

当 STRM 路径是 OpenList 下载路径时，EmbyPanel 会：

1. 从 URL 中提取 `/d/...` 后面的文件路径。
2. 请求 OpenList `/api/fs/get`。
3. 读取返回中的 `raw_url`。
4. 校验 `raw_url` 必须是 `http` 或 `https`。
5. 返回 `302 Location: raw_url`。

如果 OpenList 没有返回有效 `raw_url`，请求会失败或回落到 Emby 原始反代。

## 直链缓存

直链缓存只缓存最终重定向 URL，不缓存媒体内容。

缓存命中时流程变成：

```text
/videos/{item_id}/stream
  ↓
查询缓存
  ↓
命中直链
  ↓
直接返回 302
```

缓存受这些配置影响：

- 缓存秒数：控制直链保存多久。
- 缓存最大条数：超过后自动淘汰旧缓存。
- 缓存过滤域名：支持白名单或黑名单，只匹配直链域名部分。

## 内部重定向解析

如果开启内部重定向解析，EmbyPanel 会先对准备返回的直链发起 `HEAD` 请求，并跟随跳转，拿到最终 URL 后再返回给播放器。

```text
原始直链
  ↓ HEAD
中间跳转
  ↓
最终直链
  ↓
302 给播放器
```

这个功能适合直链本身还会继续跳转的场景，但会增加一次请求耗时。

## 播放日志

播放日志记录面板直链重定向、直链缓存命中，以及可确认的 GET 媒体入口回源请求；不是只显示 302：

```text
服务器名 + 播放用户名 + IP + 完整时间 + 重定向 strm(缓存30m) + 脱敏直链 URL
服务器名 + 播放用户名 + IP + 完整时间 + 直链缓存命中 + 脱敏直链 URL
服务器名 + 播放用户名 + IP + 完整时间 + 服务器代理播放 + 实际 HTTP 状态
服务器名 + 播放用户名 + IP + 完整时间 + 服务器代理播放失败 + 实际 HTTP 状态（网络失败为502）
```

如果该服务器未开启直链缓存，则日志显示：

```text
重定向 strm
```

回源日志在收到响应头时写入，不等待媒体流读完，不表示客户端已开始或完成播放。2xx 使用信息级别，4xx 使用警告，5xx 与连接失败使用错误；慢请求不会另追加一条运行活动日志。请求关联 ID 可连接请求明细和已有视频流诊断。回源不增加直链或缓存命中统计，上游自身 3xx 保持原运行日志语义。

播放日志显示独立请求关联 ID，技术详情保留 `request_id=diag-...` 前缀。回源详情还包含脱敏的 HTTP 入口 `path` 和可选请求 `range`，例如 `request_id=diag-... status=206 duration=12ms upstream_response_headers_received media_stream_not_measured path=/Videos/id/stream range=bytes=0-1023`。path 仅来自请求 URI 路径，不含查询串，清洗凭据及控制字符并限长 512 字符；Range 仅允许最多 256 字节的 `bytes` 数字区间，非法内容以 `[invalid]` 代替，无 Range 时不显示该字段。这些字段用于区分播放器的多个真实请求，原始记录不合并或去重；同一标题的不同 Range 请求有各自的关联 ID。直链重定向不追加 path/Range，脱敏 URL 的展示和复制契约保持不变。

当前日志入口覆盖 `/Videos/{id}/stream`、`original`（含 `.mkv`、`.mp4` 等扩展）、`master.m3u8`，及 `/Audio/{id}/stream`、`original`、`master.m3u8`、`universal`；大小写不敏感，兼容 `/emby` 前缀。此判定不改变原直链路由或播放策略；用户名继续只复用明确用户名和设备缓存，未知用户显示 `--`。HEAD、OPTIONS、PlaybackInfo、普通 API、封面、字幕和 HLS 的 `hls1/*.ts` / `.m4s` 分片不新增播放日志，避免将每个分片当成一次播放；未列出的协议或入口未确认覆盖。每个媒体/Range入口请求各记录一次，而不是按完整观看会话去重；拦截仍归拦截日志。

### 同一播放的折叠展示

身份上下文完整且一致的播放请求在页面默认折叠成一张摘要卡；例如同一播放的三个 Range 请求显示一张卡及“当前已加载请求数：3”。展开可逐条查看原始时间、级别、媒体、用户、IP、技术详情及关联 ID，直链继续沿用现有脱敏展示和保留编码的复制入口。摘要时间是最近请求时间，不表示播放开始或播放成功。警告和错误始终独立展示，缺少身份的旧记录也保持独立。

后端仅为确认的 GET 播放入口提供可选 `playback_group_id`，格式为 `play-` 加 64 位小写十六进制 SHA256。固定版本域与长度前缀隔离服务器/配置/item、明确的 PlaySessionId、DeviceId、MediaSourceId、可得 UserId 及请求认证身份摘要；UserId 与认证身份至少存在一项。没有 UserId 但带有效认证身份的请求可分组。解析出的显示用户名（包括补查失败时的 `--`）不参与分组输入，用户名补查恢复或变化不会拆散同一播放；每条记录仍保留各自当时的用户名。必需上下文缺失、空白、含控制字符或单项超过 1024 字节时不生成标识；存储边界拒绝格式不合法的标识。不输出原始会话/设备/认证输入或中间摘要，不增加网络查询。身份或配置改变会保守拆组；标识仅用于展示，绝不用于鉴权。

页面在授权和当前筛选结果内，按有效标识与服务器 ID 分组，仅纳入 success/info 播放记录。顶部和底部同时显示条目数及已加载原始请求数；这些数字不是完整会话请求总数。刷新仍替换原始快照，同一组保留稳定展开节点；加载更多仍按原始记录数判断。CSV 按当前筛选导出最多 500 条原始记录（并非仅当前加载页），不按显示卡片合并、不增加分组字段列。请求计数、持久请求明细及逐请求诊断不变，无数据库迁移或历史补填。

### 媒体摘要

播放日志的可选 `media` 对象包含 `media_id`、`name`、`series_name`、`season_number`、`episode_number`。页面独立显示电影名或“剧名 · S01E22 · 本集标题”，0 季/0 集保留；媒体摘要不改变直链 URL 展示/复制，回源技术 detail 的请求上下文见上文。标题来自 Emby `Items` 的真实 `Name` / `SeriesName` / `ParentIndexNumber` / `IndexNumber`，不使用媒体源 `Name`（通常是质量/版本）、文件路径或 ID 猜测片名。季集号仅用于 Items `Type=Episode`；旧响应缺少 Type 且有非空 SeriesName 时兼容为剧集，显式 Audio/Movie 等类型不将碟号/曲号显示成季集。元数据进入日志前统一脱敏、去除控制字符并将文本限制为 256 字符；Name/SeriesName 使用媒体标签上下文清洗，保留 `Basic Instinct`、`The Secret: Dare to Dream` 等自然语言标题，不把裸 Basic/Bearer 或普通名词引导的副标题视为凭据；URL 凭据、明确 Authorization/Token/API Key/password、敏感键的等号赋值和 JSON 字段继续清理。ID 与其它诊断文本保持原严格规则；不保存原始 Emby JSON 或 Path。

视频流复用既有 Items 查询，不增加查询；取得标签时预热独立的内存媒体缓存。缓存最多 4096 项，已知名称固定保留 10 分钟，未知/失败/超时负缓存 15 秒；按服务器稳定 ID 和 Emby 地址/配置身份隔离，地址或凭据变更不会串用旧标题。同服务器、同媒体的 generic 冷缓存请求合并为一次 Items 补查，最长等待 500ms（包括响应解析），失败或超时继续原回源；因此冷 HLS master/Audio 入口可能多一次查询及至多约 500ms 等待，并非零请求。Range 后续请求复用缓存，HLS 分片、HEAD、OPTIONS、API 和 PlaybackInfo 不新增媒体补查；原 video HEAD/OPTIONS 的路径解析行为不变。

302 与直链缓存命中也携带已知标签；缓存命中只读标签缓存，绝不为显示名称探测网络。标签已过期、原始数据无名称或历史日志无元数据时显示“媒体信息未知”，不能追补历史。媒体名称、剧名及明确 ID 可参与授权范围内的关键词搜索；CSV 在原九列之后追加 `media_id,media_name,series_name,season_number,episode_number`，运行日志对应列为空。无数据库迁移。

### 反代请求视图与播放请求

播放请求（`path_type=video_stream`）按播放会话折叠展示在播放日志中，反代请求视图默认不再重复列出它们。请求明细查询携带 `exclude_path_type=video_stream`，该排除在 SQL 的 `LIMIT` 之前生效，因此分页与计数仍与实际展示的行一致。

只有「全部请求」筛选携带该排除参数；明确选择「视频流」或其它请求类型时不发送，下拉框中的「视频流」选项保留，诊断深链（`path_type=video_stream` + 关联 ID 关键词）仍能查到原始播放请求。拦截日志视图使用独立端点，不携带该参数，被拦截的播放请求照常显示。原始行不删除、不合并，播放日志分组、CSV、诊断与请求计数语义不变。

## 播放频率限制

播放频率限制只按视频流请求计数，也就是：

```text
/videos/{item_id}/stream
/videos/{item_id}/original
```

`PlaybackInfo` 不参与计数，避免同一次播放被算两次。

如果触发屏蔽，后续访问 EmbyPanel 反代端口会被拦截。但如果播放器已经拿到了真实直链，已经发出去的直链无法被 EmbyPanel 立刻收回，只能拦截下一次经过 EmbyPanel 的播放请求。

## 回落条件

以下情况不会返回 302，会回落为普通 Emby 反代：

- 媒体源不是 STRM 直链。
- 媒体源 Path 不是 `http://` 或 `https://`。
- OpenList 未配置，且路径无法直接访问。
- 找不到媒体源。
- 请求是 `HEAD`。
- 内部重定向解析失败时，会继续使用原始直链或按错误处理。

## 注意事项

- 302 模式省服务器带宽和 CPU，因为媒体数据不经过 EmbyPanel。
- 302 模式无法控制已经交给播放器的真实直链。
- 如果需要封禁后立刻断流，不能使用纯 302 直链模式，需要改成媒体流中转代理。
- 直链 URL 可能包含临时授权参数，日志只展示脱敏 URL；回源播放日志不保存请求查询串、凭据、Location、媒体文件路径或上游错误正文。
