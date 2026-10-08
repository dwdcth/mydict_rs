//! HTTP 契约金测 —— 起完整 App（tempdir + 内存级 SQLite），对齐 Python conftest.py 语义。
//! 锁定：认证流、查询语义（@@@LINK/前缀兜底/语言路由）、生词本、Token 配额、错误包络。

use actix_web::test as actix_test;
use actix_web::App;
use serde_json::Value;
use server::AppState;

struct TestApp {
    state: std::sync::Arc<AppState>,
    #[allow(dead_code)]
    dir: tempfile::TempDir,
}

async fn spawn_app() -> TestApp {
    let dir = tempfile::tempdir().expect("tempdir");
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

// ── lite 模式：导入→查询→词条文档 全链路（真实解析管线）────────

/// 把语料拷进待导入目录后走 HTTP 导入，轮询任务直到完成，返回词典 id
async fn import_corpus_via_api(
    svc: &mut impl actix_web::dev::Service<
        actix_http::Request,
        Response = actix_web::dev::ServiceResponse,
        Error = actix_web::Error,
    >,
    admin: &str,
    inbox: &std::path::Path,
    corpus_dir: &str,
    filename: &str,
    name: &str,
    mode: &str,
) -> Value {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("testdata")
        .join(corpus_dir);
    let dst_dir = inbox.join(corpus_dir);
    std::fs::create_dir_all(&dst_dir).expect("mkdir corpus");
    std::fs::copy(src.join(filename), dst_dir.join(filename)).expect("copy corpus");

    let mut body = serde_json::json!({
        "name": name,
        "format": if corpus_dir.starts_with("stardict") { "stardict" } else { "mdict" },
        "files": [format!("{corpus_dir}/{filename}")],
    });
    if !mode.is_empty() {
        body["mode"] = serde_json::json!(mode);
    }
    let req = TestRequest::post()
        .uri("/api/admin/dictionaries/import-from-dicts-dir")
        .pipe_bearer(admin)
        .set_json(body)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(svc, req).await;
    let task_id = resp["task_id"]
        .as_i64()
        .unwrap_or_else(|| panic!("导入请求失败：{resp}"));

    // 轮询任务状态（导入在后台 bulk_write 串行队列里跑）
    for _ in 0..200 {
        let req = TestRequest::get()
            .uri(&format!("/api/admin/tasks/{task_id}"))
            .pipe_bearer(admin)
            .to_request();
        let task: Value = actix_test::call_and_read_body_json(svc, req).await;
        match task["status"].as_str().unwrap_or("") {
            "success" => {
                // 导入完默认 disabled（对齐 Python），测试里直接启用
                let dict_id = task["result"]["dictionary_id"].as_i64().unwrap_or(0);
                let req = TestRequest::put()
                    .uri(&format!("/api/admin/dictionaries/{dict_id}/enable"))
                    .pipe_bearer(admin)
                    .to_request();
                let resp = actix_test::call_service(svc, req).await;
                assert!(resp.status().is_success(), "启用词典失败");
                return task["result"].clone();
            }
            "failed" | "error" => panic!("导入任务失败：{task}"),
            other => {
                eprintln!("task {task_id} status={other} elapsed={}", task["elapsed_seconds"]);
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
    }
    panic!("导入任务超时未完成");
}

#[actix_web::test]
async fn lite_import_full_query_equivalence() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let admin = admin.as_str();
    // 开放匿名查询
    let req = TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    assert!(actix_test::call_service(&mut svc, req).await.status().is_success());

    // 同一部语料分别以 lite / full 导入（mode 省略 = 默认 lite）
    let lite = import_corpus_via_api(
        &mut svc, admin, &app.dir.path().join("dicts"), "mdx_v2_rich", "rich.mdx", "精简模式词典", "lite",
    )
    .await;
    let full = import_corpus_via_api(
        &mut svc, admin, &app.dir.path().join("dicts"), "mdx_v2_rich", "rich.mdx", "全量模式词典", "full",
    )
    .await;
    let lite_id = lite["dictionary_id"].as_i64().expect("lite id") as i32;
    let full_id = full["dictionary_id"].as_i64().expect("full id") as i32;

    // 词头数一致（同源同量）
    assert_eq!(lite["word_count"], full["word_count"]);

    // lite：库里只有词头（definition 全空、source_ordinal 全非空）
    {
        use sea_orm::ConnectionTrait;
        let row = app
            .state
            .db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                app.state.db.get_database_backend(),
                "SELECT COUNT(*) AS n FROM dict_entries \
                 WHERE dictionary_id = $1 AND source_ordinal IS NOT NULL AND definition = ''",
                [lite_id.into()],
            ))
            .await
            .unwrap()
            .expect("count row");
        let n: i64 = row.try_get("", "n").unwrap();
        assert_eq!(n, full["word_count"].as_i64().unwrap(), "lite 行应全部是远程行");
    }

    // 查询等价：appel（两跳 @@@LINK）两条结果内容一致
    let query = |dict_id: i32| {
        let uri = format!("/api/v1/query?word=appel&dict={dict_id}");
        TestRequest::get().uri(&uri).to_request()
    };
    let lite_resp: Value = actix_test::call_and_read_body_json(&mut svc, query(lite_id)).await;
    let full_resp: Value = actix_test::call_and_read_body_json(&mut svc, query(full_id)).await;
    let lite_items = lite_resp["results"].as_array().expect("lite results");
    let full_items = full_resp["results"].as_array().expect("full results");
    assert_eq!(lite_items.len(), 1);
    assert_eq!(lite_items[0]["word"], "appel");
    assert_eq!(lite_items[0]["word"], full_items[0]["word"]);
    assert_eq!(lite_items[0]["definition"], full_items[0]["definition"]);
    assert!(
        !lite_items[0]["definition"].as_str().unwrap().is_empty(),
        "lite 释义应已物化：{lite_items:?}"
    );

    // 精确词 apple 释义等价（含样式展开与资源改写管线）
    let query = |dict_id: i32| {
        let uri = format!("/api/v1/query?word=apple&dict={dict_id}");
        TestRequest::get().uri(&uri).to_request()
    };
    let lite_apple: Value = actix_test::call_and_read_body_json(&mut svc, query(lite_id)).await;
    let full_apple: Value = actix_test::call_and_read_body_json(&mut svc, query(full_id)).await;
    assert_eq!(
        lite_apple["results"][0]["definition"],
        full_apple["results"][0]["definition"],
        "apple 释义应逐字节等价（样式展开+资源改写同管线）"
    );

    // 管理端测试查询也能看到物化释义
    let req = TestRequest::get()
        .uri(&format!(
            "/api/admin/dictionaries/{lite_id}/test-query?word=apple"
        ))
        .pipe_bearer(admin)
        .to_request();
    let tq: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let items = tq.as_array().expect("test-query items");
    assert!(items.iter().any(|it| {
        it["word"].as_str().unwrap_or("") == "apple"
            && !it["definition"].as_str().unwrap_or("").is_empty()
    }));

    // 建议词在 lite 上同样工作（纯词头索引）
    let req = TestRequest::get()
        .uri("/api/v1/suggest?prefix=app&limit=10")
        .to_request();
    let sug: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let words: Vec<&str> = sug["words"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    assert!(words.contains(&"apple"), "{words:?}");

    // 词条文档（沙箱渲染）含物化释义
    let req = TestRequest::get()
        .uri(&format!("/api/dict/entry/{lite_id}?word=apple"))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert!(resp.status().is_success(), "lite 词条文档应可渲染");
    let body = actix_test::read_body(resp).await;
    let html = String::from_utf8_lossy(&body);
    assert!(html.contains("apple") || html.contains("苹果"), "文档应含释义内容");
}

#[actix_web::test]
async fn lite_is_default_import_mode_and_stardict_works() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let admin = admin.as_str();
    let req = TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    assert!(actix_test::call_service(&mut svc, req).await.status().is_success());

    // mode 省略 → 默认 lite
    let result = import_corpus_via_api(
        &mut svc, admin, &app.dir.path().join("dicts"), "mdx_v2_rich", "rich.mdx", "默认模式词典", "",
    )
    .await;
    let dict_id = result["dictionary_id"].as_i64().unwrap() as i32;
    {
        use sea_orm::ConnectionTrait;
        let row = app
            .state
            .db
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                app.state.db.get_database_backend(),
                "SELECT entry_mode FROM dictionaries WHERE id = $1",
                [dict_id.into()],
            ))
            .await
            .unwrap()
            .expect("dict row");
        assert_eq!(row.try_get::<String>("", "entry_mode").unwrap(), "lite");
    }

    // StarDict lite：别名行指向目标、物化后可查
    for f in ["test.ifo", "test.idx", "test.dict", "test.syn"] {
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join("testdata/stardict_basic")
                .join(f),
            {
                let dir = app.dir.path().join("dicts/stardict_basic");
                std::fs::create_dir_all(&dir).unwrap();
                dir.join(f)
            },
        )
        .expect("copy stardict");
    }
    let req = TestRequest::post()
        .uri("/api/admin/dictionaries/import-from-dicts-dir")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({
            "name": "星際辭典",
            "format": "stardict",
            "files": ["stardict_basic/test.ifo","stardict_basic/test.idx","stardict_basic/test.dict","stardict_basic/test.syn"],
        }))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    let status = resp.status();
    let body = actix_test::read_body(resp).await;
    let resp: Value = serde_json::from_slice(&body)
        .unwrap_or_else(|e| panic!("stardict 导入响应非 JSON：{status} {e} body={}", String::from_utf8_lossy(&body)));
    let task_id = resp["task_id"].as_i64().unwrap();
    let mut sd_id = 0i32;
    for _ in 0..200 {
        let req = TestRequest::get()
            .uri(&format!("/api/admin/tasks/{task_id}"))
            .pipe_bearer(admin)
            .to_request();
        let task: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
        if task["status"] == "success" {
            sd_id = task["result"]["dictionary_id"].as_i64().unwrap() as i32;
            let req = TestRequest::put()
                .uri(&format!("/api/admin/dictionaries/{sd_id}/enable"))
                .pipe_bearer(admin)
                .to_request();
            assert!(actix_test::call_service(&mut svc, req).await.status().is_success());
            break;
        }
        assert_ne!(task["status"], "failed", "stardict lite 导入失败：{task}");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_ne!(sd_id, 0);

    // 别名 apple fruit → 目标 apple 释义（远程物化）
    let req = TestRequest::get()
        .uri(&format!("/api/v1/query?word=apple%20fruit&d={sd_id}"))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let items = resp["results"].as_array().expect("alias results");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["word"], "apple fruit");
    let def = items[0]["definition"].as_str().unwrap_or_default();
    assert!(def.contains("苹果"), "别名行应物化目标释义：{def}");

    // ECDICT lite → 422（先把语料拷进待导入目录）
    std::fs::copy(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata/ecdict_mini.csv"),
        app.dir.path().join("dicts/ecdict_mini.csv"),
    )
    .unwrap();
    let req = TestRequest::post()
        .uri("/api/admin/dictionaries/import-from-dicts-dir")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({
            "name": "csv",
            "format": "ecdict",
            "mode": "lite",
            "files": ["ecdict_mini.csv"],
        }))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(
        resp.status().as_u16(),
        422,
        "ECDICT + lite 应被模式校验拒绝"
    );
}


#[actix_web::test]
async fn mode_switch_converts_lite_full_roundtrip() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let admin = admin.as_str();
    let req = TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    assert!(actix_test::call_service(&mut svc, req).await.status().is_success());

    // lite 导入并启用
    let result = import_corpus_via_api(
        &mut svc, admin, &app.dir.path().join("dicts"), "mdx_v2_rich", "rich.mdx", "待转换词典", "lite",
    )
    .await;
    let dict_id = result["dictionary_id"].as_i64().unwrap() as i32;

    // lite → full：后台重灌（走真实并行解析管线）
    let req = TestRequest::post()
        .uri(&format!("/api/admin/dictionaries/{dict_id}/entry-mode"))
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"mode": "full"}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["changed"], true);
    let task_id = resp["task_id"].as_i64().unwrap();
    for _ in 0..200 {
        let req = TestRequest::get()
            .uri(&format!("/api/admin/tasks/{task_id}"))
            .pipe_bearer(admin)
            .to_request();
        let task: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
        match task["status"].as_str().unwrap_or("") {
            "success" => break,
            "failed" | "error" => panic!("转换失败：{task}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }

    use sea_orm::ConnectionTrait;
    let backend = app.state.db.get_database_backend();
    // full 后：释义全部落库、source_ordinal 全空、entry_mode=full
    let row = app.state.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
        backend,
        "SELECT COUNT(*) AS n FROM dict_entries \
         WHERE dictionary_id = $1 AND source_ordinal IS NULL AND definition != ''",
        [dict_id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<i64>("", "n").unwrap(),
        result["word_count"].as_i64().unwrap(),
        "转换后应为全量行"
    );
    let row = app.state.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
        backend,
        "SELECT entry_mode FROM dictionaries WHERE id = $1",
        [dict_id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(row.try_get::<String>("", "entry_mode").unwrap(), "full");

    // 查询依旧等价（apple 走落库释义）
    let req = TestRequest::get()
        .uri(&format!("/api/v1/query?word=appel&dict={dict_id}"))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let items = resp["results"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["word"], "appel");
    assert!(items[0]["definition"].as_str().unwrap().contains("苹果"));

    // 同模式再切 → no-op
    let req = TestRequest::post()
        .uri(&format!("/api/admin/dictionaries/{dict_id}/entry-mode"))
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"mode": "full"}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["changed"], false);

    // full → lite：切回轻量
    let req = TestRequest::post()
        .uri(&format!("/api/admin/dictionaries/{dict_id}/entry-mode"))
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"mode": "lite"}))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let task_id = resp["task_id"].as_i64().expect("切回 lite 任务");
    for _ in 0..200 {
        let req = TestRequest::get()
            .uri(&format!("/api/admin/tasks/{task_id}"))
            .pipe_bearer(admin)
            .to_request();
        let task: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
        match task["status"].as_str().unwrap_or("") {
            "success" => break,
            "failed" | "error" => panic!("切回失败：{task}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    let row = app.state.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
        backend,
        "SELECT COUNT(*) AS n FROM dict_entries \
         WHERE dictionary_id = $1 AND source_ordinal IS NOT NULL AND definition = ''",
        [dict_id.into()],
    )).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<i64>("", "n").unwrap(),
        result["word_count"].as_i64().unwrap(),
        "切回后应为远程行"
    );
    // 查询继续工作（物化）
    let req = TestRequest::get()
        .uri(&format!("/api/v1/query?word=appel&dict={dict_id}"))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert!(resp["results"][0]["definition"].as_str().unwrap().contains("苹果"));

    // ECDICT 词典不支持 lite（构造一部 ecdict 全量词典再切）
    let ec_id = seed_dictionary(&app.state, "csv典", "en", "zh-Hans", &[("hello", "你好")]).await;
    let req = TestRequest::post()
        .uri(&format!("/api/admin/dictionaries/{ec_id}/entry-mode"))
        .pipe_bearer(admin)
        .set_json(serde_json::json!({"mode": "lite"}))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 422, "ECDICT 不应允许切 lite");
}

// ── 浏览器上传：压缩包 → 自动解压分组 → 导入 ─────────────────────

fn multipart_file_body(filename: &str, bytes: &[u8]) -> (String, Vec<u8>) {
    let boundary = "----mydicttestboundary";
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"{filename}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(bytes);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (
        format!("multipart/form-data; boundary={boundary}"),
        body,
    )
}

#[actix_web::test]
async fn upload_zip_analyze_and_import() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let admin = admin.as_str();

    // 打一个 zip：一部 mdict 词典 + 一个无关 txt
    let mdx_bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("testdata/mdx_v2_rich/rich.mdx"),
    )
    .expect("corpus mdx");
    let mut zip_buf = std::io::Cursor::new(Vec::new());
    {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(&mut zip_buf);
        let opts: zip::write::SimpleFileOptions = Default::default();
        zip.start_file("牛津/oxford.mdx", opts).unwrap();
        zip.write_all(&mdx_bytes).unwrap();
        zip.start_file("readme.txt", opts).unwrap();
        zip.write_all(b"not a dictionary").unwrap();
        zip.finish().unwrap();
    }
    let (content_type, body) = multipart_file_body("词典打包.zip", zip_buf.get_ref());

    // ① analyze-upload：解压 + 分组
    let req = TestRequest::post()
        .uri("/api/admin/dictionaries/analyze-upload")
        .pipe_bearer(admin)
        .insert_header((actix_web::http::header::CONTENT_TYPE, content_type))
        .set_payload(body)
        .to_request();
    let analysis: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let groups = analysis["groups"].as_array().expect("groups");
    assert_eq!(groups.len(), 1, "{groups:?}");
    assert_eq!(groups[0]["format"], "mdict");
    assert_eq!(groups[0]["name"], "oxford");
    assert_eq!(groups[0]["importable"], true);
    let files: Vec<String> = groups[0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["relpath"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(files, vec!["牛津/oxford.mdx".to_string()]);
    assert!(
        analysis["skipped"].as_array().unwrap().iter().any(|s| s.as_str().unwrap_or("").ends_with("readme.txt")),
        "无关文件应进 skipped：{analysis}"
    );
    let upload_id = analysis["upload_id"].as_str().unwrap().to_string();

    // ② import-uploaded：导入该组（默认 lite）
    let req = TestRequest::post()
        .uri("/api/admin/dictionaries/import-uploaded")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({
            "upload_id": upload_id,
            "name": "牛津高阶",
            "format": "mdict",
            "files": files,
        }))
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let task_id = resp["task_id"].as_i64().unwrap();
    let mut dict_id = 0i32;
    for _ in 0..200 {
        let req = TestRequest::get()
            .uri(&format!("/api/admin/tasks/{task_id}"))
            .pipe_bearer(admin)
            .to_request();
        let task: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
        match task["status"].as_str().unwrap_or("") {
            "success" => {
                dict_id = task["result"]["dictionary_id"].as_i64().unwrap() as i32;
                break;
            }
            "failed" | "error" => panic!("导入失败：{task}"),
            _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
        }
    }
    assert_ne!(dict_id, 0);

    // 上传导入：文件应已从暂存区移入 source/ 归档
    let source_dir = app.dir.path().join(format!("dictionaries/{dict_id}/source"));
    let archived: Vec<String> = std::fs::read_dir(&source_dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(archived, vec!["oxford.mdx".to_string()], "{archived:?}");

    // 默认 lite：测试查询能看到物化释义
    let req = TestRequest::put()
        .uri(&format!("/api/admin/dictionaries/{dict_id}/enable"))
        .pipe_bearer(admin)
        .to_request();
    assert!(actix_test::call_service(&mut svc, req).await.status().is_success());
    let req = TestRequest::get()
        .uri(&format!("/api/admin/dictionaries/{dict_id}/test-query?word=appel"))
        .pipe_bearer(admin)
        .to_request();
    let tq: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert!(
        tq.as_array().unwrap().iter().any(|it| it["word"] == "appel"
            && !it["definition"].as_str().unwrap_or("").is_empty()),
        "{tq:?}"
    );

    // 路径穿越防护：import-uploaded 带相对路径逃逸 → 422
    let req = TestRequest::post()
        .uri("/api/admin/dictionaries/import-uploaded")
        .pipe_bearer(admin)
        .set_json(serde_json::json!({
            "upload_id": upload_id,
            "name": "evil",
            "format": "mdict",
            "files": ["../../../etc/passwd"],
        }))
        .to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 422);
}

// ── 词典组（GoldenDict 式命名查询范围）────────────────────────────

#[actix_web::test]
async fn dictionary_groups_crud_and_isolation() {
    let app = spawn_app().await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let _ = admin;
    let a_id = seed_dictionary(&app.state, "组员甲", "en", "en", &[("apple", "a")]).await;
    let b_id = seed_dictionary(&app.state, "组员乙", "zh-Hans", "zh-Hans", &[("苹果", "b")]).await;

    // 两个用户
    let register = |username: &str| {
        TestRequest::post()
            .uri("/api/auth/register")
            .set_json(serde_json::json!({
                "username": username,
                "password": "password123",
                "email": format!("{username}@t.dev"),
            }))
            .to_request()
    };
    let _: Value = actix_test::call_and_read_body_json(&mut svc, register("alice")).await;
    let _: Value = actix_test::call_and_read_body_json(&mut svc, register("bob")).await;
    let login = |username: &str| {
        TestRequest::post()
            .uri("/api/auth/login")
            .set_json(serde_json::json!({"username": username, "password": "password123"}))
            .to_request()
    };
    let alice: Value = actix_test::call_and_read_body_json(&mut svc, login("alice")).await;
    let alice_token = alice["access_token"].as_str().unwrap().to_string();
    let bob: Value = actix_test::call_and_read_body_json(&mut svc, login("bob")).await;
    let bob_token = bob["access_token"].as_str().unwrap().to_string();

    // 匿名 → 401
    let req = TestRequest::get().uri("/api/dict/groups").to_request();
    let resp = actix_test::call_service(&mut svc, req).await;
    assert_eq!(resp.status().as_u16(), 401);

    // 空列表
    let req = TestRequest::get()
        .uri("/api/dict/groups")
        .pipe_bearer(&alice_token)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["groups"].as_array().unwrap().len(), 0);

    // 建组（保序）
    let req = TestRequest::post()
        .uri("/api/dict/groups")
        .pipe_bearer(&alice_token)
        .set_json(serde_json::json!({"name": "精查", "dictionary_ids": [a_id, b_id]}))
        .to_request();
    let group: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    let group_id = group["id"].as_i64().unwrap();
    assert_eq!(
        group["dictionary_ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect::<Vec<_>>(),
        vec![a_id as i64, b_id as i64],
        "组员应保序返回"
    );

    // 重名 → 409；不存在词典 → 422；空成员 → 422
    let req = TestRequest::post()
        .uri("/api/dict/groups")
        .pipe_bearer(&alice_token)
        .set_json(serde_json::json!({"name": "精查", "dictionary_ids": [a_id]}))
        .to_request();
    assert_eq!(actix_test::call_service(&mut svc, req).await.status().as_u16(), 409);
    let req = TestRequest::post()
        .uri("/api/dict/groups")
        .pipe_bearer(&alice_token)
        .set_json(serde_json::json!({"name": "坏组", "dictionary_ids": [99999]}))
        .to_request();
    assert_eq!(actix_test::call_service(&mut svc, req).await.status().as_u16(), 422);
    let req = TestRequest::post()
        .uri("/api/dict/groups")
        .pipe_bearer(&alice_token)
        .set_json(serde_json::json!({"name": "空组", "dictionary_ids": []}))
        .to_request();
    assert_eq!(actix_test::call_service(&mut svc, req).await.status().as_u16(), 422);

    // 改名 + 全量换成员
    let req = TestRequest::put()
        .uri(&format!("/api/dict/groups/{group_id}"))
        .pipe_bearer(&alice_token)
        .set_json(serde_json::json!({"name": "精查2", "dictionary_ids": [b_id, a_id]}))
        .to_request();
    let updated: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(updated["name"], "精查2");
    assert_eq!(
        updated["dictionary_ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect::<Vec<_>>(),
        vec![b_id as i64, a_id as i64],
        "成员替换应保新序"
    );

    // 隔离：bob 看不到 alice 的组，改/删都 404
    let req = TestRequest::get()
        .uri("/api/dict/groups")
        .pipe_bearer(&bob_token)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["groups"].as_array().unwrap().len(), 0, "组应用户隔离");
    let req = TestRequest::put()
        .uri(&format!("/api/dict/groups/{group_id}"))
        .pipe_bearer(&bob_token)
        .set_json(serde_json::json!({"name": "抢组"}))
        .to_request();
    assert_eq!(actix_test::call_service(&mut svc, req).await.status().as_u16(), 404);
    let req = TestRequest::delete()
        .uri(&format!("/api/dict/groups/{group_id}"))
        .pipe_bearer(&bob_token)
        .to_request();
    assert_eq!(actix_test::call_service(&mut svc, req).await.status().as_u16(), 404);

    // 删除
    let req = TestRequest::delete()
        .uri(&format!("/api/dict/groups/{group_id}"))
        .pipe_bearer(&alice_token)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["ok"], true);
    let req = TestRequest::get()
        .uri("/api/dict/groups")
        .pipe_bearer(&alice_token)
        .to_request();
    let resp: Value = actix_test::call_and_read_body_json(&mut svc, req).await;
    assert_eq!(resp["groups"].as_array().unwrap().len(), 0);
}

// ── 闪卡复习（FSRS 间隔重复）─────────────────────────────────────

#[actix_web::test]
async fn flashcards_fsrs_full_flow() {
    let app = spawn_app().await;
    let dict_id = seed_dictionary(
        &app.state,
        "闪卡词典",
        "en",
        "en",
        &[("apple", "n. 苹果"), ("banana", "n. 香蕉")],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let _: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/auth/register")
            .set_json(serde_json::json!({
                "username": "fsrsuser", "password": "password123", "email": "f@t.dev"
            }))
            .to_request(),
    )
    .await;
    let login: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/auth/login")
            .set_json(serde_json::json!({"username": "fsrsuser", "password": "password123"}))
            .to_request(),
    )
    .await;
    let token = login["access_token"].as_str().unwrap().to_string();

    // 匿名 → 401
    let resp = actix_test::call_service(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/stats").to_request(),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 401);

    // 查询的词加入闪卡（不在生词本 → 自动先收藏）
    let added: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/flashcards")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"word": "apple", "dictionary_id": dict_id}))
            .to_request(),
    )
    .await;
    assert_eq!(added["already"], false);
    let item_id = added["vocab_item_id"].as_i64().unwrap() as i32;

    // 幂等：再加一次 already=true
    let again: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/flashcards")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"word": "apple", "dictionary_id": dict_id}))
            .to_request(),
    )
    .await;
    assert_eq!(again["already"], true);
    assert_eq!(again["vocab_item_id"].as_i64().unwrap() as i32, item_id);

    // 统计：1 到期（新卡）
    let stats: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/stats").pipe_bearer(&token).to_request(),
    )
    .await;
    assert_eq!(stats["due_count"], 1);
    assert_eq!(stats["new_count"], 1);
    assert_eq!(stats["total"], 1);

    // 队列：新卡在前，四档预测齐全
    let queue: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/queue").pipe_bearer(&token).to_request(),
    )
    .await;
    let items = queue["queue"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["word"], "apple");
    for key in ["again", "hard", "good", "easy"] {
        assert!(
            items[0]["previews"][key]["interval_days"].is_i64(),
            "缺 {key} 档预测：{items:?}"
        );
    }
    // FSRS 常识：Easy 间隔 > Again 间隔
    let again_iv = items[0]["previews"]["again"]["interval_days"].as_i64().unwrap();
    let easy_iv = items[0]["previews"]["easy"]["interval_days"].as_i64().unwrap();
    assert!(easy_iv >= again_iv, "Easy 间隔应不小于 Again：{again_iv} vs {easy_iv}");

    // 评 Good → 间隔 ≥ 1 天，卡出队
    let review: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri(&format!("/api/flashcards/{item_id}/review"))
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"rating": 3}))
            .to_request(),
    )
    .await;
    let interval = review["interval_days"].as_i64().unwrap();
    assert!(interval >= 1, "新卡 Good 间隔应 ≥1 天：{review:?}");
    let stats: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/stats").pipe_bearer(&token).to_request(),
    )
    .await;
    assert_eq!(stats["due_count"], 0, "复习后不该再到期：{stats:?}");
    assert_eq!(stats["today_reviewed"], 1);
    // 日志表有记录
    {
        use sea_orm::ConnectionTrait;
        let n = app.state.db.query_one_raw(sea_orm::Statement::from_sql_and_values(
            app.state.db.get_database_backend(),
            "SELECT COUNT(*) AS n FROM flashcard_reviews",
            [],
        )).await.unwrap().unwrap();
        assert_eq!(n.try_get::<i64>("", "n").unwrap(), 1);
    }

    // 复习后的卡再进队列（新词 banana 加卡）应带记忆率与复习史
    let _: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/flashcards")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"word": "banana", "dictionary_id": dict_id}))
            .to_request(),
    )
    .await;
    let queue: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/queue").pipe_bearer(&token).to_request(),
    )
    .await;
    let banana = queue["queue"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["word"] == "banana")
        .expect("banana 新卡");
    assert!(banana["retrievability"].is_null(), "新卡无记忆率：{banana:?}");
    assert!(banana["days_since_last_review"].is_null());
    let banana_id = banana["vocab_item_id"].as_i64().unwrap() as i32;

    // 参数：retention 0.95、非法权重 422、合法 19 位 OK
    let put: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::put()
            .uri("/api/flashcards/settings")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"retention": 0.95}))
            .to_request(),
    )
    .await;
    assert_eq!(put["retention"], 0.95);
    let resp = actix_test::call_service(
        &mut svc,
        TestRequest::put()
            .uri("/api/flashcards/settings")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"weights": "[1,2,3]"}))
            .to_request(),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 422);
    // 19 位（FSRS-4.5 导出）与 21 位（FSRS-6）都被接受，短的自动补齐
    for len in [19usize, 21] {
        let weights: Vec<f64> = (0..len).map(|i| i as f64 * 0.1 + 0.4).collect();
        let put: Value = actix_test::call_and_read_body_json(
            &mut svc,
            TestRequest::put()
                .uri("/api/flashcards/settings")
                .pipe_bearer(&token)
                .set_json(serde_json::json!({"weights": serde_json::to_string(&weights).unwrap()}))
                .to_request(),
        )
        .await;
        assert!(put["weights"].as_str().unwrap_or("").starts_with('['), "len={len}: {put:?}");
    }

    // 移出复习：卡没了、生词本条目还在
    let resp: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::delete()
            .uri(&format!("/api/flashcards/{item_id}"))
            .pipe_bearer(&token)
            .to_request(),
    )
    .await;
    assert_eq!(resp["ok"], true);
    // banana 的卡一并清掉（生词本会留下两条，断言改看总数）
    let resp: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::delete()
            .uri(&format!("/api/flashcards/{banana_id}"))
            .pipe_bearer(&token)
            .to_request(),
    )
    .await;
    assert_eq!(resp["ok"], true);
    let stats: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/stats").pipe_bearer(&token).to_request(),
    )
    .await;
    assert_eq!(stats["total"], 0);
    let vocab: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/vocab").pipe_bearer(&token).to_request(),
    )
    .await;
    assert_eq!(vocab["total"], 2, "生词本条目应保留：{vocab:?}");
    // in_review 标记：两条都已移出复习
    assert!(vocab["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["in_review"] == false));
}

// ── 吸收自 PythonMDict 的学习/兼容功能 ──────────────────────────

#[actix_web::test]
async fn quiz_wordfreq_browse_wotd_anki() {
    let app = spawn_app().await;
    // 释义带英文例句（quiz 要能挖空）
    let dict_id = seed_dictionary(
        &app.state,
        "学习词典",
        "en",
        "en",
        &[
            ("apple", "n. 苹果<br>The apple is a sweet fruit that grows on trees."),
            ("apply", "v. 申请<br>She wants to apply for the job before Friday."),
            ("apricot", "n. 杏<br>An apricot is a small orange fruit."),
            ("banana", "n. 香蕉<br>The banana turned brown quickly."),
            ("bandana", "n. 头巾"),
            ("123", "数字词头"),
            ("a", "x"),
        ],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let _: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/auth/register")
            .set_json(serde_json::json!({
                "username": "learner", "password": "password123", "email": "l@t.dev"
            }))
            .to_request(),
    )
    .await;
    let login: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/auth/login")
            .set_json(serde_json::json!({"username": "learner", "password": "password123"}))
            .to_request(),
    )
    .await;
    let token = login["access_token"].as_str().unwrap().to_string();

    // ── 例句挖空测验 ──
    let added: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/flashcards")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"word": "apple", "dictionary_id": dict_id}))
            .to_request(),
    )
    .await;
    assert_eq!(added["already"], false);
    let quiz: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/flashcards/quiz").pipe_bearer(&token).to_request(),
    )
    .await;
    let questions = quiz["questions"].as_array().expect("questions");
    assert!(!questions.is_empty(), "apple 卡应能出题：{quiz:?}");
    let q = &questions[0];
    assert!(q["sentence"].as_str().unwrap().contains("____"), "{q:?}");
    assert!(!q["sentence"].as_str().unwrap().to_lowercase().contains("apple"), "句子里不该残留答案");
    assert_eq!(q["options"].as_array().unwrap().len(), 4);
    assert!(q["options"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o.as_str().unwrap().eq_ignore_ascii_case("apple")));
    let item_id = q["vocab_item_id"].as_i64().unwrap();
    let correct = q["correct_index"].as_i64().unwrap();
    let ans: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri(&format!("/api/flashcards/quiz/{item_id}/answer"))
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"correct": true}))
            .to_request(),
    )
    .await;
    assert_eq!(ans["correct"], true);
    assert!(ans["interval_days"].as_i64().unwrap_or(0) >= 1, "答对=Good 应排期：{ans:?}");

    // ── 词频分析 ──
    let freq: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::post()
            .uri("/api/tools/word-frequency")
            .pipe_bearer(&token)
            .set_json(serde_json::json!({"text": "The apple and the banana. Apple pie! She went to the apple tree."}))
            .to_request(),
    )
    .await;
    let words = freq["words"].as_array().unwrap_or_else(|| panic!("词频响应异常：{freq:?}"));
    assert!(freq["total_tokens"].as_i64().unwrap() > 0);
    // apple 3 次居首；the/and/she 等停用词被滤掉
    assert_eq!(words[0]["word"], "apple");
    assert_eq!(words[0]["count"], 3);
    assert!(words.iter().all(|w| w["word"].as_str().unwrap() != "the"));
    // in_dict：apple/banana 已收录
    let apple = words.iter().find(|w| w["word"] == "apple").unwrap();
    assert_eq!(apple["in_dict"], true);

    // ── 词条浏览（干净词头过滤 + 游标分页）──
    let page1_resp = actix_test::call_service(
        &mut svc,
        TestRequest::get().uri(&format!("/api/tools/dict/browse/{dict_id}?limit=3"))
            .pipe_bearer(&token)
            .to_request(),
    )
    .await;
    let page1_body = actix_test::read_body(page1_resp).await;
    let page1: Value = serde_json::from_slice(&page1_body)
        .unwrap_or_else(|e| panic!("browse 响应异常：{e} body={}", String::from_utf8_lossy(&page1_body)));
    let browsed: Vec<&str> = page1["words"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
    assert_eq!(browsed, vec!["apple", "apply", "apricot"], "按序且过滤了脏词头：{browsed:?}");
    assert!(page1["next_cursor"].is_string());
    let page2: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get()
            .uri(&format!("/api/tools/dict/browse/{dict_id}?limit=3&after={}", page1["next_cursor"].as_str().unwrap()))
            .pipe_bearer(&token)
            .to_request(),
    )
    .await;
    assert_eq!(page2["words"].as_array().unwrap()[0], "banana");

    // ── 每日一词：同日恒定、词头干净 ──
    let w1_resp = actix_test::call_service(
        &mut svc,
        TestRequest::get().uri("/api/dict/word-of-the-day").pipe_bearer(&token).to_request(),
    ).await;
    let w1_body = actix_test::read_body(w1_resp).await;
    let w1: Value = serde_json::from_slice(&w1_body)
        .unwrap_or_else(|e| panic!("wotd 响应异常：{e} body={}", String::from_utf8_lossy(&w1_body)));
    let w2: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get().uri("/api/dict/word-of-the-day").pipe_bearer(&token).to_request(),
    ).await;
    assert_eq!(w1["word"], w2["word"], "同日应恒定：{w1:?} vs {w2:?}");
    assert!(!w1["word"].as_str().unwrap().is_empty());

    // ── Anki 导出 ──
    let resp = actix_test::call_service(
        &mut svc,
        TestRequest::get().uri("/api/vocab/export").pipe_bearer(&token).to_request(),
    ).await;
    assert!(resp.status().is_success());
    let body = actix_test::read_body(resp).await;
    let tsv = String::from_utf8_lossy(&body);
    assert!(tsv.contains("apple\t"), "TSV 应含词列：{tsv}");
    assert!(tsv.contains("苹果"), "释义应为纯文本：{tsv}");
    assert!(!tsv.contains("<br>"), "HTML 应已剥除");
}

// ── TTS 与词典语音探测 ──────────────────────────────────────────

#[actix_web::test]
async fn word_audio_probe_and_tts_gate() {
    let app = spawn_app().await;
    let with_audio = seed_dictionary(
        &app.state,
        "带读音词典",
        "en",
        "en",
        &[("apple", r#"<a href="sound://us/apple.mp3">🔊</a>n. 苹果"#)],
    )
    .await;
    let without = seed_dictionary(
        &app.state,
        "无读音词典",
        "zh-Hans",
        "zh-Hans",
        &[("苹果", "<p>一种水果</p>")],
    )
    .await;
    let mut svc = init_service(&app.state).await;
    let admin = admin_setup(&mut svc).await;
    let req = actix_test::TestRequest::put()
        .uri("/api/admin/settings")
        .pipe_bearer(&admin)
        .set_json(serde_json::json!({"open_access": true}))
        .to_request();
    assert!(actix_test::call_service(&mut svc, req).await.status().is_success());

    // 有词典语音 → audio_url 指向 /dict-res
    let probe: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get()
            .uri(&format!("/api/dict/audio?word=apple&dict={with_audio}"))
            .to_request(),
    )
    .await;
    assert_eq!(
        probe["audio_url"].as_str().unwrap(),
        format!("/dict-res/{with_audio}/res/us/apple.mp3")
    );

    // 无词典语音 → null（前端转 TTS 兜底）
    let probe: Value = actix_test::call_and_read_body_json(
        &mut svc,
        TestRequest::get()
            .uri(&format!("/api/dict/audio?word=%E8%8B%B9%E6%9E%9C&dict={without}"))
            .to_request(),
    )
    .await;
    assert_eq!(probe["audio_url"], Value::Null);

    // TTS 未启用 → 明确的服务状态消息（不触发模型下载）
    let resp = actix_test::call_service(
        &mut svc,
        TestRequest::get().uri("/api/dict/tts?word=apple").to_request(),
    )
    .await;
    assert_eq!(resp.status().as_u16(), 500);
    let body: Value = actix_test::call_and_read_body_json(&mut svc, TestRequest::get().uri("/api/dict/tts?word=apple").to_request()).await;
    assert!(
        body["message"].as_str().unwrap_or("").contains("TTS 未启用"),
        "{body:?}"
    );
}
