use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mlua::{Function, Lua, LuaSerdeExt as _, StdLib, Table};
use serde_json::{Value, json};
use tokio::runtime::Handle;

use crate::domains::documents::Service as DocumentsService;
use crate::domains::scripts::InstalledScript;
use crate::{ErrorContext as _, Result};

use super::cookies::{Cookie, Service as CookiesService};
use super::host::{self, HostConfig};
use super::models::RunOptions;
use super::reports::{Recorder, Service as ReportsService};
use super::session::Service as SessionService;
use super::{OnScraperProgress, OnScraperQuestion, ProgressEvent, Service};

/// The `wreq_emulation`/`follow_redirects` an installed script runs with.
/// Each scraper's `manifest.json` (in `../scripts`) chooses these for a dev
/// run, but a published script doesn't carry them, so an installed one gets
/// what 110 of the 112 manifests choose — a real Chrome fingerprint, and
/// redirects followed. (`ameli` and `cesu` opt out of emulation, so they
/// won't behave as in a dev run until scripts publish these flags too.)
const INSTALLED_WREQ_EMULATION: bool = true;
const INSTALLED_FOLLOW_REDIRECTS: bool = true;

/// The default [`Service`] implementation, running scripts in a fresh
/// `mlua::Lua` VM per call, delegating session/cookie persistence to
/// injected [`SessionService`]/[`CookiesService`] implementations, this
/// run's debug report to an injected [`ReportsService`], and document
/// uploads to an injected [`DocumentsService`].
///
/// Runs share nothing but these injected services, so any number can run
/// at once — except two under the same name, which would load the same
/// session and cookies and then overwrite each other's saves (a failed run
/// even deleting the cookies a successful one just saved): `running`
/// serializes those, see [`ScrapersClient::lock_name`].
pub(super) struct ScrapersClient {
    cookies: Arc<dyn CookiesService>,
    session: Arc<dyn SessionService>,
    reports: Arc<dyn ReportsService>,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<OnScraperProgress>,
    on_question: Option<OnScraperQuestion>,
    running: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
}

impl ScrapersClient {
    pub(super) fn new(
        cookies: Arc<dyn CookiesService>,
        session: Arc<dyn SessionService>,
        reports: Arc<dyn ReportsService>,
        documents: Arc<dyn DocumentsService>,
        on_progress: Option<OnScraperProgress>,
        on_question: Option<OnScraperQuestion>,
    ) -> Self {
        Self {
            cookies,
            session,
            reports,
            documents,
            on_progress,
            on_question,
            running: Mutex::new(HashMap::new()),
        }
    }

    /// Waits until no other run holds `name`, then holds it until the
    /// returned guard drops — across the whole load, run and save of
    /// `Service::run`, so a waiting run starts from what the previous one
    /// saved. Runs under different names never wait on each other.
    async fn lock_name(&self, name: &str) -> NameGuard<'_> {
        let lock = self
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(name.to_string())
            .or_default()
            .clone();
        let guard = lock.clone().lock_owned().await;
        NameGuard {
            client: self,
            name: name.to_string(),
            lock,
            _guard: Some(guard),
        }
    }
}

/// Held by a run for as long as it owns its name — see
/// [`ScrapersClient::lock_name`]. Dropping it lets the next run under that
/// name start, and forgets the name once no run holds or awaits it.
struct NameGuard<'a> {
    client: &'a ScrapersClient,
    name: String,
    lock: Arc<tokio::sync::Mutex<()>>,
    _guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for NameGuard<'_> {
    fn drop(&mut self) {
        self._guard.take();
        let mut running = self
            .client
            .running
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        // Two references left: the map's and this guard's own, so no other
        // run holds or waits for this name.
        if Arc::strong_count(&self.lock) == 2 {
            running.remove(&self.name);
        }
    }
}

#[async_trait]
impl Service for ScrapersClient {
    async fn run_installed(&self, installed: &InstalledScript) -> Result<()> {
        let script = installed.script();
        let parameters = serde_json::to_value(installed.parameters())
            .with_context(|| format!("failed to encode parameters of script {}", script.id()))?;
        self.run(
            &script.id().to_string(),
            script.script(),
            parameters,
            false,
            INSTALLED_WREQ_EMULATION,
            INSTALLED_FOLLOW_REDIRECTS,
            script.allowed_domains().to_vec(),
            false,
        )
        .await
    }

    async fn run(
        &self,
        name: &str,
        script: &str,
        parameters: Value,
        debug_http_dump: bool,
        wreq_emulation: bool,
        follow_redirects: bool,
        allowed_domains: Vec<String>,
        browser_visible: bool,
    ) -> Result<()> {
        let _name = self.lock_name(name).await;

        let cookies = self
            .cookies
            .load(name)
            .await
            .with_context(|| format!("failed to load cookies for scraper {name:?}"))?;
        let session_data = self
            .session
            .load(name)
            .await
            .with_context(|| format!("failed to load session for scraper {name:?}"))?;
        let recorder = Arc::new(Recorder::new(name));
        recorder.record("run_started", json!({ "script": name }));

        let job = ScraperJob {
            name: name.to_string(),
            script: script.to_string(),
            parameters,
            session_data,
            cookies,
            documents: self.documents.clone(),
            on_progress: self.on_progress.clone(),
            on_question: self.on_question.clone(),
            recorder: recorder.clone(),
            runtime: Handle::current(),
            options: RunOptions {
                debug_http_dump,
                wreq_emulation,
                follow_redirects,
                allowed_domains,
                browser_visible,
            },
        };

        let (session_data, cookies, run_result) =
            tokio::task::spawn_blocking(move || run_script(job))
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
        let cookies = if run_result.is_ok() {
            cookies
        } else {
            Vec::new()
        };
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

/// Everything the blocking half of a scraper run (see [`run_script`])
/// needs, moved onto its blocking thread in one piece.
struct ScraperJob {
    name: String,
    script: String,
    parameters: Value,
    session_data: Value,
    cookies: Vec<Cookie>,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<OnScraperProgress>,
    on_question: Option<OnScraperQuestion>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    options: RunOptions,
}

/// Runs the whole blocking, synchronous half of a scraper run: builds a
/// fresh Lua VM, installs the `fyde` host table (see `host::install`),
/// evaluates `script` and calls its `run(parameters)` entrypoint, then reads
/// back `fyde.session`'s final contents and every cookie picked up this run
/// — regardless of whether the script's `run` succeeded or failed. The
/// caller is responsible for persisting the returned session data/cookies
/// (cookies only when `run_result` is `Ok`, see `Service::run`) and for
/// propagating `run_result`.
fn run_script(job: ScraperJob) -> Result<(Value, Vec<Cookie>, Result<()>)> {
    let ScraperJob {
        name,
        script,
        parameters,
        session_data,
        cookies,
        documents,
        on_progress,
        on_question,
        recorder,
        runtime,
        options,
    } = job;

    // Bound to this run's name, so the caller can tell its events and
    // questions apart from those of any other run going on at the same time.
    let on_progress = on_progress.map(|callback| -> Arc<dyn Fn(ProgressEvent) + Send + Sync> {
        let name = name.clone();
        Arc::new(move |event| callback(&name, event))
    });
    let on_question = on_question.map(|callback| -> Arc<dyn Fn(String) -> String + Send + Sync> {
        let name = name.clone();
        Arc::new(move |question| callback(&name, question))
    });

    let lua = sandboxed_lua().context("building sandboxed lua vm")?;
    // Built fresh per run, same as the Lua VM itself: the window and webview
    // it lazily opens on the shared browser thread (see
    // `browser::driver::BrowserDriver`) live only as long as
    // this run does, closed via its `Drop` impl once every `Arc` clone
    // (`host::install`'s for `fyde.browser`'s closures, and this function's
    // own for reading its cookies back below) goes out of scope at the end
    // of this function. Its only state carried across runs is cookies,
    // seeded here from the same saved jar `fyde.http` gets.
    let browser = super::browser::init(
        Arc::new(options.allowed_domains.clone()),
        options.browser_visible,
        cookies.clone(),
    );
    let browser_for_cookies = browser.clone();
    let cookies_runtime = runtime.clone();
    let cookies_recorder = recorder.clone();
    let installed = host::install(
        &lua,
        HostConfig {
            scraper_name: name.clone(),
            session_data,
            cookies,
            documents,
            browser,
            on_progress,
            on_question,
            recorder,
            runtime,
            options,
        },
    )
    .context("installing host functions into the Lua VM")?;

    let run_result = evaluate_and_run(&lua, &name, &script, parameters);

    let (session_data, http_cookies) = installed
        .drain(&lua)
        .context("reading back session data and cookies after the scraper run")?;

    // A failure here only costs the next run its browser session, so it's
    // recorded rather than allowed to fail a run that otherwise succeeded.
    let browser_cookies = cookies_runtime
        .block_on(browser_for_cookies.cookies())
        .unwrap_or_else(|err| {
            cookies_recorder.record(
                "error",
                json!({ "action": "browser.cookies", "message": err.to_string() }),
            );
            Vec::new()
        });

    Ok((
        session_data,
        merge_cookies(http_cookies, browser_cookies),
        run_result,
    ))
}

/// Combines the cookies `fyde.http` and `fyde.browser` each ended the run
/// with into the one jar the `cookies` sub-domain saves. Both started from
/// the same saved jar, so the same cookie (same origin and name) can come
/// back from both; the browser's copy wins, since a browser-driven step is
/// what a script falls back on when plain HTTP isn't enough, making it the
/// more likely of the two to hold the live session.
fn merge_cookies(http_cookies: Vec<Cookie>, browser_cookies: Vec<Cookie>) -> Vec<Cookie> {
    fn key(cookie: &Cookie) -> (&str, &str) {
        let name = cookie
            .set_cookie
            .split(['=', ';'])
            .next()
            .unwrap_or("")
            .trim();
        (cookie.origin.as_str(), name)
    }

    let from_browser: std::collections::HashSet<(&str, &str)> =
        browser_cookies.iter().map(key).collect();
    let mut merged: Vec<Cookie> = http_cookies
        .iter()
        .filter(|cookie| !from_browser.contains(&key(cookie)))
        .cloned()
        .collect();
    merged.extend(browser_cookies.iter().cloned());
    merged
}

/// Builds a fully sandboxed Lua VM for running a scraper script: only the
/// side-effect-free `table`/`string`/`math`/`utf8` subset of the standard
/// library is loaded — no `io`, `os`, `package` (so no `require`), `debug`,
/// or native-code loaders — so a script has no path to the filesystem, OS
/// environment, subprocesses, dynamic library loading, or raw sockets. The
/// only network access a script gets is through the `fyde.http`/
/// `fyde.browser` host functions `host::install` wires into the `fyde`
/// global table below — there is no other way out of this VM. `utf8` is kept
/// because `didaxis.lua` calls `utf8.len`.
///
/// See [`crate::tools::sandbox::new_vm`] for the globals it strips on top of that
/// and the limits it enforces, so a script that loops forever or allocates
/// without bound fails on its own instead of hanging or crashing the host.
/// The memory limit leaves room for the documents a script downloads, which
/// live on the VM's heap as Lua strings until `fyde.save_document` takes
/// them; the instruction budget only counts Lua code actually executing, not
/// time spent blocked in host functions (network calls, `fyde.input.ask`).
fn sandboxed_lua() -> Result<Lua> {
    crate::tools::sandbox::new_vm(
        StdLib::TABLE | StdLib::STRING | StdLib::MATH | StdLib::UTF8,
        crate::tools::sandbox::Limits {
            memory_bytes: 512 * 1024 * 1024,
            max_instructions: 5_000_000_000,
        },
    )
}

/// Loads `script` as a module (expecting it to return a table with a single
/// `run(parameters)` function — the entire contract between the Rust host
/// and a Lua scraper script, same as `demo-rust-fyde`'s `main.rs`) and calls
/// it with `parameters`.
fn evaluate_and_run(lua: &Lua, name: &str, script: &str, parameters: Value) -> Result<()> {
    let chunk = crate::tools::sandbox::load_source(lua, script).set_name(name);
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
    use std::time::Duration;

    use mockall::predicate::eq;
    use serde_json::json;

    use super::super::ProgressStep;
    use super::super::cookies::{FakeCookie, MockService as MockCookiesService};
    use super::super::reports::MockService as MockReportsService;
    use super::super::session::MockService as MockSessionService;
    use super::*;
    use crate::domains::documents::MockService as MockDocumentsService;
    use crate::domains::scripts::{FakeInstalledScript, FakeScript};
    use std::collections::HashMap;

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

    #[test]
    fn merge_cookies_prefers_the_browsers_copy_of_the_same_cookie() {
        let stale = FakeCookie::new()
            .with_origin("https://example.com")
            .with_set_cookie("session=old; Path=/")
            .build();
        let fresh = FakeCookie::new()
            .with_origin("https://example.com")
            .with_set_cookie("session=new; Path=/; HttpOnly")
            .build();

        let merged = merge_cookies(vec![stale], vec![fresh.clone()]);

        assert_eq!(merged, vec![fresh]);
    }

    #[test]
    fn merge_cookies_keeps_distinct_cookies_from_both_sides() {
        let from_http = FakeCookie::new()
            .with_origin("https://example.com")
            .with_set_cookie("csrf=1")
            .build();
        let other_host = FakeCookie::new()
            .with_origin("https://other.example.com")
            .with_set_cookie("session=a")
            .build();
        let from_browser = FakeCookie::new()
            .with_origin("https://example.com")
            .with_set_cookie("session=b")
            .build();

        let merged = merge_cookies(
            vec![from_http.clone(), other_host.clone()],
            vec![from_browser.clone()],
        );

        assert_eq!(merged, vec![from_http, other_host, from_browser]);
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
                false,
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
                false,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn run_installed_runs_the_script_with_its_installed_parameters_under_its_id() {
        let script = FakeScript::new()
            .with_script(
                r#"
                local M = {}
                function M.run(parameters)
                    assert(parameters.username == "alice", "unexpected username")
                end
                return M
            "#,
            )
            .build();
        let name = script.id().to_string();
        let installed = FakeInstalledScript::new()
            .with_script(script)
            .with_parameters(HashMap::from([("username".to_string(), json!("alice"))]));

        let mut cookies = MockCookiesService::new();
        let loaded_name = name.clone();
        cookies
            .expect_load()
            .withf(move |requested| requested == loaded_name)
            .times(1)
            .returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session
            .expect_load()
            .withf(move |requested| requested == name)
            .times(1)
            .returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));

        let client = client(cookies, session, MockDocumentsService::new());

        client.run_installed(&installed.build()).await.unwrap();
    }

    #[tokio::test]
    async fn run_saves_session_but_deletes_cookies_when_the_script_fails() {
        let mut cookies = MockCookiesService::new();
        let stale = FakeCookie::new().build();
        cookies
            .expect_load()
            .returning(move |_| Ok(vec![stale.clone()]));
        cookies
            .expect_save()
            .withf(|_, saved| saved.is_empty())
            .times(1)
            .returning(|_, _| Ok(()));
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
            .run(
                "didaxis",
                script,
                json!({}),
                false,
                true,
                true,
                Vec::new(),
                false,
            )
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
        let on_progress: OnScraperProgress = Arc::new(move |name: &str, event| {
            callback.lock().unwrap().push((name.to_string(), event))
        });

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
                fyde.progress.step("logging_in")
                fyde.progress.update(1, 2)
            end
            return M
        "#;

        client
            .run(
                "didaxis",
                script,
                json!({}),
                false,
                true,
                true,
                Vec::new(),
                false,
            )
            .await
            .unwrap();

        assert_eq!(
            *received.lock().unwrap(),
            vec![
                (
                    "didaxis".to_string(),
                    ProgressEvent::Step {
                        step: ProgressStep::LoggingIn
                    }
                ),
                (
                    "didaxis".to_string(),
                    ProgressEvent::Update {
                        current: 1,
                        total: 2
                    }
                ),
            ]
        );
    }

    /// Script whose `run` asks one question, so a test's `on_question`
    /// callback runs inside the run, on its blocking thread.
    const ASKING_SCRIPT: &str = r#"
        local M = {}
        function M.run(parameters)
            fyde.session.answer = fyde.input.ask("code?")
        end
        return M
    "#;

    /// A client whose session/cookie storage accepts any load or save, with
    /// `on_question` as its question handler.
    fn asking_client(on_question: OnScraperQuestion) -> Arc<ScrapersClient> {
        let mut cookies = MockCookiesService::new();
        cookies.expect_load().returning(|_| Ok(Vec::new()));
        cookies.expect_save().returning(|_, _| Ok(()));
        let mut session = MockSessionService::new();
        session.expect_load().returning(|_| Ok(json!({})));
        session.expect_save().returning(|_, _| Ok(()));
        let mut reports = MockReportsService::new();
        reports.expect_save().returning(|_| Ok(()));

        Arc::new(ScrapersClient::new(
            Arc::new(cookies),
            Arc::new(session),
            Arc::new(reports),
            Arc::new(MockDocumentsService::new()),
            None,
            Some(on_question),
        ))
    }

    async fn run_asking(client: Arc<ScrapersClient>, name: &'static str) -> Result<()> {
        client
            .run(
                name,
                ASKING_SCRIPT,
                json!({}),
                false,
                true,
                true,
                Vec::new(),
                false,
            )
            .await
    }

    #[tokio::test]
    async fn run_runs_scrapers_with_different_names_in_parallel() {
        // Both questions only get answered once both runs are asking at
        // the same time: run one after the other, they'd time out.
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let for_callback = asked.clone();
        let client = asking_client(Arc::new(move |name: &str, _question| {
            for_callback.lock().unwrap().push(name.to_string());
            barrier.wait();
            format!("answer for {name}")
        }));

        let both = async {
            tokio::try_join!(
                run_asking(client.clone(), "didaxis"),
                run_asking(client.clone(), "impots"),
            )
        };
        tokio::time::timeout(Duration::from_secs(10), both)
            .await
            .expect("the two runs should not wait on each other")
            .unwrap();

        let mut asked = asked.lock().unwrap().clone();
        asked.sort();
        assert_eq!(asked, vec!["didaxis", "impots"]);
    }

    #[tokio::test]
    async fn run_serializes_two_runs_of_the_same_scraper() {
        let active = Arc::new(Mutex::new((0, 0)));
        let for_callback = active.clone();
        let client = asking_client(Arc::new(move |_name: &str, _question| {
            {
                let mut active = for_callback.lock().unwrap();
                active.0 += 1;
                active.1 = active.1.max(active.0);
            }
            std::thread::sleep(Duration::from_millis(100));
            for_callback.lock().unwrap().0 -= 1;
            "ok".to_string()
        }));

        tokio::try_join!(
            run_asking(client.clone(), "didaxis"),
            run_asking(client.clone(), "didaxis"),
        )
        .unwrap();

        assert_eq!(active.lock().unwrap().1, 1, "runs overlapped");
        assert!(
            client.running.lock().unwrap().is_empty(),
            "the name should be forgotten once no run holds it"
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
                false,
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
            .run(
                "didaxis",
                script,
                json!({}),
                false,
                true,
                true,
                Vec::new(),
                false,
            )
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
                false,
            )
            .await;

        let err = result.unwrap_err().to_string();
        assert!(err.contains("evil.example.com"), "unexpected error: {err}");
    }
}
