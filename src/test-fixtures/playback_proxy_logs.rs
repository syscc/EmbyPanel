mod playback_proxy_logs {
    use super::*;

    const GROUP_QUERY: &str = "PlaySessionId=group-session-fixture&DeviceId=group-device-fixture&MediaSourceId=group-source-fixture&api_key=group-token-fixture&UserName=Fixture";

    #[test]
    fn playback_group_identity_is_partitioned_bounded_and_get_only() {
        let config = with_trusted_proxy(test_config("http://example.invalid", "http://unused.invalid", 18096));
        let headers = axum::http::HeaderMap::new();
        let uri: Uri = format!("/Videos/id/stream?{GROUP_QUERY}").parse().unwrap();
        let base = playback_group_id(&config, &Method::GET, &uri, &headers).unwrap();
        assert!(base.starts_with("play-") && base.len() == 69);
        assert!(base[5..].bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)));
        for (from, to) in [
            ("group-session-fixture", "other-session"), ("group-device-fixture", "other-device"),
            ("group-source-fixture", "other-source"), ("group-token-fixture", "other-token"),
            ("/id/", "/other/"),
        ] {
            let changed = uri.to_string().replace(from, to).parse().unwrap();
            assert_ne!(playback_group_id(&config, &Method::GET, &changed, &headers), Some(base.clone()));
        }
        // The resolved display name is not a grouping input: a failed lookup ("--")
        // and a recovered name must not split one playback. The helper takes no
        // display name at all, so the query's UserName cannot change the key.
        for user in ["Fixture Two", "--"] {
            let renamed = uri.to_string().replace("UserName=Fixture", &format!("UserName={}", user.replace(' ', "+"))).parse().unwrap();
            assert_eq!(playback_group_id(&config, &Method::GET, &renamed, &headers), Some(base.clone()));
        }
        let with_user = |user: &str, token: &str| -> Uri {
            format!("/Videos/id/stream?PlaySessionId=session&DeviceId=device&MediaSourceId=source&UserId={user}&api_key={token}").parse().unwrap()
        };
        let user_base = playback_group_id(&config, &Method::GET, &with_user("user-a", "token-a"), &headers);
        assert!(user_base.is_some());
        assert_ne!(user_base, playback_group_id(&config, &Method::GET, &with_user("user-b", "token-a"), &headers));
        assert_ne!(user_base, playback_group_id(&config, &Method::GET, &with_user("user-a", "token-b"), &headers));
        let only_user: Uri = "/Videos/id/stream?PlaySessionId=session&DeviceId=device&MediaSourceId=source&UserId=user-a".parse().unwrap();
        assert!(playback_group_id(&config, &Method::GET, &only_user, &headers).is_some());
        for field in ["host", "key", "server"] {
            let mut changed = config.clone();
            match field {
                "host" => changed.emby_host = "http://other.invalid".into(),
                "key" => changed.emby_api_key = "other-config-key".into(),
                _ => changed.servers[0].id = "other-server".into(),
            }
            assert_ne!(playback_group_id(&changed, &Method::GET, &uri, &headers), Some(base.clone()));
        }
        for field in ["PlaySessionId", "DeviceId", "MediaSourceId", "api_key"] {
            let params: Vec<_> = url::form_urlencoded::parse(GROUP_QUERY.as_bytes()).into_owned().collect();
            for bad in [None, Some("".to_string()), Some(" ".to_string()), Some("bad\nidentity".to_string()), Some("x".repeat(1025))] {
                let mut query = url::form_urlencoded::Serializer::new(String::new());
                for (name, value) in &params {
                    if name == field {
                        if let Some(bad) = &bad { query.append_pair(name, bad); }
                    } else { query.append_pair(name, value); }
                }
                let invalid: Uri = format!("/Videos/id/stream?{}", query.finish()).parse().unwrap();
                assert!(playback_group_id(&config, &Method::GET, &invalid, &headers).is_none());
            }
        }
        for size in [1024, 1025] {
            let bounded: Uri = format!("/Videos/id/stream?{GROUP_QUERY}&UserId={}", "x".repeat(size)).parse().unwrap();
            assert_eq!(playback_group_id(&config, &Method::GET, &bounded, &headers).is_some(), size == 1024);
        }
        assert!(playback_group_id(&config, &Method::HEAD, &uri, &headers).is_none());
        for path in ["/Videos/id/hls1/main/1.ts", "/Videos/id/subtitles/1/stream.srt", "/Items/id/Images/Primary", "/Items/id/PlaybackInfo"] {
            let uri = format!("{path}?{GROUP_QUERY}").parse().unwrap();
            assert!(playback_group_id(&config, &Method::GET, &uri, &headers).is_none());
        }
        for path in ["/emby/Audio/id/stream.mp3", "/Videos/id/master.m3u8", "/Audio/id/universal"] {
            let uri = format!("{path}?{GROUP_QUERY}").parse().unwrap();
            assert_eq!(playback_group_id(&config, &Method::GET, &uri, &headers), Some(base.clone()));
        }
    }

    #[tokio::test]
    async fn grouped_ranges_preserve_all_requests_stats_and_csv_rows() {
        for path in ["/Videos/id/stream", "/Audio/id/stream.mp3", "/Videos/id/master.m3u8"] {
            let forwards = Arc::new(AtomicUsize::new(0));
            let count = forwards.clone();
            let emby = spawn_mock_server(move |request| {
                let count = count.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        return response_json(json!({"Items":[{"Name":"Grouped title","MediaSources":[]}]}));
                    }
                    let index = count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(request.headers()[header::RANGE], ["bytes=0-99", "bytes=100-199", "bytes=200-"][index]);
                    (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, "bytes 0-99/400"), (header::ACCEPT_RANGES, "bytes")], "stream").into_response()
                }
            }).await;
            let store = test_store();
            let owner = store.list_admin_users().unwrap()[0].id;
            store.create_session(owner, &auth::hash_token("group-export-fixture"), auth::now_ts() + 3600).unwrap();
            let state = test_state(test_config(&emby, "http://unused.invalid", 18096), store);
            let app = build_app(state.clone());
            for range in ["bytes=0-99", "bytes=100-199", "bytes=200-"] {
                let response = app.clone().oneshot(HttpRequest::builder().uri(format!("{path}?{GROUP_QUERY}"))
                    .header(header::RANGE, range).body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
                assert_eq!(read_text(response).await, "stream");
            }
            let rows = entries(&state);
            assert_eq!(rows.len(), 3);
            let group = rows[0].playback_group_id.as_ref().unwrap();
            assert!(rows.iter().all(|row| row.playback_group_id.as_ref() == Some(group)));
            let ids: std::collections::HashSet<_> = rows.iter().map(|row| row.detail.split_whitespace().next().unwrap()).collect();
            assert_eq!(ids.len(), 3);
            assert_eq!(forwards.load(Ordering::SeqCst), 3);
            let dto = serde_json::to_string(&rows).unwrap();
            for value in ["group-session-fixture", "group-device-fixture", "group-token-fixture"] { assert!(!dto.contains(value)); }
            let csv_response = build_management_app(state.clone()).oneshot(HttpRequest::builder()
                .uri("/api/monitoring/logs/export?kind=playback")
                .header(header::AUTHORIZATION, "Bearer group-export-fixture").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(csv_response.status(), StatusCode::OK);
            let csv = read_text(csv_response).await;
            assert_eq!(csv.lines().count(), 4);
            assert_eq!(csv.lines().next().unwrap(), "id,time,kind,level,server,user,ip,message,detail,media_id,media_name,series_name,season_number,episode_number");
            assert!(!csv.contains(group));
            state.telemetry_writes.shutdown().unwrap();
            let details = state.settings_store.list_proxy_request_details(db::ProxyRequestDetailFilter {
                server_id: None, path_type: None, exclude_path_type: None, keyword: None, since_ms: None, until_ms: None, limit: 20,
            }).unwrap();
            assert_eq!(details.len(), 3);
        }
    }

    #[tokio::test]
    async fn grouped_requests_survive_user_name_lookup_failure_and_recovery() {
        let user_lookups = Arc::new(AtomicUsize::new(0));
        let forwards = Arc::new(AtomicUsize::new(0));
        let emby = spawn_mock_server({
            let user_lookups = user_lookups.clone();
            let forwards = forwards.clone();
            move |request| {
                let user_lookups = user_lookups.clone();
                let forwards = forwards.clone();
                async move {
                    match request.uri().path() {
                        "/Items" => response_json(json!({"Items":[{"Name":"Grouped title","MediaSources":[{"Id":"group-source-fixture","Path":"/media/local.mkv"}]}]})),
                        "/Users/group-user-fixture" => {
                            let index = user_lookups.fetch_add(1, Ordering::SeqCst);
                            if index == 0 {
                                response_text(StatusCode::INTERNAL_SERVER_ERROR, "user lookup unavailable")
                            } else {
                                response_json(json!({"Name":"Fixture"}))
                            }
                        }
                        "/Users/Me" => response_text(StatusCode::NOT_FOUND, "no token user"),
                        "/Sessions" => response_json(json!([])),
                        _ => {
                            let index = forwards.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(request.headers()[header::RANGE], ["bytes=0-99", "bytes=100-199", "bytes=200-"][index]);
                            (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, "bytes 0-99/400"), (header::ACCEPT_RANGES, "bytes")], "stream").into_response()
                        }
                    }
                }
            }
        }).await;
        let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
        let app = build_app(state.clone());
        for range in ["bytes=0-99", "bytes=100-199", "bytes=200-"] {
            let response = app.clone().oneshot(HttpRequest::builder()
                .uri("/Videos/id/stream?PlaySessionId=group-session-fixture&DeviceId=group-device-fixture&MediaSourceId=group-source-fixture&UserId=group-user-fixture")
                .header(header::RANGE, range)
                .header("X-Emby-Token", "group-token-fixture")
                .body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(read_text(response).await, "stream");
        }
        let rows = entries(&state);
        assert_eq!(rows.len(), 3);
        let group = rows[0].playback_group_id.as_ref().unwrap();
        assert!(rows.iter().all(|row| row.playback_group_id.as_ref() == Some(group)));
        let ids: std::collections::HashSet<_> = rows.iter().map(|row| row.detail.split_whitespace().next().unwrap()).collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(forwards.load(Ordering::SeqCst), 3);
        // The first lookup failed, later ones recovered; the display name changed but
        // the stable identity kept one group.
        let users: std::collections::HashSet<_> = rows.iter().map(|row| row.playback_user.clone().unwrap()).collect();
        assert_eq!(users, std::collections::HashSet::from(["--".to_string(), "Fixture".to_string()]));
        let dto = serde_json::to_string(&rows).unwrap();
        for value in ["group-session-fixture", "group-device-fixture", "group-token-fixture", "group-user-fixture"] {
            assert!(!dto.contains(value), "{value} leaked");
        }
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn grouped_token_only_requests_survive_user_name_lookup_recovery() {
        let user_lookups = Arc::new(AtomicUsize::new(0));
        let forwards = Arc::new(AtomicUsize::new(0));
        let emby = spawn_mock_server({
            let user_lookups = user_lookups.clone();
            let forwards = forwards.clone();
            move |request| {
                let user_lookups = user_lookups.clone();
                let forwards = forwards.clone();
                async move {
                    match request.uri().path() {
                        "/Items" => response_json(json!({"Items":[{"Name":"Grouped title","MediaSources":[{"Id":"group-source-fixture","Path":"/media/local.mkv"}]}]})),
                        "/Users/Me" => {
                            let index = user_lookups.fetch_add(1, Ordering::SeqCst);
                            if index == 0 {
                                response_text(StatusCode::NOT_FOUND, "no token user")
                            } else {
                                response_json(json!({"Name":"Fixture"}))
                            }
                        }
                        "/Users/group-user-fixture" => response_text(StatusCode::NOT_FOUND, "no user id"),
                        "/Sessions" => response_json(json!([])),
                        _ => {
                            let index = forwards.fetch_add(1, Ordering::SeqCst);
                            assert_eq!(request.headers()[header::RANGE], ["bytes=0-99", "bytes=100-199", "bytes=200-"][index]);
                            (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, "bytes 0-99/400"), (header::ACCEPT_RANGES, "bytes")], "stream").into_response()
                        }
                    }
                }
            }
        }).await;
        let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
        let app = build_app(state.clone());
        for range in ["bytes=0-99", "bytes=100-199", "bytes=200-"] {
            let response = app.clone().oneshot(HttpRequest::builder()
                .uri("/Videos/id/stream?PlaySessionId=group-session-fixture&DeviceId=group-device-fixture&MediaSourceId=group-source-fixture")
                .header(header::RANGE, range)
                .header("X-Emby-Token", "group-token-fixture")
                .body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(read_text(response).await, "stream");
        }
        let rows = entries(&state);
        assert_eq!(rows.len(), 3);
        let group = rows[0].playback_group_id.as_ref().unwrap();
        assert!(rows.iter().all(|row| row.playback_group_id.as_ref() == Some(group)));
        assert_eq!(forwards.load(Ordering::SeqCst), 3);
        let users: std::collections::HashSet<_> = rows.iter().map(|row| row.playback_user.clone().unwrap()).collect();
        assert_eq!(users, std::collections::HashSet::from(["--".to_string(), "Fixture".to_string()]));
        state.telemetry_writes.shutdown().unwrap();
    }

    #[test]
    fn grouping_reuses_auth_and_context_headers_without_exposing_values() {
        let config = test_config("http://example.invalid", "http://unused.invalid", 18096);
        let uri: Uri = "/Audio/id/stream.mp3?PlaySessionId=session&MediaSourceId=source".parse().unwrap();
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("X-Emby-Device-Id", "device".parse().unwrap());
        headers.insert("X-Emby-Token", "synthetic-token".parse().unwrap());
        let expected = playback_group_id(&config, &Method::GET, &uri, &headers);
        assert!(expected.is_some());
        headers.remove("X-Emby-Token");
        headers.insert(header::AUTHORIZATION, "MediaBrowser Token=\"synthetic-token\"".parse().unwrap());
        assert_eq!(playback_group_id(&config, &Method::GET, &uri, &headers), expected);
        headers.insert("X-Emby-Device-Id", "device\t".parse().unwrap());
        assert!(playback_group_id(&config, &Method::GET, &uri, &headers).is_none());
        headers.insert("X-Emby-Device-Id", axum::http::HeaderValue::from_bytes(b"device\x80").unwrap());
        assert!(playback_group_id(&config, &Method::GET, &uri, &headers).is_none());
    }

    #[tokio::test]
    async fn grouped_failure_requests_keep_raw_levels_and_details() {
        for status in [StatusCode::NOT_FOUND, StatusCode::INTERNAL_SERVER_ERROR, StatusCode::BAD_GATEWAY] {
            let host = if status == StatusCode::BAD_GATEWAY {
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                format!("http://{}", listener.local_addr().unwrap())
            } else {
                spawn_mock_server(move |request| async move {
                    if request.uri().path() == "/Items" { return response_json(json!({"Items":[]})); }
                    response_text(status, "synthetic-failure-body")
                }).await
            };
            let state = test_state(test_config(&host, "http://unused.invalid", 18096), test_store());
            let app = build_app(state.clone());
            for range in ["bytes=0-99", "bytes=100-"] {
                let response = app.clone().oneshot(HttpRequest::builder().uri(format!("/Audio/id/stream.mp3?{GROUP_QUERY}"))
                    .header(header::RANGE, range).body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(response.status(), status);
            }
            let rows = entries(&state);
            assert_eq!(rows.len(), 2);
            assert!(rows[0].playback_group_id.is_some());
            assert_eq!(rows[0].playback_group_id, rows[1].playback_group_id);
            assert!(rows.iter().all(|row| row.level == if status.is_client_error() { "warn" } else { "error" }));
            assert!(rows.iter().all(|row| row.message == "服务器代理播放失败" && row.detail.contains(&format!("status={}", status.as_u16()))));
            assert_ne!(rows[0].id, rows[1].id);
            state.telemetry_writes.shutdown().unwrap();
        }
    }

    fn entries(state: &AppState) -> Vec<activity_log::ActivityLogEntry> {
        state.activity_log.list(None, None, 100)
    }

    fn assert_playback(state: &AppState, status: u16, level: &str) -> String {
        let logs = entries(state);
        assert_eq!(logs.len(), 1, "one activity, not General + Playback");
        let log = &logs[0];
        assert_eq!(log.kind, "playback");
        assert_eq!(log.level, level);
        assert!(log.server_id.is_some());
        assert!(!log.server_name.is_empty());
        assert_eq!(log.playback_user.as_deref(), Some("Fixture"));
        assert_eq!(log.playback_ip.as_deref(), Some("127.0.0.1"));
        assert!(log.detail.contains(&format!("status={status}")));
        assert!(log.detail.contains("media_stream_not_measured"));
        assert!(log.detail.starts_with("request_id="));
        for secret in [
            "secret-query",
            "secret-auth",
            "private-media",
            "secret-body",
            "secret-location",
        ] {
            assert!(
                !log.detail.contains(secret),
                "secret in activity: {}",
                log.detail
            );
        }
        log.detail
            .split_whitespace()
            .next()
            .unwrap()
            .trim_start_matches("request_id=")
            .to_string()
    }

    #[tokio::test]
    async fn review_media_audio_disc_and_track_are_not_episode_numbers() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({"Items":[{"Type":"Audio", "Name":"Song", "ParentIndexNumber":1, "IndexNumber":5}]}));
            }
            response_text(StatusCode::OK, "audio")
        }).await;
        let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
        let response = build_app(state.clone()).oneshot(HttpRequest::builder()
            .uri("/Audio/id/stream.mp3?UserName=Fixture").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(read_text(response).await, "audio");
        let logs = entries(&state);
        let media = logs[0].media.as_ref().unwrap();
        assert_eq!(media.name.as_deref(), Some("Song"));
        assert_eq!(media.season_number, None);
        assert_eq!(media.episode_number, None);
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn review_media_video_proxy_keeps_title_and_episode_zero_only_for_episode() {
        for (kind, expected) in [("Movie", None), ("Episode", Some(0))] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let emby = spawn_mock_server(move |request| {
                let count = count.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        count.fetch_add(1, Ordering::SeqCst);
                        return response_json(json!({"Items":[{"Type":kind, "Name":"Basic Instinct", "SeriesName":"The Secret: Dare to Dream", "ParentIndexNumber":0,"IndexNumber":0,"MediaSources":[]}]}));
                    }
                    response_text(StatusCode::PARTIAL_CONTENT, "video")
                }
            }).await;
            let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
            let response = build_app(state.clone()).oneshot(HttpRequest::builder()
                .uri("/Videos/id/stream?UserName=Fixture").body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(read_text(response).await, "video");
            let logs = entries(&state);
            let media = logs[0].media.as_ref().unwrap();
            assert_eq!(media.name.as_deref(), Some("Basic Instinct"));
            assert_eq!(media.series_name.as_deref(), Some("The Secret: Dare to Dream"));
            assert_eq!(media.season_number, expected);
            assert_eq!(media.episode_number, expected);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            state.telemetry_writes.shutdown().unwrap();
        }
    }

    #[test]
    fn review_media_only_episode_type_or_legacy_series_has_season_numbers() {
        for (kind, series, expected) in [
            (Some("Episode"), None, true),
            (Some("Audio"), Some("Series"), false),
            (Some("Movie"), Some("Series"), false),
            (None, Some("Series"), true),
            (None, None, false),
            (None, Some(" "), false),
        ] {
            let resolution = emby::MediaPathResolution {
                item_type: kind.map(str::to_string), media_name: Some("Basic Instinct".into()),
                series_name: series.map(str::to_string), season_number: Some(0), episode_number: Some(0),
                ..Default::default()
            };
            let media = resolved_playback_media("id", &resolution);
            assert_eq!(media.name.as_deref(), Some("Basic Instinct"));
            assert_eq!(media.season_number, expected.then_some(0));
            assert_eq!(media.episode_number, expected.then_some(0));
        }
    }

    #[test]
    fn playback_entry_boundaries() {
        for path in [
            "/videos/id/stream",
            "/Videos/id/Stream.MKV",
            "/emby/Videos/id/Original.mp4",
            "/Videos/id/master.m3u8",
            "/Audio/id/stream.mp3",
            "/audio/id/original",
            "/Audio/id/master.m3u8",
            "/Audio/id/universal",
        ] {
            assert!(rewrite::is_playback_log_path(path), "{path}");
        }
        for path in [
            "/videos/id/hls1/main/1.ts",
            "/Videos/id/hls1/main/1.m4s",
            "/Videos/id/subtitles/1/stream.srt",
            "/Videos/id/Images/Primary",
            "/Items/id/PlaybackInfo",
            "/Videos/id/streaming.json",
            "/videos//stream",
            "/api/videos/id/stream",
            "/Videos/id/stream/segment.ts",
            "/Sessions",
        ] {
            assert!(!rewrite::is_playback_log_path(path), "{path}");
        }
    }

    #[tokio::test]
    async fn local_and_missing_media_paths_log_200_and_range_206() {
        let items_calls = Arc::new(AtomicUsize::new(0));
        for (path, media_path, status) in [
            (
                "/Videos/local/Original.MKV",
                Some("/private-media/local.mkv"),
                StatusCode::OK,
            ),
            (
                "/videos/missing/stream.mp4",
                None,
                StatusCode::PARTIAL_CONTENT,
            ),
        ] {
            let count = items_calls.clone();
            let emby = spawn_mock_server(move |request| {
                let count = count.clone();
                async move {
                if request.uri().path() == "/Items" {
                    count.fetch_add(1, Ordering::SeqCst);
                    return response_json(json!({"Items":[{"Name": if media_path.is_some() { "电影名" } else { "本集标题" }, "SeriesName": if media_path.is_some() { None } else { Some("剧名") }, "ParentIndexNumber": if media_path.is_some() { None } else { Some(1) }, "IndexNumber": if media_path.is_some() { None } else { Some(22) }, "MediaSources": media_path.map(|path| vec![json!({"Id":"source", "Name":"NOT A TITLE", "Path":path})]).unwrap_or_default()}]}));
                }
                assert_eq!(request.headers().get(header::RANGE).unwrap(), "bytes=2-5");
                (status, [(header::CONTENT_TYPE, "video/mp4"), (header::CONTENT_RANGE, "bytes 2-5/10"), (header::ACCEPT_RANGES, "bytes")], "body").into_response()
            }}).await;
            let state = test_state(
                test_config(&emby, "http://unused.invalid", 18096),
                test_store(),
            );
            let response = build_app(state.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri(format!(
                            "{path}?MediaSourceId=source&UserName=Fixture&api_key=secret-query"
                        ))
                        .header(header::RANGE, "bytes=2-5")
                        .header(header::AUTHORIZATION, "secret-auth")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
            assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
            assert_eq!(response.headers()[header::CONTENT_TYPE], "video/mp4");
            assert_eq!(read_text(response).await, "body");
            let id = assert_playback(&state, status.as_u16(), "info");
            let detail = entries(&state)[0].detail.clone();
            assert!(detail.contains(&format!(" path={path} range=bytes=2-5")));
            assert!(!detail.contains('?'));
            assert_eq!(entries(&state)[0].message, "服务器代理播放");
            assert_capture_correlated_logs(&state, &id);
            let media = entries(&state)[0].media.clone().unwrap();
            if media_path.is_some() {
                assert_eq!(media.name.as_deref(), Some("电影名"));
                assert!(media.series_name.is_none());
            } else {
                assert_eq!(media.name.as_deref(), Some("本集标题"));
                assert_eq!(media.series_name.as_deref(), Some("剧名"));
                assert_eq!(media.season_number, Some(1));
                assert_eq!(media.episode_number, Some(22));
            }
        }
        assert_eq!(items_calls.load(Ordering::SeqCst), 2, "reuse each video Items request");
    }

    #[tokio::test]
    async fn distinct_range_requests_have_distinct_ids_and_one_activity_each() {
        for path in ["/Videos/id/stream", "/Audio/id/stream.mp3"] {
            let forwards = Arc::new(AtomicUsize::new(0));
            let count = forwards.clone();
            let emby = spawn_mock_server(move |request| {
                let count = count.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        return response_json(json!({"Items":[{"Name":"Same title","MediaSources":[]}]}));
                    }
                    let index = count.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(request.headers()[header::RANGE], ["bytes=0-1023", "bytes=1024-2047"][index]);
                    (StatusCode::PARTIAL_CONTENT, [(header::CONTENT_RANGE, "bytes 0-1023/4096"), (header::ACCEPT_RANGES, "bytes")], "stream").into_response()
                }
            }).await;
            let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
            let app = build_app(state.clone());
            let mut ids = std::collections::HashSet::new();
            for (index, range) in ["bytes=0-1023", "bytes=1024-2047"].into_iter().enumerate() {
                let response = app.clone().oneshot(HttpRequest::builder()
                    .uri(format!("{path}?UserName=Fixture&api_key=secret-query&opaque=secret-query"))
                    .header(header::RANGE, range)
                    .header(header::AUTHORIZATION, "secret-auth")
                    .body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
                assert_eq!(read_text(response).await, "stream");
                let logs = entries(&state);
                assert_eq!(logs.len(), index + 1, "one activity per HTTP request");
                let log = &logs[0];
                assert_eq!(log.kind, "playback");
                assert_eq!(log.media.as_ref().unwrap().name.as_deref(), Some("Same title"));
                assert!(log.detail.starts_with("request_id=diag-"));
                assert!(ids.insert(log.detail.split_whitespace().next().unwrap().to_string()));
                assert!(log.detail.contains("status=206"));
                assert!(log.detail.contains("upstream_response_headers_received media_stream_not_measured"));
                assert!(log.detail.ends_with(&format!(" path={path} range={range}")));
                assert!(!log.detail.contains("secret-"));
                assert!(!log.detail.contains('?'));
            }
            assert_eq!(forwards.load(Ordering::SeqCst), 2);
            state.telemetry_writes.shutdown().unwrap();
        }
    }

    #[tokio::test]
    async fn obs_text_range_is_invalid_in_successful_audio_playback_log() {
        for with_range in [false, true] {
            let emby = spawn_mock_server(move |request| async move {
                if request.uri().path() == "/Items" {
                    return response_json(json!({"Items":[]}));
                }
                assert_eq!(request.headers().get(header::RANGE).map(|value| value.as_bytes()),
                    with_range.then_some(b"bytes=0-\x80".as_slice()));
                response_text(StatusCode::OK, "audio")
            }).await;
            let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
            let mut request = HttpRequest::builder().uri("/Audio/id/stream.mp3?UserName=Fixture");
            if with_range {
                request = request.header(header::RANGE, axum::http::HeaderValue::from_bytes(b"bytes=0-\x80").unwrap());
            }
            let response = build_app(state.clone()).oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(read_text(response).await, "audio");
            assert_playback(&state, 200, "info");
            state.telemetry_writes.shutdown().unwrap();
            let detail = &entries(&state)[0].detail;
            assert!(detail.contains(" path=/Audio/id/stream.mp3"));
            assert!(detail.is_ascii());
            if with_range {
                assert!(detail.ends_with(" range=[invalid]"), "{detail}");
            } else {
                assert!(!detail.contains(" range="));
            }
        }
    }

    #[tokio::test]
    async fn obs_text_range_is_invalid_in_failed_audio_playback_log() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", socket.local_addr().unwrap());
        drop(socket);
        for with_range in [false, true] {
            let state = test_state(test_config(&host, "http://unused.invalid", 18096), test_store());
            let mut request = HttpRequest::builder().uri("/Audio/id/stream.mp3?UserName=Fixture");
            if with_range {
                request = request.header(header::RANGE, axum::http::HeaderValue::from_bytes(b"bytes=0-\x80").unwrap());
            }
            let response = build_app(state.clone()).oneshot(request.body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_playback(&state, 502, "error");
            state.telemetry_writes.shutdown().unwrap();
            let detail = &entries(&state)[0].detail;
            assert!(detail.contains(" path=/Audio/id/stream.mp3"));
            assert!(detail.is_ascii());
            if with_range {
                assert!(detail.ends_with(" range=[invalid]"), "{detail}");
            } else {
                assert!(!detail.contains(" range="));
            }
        }
    }

    #[tokio::test]
    async fn generic_master_and_audio_share_one_coalesced_metadata_lookup() {
        let calls = Arc::new(AtomicUsize::new(0));
        let emby = spawn_mock_server({
            let calls = calls.clone();
            move |request| {
                let calls = calls.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        calls.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(40)).await;
                        return response_json(json!({"Items":[{"Name":"电影名", "MediaSources":[{"Name":"2160p"}]}]}));
                    }
                    response_text(StatusCode::OK, "media")
                }
            }
        }).await;
        let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
        let app = build_app(state.clone());
        let mut requests = tokio::task::JoinSet::new();
        for path in ["/Videos/id/master.m3u8", "/Audio/id/stream.mp3", "/Audio/id/universal"] {
            let app = app.clone();
            requests.spawn(async move {
                let response = app.oneshot(HttpRequest::builder()
                    .uri(format!("{path}?UserName=Fixture"))
                    .body(Body::empty()).unwrap()).await.unwrap();
                assert_eq!(read_text(response).await, "media");
            });
        }
        while let Some(result) = requests.join_next().await { result.unwrap(); }
        let response = app.oneshot(HttpRequest::builder().uri("/Audio/id/universal?UserName=Fixture").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(read_text(response).await, "media");
        let logs = entries(&state);
        assert_eq!(logs.len(), 4);
        assert!(logs.iter().all(|entry| entry.detail.contains(" path=") && !entry.detail.contains(" range=")));
        assert!(logs.iter().all(|entry| entry.media.as_ref().unwrap().name.as_deref() == Some("电影名")));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn generic_metadata_timeout_and_failure_are_negative_cached_and_transparent() {
        for mode in ["failure", "timeout", "unknown"] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let emby = spawn_mock_server(move |request| {
                let count = count.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        count.fetch_add(1, Ordering::SeqCst);
                        if mode == "timeout" { tokio::time::sleep(Duration::from_secs(2)).await; }
                        if mode == "unknown" {
                            return response_json(json!({"Items":[{"MediaSources":[{"Name":"2160p version, not a title", "Path":"/private-media/file.mkv"}]}]}));
                        }
                        return response_text(StatusCode::INTERNAL_SERVER_ERROR, "secret-body");
                    }
                    response_text(StatusCode::PARTIAL_CONTENT, "media")
                }
            }).await;
            let state = test_state(test_config(&emby, "http://unused.invalid", 18096), test_store());
            let app = build_app(state.clone());
            let start = Instant::now();
            for _ in 0..2 {
                let response = tokio::time::timeout(Duration::from_millis(900), app.clone().oneshot(
                    HttpRequest::builder().uri("/Videos/id/master.m3u8?UserName=Fixture")
                        .body(Body::empty()).unwrap()
                )).await.unwrap().unwrap();
                assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                assert_eq!(read_text(response).await, "media");
            }
            assert!(start.elapsed() < Duration::from_millis(1200));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(entries(&state).iter().all(|entry| entry.media.is_none()));
            state.telemetry_writes.shutdown().unwrap();
        }
    }

    #[test]
    fn metadata_cache_expiry_is_bounded_for_known_and_unknown_results() {
        use moka::Expiry;
        let key = playback_media_key(&test_config("http://example.invalid", "http://unused.invalid", 18096), "id");
        let known = Some(PlaybackMedia { name: Some("Title".into()), ..Default::default() });
        let expiry = PlaybackMediaExpiry;
        let now = Instant::now();
        assert_eq!(expiry.expire_after_create(&key, &known, now), Some(Duration::from_secs(600)));
        assert_eq!(expiry.expire_after_create(&key, &None, now), Some(Duration::from_secs(15)));
        assert_eq!(expiry.expire_after_update(&key, &known, now, Some(Duration::from_secs(1))), Some(Duration::from_secs(600)));
        assert_eq!(expiry.expire_after_read(&key, &known, now, Some(Duration::from_secs(10)), now), Some(Duration::from_secs(10)), "reads must not extend TTL");
    }

    #[tokio::test]
    async fn metadata_cache_isolates_servers_and_changed_origins() {
        let state = test_state(test_config("http://unused.invalid", "http://unused.invalid", 18096), test_store());
        let mut config = state.config.read().await.clone();
        config.servers.push(config::EmbyServerConfig { id: "server-a".into(), ..Default::default() });
        let first = spawn_mock_server(|_| async { response_json(json!({"Items":[{"Name":"Server A"}]})) }).await;
        let second = spawn_mock_server(|_| async { response_json(json!({"Items":[{"Name":"Server B"}]})) }).await;
        let uri = "/Audio/id/stream.mp3".parse().unwrap();
        config.emby_host = first;
        assert_eq!(generic_playback_media(&state, &config, &uri).await.unwrap().name.as_deref(), Some("Server A"));
        let old_key = playback_media_key(&config, "id");
        config.servers[0].id = "other-server".into();
        assert!(old_key != playback_media_key(&config, "id"));
        config.emby_host = second;
        assert_eq!(generic_playback_media(&state, &config, &uri).await.unwrap().name.as_deref(), Some("Server B"));
        config.servers[0].id = old_key.server_id.clone();
        assert!(old_key != playback_media_key(&config, "id"), "same ID, changed origin isolated");
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn probes_apis_and_hls_segments_do_not_log_playback() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let emby = spawn_mock_server(move |request| {
            let count = count.clone();
            async move {
            if request.uri().path() == "/Items" {
                count.fetch_add(1, Ordering::SeqCst);
                return response_json(json!({"Items":[{"MediaSources":[]}]}));
            }
            response_json(json!({"MediaSources":[]}))
        }})
        .await;
        let state = test_state(
            test_config(&emby, "http://unused.invalid", 18096),
            test_store(),
        );
        let app = build_app(state.clone());
        for (method, path) in [
            ("HEAD", "/videos/id/stream"),
            ("OPTIONS", "/videos/id/original"),
            ("HEAD", "/Videos/id/master.m3u8"),
            ("OPTIONS", "/Audio/id/stream.mp3"),
            ("GET", "/Sessions"),
            ("GET", "/Items/id/PlaybackInfo"),
            ("GET", "/Videos/id/hls1/main/1.ts"),
            ("GET", "/Videos/id/hls1/main/1.m4s"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method(method)
                        .uri(format!("{path}?UserName=Fixture"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert!(
            state
                .activity_log
                .list(None, Some(ActivityKind::Playback), 100)
                .is_empty()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "only existing video HEAD/OPTIONS path resolution; no added metadata requests");
        assert!(state.playback_media.get(&playback_media_key(&*state.config.read().await, "id")).await.is_none());
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn upstream_http_failures_are_playback_with_diagnosis_and_safe_details() {
        for status in [StatusCode::NOT_FOUND, StatusCode::INTERNAL_SERVER_ERROR] {
            let emby = spawn_mock_server(move |request| async move {
                if request.uri().path() == "/Items" {
                    return response_json(json!({"Items":[{"Name":"Known failure title", "MediaSources":[]}]}));
                }
                (
                    status,
                    [(header::LOCATION, "http://example.invalid/secret-location")],
                    "secret-body",
                )
                    .into_response()
            })
            .await;
            let state = test_state(
                test_config(&emby, "http://unused.invalid", 18096),
                test_store(),
            );
            let response = build_app(state.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri("/videos/failure/stream?UserName=Fixture&api_key=secret-query")
                        .header(header::AUTHORIZATION, "secret-auth")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), status);
            assert_eq!(
                read_text(response).await,
                "secret-body",
                "transport stays transparent"
            );
            let id = assert_playback(
                &state,
                status.as_u16(),
                if status.is_client_error() {
                    "warn"
                } else {
                    "error"
                },
            );
            assert_eq!(entries(&state)[0].media.as_ref().unwrap().name.as_deref(), Some("Known failure title"));
            let record = diagnostics::get_diagnosis_records()
                .into_iter()
                .find(|r| r.request_id == id)
                .unwrap();
            assert_eq!(record.result.outcome.as_deref(), Some("failed"));
            assert_eq!(record.result.status_code, Some(status.as_u16()));
            assert_capture_correlated_logs(&state, &id);
        }
    }

    #[tokio::test]
    async fn connection_failure_logs_one_safe_playback_and_request_error() {
        let socket = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", socket.local_addr().unwrap());
        drop(socket);
        let state = test_state(
            test_config(&host, "http://unused.invalid", 18096),
            test_store(),
        );
        let response = build_app(state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/Videos/id/master.m3u8?UserName=Fixture&api_key=secret-query")
                    .header(header::AUTHORIZATION, "secret-auth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        let id = assert_playback(&state, 502, "error");
        state.telemetry_writes.shutdown().unwrap();
        let requests = state
            .settings_store
            .list_proxy_request_details(db::ProxyRequestDetailFilter {
                server_id: None,
                path_type: None,
                exclude_path_type: None,
                keyword: Some(&id),
                since_ms: None,
                until_ms: None,
                limit: 10,
            })
            .unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].status_code, 502);
    }

    #[tokio::test]
    async fn slow_success_has_only_playback_and_upstream_redirect_is_not_proxy_success() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path().contains("slow") {
                tokio::time::sleep(Duration::from_millis(3050)).await;
                return response_text(StatusCode::OK, "media");
            }
            (
                StatusCode::FOUND,
                [(header::LOCATION, "http://example.invalid/secret-location")],
            )
                .into_response()
        })
        .await;
        let state = test_state(
            test_config(&emby, "http://unused.invalid", 18096),
            test_store(),
        );
        let app = build_app(state.clone());
        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/Videos/slow/master.m3u8?UserName=Fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_playback(&state, 200, "info");
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri("/Audio/id/stream.mp3?UserName=Fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FOUND);
        assert_eq!(
            entries(&state).len(),
            1,
            "upstream redirect not logged as successful playback"
        );
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn slow_video_proxy_replaces_general_and_keeps_diagnostic_correlation() {
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(json!({"Items":[{"MediaSources":[]}]}));
            }
            tokio::time::sleep(Duration::from_millis(3050)).await;
            response_text(StatusCode::PARTIAL_CONTENT, "media")
        })
        .await;
        let state = test_state(
            test_config(&emby, "http://unused.invalid", 18096),
            test_store(),
        );
        let response = build_app(state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/slow/stream?UserName=Fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let id = assert_playback(&state, 206, "info");
        assert_capture_correlated_logs(&state, &id);
        let stats = state.settings_store.today_request_stats().unwrap();
        assert_eq!(
            stats
                .iter()
                .map(|stat| stat.redirects + stat.cache_hits)
                .sum::<i64>(),
            0
        );
    }

    #[tokio::test]
    async fn video_connection_failure_keeps_failed_diagnosis_and_error_counter() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        // Resolve Items once, then close the listener before media forwarding.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            drop(listener);
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let size = socket.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buffer[..size]);
            }
            assert!(request.starts_with(b"GET /Items?"));
            let body = r#"{"Items":[{"MediaSources":[]}]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        });
        let state = test_state(
            test_config(&host, "http://unused.invalid", 18096),
            test_store(),
        );
        let response = build_app(state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/network-failure/stream?UserName=Fixture&api_key=secret-query")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        upstream.await.unwrap();
        let id = assert_playback(&state, 502, "error");
        let record = diagnostics::get_diagnosis_records()
            .into_iter()
            .find(|r| r.request_id == id)
            .unwrap();
        assert_eq!(record.result.outcome.as_deref(), Some("failed"));
        assert_capture_correlated_logs(&state, &id);
        let stats = state.settings_store.today_request_stats().unwrap();
        assert_eq!(stats.iter().map(|stat| stat.errors).sum::<i64>(), 1);
        assert_eq!(
            stats
                .iter()
                .map(|stat| stat.redirects + stat.cache_hits)
                .sum::<i64>(),
            0
        );
    }

    #[tokio::test]
    async fn application_redirect_and_cache_hit_remain_one_playback_each() {
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let emby = spawn_mock_server(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            async { response_json(json!({"Items":[{"Name":"电影名", "MediaSources":[{"Id":"group-source-fixture", "Path":"https://example.invalid/media.mp4"}]}]})) }
        }).await;
        let mut config = test_config(&emby, "http://unused.invalid", 18096);
        config.openlist_addr = None;
        let state = test_state(config, test_store());
        let app = build_app(state.clone());
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(format!("/videos/id/stream?{GROUP_QUERY}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FOUND);
            assert_eq!(response.headers()[header::LOCATION], "https://example.invalid/media.mp4");
        }
        let logs = entries(&state);
        assert_eq!(logs.len(), 2);
        assert!(logs[0].playback_group_id.is_some());
        assert_eq!(logs[0].playback_group_id, logs[1].playback_group_id);
        assert!(logs.iter().all(|log| log.detail.split_once(' ').unwrap().1 == "https://example.invalid/media.mp4"));
        assert!(
            logs.iter()
                .all(|log| log.kind == "playback" && log.level == "success")
        );
        assert_eq!(
            logs.iter()
                .filter(|log| log.message == "直链缓存命中")
                .count(),
            1
        );
        assert!(logs.iter().all(|log| log.message != "服务器代理播放"));
        assert!(logs.iter().all(|log| log.media.as_ref().unwrap().name.as_deref() == Some("电影名")));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "cache hit performs no Items request");
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn grouped_openlist_redirect_and_fallback_preserve_raw_url_and_requests() {
        for fallback in [false, true] {
            let items = Arc::new(AtomicUsize::new(0));
            let count = items.clone();
            let emby = spawn_mock_server(move |request| {
                let count = count.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        count.fetch_add(1, Ordering::SeqCst);
                        return response_json(json!({"Items":[{"Name":"OpenList title","MediaSources":[{"Id":"group-source-fixture","Path": if fallback { "/media/local.mkv" } else { "http://openlist.local/d/movie.mkv" }}]}]}));
                    }
                    response_text(StatusCode::PARTIAL_CONTENT, "fallback-stream")
                }
            }).await;
            let openlist = spawn_mock_server(move |_| async move {
                if fallback { return response_json(json!({"code":500,"message":"fixture unavailable"})); }
                response_json(json!({"code":200,"data":{"raw_url":"https://cdn.example.invalid/movie.mkv?opaque=fixture-copy-value"}}))
            }).await;
            let state = test_state(test_config(&emby, &openlist, 18096), test_store());
            let app = build_app(state.clone());
            for _ in 0..2 {
                let response = app.clone().oneshot(HttpRequest::builder().uri(format!("/Videos/id/stream?{GROUP_QUERY}"))
                    .body(Body::empty()).unwrap()).await.unwrap();
                if fallback {
                    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
                    assert_eq!(read_text(response).await, "fallback-stream");
                } else {
                    assert_eq!(response.status(), StatusCode::FOUND);
                    assert_eq!(response.headers()[header::LOCATION], "https://cdn.example.invalid/movie.mkv?opaque=fixture-copy-value");
                }
            }
            let rows: Vec<_> = entries(&state).into_iter().filter(|row| row.kind == "playback").collect();
            assert_eq!(rows.len(), 2);
            assert!(rows[0].playback_group_id.is_some());
            assert_eq!(rows[0].playback_group_id, rows[1].playback_group_id);
            if !fallback {
                assert_eq!(items.load(Ordering::SeqCst), 1);
                let expected = diagnostics::sanitize_diagnostic_text(&sanitized_detail_for_log("https://cdn.example.invalid/movie.mkv?opaque=fixture-copy-value"));
                assert!(rows.iter().all(|row| row.detail.split_once(' ').unwrap().1 == expected));
            }
            state.telemetry_writes.shutdown().unwrap();
        }
    }

    #[tokio::test]
    async fn open_multichunk_stream_logs_at_headers_without_waiting_for_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let upstream = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let size = socket.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buffer[..size]);
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: audio/mpeg\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n3\r\none\r\n").await.unwrap();
            finish_rx.await.unwrap();
            socket
                .write_all(b"3\r\ntwo\r\n5\r\nthree\r\n0\r\n\r\n")
                .await
                .unwrap();
        });
        let state = test_state(
            test_config(&host, "http://unused.invalid", 18096),
            test_store(),
        );
        // Known labels avoid a cold lookup; the socket exclusively tests streaming.
        let config = state.config.read().await.clone();
        state.playback_media.insert(playback_media_key(&config, "id"), Some(PlaybackMedia {
            media_id: "id".into(), name: Some("Audio title".into()), ..Default::default()
        })).await;
        let response = tokio::time::timeout(
            Duration::from_secs(2),
            build_app(state.clone()).oneshot(
                HttpRequest::builder()
                    .uri("/Audio/id/stream.mp3?UserName=Fixture")
                    .body(Body::empty())
                    .unwrap(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert_playback(&state, 200, "info");
        finish_tx.send(()).unwrap();
        assert_eq!(read_text(response).await, "onetwothree");
        upstream.await.unwrap();
        assert_eq!(entries(&state).len(), 1, "chunks do not create activities");
        state.telemetry_writes.shutdown().unwrap();
    }
}
