// Integration regressions for the review fixes, using the production management Router.
mod review_api_fixes {
    use super::*;
    use aes_gcm::{
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rsa::{Oaep, RsaPublicKey, pkcs8::DecodePublicKey, rand_core::OsRng};
    use sha2::Sha256;

    struct Fixture {
        state: AppState,
        app: Router,
        reader: i64,
        upstream_writes: Arc<AtomicUsize>,
        upstream_reads: Arc<AtomicUsize>,
        fail_task_reads: Arc<std::sync::atomic::AtomicBool>,
        upstream: tokio::task::JoinHandle<()>,
    }

    impl Fixture {
        async fn new() -> Self {
            let writes = Arc::new(AtomicUsize::new(0));
            let reads = Arc::new(AtomicUsize::new(0));
            let fail_task_reads = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let mock = {
                let writes = writes.clone();
                let reads = reads.clone();
                let fail_task_reads = fail_task_reads.clone();
                Router::new().fallback(any(move |request: Request| {
                    let writes = writes.clone();
                    let reads = reads.clone();
                    let fail_task_reads = fail_task_reads.clone();
                    async move {
                        if request.method() != Method::GET {
                            writes.fetch_add(1, Ordering::Relaxed);
                            return StatusCode::NO_CONTENT.into_response();
                        }
                        reads.fetch_add(1, Ordering::Relaxed);
                        if request.uri().path().starts_with("/ScheduledTasks")
                            && fail_task_reads.load(Ordering::Relaxed)
                        {
                            return StatusCode::BAD_GATEWAY.into_response();
                        }
                        match request.uri().path() {
                            "/Sessions" => response_json(json!([{
                                "Id":"session-a","Client":"Fixture","DeviceName":"Fixture",
                                "NowPlayingItem":{"Id":"movie-a","Name":"Movie"},
                                "PlayState":{"PositionTicks":0,"PlayMethod":"DirectPlay"}
                            }])),
                            "/ScheduledTasks" => response_json(json!([{
                                "Id":"scan","Name":"Scan","State":"Idle",
                                "LastExecutionResult":{"Status":"Failed","ErrorMessage":
                                    "Download failed https://operator:URL_CANARY@upstream.invalid/movie?sign=SIGN_CANARY"}
                            }])),
                            "/ScheduledTasks/scan" => response_json(json!({
                                "Id":"scan","Name":"Scan","State":"Idle",
                                "LastExecutionResult":{"Status":"Failed","ErrorMessage":
                                    "Download failed https://operator:URL_CANARY@upstream.invalid/movie?sign=SIGN_CANARY"}
                            })),
                            _ => response_json(json!([])),
                        }
                    }
                }))
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let host = format!("http://{}", listener.local_addr().unwrap());
            let upstream = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
            let store = test_store();
            let owner = store.list_admin_users().unwrap()[0].id;
            let reader = store
                .create_admin_with_roles(
                    "review-reader",
                    "fixture-hash",
                    None,
                    &["viewer"],
                    Some(owner),
                )
                .unwrap();
            let operator = store
                .create_admin_with_roles(
                    "review-operator",
                    "fixture-hash",
                    None,
                    &["operator"],
                    Some(owner),
                )
                .unwrap();
            for (user, token) in [
                (owner, "review-owner-session"),
                (reader, "review-reader-session"),
                (operator, "review-operator-session"),
            ] {
                store
                    .create_session(user, &auth::hash_token(token), auth::now_ts() + 3600)
                    .unwrap();
                if user != owner {
                    store
                        .replace_admin_server_grants(
                            user,
                            &[db::AdminServerGrantInput {
                                server_id: "srv-a".into(),
                                scope: "full".into(),
                            }],
                            Some(owner),
                            auth::now_ts(),
                        )
                        .unwrap();
                }
            }
            store
                .create_admin_api_token(
                    owner,
                    "read-only",
                    "prefix",
                    &auth::hash_token("review-read-token"),
                    &["monitoring.read".into()],
                    Some(&["srv-a".into()]),
                    None,
                    Some(owner),
                    auth::now_ts(),
                )
                .unwrap();
            let mut config = Config::default_runtime();
            config.servers = ["srv-a", "srv-b"].into_iter().enumerate().map(|(index, id)| {
                serde_json::from_value(json!({"id":id,"name":id,"emby_host":host,"emby_api_key":"API_KEY_CANARY","port":18096+index})).unwrap()
            }).collect();
            let state = test_state(config, store);
            let app = build_management_app(state.clone());
            Self {
                state,
                app,
                reader,
                upstream_writes: writes,
                upstream_reads: reads,
                fail_task_reads,
                upstream,
            }
        }

        async fn request(
            &self,
            method: Method,
            path: &str,
            token: &str,
            body: serde_json::Value,
        ) -> Response {
            self.app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method(method)
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {token}"))
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }

        async fn encrypted(&self, name: &str, value: serde_json::Value) -> serde_json::Value {
            let response = crate::crypto_api::public_key(State(self.state.clone())).await;
            let response = read_json(response).await;
            let key =
                RsaPublicKey::from_public_key_pem(response["public_key_pem"].as_str().unwrap())
                    .unwrap();
            let aes_key = [7u8; 32];
            let nonce = [9u8; 12];
            let cipher = Aes256Gcm::new_from_slice(&aes_key).unwrap();
            let clear = serde_json::to_vec(&json!({"name":name,"value":value})).unwrap();
            let encrypted = cipher
                .encrypt(Nonce::from_slice(&nonce), clear.as_slice())
                .unwrap();
            let wrapped = key
                .encrypt(&mut OsRng, Oaep::new::<Sha256>(), &aes_key)
                .unwrap();
            json!({"encrypted_key":URL_SAFE_NO_PAD.encode(wrapped),"fields":{"fixture":{"iv":URL_SAFE_NO_PAD.encode(nonce),"value":URL_SAFE_NO_PAD.encode(encrypted)}}})
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.state.telemetry_writes.shutdown().unwrap();
            self.upstream.abort();
        }
    }

    #[tokio::test]
    async fn session_control_requires_write_permission_and_keeps_operator_actions() {
        let fixture = Fixture::new().await;
        for (field, path, value) in [
            (
                "stop_session",
                "/api/sessions/session-a/stop",
                json!({"server_id":"srv-a"}),
            ),
            (
                "send_session_message",
                "/api/sessions/session-a/message",
                json!({"server_id":"srv-a","message":"Fixture message"}),
            ),
        ] {
            let encrypted = fixture.encrypted(field, value).await;
            for token in ["review-reader-session", "review-read-token"] {
                let response = fixture
                    .request(Method::POST, path, token, encrypted.clone())
                    .await;
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
            assert_eq!(
                fixture.upstream_writes.load(Ordering::Relaxed),
                if field == "stop_session" { 0 } else { 1 }
            );
            let response = fixture
                .request(Method::POST, path, "review-operator-session", encrypted)
                .await;
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(fixture.upstream_writes.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn empty_server_grants_return_empty_libraries_without_expiring_login() {
        let fixture = Fixture::new().await;
        fixture
            .state
            .settings_store
            .replace_admin_server_grants(fixture.reader, &[], Some(1), auth::now_ts())
            .unwrap();
        for path in ["/api/library/libraries", "/api/library/tasks"] {
            let response = fixture
                .request(Method::GET, path, "review-reader-session", json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = read_json(response).await;
            assert_eq!(body["items"], json!([]));
            assert_eq!(body["failures"], json!([]));
        }
        assert_eq!(
            fixture
                .request(
                    Method::GET,
                    "/api/profile",
                    "review-reader-session",
                    json!(null)
                )
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            fixture
                .request(
                    Method::GET,
                    "/api/library/tasks?server_id=srv-a",
                    "review-reader-session",
                    json!(null)
                )
                .await
                .status(),
            StatusCode::FORBIDDEN
        );
        assert_eq!(fixture.upstream_reads.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn server_options_are_scoped_and_need_no_global_settings_permission() {
        let fixture = Fixture::new().await;
        for (token, can_control) in [
            ("review-read-token", false),
            ("review-operator-session", true),
        ] {
            let response = fixture
                .request(Method::GET, "/api/servers/options", token, json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = read_json(response).await;
            assert_eq!(body["can_control_sessions"], json!(can_control));
            assert_eq!(body["servers"], json!([{"id":"srv-a","name":"srv-a"}]));
            let text = body.to_string();
            assert!(!text.contains("emby_host"));
            assert!(!text.contains("API_KEY_CANARY"));
        }
        assert_eq!(fixture.upstream_reads.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn live_and_persisted_task_failures_redact_signed_url_credentials() {
        let fixture = Fixture::new().await;
        fixture
            .state
            .settings_store
            .task_failure_history_store()
            .record_failure(db::ScheduledTaskFailureInput {
                server_id: "srv-a".into(),
                task_id: "scan".into(),
                run_identity: "old-run".into(),
                observed_at: auth::now_ts() * 1000,
                status: "failed".into(),
                error_summary: Some(
                    "Read https://legacy:HISTORY_CANARY@host.invalid/movie?sign=HISTORY_SIGN"
                        .into(),
                ),
                started_at: None,
                ended_at: None,
            })
            .unwrap();
        for path in [
            "/api/library/tasks?server_id=srv-a",
            "/api/library/tasks/scan/failure?server_id=srv-a",
        ] {
            let response = fixture
                .request(Method::GET, path, "review-reader-session", json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let text = read_text(response).await;
            for value in [
                "URL_CANARY",
                "SIGN_CANARY",
                "HISTORY_CANARY",
                "HISTORY_SIGN",
            ] {
                assert!(
                    !text.contains(value),
                    "sensitive task failure data reached response"
                );
            }
        }
        fixture.fail_task_reads.store(true, Ordering::Relaxed);
        let response = fixture
            .request(
                Method::GET,
                "/api/library/tasks/scan/failure?server_id=srv-a",
                "review-reader-session",
                json!(null),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = read_json(response).await;
        assert!(body["summary"].is_string());
        assert!(!body["history"].as_array().unwrap().is_empty());
        for secret in [
            "URL_CANARY",
            "SIGN_CANARY",
            "HISTORY_CANARY",
            "HISTORY_SIGN",
        ] {
            assert!(
                !body.to_string().contains(secret),
                "stored fallback returned sensitive data"
            );
        }
    }
}
