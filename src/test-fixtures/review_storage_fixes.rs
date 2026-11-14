#[test]
fn review_storage_source_session_revocation_blocks_issuance() {
    let (store, _keeper) = memory_security_store("review-token");
    let user = store.create_admin("synthetic", "synthetic-hash").unwrap();
    let now = now_seconds();
    store.create_session(user, "source", now + 3600).unwrap();
    let issue = |hash| {
        store.create_admin_api_token_from_session(
            "source",
            user,
            "fixture",
            "prefix",
            hash,
            &["monitoring.read".into()],
            None,
            None,
            Some(user),
        )
    };
    assert!(issue("first").is_ok());
    store.revoke_all_admin_sessions(user, now).unwrap();
    assert!(matches!(issue("second"), Err(AppError::Unauthorized(_))));
    assert_eq!(store.list_admin_api_tokens(user).unwrap().len(), 1);
}

#[test]
fn review_storage_token_issuance_rechecks_expiry_after_waiting_for_write_lock() {
    use std::{sync::mpsc, thread, time::Duration};
    let path = test_database_path("review-token-expiry-lock");
    let store = SettingsStore::open_for_test(path.clone()).unwrap();
    let user = store
        .create_admin("expiry-writer", "synthetic-hash")
        .unwrap();
    let source = "expiry-source-session";
    let stale_now = now_seconds();
    let expires_at = stale_now + 1;
    store.create_session(user, source, expires_at).unwrap();

    let mut blocker = store.connection().unwrap();
    let tx = blocker
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    let worker_store = store.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        started_tx.send(()).unwrap();
        worker_store.create_admin_api_token_from_session(
            source,
            user,
            "fixture",
            "prefix",
            "late-token-hash",
            &["monitoring.read".into()],
            None,
            None,
            Some(user),
        )
    });
    started_rx.recv().unwrap();
    while now_seconds() <= expires_at {
        thread::sleep(Duration::from_millis(10));
    }
    drop(tx);
    assert!(matches!(
        worker.join().unwrap(),
        Err(AppError::Unauthorized(_))
    ));
    assert!(store.list_admin_api_tokens(user).unwrap().is_empty());
    drop(blocker);
    drop(store);
    remove_test_database(&path);
}

#[test]
fn review_storage_permission_migration_is_independent_and_preserves_custom_roles() {
    let (store, mut conn) = memory_security_store("review-permission");
    conn.execute(
        "DELETE FROM admin_role_permissions WHERE permission = 'sessions.control'",
        [],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM schema_migrations WHERE version = 'admin_sessions_control_v1'",
        [],
    )
    .unwrap();
    store.migrate(&mut conn).unwrap();
    store.migrate(&mut conn).unwrap();
    let granted: Vec<String> = conn.prepare(
        "SELECT r.key FROM admin_roles r JOIN admin_role_permissions p ON p.role_id = r.id WHERE p.permission = 'sessions.control'"
    ).unwrap().query_map([], |row| row.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
    assert_eq!(granted, vec!["operator"]);
    conn.execute(
        "UPDATE admin_roles SET is_system = 0 WHERE key = 'operator'",
        [],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM admin_role_permissions WHERE permission = 'sessions.control'",
        [],
    )
    .unwrap();
    conn.execute(
        "DELETE FROM schema_migrations WHERE version = 'admin_sessions_control_v1'",
        [],
    )
    .unwrap();
    store.migrate(&mut conn).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM admin_role_permissions WHERE permission = 'sessions.control'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn review_storage_analytics_filters_before_latest_cap() {
    let (store, conn) = memory_security_store("review-analytics");
    let now = now_ms() as i64;
    for (index, server, kind, bandwidth) in [
        (0, "allowed", "playback", Some(100)),
        (1, "other", "playback", Some(200)),
        (2, "allowed", "request", None),
        (3, "allowed", "playback", Some(300)),
    ] {
        conn.execute(
            "INSERT INTO proxy_analytics_samples(timestamp_ms, server_id, server_name, sample_kind, session_key, playback_user, client, bandwidth, created_at_ms)
             VALUES (?1, ?2, ?2, ?3, ?4, 'user', 'client', ?5, ?1)",
            params![now - 100 + index, server, kind, format!("session-{index}"), bandwidth],
        ).unwrap();
    }
    let allowed = std::collections::HashSet::from(["allowed".to_string()]);
    let samples = store
        .list_proxy_analytics_samples_filtered(
            1,
            None,
            Some(&allowed),
            Some("playback"),
            true,
            Some("user"),
            Some("client"),
            1,
        )
        .unwrap();
    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].bandwidth, Some(300));
    assert!(
        store
            .list_proxy_analytics_samples_filtered(
                1,
                None,
                Some(&std::collections::HashSet::new()),
                None,
                false,
                None,
                None,
                10,
            )
            .unwrap()
            .is_empty()
    );
}

#[test]
fn review_storage_failure_retention_runs_on_write_and_is_throttled() {
    let (store, conn) = memory_security_store("review-retention");
    let now = now_ms() as i64;
    conn.execute(
        "INSERT INTO scheduled_task_failure_history(server_id, task_id, run_identity, observed_at, status)
         VALUES ('s', 't', 'expired', ?1, 'failed')", [now - 91 * TASK_FAILURE_HISTORY_DAY_MS],
    ).unwrap();
    conn.execute_batch(
        "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < 20001)
         INSERT INTO scheduled_task_failure_history(server_id, task_id, run_identity, observed_at, status)
         SELECT 's', 't', CAST(x AS TEXT), CAST(strftime('%s','now') AS INTEGER) * 1000 + x, 'failed' FROM n"
    ).unwrap();
    let history = store.task_failure_history_store();
    let input = |run: &str| ScheduledTaskFailureInput {
        server_id: "s".into(),
        task_id: "t".into(),
        run_identity: run.into(),
        observed_at: now,
        status: "failed".into(),
        error_summary: None,
        started_at: None,
        ended_at: None,
    };
    history.record_failure(input("new")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM scheduled_task_failure_history",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 20000);
    let expired: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM scheduled_task_failure_history WHERE run_identity = 'expired'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(expired, 0);
    history.record_failure(input("another")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM scheduled_task_failure_history",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 20001);
}

#[test]
fn review_storage_task_failure_sanitizes_signed_url_before_persisting() {
    let (store, conn) = memory_security_store("review-task-url-redaction");
    let history = store.task_failure_history_store();
    let input = ScheduledTaskFailureInput {
        server_id: "s".into(), task_id: "t".into(), run_identity: "run".into(),
        observed_at: now_ms() as i64, status: "failed".into(),
        error_summary: Some("download https://operator:PASSWORD_CANARY@cdn.invalid/movie?sign=SIGN_CANARY&X-Amz-Credential=AWS_CANARY".into()),
        started_at: None, ended_at: None,
    };
    let stored = history.record_failure(input).unwrap();
    let value = stored.safe_error_summary.as_deref().unwrap();
    for canary in ["PASSWORD_CANARY", "SIGN_CANARY", "AWS_CANARY", "operator:"] {
        assert!(!value.contains(canary), "persisted task summary leaked {canary}");
    }
    let persisted: String = conn.query_row(
        "SELECT safe_error_summary FROM scheduled_task_failure_history WHERE run_identity='run'",
        [], |row| row.get(0),
    ).unwrap();
    assert_eq!(persisted, value);
}

#[test]
fn review_storage_viewer_and_read_token_cannot_control_sessions() {
    use crate::auth::{AuthContext, AuthKind, require_permission};
    let mut ctx = AuthContext {
        user_id: 1,
        kind: AuthKind::Session,
        scopes: None,
        server_ids: None,
        role_keys: vec!["viewer".into()],
        permission_keys: vec!["monitoring.read".into()],
        server_grants: vec![],
    };
    assert!(matches!(
        require_permission(&ctx, "sessions.control"),
        Err(AppError::Forbidden(_))
    ));
    ctx.kind = AuthKind::ApiToken;
    ctx.role_keys = vec!["operator".into()];
    ctx.permission_keys.push("sessions.control".into());
    ctx.scopes = Some(vec!["monitoring.read".into()]);
    assert!(matches!(
        require_permission(&ctx, "sessions.control"),
        Err(AppError::Forbidden(_))
    ));
    ctx.scopes = Some(vec!["sessions.control".into()]);
    assert!(require_permission(&ctx, "sessions.control").is_ok());
}

#[test]
fn review_storage_request_scope_precedes_cap() {
    let (store, conn) = memory_security_store("review-requests");
    for (timestamp, server) in [(1, "allowed"), (2, "other")] {
        conn.execute(
            "INSERT INTO proxy_request_logs(timestamp_ms,server_id,server_name,port,method,path,path_type,status_code,outcome,duration_ms,playback_user,playback_ip,detail)
             VALUES (?1,?2,?2,8096,'GET','/test','other',200,'ok',1,'','','')",
            params![timestamp, server],
        ).unwrap();
    }
    let allowed = std::collections::HashSet::from(["allowed".to_string()]);
    let rows = store
        .list_proxy_request_details_scoped(
            ProxyRequestDetailFilter {
                server_id: None,
                path_type: None,
                exclude_path_type: None,
                keyword: None,
                since_ms: None,
                until_ms: None,
                limit: 1,
            },
            Some(&allowed),
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].server_id, "allowed");
}

#[test]
fn review_storage_issuance_rechecks_current_permissions_and_owner() {
    let (store, conn) = memory_security_store("review-issuance-permissions");
    let user = store
        .create_admin_with_roles("writer", "synthetic-hash", None, &["operator"], None)
        .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO admin_role_permissions(role_id,permission) SELECT id,'admin.security.write' FROM admin_roles WHERE key = 'operator'", [],
    ).unwrap();
    let now = now_seconds();
    store.create_session(user, "source", now + 3600).unwrap();
    let issue = |hash| {
        store.create_admin_api_token_from_session(
            "source",
            user,
            "fixture",
            "prefix",
            hash,
            &["monitoring.read".into()],
            None,
            None,
            Some(user),
        )
    };
    assert!(issue("first").is_ok());
    conn.execute(
        "DELETE FROM admin_role_permissions WHERE permission = 'admin.security.write'",
        [],
    )
    .unwrap();
    assert!(matches!(
        issue("missing-write"),
        Err(AppError::Forbidden(_))
    ));
    conn.execute("UPDATE admin_users SET enabled = 0 WHERE id = ?1", [user])
        .unwrap();
    assert!(matches!(issue("disabled"), Err(AppError::Unauthorized(_))));
    assert_eq!(store.list_admin_api_tokens(user).unwrap().len(), 1);
}
