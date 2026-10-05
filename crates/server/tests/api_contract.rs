//! HTTP 契约金测 —— 起完整 App（tempdir + 内存级 SQLite），对齐 Python conftest.py 语义。
//! 锁定：认证流、查询语义（@@@LINK/前缀兜底/语言路由）、生词本、Token 配额、错误包络。

use actix_web::test as actix_test;
use actix_web::{web, App};
use serde_json::Value;
use server::AppState;

struct TestApp {
    state: std::sync::Arc<AppState>,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

async fn spawn_app() -> TestApp {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("test.sqlite3");
    // 先跑迁移
    let settings = test_settings(&dir);
    let db = server::core::db::connect(&settings).await.expect("connect");
    use sea_orm_migration::MigratorTrait;
    migration::Migrator::up(&db, None).await.expect("migrate");
    let state = std::sync::Arc::new(AppState::new(settings, db));
    TestApp { state, dir }
}

fn test_settings(dir: &tempfile::TempDir) -> server::core::config::Settings {
    let mut s = server::core::config::Settings::from_env();
    s.jwt_secret = "integration-test-secret".to_string();
    s.database_path = dir.path().join("test.sqlite3").to_string_lossy().to_string();
    s.database_url = format!("sqlite:///{}", s.database_path);
    s.config_storage_path = dir.path().join("config").to_string_lossy().to_string();
    s.dicts_inbox_path = dir.path().join("dicts").to_string_lossy().to_string();
    s.dictionary_storage_path = dir.path().join("dictionaries").to_string_lossy().to_string();
    s.log_dir = dir.path().join("logs").to_string_lossy().to_string();
    s
}

async fn init_service(
    state: &std::sync::Arc<AppState>,
) -> impl actix_web::dev::Service<actix_http::Request, Response = actix_web::dev::ServiceResponse, Error = actix_web::Error> {
    let state = state.clone();
    actix_test::init_service(
        App::new().configure(move |cfg| {
            server::configure_app(cfg, state.clone());
        }),
    )
    .await
}

/// 造一部「英→中」测试词典：直接写 dict_entries（跳过解析管线，专注查询语义）
async fn seed_dictionary(
    state: &AppState,
    name: &str,
    lang_from: &str,
    lang_to: &str,
    entries: &[(&str, &str)],
) -> i32 {
    use sea_orm::ConnectionTrait;
    let backend = state.db.get_database_backend();
    let now = chrono::Utc::now().timestamp();
    let row = state
        .db
        .query_one_raw(sea_orm::Statement::from_sql_and_values(
            backend,
            "INSERT INTO dictionaries (name, format, lang_from, lang_to, status, imported_at, word_count) \
             VALUES ($1, 'ecdict', $2, $3, 'enabled', $4, $5) RETURNING id",
            [
                name.into(),
                lang_from.into(),
                lang_to.into(),
                now.into(),
                (entries.len() as i32).into(),
            ],
        ))
        .await
        .expect("insert dict");
    let dict_id: i32 = row.unwrap().try_get("", "id").unwrap();
    for (word, definition) in entries {
        state
            .db
            .execute_raw(sea_orm::Statement::from_sql_and_values(
                backend,
                "INSERT INTO dict_entries (dictionary_id, word, word_lower, definition, generation) \
                 VALUES ($1, $2, $3, $4, 0)",
                [
                    dict_id.into(),
                    (*word).into(),
                    word.to_lowercase().into(),
                    (*definition).into(),
                ],
            ))
            .await
            .expect("insert entry");
    }
    dict_id
}

async fn admin_setup<S>(app: &mut S) -> String
where
    S: actix_web::dev::Service<actix_http::Request, Response = actix_web::dev::ServiceResponse, Error = actix_web::Error>,
{
    let req = actix_test::TestRequest::post()
        .uri("/api/admin/setup")
        .set_json(serde_json::json!({"username": "admin", "password": "password123"}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(app, req).await;
    resp["access_token"].as_str().expect("token").to_string()
}

use actix_web::test::TestRequest;

trait BearerExt {
    fn pipe_bearer(self, token: &str) -> Self;
}

impl BearerExt for TestRequest {
    fn pipe_bearer(self, token: &str) -> Self {
        self.insert_header((
            actix_web::http::header::AUTHORIZATION,
            actix_web::http::header::HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        ))
    }
}

// ── 认证流 ───────────────────────────────────────────────────────

#[actix_web::test]
async fn admin_setup_login_refresh_me_flow() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;

    // bootstrap-status：false
    let req = actix_test::TestRequest::get().uri("/api/admin/bootstrap-status").to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["initialized"], false);

    // 用户名过短 → 422
    let req = actix_test::TestRequest::post()
        .uri("/api/admin/setup")
        .set_json(serde_json::json!({"username": "ab", "password": "password123"}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 422);

    // setup 成功 → token pair
    let token = admin_setup(&mut svc).await;
    assert!(!token.is_empty());

    // 重复 setup → 409 admin_already_initialized
    let req = actix_test::TestRequest::post()
        .uri("/api/admin/setup")
        .set_json(serde_json::json!({"username": "admin2", "password": "password123"}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 409);
    let body: Value = actix_test::read_body_json(resp).await;
    assert_eq!(body["code"], "admin_already_initialized");

    // me
    let req = actix_test::TestRequest::get()
        .uri("/api/admin/me")
        .pipe_bearer(&token)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["username"], "admin");

    // 错 token → 401
    let req = actix_test::TestRequest::get()
        .uri("/api/admin/me")
        .pipe_bearer("bad.jwt.token")
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 401);

    // login
    let req = actix_test::TestRequest::post()
        .uri("/api/admin/login")
        .set_json(serde_json::json!({"username": "admin", "password": "wrongpass99"}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = actix_test::read_body_json(resp).await;
    assert_eq!(body["code"], "invalid_credentials");

    let req = actix_test::TestRequest::post()
        .uri("/api/admin/login")
        .set_json(serde_json::json!({"username": "admin", "password": "password123"}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let refresh = resp["refresh_token"].as_str().unwrap().to_string();
    assert_eq!(resp["token_type"], "bearer");

    // refresh：换新 access，refresh 原样返回
    let req = actix_test::TestRequest::post()
        .uri("/api/admin/refresh")
        .set_json(serde_json::json!({"refresh_token": refresh}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert!(!resp["access_token"].as_str().unwrap().is_empty());
    assert_eq!(resp["refresh_token"].as_str().unwrap(), &refresh);
}

#[actix_web::test]
async fn user_register_login_and_self_dictionaries() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;

    // 注册
    let req = actix_test::TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({"username": "alice", "password": "alicepass1"}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["username"], "alice");

    // 重名 → 409
    let req = actix_test::TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({"username": "alice", "password": "alicepass1"}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 409);

    // 登录
    let req = actix_test::TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({"username": "alice", "password": "alicepass1"}))
        .to_request();
    let user_token: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let user_access = user_token["access_token"].as_str().unwrap().to_string();

    // 造两部词典，管理端给 alice 划上限 [1]
    seed_dictionary(&app.state, "词典A", "en", "en", &[("apple", "a")]).await;
    seed_dictionary(&app.state, "词典B", "en", "en", &[("banana", "b")]).await;
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/users/1/allowed-dictionaries")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"dictionary_ids": [1]}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["allowed_dictionary_ids"], serde_json::json!([1]), "实际: {resp}");

    // 用户自选 [1,2]：夹在上限内 → 只剩 [1]
    let req = actix_test::TestRequest::put()
        .uri("/api/auth/allowed-dictionaries")
        .pipe_bearer(&user_access)
        .set_json(serde_json::json!({"dictionary_ids": [1, 2]}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["allowed_dictionary_ids"], serde_json::json!([1]));

    // 实际可用词典 = 上限 ∩ 自选 = [1]
    let req = actix_test::TestRequest::get()
        .uri("/api/dict/dictionaries")
        .pipe_bearer(&user_access)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp.as_array().unwrap().len(), 1, "实际可用应只有 1 部");
}

// ── 查询语义 ─────────────────────────────────────────────────────

#[actix_web::test]
async fn query_link_redirect_and_prefix_fallback() {
    let app = spawn_app().await;
    // 词典：appel → @@@LINK=apple（链式 appel→apple2→apple 双跳）；注记后缀词头
    seed_dictionary(
        &app.state,
        "测试词典",
        "en",
        "zh-Hans",
        &[
            ("apple", "<p>n. 苹果</p>"),
            ("apple2", "@@@LINK=apple"),
            ("appel", "@@@LINK=apple2"),
            ("あらし", "暴风雨（注记后缀词头：あらし【嵐】的精确匹配打不中，靠前缀兜底）"),
        ],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    // 开放匿名查询
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success(), "开启 open_access 失败");

    // 链式 @@@LINK：appel → apple2 → apple，词头仍显示 appel
    let req = actix_test::TestRequest::get()
        .uri("/api/v1/query?word=appel")
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["word"], "appel");
    assert_eq!(results[0]["definition"], "n. 苹果");

    // 前缀兜底：あらし 精确未命中 → 前缀命中「あらし【嵐】」类词头
    let req = actix_test::TestRequest::get()
        .uri("/api/v1/query?word=%E3%81%82%E3%82%89%E3%81%97") // あらし
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let results = resp["results"].as_array().unwrap();
    assert!(!results.is_empty(), "前缀兜底应命中注记后缀词头");
    assert!(results[0]["word"].as_str().unwrap().starts_with("あらし"));

    // % 与 _ 按字面匹配（不作为 LIKE 通配符）
    seed_dictionary(&app.state, "符号词典", "en", "en", &[("100%", "百分之百")]).await;
    let req = actix_test::TestRequest::get().uri("/api/v1/query?word=100%25").to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["results"].as_array().unwrap().len(), 1);
}

#[actix_web::test]
async fn query_language_routing_and_fallback() {
    let app = spawn_app().await;
    // 中文输入 + 一部被误判成 en 的中文词典（lang_from=en）→ 兜底路径仍可达
    seed_dictionary(&app.state, "正常中文词典", "zh-Hans", "en", &[("苹果", "apple (正确路由)")]).await;
    seed_dictionary(&app.state, "误判词典", "en", "zh-Hans", &[("香蕉", "banana (语言兜底命中)")]).await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;

    // 开放匿名查询
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success());

    // 中文输入命中正常词典
    let req = actix_test::TestRequest::get().uri("/api/v1/query?word=%E8%8B%B9%E6%9E%9C").to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["dictionary_name"], "正常中文词典");
    assert_eq!(results[0]["lang_match"], true);

    // 优先语言（zh）没命中时，其余语言（被误判的 en 词典）参与兜底
    let req = actix_test::TestRequest::get().uri("/api/v1/query?word=%E9%A6%99%E8%95%89").to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "语言误判的词典仍应可达");
    assert_eq!(results[0]["dictionary_name"], "误判词典");
    assert_eq!(results[0]["lang_match"], false);
}

#[actix_web::test]
async fn full_style_controls_html_stripping() {
    let app = spawn_app().await;
    seed_dictionary(
        &app.state,
        "样式词典",
        "en",
        "zh-Hans",
        &[("豫章", "<p>豫章</p><p>江西省的别称。</p>")],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success(), "开启 open_access 失败");

    // 默认 full_style=false：HTML → 纯文本（Python 用例锁定的输出）
    let req = actix_test::TestRequest::get()
        .uri("/api/v1/query?word=%E8%B1%AB%E7%AB%A0")
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["results"][0]["definition"], "豫章\n江西省的别称。");

    // full_style=true：保留原文
    let req = actix_test::TestRequest::get()
        .uri("/api/v1/query?word=%E8%B1%AB%E7%AB%A0&full_style=true")
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["results"][0]["definition"], "<p>豫章</p><p>江西省的别称。</p>");
}

#[actix_web::test]
async fn token_daily_quota_returns_429_with_retry_after() {
    let app = spawn_app().await;
    seed_dictionary(&app.state, "配额词典", "en", "en", &[("apple", "n. 苹果")]).await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;

    // 创建 daily_limit=2 的 Token
    let req = actix_test::TestRequest::post()
        .uri("/api/admin/tokens")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"name": "小配额", "daily_limit": 2}))
        .to_request();
    let created: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let raw_token = created["token"].as_str().unwrap().to_string();
    assert!(raw_token.starts_with("sk-"));

    for i in 1..=3 {
        let req = actix_test::TestRequest::get()
            .uri("/api/v1/query?word=apple")
            .pipe_bearer(&raw_token)
            .to_request();
        let resp = actix_test::call_service(&mut svc, req).await;
        match i {
            1 | 2 => assert_eq!(resp.status().as_u16(), 200, "第 {i} 次应放行"),
            3 => {
                assert_eq!(resp.status().as_u16(), 429, "第 3 次应 429");
                let retry = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<i64>().ok())
                    .expect("Retry-After 头");
                assert!(retry > 0);
                let body: Value = actix_test::read_body_json(resp).await;
                assert_eq!(body["code"], "rate_limited");
            }
            _ => unreachable!(),
        }
    }

    // 无效 Token → 401
    let req = actix_test::TestRequest::get()
        .uri("/api/v1/query?word=apple")
        .pipe_bearer("sk-invalid-token-value")
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = actix_test::read_body_json(resp).await;
    assert_eq!(body["code"], "unauthorized");
}

#[actix_web::test]
async fn open_access_gate_for_api_and_web() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;

    // open_access 关闭 → 匿名 401
    let req = actix_test::TestRequest::get().uri("/api/v1/query?word=x").to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 401);
    let body: Value = actix_test::read_body_json(resp).await;
    assert_eq!(body["code"], "unauthorized");

    let req = actix_test::TestRequest::get().uri("/api/dict/search?word=x").to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 401);

    // 开启后放行
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success());
    let req = actix_test::TestRequest::get().uri("/api/v1/query?word=x").to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 200);
}

#[actix_web::test]
async fn vocab_snapshot_stores_queried_word_and_resolved_content() {
    let app = spawn_app().await;
    seed_dictionary(
        &app.state,
        "链接词典",
        "en",
        "en",
        &[("appel", "@@@LINK=apple"), ("apple", "<b>n. 苹果</b>")],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;

    // 用户 Token 收藏：词头=appel（用户查的），快照=apple 的释义
    let req = actix_test::TestRequest::post()
        .uri("/api/auth/register")
        .set_json(serde_json::json!({"username": "bob", "password": "bobpasswd1"}))
        .to_request();
    let _ = actix_test::call_service(&mut svc, req).await;
    let req = actix_test::TestRequest::post()
        .uri("/api/auth/login")
        .set_json(serde_json::json!({"username": "bob", "password": "bobpasswd1"}))
        .to_request();
    let login: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let bob_access = login["access_token"].as_str().unwrap().to_string();
    let req = actix_test::TestRequest::post()
        .uri("/api/auth/api-token")
        .pipe_bearer(&bob_access)
        .to_request();
    let token_resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let bob_api_token = token_resp["api_token"].as_str().unwrap().to_string();

    let req = actix_test::TestRequest::post()
        .uri("/api/v1/vocab")
        .pipe_bearer(&bob_api_token)
        .set_json(serde_json::json!({"word": "appel"}))
        .to_request();
    let item: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(item["word"], "appel", "词头是用户查到的那个");
    assert_eq!(item["definition"], "<b>n. 苹果</b>", "快照是目标的释义");

    // 用户生词本列表
    let req = actix_test::TestRequest::get()
        .uri("/api/vocab")
        .pipe_bearer(&bob_access)
        .to_request();
    let list: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(list["total"], 1);
    assert_eq!(list["items"][0]["word"], "appel");
    let _ = admin;
}

#[actix_web::test]
async fn entry_document_renders_multi_entries_with_sandbox() {
    let app = spawn_app().await;
    seed_dictionary(
        &app.state,
        "双条词典",
        "en",
        "en",
        &[("bank", "<p>银行</p>"), ("bank", "<p>河岸</p>")],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;

    let req = actix_test::TestRequest::get()
        .uri("/api/dict/entry/1?word=bank&theme=dark")
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    // open_access 关闭且未登录 → 401（统一口诀另一档：登录后 404/200）
    assert_eq!(resp.status().as_u16(), 401);

    // 开放后匿名可取
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success());
    let req = actix_test::TestRequest::get()
        .uri("/api/dict/entry/1?word=bank&theme=dark")
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 200);
    let body = String::from_utf8(actix_test::read_body(resp).await.to_vec()).unwrap();
    assert!(body.contains("sandbox allow-scripts"));
    assert!(body.contains("data-mydict-theme','dark'"));
    assert!(body.contains("mydict-entry-index\">1/2<"));
    assert!(body.contains("银行"));
    assert!(body.contains("河岸"));

    // 不存在的词 → 404
    let req = actix_test::TestRequest::get().uri("/api/dict/entry/1?word=nonexistent").to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 404);
    // 禁用的词典 → 404（统一口径，不泄漏存在性）
    let req = actix_test::TestRequest::get().uri("/api/dict/entry/999?word=x").to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 404);
}

#[actix_web::test]
async fn suggest_prefix_dedupes_across_dictionaries() {
    let app = spawn_app().await;
    seed_dictionary(&app.state, "联想A", "en", "en", &[("apple", "a"), ("application", "b")]).await;
    seed_dictionary(&app.state, "联想B", "en", "en", &[("apple", "dup"), ("apply", "c")]).await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success(), "开启 open_access 失败");

    let req = actix_test::TestRequest::get().uri("/api/v1/suggest?prefix=app&limit=10").to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let words: Vec<&str> = resp["words"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert_eq!(words, vec!["apple", "application", "apply"]);
}

#[actix_web::test]
async fn chinese_variants_matched_by_simplified_input() {
    let app = spawn_app().await;
    // 词典只收繁体「頭髮」，输入简体「头发」也必须命中（繁简通搜）
    seed_dictionary(&app.state, "繁体词典", "zh-Hant", "zh-Hans", &[("頭髮", "hair（繁体词头）")]).await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success(), "开启 open_access 失败");

    let req = actix_test::TestRequest::get()
        .uri("/api/v1/query?word=%E5%A4%B4%E5%8F%91") // 头发
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let results = resp["results"].as_array().unwrap();
    assert_eq!(results.len(), 1, "简体输入必须命中繁体词头");
    assert_eq!(results[0]["word"], "頭髮");
}
