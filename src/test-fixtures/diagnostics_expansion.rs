// Synthetic integration fixtures only; never connect to configured Emby servers.
mod diagnostics_expansion {
    use super::*;
    use crate::activity_log::ActivityKind;
    use crate::diagnostics::{DiagnosisContext, DiagnosisRecord, DiagnosisResult};

    fn record(id: &str, server_id: &str, started: i64, user: &str) -> DiagnosisRecord {
        DiagnosisRecord {
            request_id: id.into(),
            server_id: server_id.into(),
            media_id: "synthetic-media".into(),
            started_at_ms: Some(started),
            finished_at_ms: Some(started + 20),
            duration_ms: Some(20),
            context: DiagnosisContext {
                user_name: Some(user.into()),
                media_name: Some("Synthetic episode".into()),
                ..Default::default()
            },
            result: DiagnosisResult {
                outcome: Some("redirect".into()),
                status_code: Some(302),
                reason: Some("direct_url".into()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn diagnostics_network_real_upstream_operations_aggregate_only_completed_attempts() {
        diagnostics::clean_diagnostics();
        let calls = Arc::new(AtomicUsize::new(0));
        let upstream = spawn_mock_server({
            let calls = calls.clone();
            move |request| {
                calls.fetch_add(1, Ordering::Relaxed);
                async move {
                    let response = match request.uri().path() {
                        "/Items" => response_json(json!({"Items": [{"Name":"Synthetic", "MediaSources":[{"Path":"/synthetic.mkv"}]}]})),
                        "/api/fs/get" => response_json(json!({"code":200,"data":{"raw_url":"https://unused.invalid/synthetic.mkv"}})),
                        "/redirect" => (StatusCode::FOUND, [(header::LOCATION, "/never-requested")]).into_response(),
                        _ => response_text(StatusCode::PARTIAL_CONTENT, "synthetic stream"),
                    };
                    // Each operation gets a controlled new attempt, not a pool guess.
                    let mut response = response;
                    response.headers_mut().insert(header::CONNECTION, "close".parse().unwrap());
                    response
                }
            }
        }).await;
        let config = test_config(&upstream, &upstream, 18096);
        let client = diagnostics::network::instrument_client(reqwest::Client::builder())
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let mut trace = proxy::PlaybackDiagnosisTrace::new("s", "network-operations", "Fixture");
        trace.begin_step(proxy::STEP_EMBY_PATH, "query");
        emby::get_media_path_observed(
            &client,
            &config,
            "synthetic",
            None,
            "Fixture",
            &mut trace.observation,
        )
        .await
        .unwrap();
        assert_eq!(trace.observation.connection_attempts, Some(1));
        trace.complete_step("queried");
        trace.begin_step(proxy::STEP_OPENLIST, "get");
        openlist::fs_get_observed(
            &client,
            &config,
            "/synthetic.mkv",
            "Fixture",
            &mut trace.observation,
        )
        .await
        .unwrap();
        assert_eq!(trace.observation.connection_attempts, Some(1));
        trace.complete_step("resolved");
        trace.begin_step(proxy::STEP_INTERNAL_REDIRECT, "redirect");
        let resolved = internal_redirect::resolve_redirect_observed(
            &client,
            &format!("{upstream}/redirect"),
            3,
            "Fixture",
            &mut trace.observation,
        )
        .await
        .unwrap();
        assert!(resolved.url.ends_with("/never-requested"));
        assert_eq!(trace.observation.connection_attempts, Some(1));
        trace.complete_step("Location received");
        trace.begin_step(proxy::STEP_PROXY, "headers only");
        let response = proxy::proxy_to_emby_observed(
            &client,
            &config,
            Method::GET,
            &"/videos/synthetic/stream".parse().unwrap(),
            &Default::default(),
            Bytes::new(),
            "127.0.0.1".parse().unwrap(),
            &mut trace.observation,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(trace.observation.connection_attempts, Some(1));
        assert!(trace.observation.response_body_ms.is_none());
        trace.complete_step("headers received");
        trace.finish().await;
        let rows = diagnostics::get_diagnosis_records();
        let timing = &rows[0].timing;
        assert_eq!(
            calls.load(Ordering::Relaxed),
            4,
            "no redirect target or extra probe"
        );
        assert_eq!(timing.connection_attempts, Some(4));
        assert!(timing.connection_setup_ms.is_some());
        assert!(timing.name_resolution_ms.is_none() && timing.name_resolution_calls.is_none());
        assert_eq!(timing.connection_observation.as_deref(), Some("observed"));
        assert_eq!(rows[0].steps.len(), 4);
        for step in &rows[0].steps {
            assert_eq!(step.connection_attempts, Some(1));
            assert!(step.connection_setup_ms.is_some());
            assert!(step.name_resolution_ms.is_none() && step.name_resolution_calls.is_none());
            assert_eq!(step.connection_observation.as_deref(), Some("observed"));
        }
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn diagnostics_network_real_send_cancellation_retains_completed_connection_as_partial() {
        diagnostics::clean_diagnostics();
        let (seen, received) = tokio::sync::oneshot::channel();
        let seen = Arc::new(std::sync::Mutex::new(Some(seen)));
        let upstream = spawn_mock_server(move |_| {
            let seen = seen.clone();
            async move {
                if let Some(seen) = seen.lock().unwrap().take() {
                    seen.send(()).unwrap();
                }
                std::future::pending::<Response>().await
            }
        })
        .await;
        let config = test_config(&upstream, &upstream, 18096);
        let client = diagnostics::network::instrument_client(reqwest::Client::builder())
            .no_proxy()
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let mut trace = proxy::PlaybackDiagnosisTrace::new("s", "network-cancel", "Fixture");
        trace.begin_step(proxy::STEP_EMBY_PATH, "query");
        let mut pending = Box::pin(emby::get_media_path_observed(
            &client,
            &config,
            "synthetic",
            None,
            "Fixture",
            &mut trace.observation,
        ));
        tokio::select! {
            _ = &mut pending => panic!("controlled response must remain pending"),
            _ = received => {},
            _ = tokio::time::sleep(Duration::from_secs(3)) => panic!("controlled upstream not reached"),
        }
        drop(pending);
        drop(trace);
        let rows = diagnostics::get_diagnosis_records();
        assert_eq!(rows[0].result.outcome.as_deref(), Some("cancelled"));
        assert_eq!(rows[0].timing.connection_attempts, Some(1));
        assert!(rows[0].timing.connection_setup_ms.is_some());
        assert_eq!(
            rows[0].timing.connection_observation.as_deref(),
            Some("partial")
        );
        assert_eq!(rows[0].steps[0].connection_attempts, Some(1));
        assert_eq!(
            rows[0].steps[0].connection_setup_ms,
            rows[0].timing.connection_setup_ms
        );
        assert_eq!(
            rows[0].steps[0].connection_observation.as_deref(),
            Some("partial")
        );
        diagnostics::clean_diagnostics();
    }

    fn identity(store: &SettingsStore, token: &str) -> i64 {
        let owner = store.list_admin_users().unwrap()[0].id;
        store
            .create_session(owner, &auth::hash_token(token), auth::now_ts() + 3600)
            .unwrap();
        owner
    }

    async fn get(app: Router, path: &str, token: &str) -> serde_json::Value {
        let response = app
            .oneshot(
                HttpRequest::builder()
                    .uri(path)
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        read_json(response).await
    }

    #[tokio::test]
    async fn diagnostics_persistence_reopens_with_authorized_totals_and_page_boundaries() {
        diagnostics::clean_diagnostics();
        let path = std::env::temp_dir().join(format!(
            "embypanel-diagnostics-expansion-{}-{}.db",
            std::process::id(),
            uuid_like_timestamp()
        ));
        let store = SettingsStore::open_for_test(path.clone()).unwrap();
        store
            .create_admin("synthetic-owner", "synthetic-hash")
            .unwrap();
        let owner = identity(&store, "synthetic-diagnostics-owner");
        let reader = store
            .create_admin_with_roles(
                "synthetic-reader",
                "synthetic-hash",
                None,
                &["viewer"],
                Some(owner),
            )
            .unwrap();
        store
            .replace_admin_server_grants(
                reader,
                &[db::AdminServerGrantInput {
                    server_id: "server-a".into(),
                    scope: "full".into(),
                }],
                Some(owner),
                auth::now_ts(),
            )
            .unwrap();
        store
            .create_session(
                reader,
                &auth::hash_token("synthetic-diagnostics-reader"),
                auth::now_ts() + 3600,
            )
            .unwrap();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        let now = now_ms() as i64;
        queue.diagnosis_record(record("persist-a-older", "server-a", now - 4, "Alice"));
        queue.diagnosis_record(record("persist-b", "server-b", now - 2, "Bob"));
        queue.diagnosis_record(record("persist-a-newer", "server-a", now - 1, "Alice"));
        queue.shutdown().unwrap();
        drop(queue);
        drop(store);

        let reopened = SettingsStore::open_for_test(path).unwrap();
        let state = test_state(Config::default_runtime(), reopened);
        let app = build_management_app(state.clone());
        let first = get(
            app.clone(),
            "/api/diagnostics?limit=1",
            "synthetic-diagnostics-reader",
        )
        .await;
        assert_eq!(first["total"], 2);
        assert_eq!(first["has_more"], true);
        assert_eq!(first["records"][0]["request_id"], "persist-a-newer");
        assert_eq!(first["storage"]["persistent"], true);
        assert_eq!(first["storage"]["retention_days"], 7);
        assert_eq!(first["storage"]["max_records"], 10000);
        let second = get(
            app.clone(),
            "/api/diagnostics?limit=1&page=2",
            "synthetic-diagnostics-reader",
        )
        .await;
        assert_eq!(second["total"], 2);
        assert_eq!(second["has_more"], false);
        assert_eq!(second["records"][0]["request_id"], "persist-a-older");
        let owner_rows = get(app, "/api/diagnostics", "synthetic-diagnostics-owner").await;
        assert_eq!(owner_rows["total"], 3);
        state.telemetry_writes.shutdown().unwrap();
    }

    #[tokio::test]
    async fn diagnostics_merge_filters_measured_values_and_preserves_newer_memory_record() {
        diagnostics::clean_diagnostics();
        let store = test_store();
        identity(&store, "synthetic-diagnostics-merge");
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        let now = now_ms() as i64;
        queue.diagnosis_record(record("merge-duplicate", "server-a", now - 10, "Before"));
        queue.diagnosis_record(record("merge-outside", "server-a", now - 1000, "Alice"));
        queue.shutdown().unwrap();
        diagnostics::record_diagnosis_now(record("merge-duplicate", "server-a", now - 10, "Alice"));
        diagnostics::record_diagnosis_now(record("merge-other", "server-a", now - 5, "Bob"));
        let state = test_state(Config::default_runtime(), store);
        let app = build_management_app(state.clone());
        let path = format!(
            "/api/diagnostics?user=alice&outcome=redirect&keyword=Synthetic&min_duration_ms=20&since_ms={}&until_ms={}",
            now - 20,
            now
        );
        let rows = get(app.clone(), &path, "synthetic-diagnostics-merge").await;
        assert_eq!(rows["total"], 1);
        assert_eq!(rows["records"][0]["request_id"], "merge-duplicate");
        assert_eq!(rows["records"][0]["context"]["user_name"], "Alice");
        let none = get(
            app,
            "/api/diagnostics?min_duration_ms=21",
            "synthetic-diagnostics-merge",
        )
        .await;
        assert_eq!(none["total"], 0);
        state.telemetry_writes.shutdown().unwrap();
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn diagnostics_confirmed_cleanup_drains_prior_records_and_keeps_writer_available() {
        diagnostics::clean_diagnostics();
        let store = test_store();
        identity(&store, "synthetic-diagnostics-clean");
        let state = test_state(Config::default_runtime(), store);
        let now = now_ms() as i64;
        for index in 0..20 {
            state.telemetry_writes.diagnosis_record(record(
                &format!("clear-old-{index}"),
                "server-a",
                now,
                "Alice",
            ));
        }
        diagnostics::record_diagnosis_now(record("clear-memory", "server-a", now, "Alice"));
        let app = build_management_app(state.clone());
        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .method(Method::POST)
                    .uri("/api/diagnostics/clean")
                    .header(header::AUTHORIZATION, "Bearer synthetic-diagnostics-clean")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"confirm":"yes"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(diagnostics::get_diagnosis_records().is_empty());
        assert!(
            state
                .settings_store
                .list_diagnosis_records_scoped(None)
                .unwrap()
                .is_empty()
        );
        state
            .telemetry_writes
            .diagnosis_record(record("clear-new", "server-a", now + 1, "Alice"));
        state.telemetry_writes.shutdown().unwrap();
        let rows = get(app, "/api/diagnostics", "synthetic-diagnostics-clean").await;
        assert_eq!(rows["total"], 1);
        assert_eq!(rows["records"][0]["request_id"], "clear-new");
    }

    #[tokio::test]
    async fn diagnostics_redirect_capture_has_context_truthful_result_and_log_correlation() {
        diagnostics::clean_diagnostics();
        let queries = Arc::new(AtomicUsize::new(0));
        let emby = spawn_mock_server({
            let queries = queries.clone();
            move |request| {
                let queries = queries.clone();
                async move {
                    if request.uri().path() == "/Items" {
                        queries.fetch_add(1, Ordering::Relaxed);
                        return response_json(json!({"Items": [{
                            "Id": "synthetic-item", "Name": "Synthetic episode", "SeriesName": "Synthetic series",
                            "ParentIndexNumber": 2, "IndexNumber": 3,
                            "MediaSources": [{"Id": "source-one", "Path": "https://media.invalid/movie.mkv"}]
                        }]}));
                    }
                    response_json(json!([]))
                }
            }
        }).await;
        let mut config = test_config(&emby, "http://127.0.0.1:1", 18096);
        config.openlist_addr = None;
        config.openlist_token = None;
        let state = test_state(config, test_store());
        diagnostics::enable_persistence(state.telemetry_writes.clone());
        let app = build_app(state.clone());
        let request = || {
            HttpRequest::builder()
            .uri("/videos/synthetic-item/stream?MediaSourceId=source-one&DeviceId=fixture-device&UserName=Fixture")
            .header(header::USER_AGENT, "FixturePlayer/1.0")
            .header(header::RANGE, "bytes=0-")
            .body(Body::empty()).unwrap()
        };
        assert_eq!(
            app.clone().oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );
        assert_eq!(
            app.oneshot(request()).await.unwrap().status(),
            StatusCode::FOUND
        );
        assert_eq!(
            queries.load(Ordering::Relaxed),
            1,
            "cache hit must not add a media query"
        );
        let records = diagnostics::get_diagnosis_records();
        assert_eq!(records.len(), 2);
        assert_ne!(records[0].request_id, records[1].request_id);
        for row in &records {
            assert_eq!(row.result.outcome.as_deref(), Some("redirect"));
            assert_eq!(row.result.status_code, Some(302));
            assert_eq!(row.result.target_host.as_deref(), Some("media.invalid"));
            assert_eq!(row.context.method.as_deref(), Some("GET"));
            assert_eq!(row.context.range.as_deref(), Some("bytes=0-"));
            assert_eq!(row.context.user_name.as_deref(), Some("Fixture"));
            assert_eq!(row.context.device_id.as_deref(), Some("fixture-device"));
        }
        assert_eq!(
            records[0].context.media_name.as_deref(),
            Some("Synthetic episode")
        );
        assert_eq!(
            records[0].context.series_name.as_deref(),
            Some("Synthetic series")
        );
        assert_eq!(records[0].cache.status.as_deref(), Some("miss"));
        assert_eq!(records[1].cache.status.as_deref(), Some("hit"));
        state.telemetry_writes.shutdown().unwrap();
        assert_eq!(
            state
                .settings_store
                .list_diagnosis_records_scoped(None)
                .unwrap()
                .len(),
            2,
            "collector must enqueue both real request records"
        );
        let request_logs = state
            .settings_store
            .list_proxy_request_details(db::ProxyRequestDetailFilter {
                server_id: None,
                path_type: Some("video_stream"),
                exclude_path_type: None,
                keyword: None,
                since_ms: None,
                until_ms: None,
                limit: 10,
            })
            .unwrap();
        let activity = state
            .activity_log
            .list(None, Some(ActivityKind::Playback), 10);
        for row in &records {
            assert!(
                request_logs
                    .iter()
                    .any(|log| log.detail.contains(&row.request_id))
            );
            assert!(
                activity
                    .iter()
                    .any(|log| log.detail.contains(&row.request_id))
            );
        }
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn diagnostics_client_policy_notification_cancellation_records_current_request_only() {
        diagnostics::clean_diagnostics();
        let (seen, notified) = tokio::sync::oneshot::channel();
        let seen = Arc::new(std::sync::Mutex::new(Some(seen)));
        let webhook = spawn_mock_server(move |_| {
            let seen = seen.clone();
            async move {
                if let Some(seen) = seen.lock().unwrap().take() {
                    let _ = seen.send(());
                }
                std::future::pending::<()>().await;
                StatusCode::NO_CONTENT.into_response()
            }
        })
        .await;
        let config: client_control::ClientControlConfig = serde_json::from_value(json!({
            "enabled": true, "notify_enabled": true,
            "webhooks": [{ "id": "fixture", "enabled": true, "name": "Fixture", "url": webhook }],
            "records": [{ "id": "fixture-rule", "client_name": "Fixture", "device_name": "Historical device",
                "user_name": "Historical user", "user_agent": "SyntheticBlockedPlayer", "source": "manual",
                "enabled": true, "created_at": "", "updated_at": "", "note": "" }]
        })).unwrap();
        let store = test_store();
        store.save_setting_json("client_control", &config).unwrap();
        let state = test_state(Config::default_runtime(), store);
        let mut request = Box::pin(
            build_app(state.clone()).oneshot(
                HttpRequest::builder()
                    .uri("/videos/synthetic-blocked/stream")
                    .header(header::USER_AGENT, "SyntheticBlockedPlayer/1.0")
                    .body(Body::empty())
                    .unwrap(),
            ),
        );
        tokio::select! {
            _ = &mut request => panic!("synthetic notification must still be pending"),
            result = notified => result.unwrap(),
            _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("synthetic notification not reached"),
        }
        drop(request);
        let records = diagnostics::get_diagnosis_records();
        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.result.outcome.as_deref(), Some("cancelled"));
        assert_eq!(record.result.status_code, None);
        assert_eq!(
            record.context.user_name, None,
            "historical rule user is not the request user"
        );
        assert_eq!(record.failure_node.as_deref(), Some(proxy::STEP_POLICY));
        assert_eq!(record.timing.connection_attempts, Some(1));
        assert_eq!(
            record.timing.connection_observation.as_deref(),
            Some("partial")
        );
        assert_eq!(record.steps[0].connection_attempts, Some(1));
        assert_eq!(
            record.steps[0].connection_observation.as_deref(),
            Some("partial")
        );
        assert_capture_correlated_logs(&state, &record.request_id);
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn diagnostics_fast_proxy_has_exactly_one_correlated_activity_record() {
        diagnostics::clean_diagnostics();
        let emby = spawn_mock_server(|request| async move {
            if request.uri().path() == "/Items" {
                return response_json(
                    json!({"Items":[{"Name":"Synthetic movie","MediaSources":[]}]}),
                );
            }
            response_text(StatusCode::PARTIAL_CONTENT, "synthetic body")
        })
        .await;
        let state = test_state(
            test_config(&emby, "http://unused.invalid", 18096),
            test_store(),
        );
        let response = build_app(state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/videos/synthetic-fast-proxy/stream?UserName=Fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let records = diagnostics::get_diagnosis_records();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].result.outcome.as_deref(), Some("proxy"));
        drop(response);
        assert_capture_correlated_logs(&state, &records[0].request_id);
        diagnostics::clean_diagnostics();
    }
}
