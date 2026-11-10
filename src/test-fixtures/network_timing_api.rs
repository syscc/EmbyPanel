// Synthetic DTO and loopback fixtures only; no configured upstream or system DNS is used.
mod network_timing_api {
    use super::*;

    #[tokio::test]
    async fn network_metrics_roundtrip_storage_and_scoped_api_preserve_unknown_legacy_values() {
        diagnostics::clean_diagnostics();
        let store = test_store();
        let owner = store.list_admin_users().unwrap()[0].id;
        let reader = store
            .create_admin_with_roles(
                "network-reader",
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
                    server_id: "network-a".into(),
                    scope: "full".into(),
                }],
                Some(owner),
                auth::now_ts(),
            )
            .unwrap();
        store
            .create_session(
                reader,
                &auth::hash_token("synthetic-network-reader"),
                auth::now_ts() + 3600,
            )
            .unwrap();
        let now = now_ms() as i64;
        let timed: diagnostics::DiagnosisRecord = serde_json::from_value(json!({
            "request_id": "diag-network-timed", "server_id": "network-a", "media_id": "fixture", "ua": "FixturePlayer",
            "started_at_ms": now, "finished_at_ms": now + 50, "duration_ms": 50,
            "cache_hit": false, "failure_node": null, "sanitized_errors": [],
            "timing": { "name_resolution_ms": 0, "name_resolution_calls": 1,
                "connection_setup_ms": 12, "connection_attempts": 2, "connection_observation": "partial" },
            "steps": [{ "name": "Emby Path", "state": "recorded", "duration_ms": 40, "details": "synthetic",
                "name_resolution_ms": 0, "name_resolution_calls": 1, "connection_setup_ms": 12,
                "connection_attempts": 2, "connection_observation": "partial" }]
        })).unwrap();
        let legacy: diagnostics::DiagnosisRecord = serde_json::from_value(json!({
            "request_id": "diag-network-legacy", "server_id": "network-a", "media_id": "legacy", "ua": "FixturePlayer",
            "cache_hit": false, "failure_node": null, "sanitized_errors": [],
            "timing": { "dns_ms": 11, "tcp_ms": 12, "tls_ms": 13 },
            "steps": [{ "name": "Emby Path", "duration_ms": 10, "details": "legacy" }]
        })).unwrap();
        let mut other = timed.clone();
        other.request_id = "diag-network-other".into();
        other.server_id = "network-b".into();
        let queue = TelemetryWriteQueue::start(store.clone()).unwrap();
        for record in [timed, legacy, other] {
            queue.diagnosis_record(record);
        }
        queue.shutdown().unwrap();
        let state = test_state(Config::default_runtime(), store);
        let response = build_management_app(state.clone())
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/diagnostics")
                    .header(header::AUTHORIZATION, "Bearer synthetic-network-reader")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert_eq!(body["total"], 2);
        let rows = body["records"].as_array().unwrap();
        let timed = rows
            .iter()
            .find(|r| r["request_id"] == "diag-network-timed")
            .unwrap();
        for values in [&timed["timing"], &timed["steps"][0]] {
            assert_eq!(values["name_resolution_ms"], 0);
            assert_eq!(values["name_resolution_calls"], 1);
            assert_eq!(values["connection_setup_ms"], 12);
            assert_eq!(values["connection_attempts"], 2);
            assert_eq!(values["connection_observation"], "partial");
        }
        for key in ["dns_ms", "tcp_ms", "tls_ms"] {
            assert!(timed["timing"].get(key).is_none(), "unsupported {key} is omitted from the API");
        }
        assert_eq!(
            timed["duration_ms"], 50,
            "nested observations never add to trace wall time"
        );
        let legacy = rows
            .iter()
            .find(|r| r["request_id"] == "diag-network-legacy")
            .unwrap();
        for values in [&legacy["timing"], &legacy["steps"][0]] {
            for key in [
                "name_resolution_ms",
                "name_resolution_calls",
                "connection_setup_ms",
                "connection_attempts",
                "connection_observation",
            ] {
                assert!(values[key].is_null(), "legacy {key} must remain unknown");
            }
        }
        for values in [&legacy["timing"], &legacy["steps"][0]] {
            for key in ["dns_ms", "tcp_ms", "tls_ms"] {
                assert!(values.get(key).is_none(), "unsupported {key} is omitted from the API");
            }
        }
        state.telemetry_writes.shutdown().unwrap();
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn network_context_queries_count_connections_before_items_or_cache_hit() {
        for identity in [
            "UserId=fixture-user",
            "X-Emby-Token=fixture-token",
            "DeviceId=fixture-device",
        ] {
            for cache_hit in [false, true] {
                diagnostics::clean_diagnostics();
                let paths = Arc::new(std::sync::Mutex::new(Vec::new()));
                let emby = spawn_mock_server({
                    let paths = paths.clone();
                    move |request| {
                        paths.lock().unwrap().push(request.uri().path().to_string());
                        async move {
                            match request.uri().path() {
                                "/Users/fixture-user" | "/Users/Me" => response_json(json!({"Name":"Fixture"})),
                                "/Sessions" => response_json(json!([{"DeviceId":"fixture-device", "UserName":"Fixture"}])),
                                "/Items" => response_json(json!({"Items":[{"Name":"Fixture movie", "MediaSources":[{"Path":"https://unused.invalid/movie.mkv"}]}]})),
                                _ => panic!("unexpected synthetic request"),
                            }
                        }
                    }
                }).await;
                let mut config = test_config(&emby, "http://unused.invalid", 18096);
                config.openlist_addr = None;
                config.openlist_token = None;
                let mut state = test_state(config, test_store());
                state.client = diagnostics::network::instrument_client(reqwest::Client::builder())
                    .no_proxy()
                    .build()
                    .unwrap();
                if cache_hit {
                    state
                        .cache
                        .read()
                        .await
                        .set(
                            DirectLinkCache::key("default", "network-context", "default", ""),
                            "https://unused.invalid/cached.mkv".into(),
                        )
                        .await;
                }
                let response = build_app(state.clone())
                    .oneshot(
                        HttpRequest::builder()
                            .uri(format!("/videos/network-context/stream?{identity}"))
                            .body(Body::empty())
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FOUND);
                let record = diagnostics::get_diagnosis_records().pop().unwrap();
                assert_eq!(record.context.user_name.as_deref(), Some("Fixture"));
                assert_eq!(record.cache_hit, cache_hit);
                let context = record
                    .steps
                    .iter()
                    .find(|step| step.name == proxy::STEP_CONTEXT)
                    .unwrap();
                assert_eq!(context.connection_attempts, Some(1));
                assert!(context.connection_setup_ms.is_some());
                assert_eq!(context.connection_observation.as_deref(), Some("observed"));
                let item = record
                    .steps
                    .iter()
                    .find(|step| step.name == proxy::STEP_EMBY_PATH)
                    .unwrap();
                assert!(
                    item.connection_attempts.is_none(),
                    "same client reuses the user-query connection, or skips Items on cache hit"
                );
                assert_eq!(record.timing.connection_attempts, Some(1));
                assert_eq!(
                    record.timing.connection_observation.as_deref(),
                    Some("observed")
                );
                assert_eq!(paths.lock().unwrap().len(), if cache_hit { 1 } else { 2 });
                state.telemetry_writes.shutdown().unwrap();
            }
        }
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn network_policy_queries_preserve_connection_counts_and_optional_error_semantics() {
        for session_status in [StatusCode::OK, StatusCode::SERVICE_UNAVAILABLE] {
            diagnostics::clean_diagnostics();
            let calls = Arc::new(AtomicUsize::new(0));
            let emby = spawn_mock_server({
                let calls = calls.clone();
                move |request| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    async move {
                        let mut response = match request.uri().path() {
                            "/Sessions" if session_status.is_success() => response_json(json!([])),
                            "/Sessions" => response_text(session_status, "synthetic optional failure"),
                            "/Items" => response_json(json!({"Items":[{"MediaSources":[{"Path":"https://unused.invalid/movie.mkv"}]}]})),
                            _ => panic!("unexpected synthetic request"),
                        };
                        response.headers_mut().insert(header::CONNECTION, "close".parse().unwrap());
                        response
                    }
                }
            }).await;
            let mut config = test_config(&emby, "http://unused.invalid", 18096);
            config.openlist_addr = None;
            config.openlist_token = None;
            let store = test_store();
            store.save_setting_json("client_control", &json!({
                "enabled":false, "concurrent_playback_limit_enabled":true, "concurrent_playback_limit_max":1
            })).unwrap();
            let mut state = test_state(config, store);
            state.client = diagnostics::network::instrument_client(reqwest::Client::builder())
                .no_proxy()
                .build()
                .unwrap();
            let response = build_app(state.clone())
                .oneshot(
                    HttpRequest::builder()
                        .uri("/videos/network-policy/stream?UserName=Fixture")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FOUND);
            let record = diagnostics::get_diagnosis_records().pop().unwrap();
            let policy = record
                .steps
                .iter()
                .find(|step| step.name == proxy::STEP_POLICY)
                .unwrap();
            assert_eq!(policy.connection_attempts, Some(1));
            assert_eq!(policy.connection_observation.as_deref(), Some("observed"));
            assert!(
                policy.error_kind.is_none() && policy.upstream_status.is_none(),
                "optional policy lookup does not become the overall result"
            );
            assert_eq!(record.result.outcome.as_deref(), Some("redirect"));
            assert_eq!(record.timing.connection_attempts, Some(2));
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            state.telemetry_writes.shutdown().unwrap();
        }
        diagnostics::clean_diagnostics();
    }

    #[tokio::test]
    async fn network_context_and_policy_cancellation_keep_completed_connections() {
        for context_step in [true, false] {
            diagnostics::clean_diagnostics();
            let (seen, received) = tokio::sync::oneshot::channel();
            let seen = Arc::new(std::sync::Mutex::new(Some(seen)));
            let emby = spawn_mock_server(move |_| {
                let seen = seen.clone();
                async move {
                    if let Some(seen) = seen.lock().unwrap().take() {
                        seen.send(()).unwrap();
                    }
                    std::future::pending::<Response>().await
                }
            })
            .await;
            let store = test_store();
            if !context_step {
                store.save_setting_json("client_control", &json!({
                    "enabled":false, "concurrent_playback_limit_enabled":true, "concurrent_playback_limit_max":1
                })).unwrap();
            }
            let mut state = test_state(test_config(&emby, "http://unused.invalid", 18096), store);
            state.client = diagnostics::network::instrument_client(reqwest::Client::builder())
                .no_proxy()
                .build()
                .unwrap();
            let query = if context_step {
                "UserId=fixture-user"
            } else {
                "UserName=Fixture"
            };
            let mut request = Box::pin(
                build_app(state.clone()).oneshot(
                    HttpRequest::builder()
                        .uri(format!("/videos/network-operation-cancel/stream?{query}"))
                        .body(Body::empty())
                        .unwrap(),
                ),
            );
            tokio::select! {
                _ = &mut request => panic!("controlled request must remain pending"),
                result = received => result.unwrap(),
                _ = tokio::time::sleep(Duration::from_secs(5)) => panic!("controlled request not received"),
            }
            drop(request);
            let record = diagnostics::get_diagnosis_records().pop().unwrap();
            let step_name = if context_step {
                proxy::STEP_CONTEXT
            } else {
                proxy::STEP_POLICY
            };
            let step = record
                .steps
                .iter()
                .find(|step| step.name == step_name)
                .unwrap();
            assert_eq!(record.result.outcome.as_deref(), Some("cancelled"));
            assert_eq!(step.connection_attempts, Some(1));
            assert_eq!(step.connection_observation.as_deref(), Some("partial"));
            assert_eq!(record.timing.connection_attempts, Some(1));
            assert_eq!(
                record.timing.connection_observation.as_deref(),
                Some("partial")
            );
            state.telemetry_writes.shutdown().unwrap();
        }
        diagnostics::clean_diagnostics();
    }
}
