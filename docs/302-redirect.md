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

播放日志会记录两类状态：

```text
服务器名 + 播放用户名 + IP + 完整时间 + 重定向 strm(缓存30m) + 完整直链 URL
服务器名 + 播放用户名 + IP + 完整时间 + 直链缓存命中 + 完整直链 URL
```

如果该服务器未开启直链缓存，则日志显示：

```text
重定向 strm
```

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
- 直链 URL 可能包含临时授权参数，播放日志会显示完整 URL，只有管理员可查看。
