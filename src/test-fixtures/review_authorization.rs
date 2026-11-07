// Included by main.rs tests; requests go through the production management Router.
mod review_authorization {
    use super::*;
    use aes_gcm::{
        Aes256Gcm, Nonce,
        aead::{Aead, KeyInit},
    };
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use rsa::{Oaep, RsaPublicKey, pkcs8::DecodePublicKey, rand_core::OsRng};
    use sha2::Sha256;

    const VIEWER_SESSION: &str = "synthetic-viewer-session";
    const READ_TOKEN: &str = "synthetic-read-token";
    const USERS_TOKEN: &str = "synthetic-users-only-token";

    struct Fixture {
        state: AppState,
        app: Router,
        viewer_id: i64,
        upstream_counts: [Arc<AtomicUsize>; 2],
        upstream_tasks: Vec<tokio::task::JoinHandle<()>>,
    }

    impl Fixture {
        async fn new() -> Self {
            let counts = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
            let mut hosts = Vec::new();
            let mut upstream_tasks = Vec::new();
            for counter in &counts {
                let counter = counter.clone();
                let mock = Router::new().fallback(any(move |request: Request| {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::Relaxed);
                        assert_eq!(
                            request.method(),
                            Method::GET,
                            "no writes may reach mock Emby"
                        );
                        response_json(json!([]))
                    }
                }));
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                hosts.push(format!("http://{}", listener.local_addr().unwrap()));
                upstream_tasks.push(tokio::spawn(async move {
                    axum::serve(listener, mock).await.unwrap();
                }));
            }
            let store = test_store();
            let super_id = store.list_admin_users().unwrap()[0].id;
            let viewer_id = store
                .create_admin_with_roles(
                    "fixture-viewer",
                    "synthetic-hash",
                    None,
                    &["viewer"],
                    Some(super_id),
                )
                .unwrap();
            store
                .replace_admin_server_grants(
                    viewer_id,
                    &[db::AdminServerGrantInput {
                        server_id: "srv-a".into(),
                        scope: "full".into(),
                    }],
                    Some(super_id),
                    auth::now_ts(),
                )
                .unwrap();
            store
                .create_session(
                    viewer_id,
                    &auth::hash_token(VIEWER_SESSION),
                    auth::now_ts() + 3600,
                )
                .unwrap();
            // A super owner makes the token's own scope/server restrictions decisive.
            for (token, scopes) in [
                (
                    READ_TOKEN,
                    vec![
                        "users.read".into(),
                        "monitoring.read".into(),
                        "settings.read".into(),
                    ],
                ),
                (USERS_TOKEN, vec!["users.read".into()]),
            ] {
                store
                    .create_admin_api_token(
                        super_id,
                        token,
                        "synthetic-prefix",
                        &auth::hash_token(token),
                        &scopes,
                        Some(&["srv-a".into()]),
                        None,
                        Some(super_id),
                        auth::now_ts(),
                    )
                    .unwrap();
            }
            let mut config = Config::default_runtime();
            config.emby_host = hosts[1].clone();
            config.emby_api_key = "global-secret-canary".into();
            config.openlist_token = Some("openlist-secret-canary".into());
            config.servers = ["srv-a", "srv-b"]
                .iter()
                .enumerate()
                .map(|(index, id)| {
                    serde_json::from_value(json!({
                        "id": id, "name": id, "emby_host": hosts[index],
                        "emby_api_key": "server-secret-canary", "port": 18096 + index,
                        "openlist_addr_override": "http://127.0.0.1:1",
                        "openlist_token_override": "override-secret-canary",
                    }))
                    .unwrap()
                })
                .collect();
            let state = test_state(config, store);
            for id in ["srv-a", "srv-b"] {
                state
                    .cache
                    .read()
                    .await
                    .set(
                        format!("{id}:media:source:fixture-ua"),
                        "http://synthetic.invalid/secret-canary".into(),
                    )
                    .await;
            }
            let app = build_management_app(state.clone());
            Self {
                state,
                app,
                viewer_id,
                upstream_counts: counts,
                upstream_tasks,
            }
        }

        async fn request(
            &self,
            method: Method,
            uri: &str,
            token: Option<&str>,
            payload: serde_json::Value,
        ) -> Response {
            let mut request = HttpRequest::builder()
                .method(method)
                .uri(uri)
                .header(header::CONTENT_TYPE, "application/json");
            if let Some(token) = token {
                request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            self.app
                .clone()
                .oneshot(request.body(Body::from(payload.to_string())).unwrap())
                .await
                .unwrap()
        }

        async fn encrypted(&self, name: &str, value: serde_json::Value) -> serde_json::Value {
            let response = self
                .request(Method::GET, "/api/public-key", None, json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let public_key = read_json(response).await;
            let public_key =
                RsaPublicKey::from_public_key_pem(public_key["public_key_pem"].as_str().unwrap())
                    .unwrap();
            let aes_key = [7u8; 32];
            let nonce = [9u8; 12];
            let cipher = Aes256Gcm::new_from_slice(&aes_key).unwrap();
            let plaintext = serde_json::to_vec(&json!({"name": name, "value": value})).unwrap();
            let ciphertext = cipher
                .encrypt(Nonce::from_slice(&nonce), plaintext.as_slice())
                .unwrap();
            let wrapped_key = public_key
                .encrypt(&mut OsRng, Oaep::new::<Sha256>(), &aes_key)
                .unwrap();
            json!({"encrypted_key": URL_SAFE_NO_PAD.encode(wrapped_key), "fields": {
                "field": {"iv": URL_SAFE_NO_PAD.encode(nonce), "value": URL_SAFE_NO_PAD.encode(ciphertext)}
            }})
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.state.telemetry_writes.shutdown().unwrap();
            for task in &self.upstream_tasks {
                task.abort();
            }
        }
    }

    #[tokio::test]
    async fn viewer_and_read_token_write_matrix_returns_403_and_preserves_state() {
        let fixture = Fixture::new().await;
        // Structurally valid envelopes reach authorization; forbidden requests
        // must be rejected before their deliberately invalid encryption is read.
        let envelope = json!({"encrypted_key":"invalid-synthetic-key", "fields":{}});
        let writes = [
            (Method::POST, "/api/users/srv-a", envelope.clone()),
            (Method::DELETE, "/api/users/srv-a/user", envelope.clone()),
            (
                Method::PUT,
                "/api/users/srv-a/user/policy",
                envelope.clone(),
            ),
            (
                Method::POST,
                "/api/users/srv-a/user/password",
                envelope.clone(),
            ),
            (Method::POST, "/api/user-templates/srv-a", envelope.clone()),
            (
                Method::DELETE,
                "/api/user-templates/srv-a/template",
                envelope.clone(),
            ),
            (
                Method::POST,
                "/api/cache/cleanup",
                json!({"scope":"server","serverId":"srv-a"}),
            ),
            (Method::PUT, "/api/settings", envelope.clone()),
            (Method::POST, "/api/settings/validate", envelope.clone()),
            (
                Method::POST,
                "/api/settings/servers/srv-b/api-key",
                json!(null),
            ),
            (
                Method::POST,
                "/api/settings/backup/export",
                envelope.clone(),
            ),
            (
                Method::POST,
                "/api/settings/backup/import",
                envelope.clone(),
            ),
            (Method::PUT, "/api/settings/log-config", envelope.clone()),
            (
                Method::POST,
                "/api/settings/restart-proxy",
                json!({"server_id":"srv-a"}),
            ),
            (
                Method::POST,
                "/api/settings/toggle-proxy",
                json!({"server_id":"srv-a","enabled":false}),
            ),
        ];
        let before = serde_json::to_value(&*fixture.state.config.read().await).unwrap();
        for (method, uri, payload) in writes {
            for (token, expected) in [
                (None, StatusCode::UNAUTHORIZED),
                (Some("synthetic-unknown-token"), StatusCode::UNAUTHORIZED),
                (Some(VIEWER_SESSION), StatusCode::FORBIDDEN),
                (Some(READ_TOKEN), StatusCode::FORBIDDEN),
            ] {
                let response = fixture
                    .request(method.clone(), uri, token, payload.clone())
                    .await;
                assert_eq!(
                    response.status(),
                    expected,
                    "{method} {uri}, token={token:?}"
                );
                assert_eq!(
                    read_text(response).await,
                    if expected == StatusCode::FORBIDDEN {
                        "forbidden"
                    } else {
                        "unauthorized"
                    }
                );
            }
        }
        assert_eq!(
            serde_json::to_value(&*fixture.state.config.read().await).unwrap(),
            before
        );
        for server in ["srv-a", "srv-b"] {
            assert!(
                fixture
                    .state
                    .cache
                    .read()
                    .await
                    .get(&format!("{server}:media:source:fixture-ua"))
                    .await
                    .is_some()
            );
        }
        assert!(
            fixture
                .state
                .settings_store
                .list_audit_logs(None, None, 100)
                .unwrap()
                .is_empty()
        );
        assert!(
            fixture
                .upstream_counts
                .iter()
                .all(|count| count.load(Ordering::Relaxed) == 0)
        );
        for token in [VIEWER_SESSION, READ_TOKEN] {
            assert_eq!(
                fixture
                    .request(Method::GET, "/api/settings", Some(token), json!(null))
                    .await
                    .status(),
                StatusCode::OK
            );
        }
    }

    #[tokio::test]
    async fn authorized_reads_filter_servers_and_wrong_scopes_are_forbidden() {
        let fixture = Fixture::new().await;
        for token in [VIEWER_SESSION, READ_TOKEN] {
            let response = fixture
                .request(Method::GET, "/api/settings", Some(token), json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let settings = read_json(response).await;
            assert_eq!(settings["servers"].as_array().unwrap().len(), 1);
            assert_eq!(settings["servers"][0]["id"], "srv-a");
            let text = settings.to_string();
            assert!(!text.contains("srv-b"));
            assert!(!text.contains("secret-canary"));
            let response = fixture
                .request(Method::GET, "/api/cache/entries", Some(token), json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let entries = read_json(response).await;
            assert_eq!(entries.as_array().unwrap().len(), 1);
            assert_eq!(entries[0]["serverId"], "srv-a");
            let response = fixture
                .request(Method::GET, "/api/users", Some(token), json!(null))
                .await;
            assert_eq!(response.status(), StatusCode::OK);
            let users = read_json(response).await;
            assert_eq!(users["servers"].as_array().unwrap().len(), 1);
            assert_eq!(users["servers"][0]["id"], "srv-a");
            assert_eq!(users["server_errors"], json!([]));
            for uri in [
                "/api/users?server_id=srv-b",
                "/api/cache/entries?serverId=srv-b",
            ] {
                assert_eq!(
                    fixture
                        .request(Method::GET, uri, Some(token), json!(null))
                        .await
                        .status(),
                    StatusCode::FORBIDDEN,
                    "{uri}"
                );
            }
        }
        assert!(fixture.upstream_counts[0].load(Ordering::Relaxed) > 0);
        assert_eq!(fixture.upstream_counts[1].load(Ordering::Relaxed), 0);
        for uri in [
            "/api/settings",
            "/api/cache/entries",
            "/api/history",
            "/api/diagnostics",
        ] {
            assert_eq!(
                fixture
                    .request(Method::GET, uri, Some(USERS_TOKEN), json!(null))
                    .await
                    .status(),
                StatusCode::FORBIDDEN,
                "{uri}"
            );
            for token in [None, Some("synthetic-unknown-token")] {
                assert_eq!(
                    fixture
                        .request(Method::GET, uri, token, json!(null))
                        .await
                        .status(),
                    StatusCode::UNAUTHORIZED,
                    "{uri}"
                );
            }
        }
    }

    #[tokio::test]
    async fn account_changes_require_session_and_allow_non_super_self_profile_update() {
        let fixture = Fixture::new().await;
        let profile = fixture
            .encrypted("profile", json!({"username":"renamed-viewer"}))
            .await;
        let password = fixture
            .encrypted(
                "password",
                json!({"current_password":"synthetic", "new_password":"synthetic-next"}),
            )
            .await;
        let owner_before = fixture
            .state
            .settings_store
            .admin_password_hash_by_id(1)
            .unwrap()
            .unwrap()
            .password_hash;
        for (method, uri, payload) in [
            (Method::PUT, "/api/profile", profile.clone()),
            (Method::POST, "/api/change-password", password),
        ] {
            for (token, expected) in [
                (Some(READ_TOKEN), StatusCode::FORBIDDEN),
                (Some(USERS_TOKEN), StatusCode::FORBIDDEN),
                (None, StatusCode::UNAUTHORIZED),
                (Some("synthetic-unknown-token"), StatusCode::UNAUTHORIZED),
            ] {
                let response = fixture
                    .request(method.clone(), uri, token, payload.clone())
                    .await;
                assert_eq!(response.status(), expected, "{uri}");
                assert_eq!(
                    read_text(response).await,
                    if expected == StatusCode::FORBIDDEN {
                        "forbidden"
                    } else {
                        "unauthorized"
                    }
                );
            }
        }
        assert_eq!(
            fixture
                .state
                .settings_store
                .admin_password_hash_by_id(1)
                .unwrap()
                .unwrap()
                .password_hash,
            owner_before
        );
        assert_eq!(
            fixture
                .state
                .settings_store
                .admin_username_by_id(1)
                .unwrap()
                .as_deref(),
            Some("admin")
        );
        assert_eq!(
            fixture
                .state
                .settings_store
                .admin_username_by_id(fixture.viewer_id)
                .unwrap()
                .as_deref(),
            Some("fixture-viewer")
        );
        let response = fixture
            .request(Method::PUT, "/api/profile", Some(VIEWER_SESSION), profile)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(read_json(response).await["username"], "renamed-viewer");
        assert_eq!(
            fixture
                .state
                .settings_store
                .admin_username_by_id(fixture.viewer_id)
                .unwrap()
                .as_deref(),
            Some("renamed-viewer")
        );
        let identity = fixture
            .state
            .settings_store
            .admin_security_identity(fixture.viewer_id)
            .unwrap()
            .unwrap();
        assert_eq!(identity.role_keys, ["viewer"]);
        let response = fixture
            .request(
                Method::GET,
                "/api/profile",
                Some(VIEWER_SESSION),
                json!(null),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(read_json(response).await["username"], "renamed-viewer");
        assert!(
            fixture
                .upstream_counts
                .iter()
                .all(|count| count.load(Ordering::Relaxed) == 0)
        );
    }
}
