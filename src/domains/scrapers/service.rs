use std::sync::Arc;

use async_trait::async_trait;
use mlua::{Function, Lua, LuaOptions, LuaSerdeExt as _, StdLib, Table};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::domains::documents::Service as DocumentsService;
use crate::{ErrorContext as _, Result};

use super::cookies::{Cookie, Service as CookiesService};
use super::host;
use super::reports::{Recorder, Service as ReportsService};
use super::session::Service as SessionService;
use super::{ProgressEvent, Service};

/// The default [`Service`] implementation, running scripts in a fresh
/// `mlua::Lua` VM per call, delegating session/cookie persistence to
/// injected [`SessionService`]/[`CookiesService`] implementations, this
/// run's debug report to an injected [`ReportsService`], and document
/// uploads to an injected [`DocumentsService`].
pub(super) struct ScrapersClient {
    cookies: Arc<dyn CookiesService>,
    session: Arc<dyn SessionService>,
    reports: Arc<dyn ReportsService>,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
}

impl ScrapersClient {
    pub(super) fn new(
        cookies: Arc<dyn CookiesService>,
        session: Arc<dyn SessionService>,
        reports: Arc<dyn ReportsService>,
        documents: Arc<dyn DocumentsService>,
        on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
        on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
    ) -> Self {
        Self {
            cookies,
            session,
            reports,
            documents,
            on_progress,
            on_question,
        }
    }
}

#[async_trait]
impl Service for ScrapersClient {
    async fn run(
        &self,
        name: &str,
        script: &str,
        parameters: Value,
        debug_http_dump: bool,
        wreq_emulation: bool,
        follow_redirects: bool,
        allowed_domains: Vec<String>,
    ) -> Result<()> {
        let cookies = self.cookies.load(name).await?;
        let session_data = self.session.load(name).await?;
        let handle = Handle::current();
        let recorder = Arc::new(Recorder::new(name));
        recorder.record("run_started", json!({ "script": name }));

        let documents = self.documents.clone();
        let on_progress = self.on_progress.clone();
        let on_question = self.on_question.clone();
        let name_owned = name.to_string();
        let script_owned = script.to_string();
        let recorder_for_run = recorder.clone();

        let (session_data, cookies, run_result) = tokio::task::spawn_blocking(move || {
            run_script(
                &name_owned,
                &script_owned,
                parameters,
                session_data,
                cookies,
                documents,
                on_progress,
                on_question,
                recorder_for_run,
                handle,
                debug_http_dump,
                wreq_emulation,
                follow_redirects,
                allowed_domains,
            )
        })
        .await
        .with_context(|| format!("scraper {name:?} task panicked"))??;

        match &run_result {
            Ok(()) => recorder.record("run_finished", json!({ "status": "ok" })),
            Err(err) => recorder.record(
                "error",
                json!({ "action": "run", "message": err.to_string() }),
            ),
        }

        self.session
            .save(name, session_data)
            .await
            .with_context(|| format!("failed to save session for scraper {name:?}"))?;
        self.cookies
            .save(name, cookies)
            .await
            .with_context(|| format!("failed to save cookies for scraper {name:?}"))?;
        self.reports
            .save(recorder.finish())
            .await
            .with_context(|| format!("failed to save report for scraper {name:?}"))?;

        run_result
    }
}

/// Runs the whole blocking, synchronous half of a scraper run: builds a
/// fresh Lua VM, installs the `fyde` host table (see `host::install`),
/// evaluates `script` and calls its `run(parameters)` entrypoint, then reads
/// back `fyde.session`'s final contents and every cookie picked up this run
/// — regardless of whether the script's `run` succeeded or failed, mirroring
/// `demo-rust-fyde`'s `main.rs`, which saves the session/cookie file either
/// way. The caller is responsible for persisting the returned session
/// data/cookies and for propagating `run_result`.
#[allow(clippy::too_many_arguments)]
fn run_script(
    name: &str,
    script: &str,
    parameters: Value,
    session_data: Value,
    cookies: Vec<Cookie>,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    debug_http_dump: bool,
    wreq_emulation: bool,
    follow_redirects: bool,
    allowed_domains: Vec<String>,
) -> Result<(Value, Vec<Cookie>, Result<()>)> {
    let lua = sandboxed_lua().context("building sandboxed lua vm")?;
    // Built fresh per run, same as the Lua VM itself: there's no persisted
    // state to carry across runs the way `session`/`cookies` have, so the
    // webview it lazily spawns (see `browser::driver::BrowserDriver`) lives
    // only as long as this run does, closed via its `Drop` impl once every
    // `Arc` clone `host::install` handed to `fyde.browser`'s closures goes
    // out of scope at the end of this function.
    let browser = super::browser::init(Arc::new(allowed_domains.clone()));
    let installed = host::install(
        &lua,
        name,
        session_data,
        cookies,
        documents,
        browser,
        on_progress,
        on_question,
        recorder,
        runtime,
        debug_http_dump,
        wreq_emulation,
        follow_redirects,
        allowed_domains,
    )
    .context("installing host functions into the Lua VM")?;

    let run_result = evaluate_and_run(&lua, name, script, parameters);

    let (session_data, cookies) = installed
        .drain(&lua)
        .context("reading back session data and cookies after the scraper run")?;

    Ok((session_data, cookies, run_result))
}

/// Builds a fully sandboxed Lua VM for running a scraper script: only the
/// side-effect-free `table`/`string`/`math`/`utf8` subset of the standard
/// library is loaded — no `io`, `os`, `package` (so no `require`), `debug`,
/// or FFI libraries — so a script has no path to the filesystem, OS
/// environment, subprocesses, dynamic library loading, or raw sockets. The
/// only network access a script gets is through the `fyde.http`/
/// `fyde.browser` host functions `host::install` wires into the `fyde`
/// global table below — there is no other way out of this VM. `utf8` (unlike
/// `documents::parser::vm::sandboxed`, which doesn't need it) is kept because
/// `didaxis.lua` calls `utf8.len`.
///
/// `Lua::new_with` always loads the base library (`_G`) regardless of the
/// requested `StdLib` flags — it unconditionally calls `luaopen_base`
/// internally, with no `StdLib` flag to opt out — so `dofile`/`loadfile`
/// (direct filesystem access) and `load` (arbitrary/binary chunk loading)
/// are stripped from the globals table by hand afterwards to close that gap,
/// same approach as `documents::parser::vm::sandboxed`.
fn sandboxed_lua() -> Result<Lua> {
    let libs = StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8;
    let lua = Lua::new_with(libs, LuaOptions::new())
        .context("failed to create sandboxed scraper lua vm")?;

    let globals = lua.globals();
    for unsafe_global in ["dofile", "loadfile", "load"] {
        globals
            .set(unsafe_global, mlua::Value::Nil)
            .context("failed to strip an unsafe global from the sandboxed scraper lua vm")?;
    }

    Ok(lua)
}

/// Loads `script` as a module (expecting it to return a table with a single
/// `run(parameters)` function — the entire contract between the Rust host
/// and a Lua scraper script, same as `demo-rust-fyde`'s `main.rs`) and calls
/// it with `parameters`.
fn evaluate_and_run(lua: &Lua, name: &str, script: &str, parameters: Value) -> Result<()> {
    let chunk = lua.load(script).set_name(name);
    let scraper: Table = chunk.eval().context("evaluating Lua scraper script")?;

    let run: Function = scraper
        .get("run")
        .context("Lua script must return a table with a `run(parameters)` function")?;
    let parameters_value = lua
        .to_value(&parameters)
        .context("converting parameters to a Lua value")?;

    run.call::<()>(parameters_value)
        .context("running the scraper's `run` function")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use mockall::predicate::eq;
    use serde_json::json;

    use super::super::cookies::{FakeCookie, MockService as MockCookiesService};
    use super::super::reports::MockService as MockReportsService;
    use super::super::session::MockService as MockSessionService;
    use super::*;
    use crate::domains::documents::MockService as MockDocumentsService;

    const TRIVIAL_SCRIPT: &str = r#"
        local M = {}
        function M.run(parameters)
        end
        return M
    "#;

    /// A [`MockReportsService`] that accepts exactly one `save` call,
    /// without asserting anything about the report's contents — the
    /// `reports::service`/`recorder` tests already cover what gets
    /// recorded; these tests only care that a run always saves a report.
    fn any_reports() -> MockReportsService {
        let mut reports = MockReportsService::new();
        reports.expect_save().times(1).returning(|_| Ok(()));
        reports
    }

    fn client(
        cookies: MockCookiesService,
        session: MockSessionService,
        documents: MockDocumentsService,
    ) -> ScrapersClient {
        ScrapersClient::new(
            Arc::new(cookies),
            Arc::new(session),
            Arc::new(any_reports()),
            Arc::new(documents),
            None,
            None,
        )
    }

    #[tokio::test]
    async fn run_loads_session_and_cookies_before_running_and_saves_them_after() {
        let loaded_cookie = FakeCookie::new().build();

        let mut cookies = MockCookiesService::new();
        let loaded_cookies = vec![loaded_cookie];
        cookies
            .expect_load()
            .with(eq("didaxis"))
            .times(1)
            .returning(move |_| Ok(loaded_cookies.clone()));
        // The trivial script never makes an HTTP call, so no origin is
        // "visited" this run — a loaded cookie is only re-exported for an
        // origin `fyde.http` actually visited (see `host::http::export_
        // cookies`), so the saved set comes back empty here, not unchanged.
        cookies
            .expect_save()
            .withf(|name, saved| name == "didaxis" && saved.is_empty())
            .times(1)
            .returning(|_, _| Ok(()));

        let mut session = MockSessionService::new();
        session
            .expect_load()
            .with(eq("didaxis"))
            .times(1)
            .returning(|_| Ok(json!({"cursor": "abc"})));
        session
            .expect_save()
            .withf(|name, data| name == "didaxis" && *data == json!({"cursor": "abc"}))
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        client
            .run(
                "didaxis",
                TRIVIAL_SCRIPT,
                json!({}),
                false,
                true,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_passes_parameters_through_to_the_scripts_run_function() {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        let script = r#"
            local M = {}
            function M.run(parameters)
                assert(parameters.username == "alice", "unexpected username")
            end
            return M
        "#;

        client
            .run(
                "didaxis",
                script,
                json!({"username": "alice"}),
                false,
                true,
                true,
                Vec::new(),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_saves_session_and_cookies_even_when_the_script_fails() {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().times(1).returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session
            .expect_save()
            .withf(|_, data| *data == json!({"partial": true}))
            .times(1)
            .returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        let script = r#"
            local M = {}
            function M.run(parameters)
                fyde.session.partial = true
                error("boom")
            end
            return M
        "#;

        let result = client
            .run("didaxis", script, json!({}), false, true, true, Vec::new())
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn run_invokes_on_progress_for_every_step_and_update_call() {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));

        let received = Arc::new(Mutex::new(Vec::new()));
        let callback = received.clone();
        let on_progress: Arc<dyn Fn(ProgressEvent) + Send + Sync> =
            Arc::new(move |event| callback.lock().unwrap().push(event));

        let client = ScrapersClient::new(
            Arc::new(cookies),
            Arc::new(session),
            Arc::new(any_reports()),
            Arc::new(MockDocumentsService::new()),
            Some(on_progress),
            None,
        );

        let script = r#"
            local M = {}
            function M.run(parameters)
                fyde.progress.step("Logging in")
                fyde.progress.update(1, 2, "halfway")
            end
            return M
        "#;

        client
            .run("didaxis", script, json!({}), false, true, true, Vec::new())
            .await
            .unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![
                ProgressEvent::Step {
                    name: "Logging in".to_string()
                },
                ProgressEvent::Update {
                    current: 1,
                    total: 2,
                    message: "halfway".to_string()
                },
            ]
        );
    }

    #[tokio::test]
    async fn run_fails_when_the_script_has_no_run_function() {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        let result = client
            .run(
                "didaxis",
                "return {}",
                json!({}),
                false,
                true,
                true,
                Vec::new(),
            )
            .await;

        assert!(result.is_err());
    }

    #[test]
    fn sandboxed_lua_has_no_filesystem_process_or_module_loading_access() {
        let lua = sandboxed_lua().unwrap();

        for global in [
            "os", "io", "require", "dofile", "loadfile", "load", "package", "debug",
        ] {
            assert!(
                matches!(
                    lua.globals().get::<mlua::Value>(global).unwrap(),
                    mlua::Value::Nil
                ),
                "expected global {global:?} to be unavailable in the sandboxed scraper lua vm"
            );
        }
    }

    #[test]
    fn sandboxed_lua_can_still_run_safe_lua() {
        let lua = sandboxed_lua().unwrap();

        let sum: i64 = lua.load("return 1 + 41").eval().unwrap();
        assert_eq!(sum, 42);

        let upper: String = lua.load(r#"return string.upper("ok")"#).eval().unwrap();
        assert_eq!(upper, "OK");

        let len: i64 = lua.load(r#"return utf8.len("héllo")"#).eval().unwrap();
        assert_eq!(len, 5);
    }

    #[tokio::test]
    async fn run_rejects_a_script_that_tries_to_reach_the_os_or_filesystem() {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        let script = r#"
            local M = {}
            function M.run(parameters)
                os.execute("echo should not run")
            end
            return M
        "#;

        let result = client
            .run("didaxis", script, json!({}), false, true, true, Vec::new())
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn run_blocks_an_http_call_to_a_domain_outside_allowed_domains() {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        let script = r#"
            local M = {}
            function M.run(parameters)
                fyde.http.get("https://evil.example.com/steal")
            end
            return M
        "#;

        let result = client
            .run(
                "didaxis",
                script,
                json!({}),
                false,
                true,
                true,
                vec!["allowed.example.com".to_string()],
            )
            .await;

        let err = result.unwrap_err().to_string();
        assert!(err.contains("evil.example.com"), "unexpected error: {err}");
    }
}
