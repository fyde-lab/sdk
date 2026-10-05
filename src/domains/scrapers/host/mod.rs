mod documents;
mod html;
mod http;
mod input;
mod json;
mod log;
mod progress;

pub(super) use http::{build_jar, export_cookies};

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mlua::{Lua, LuaSerdeExt, Table, Value as LuaValue};
use serde_json::Value as JsonValue;
use tokio::runtime::Handle;
use wreq::cookie::Jar;

use crate::domains::documents::Service as DocumentsService;
use crate::{ErrorContext as _, Result};

use super::ProgressEvent;
use super::cookies::Cookie;
use super::reports::Recorder;

/// Handle returned by [`install`], letting `service.rs` read back
/// `fyde.session`'s final contents and every cookie this run's `fyde.http`
/// picked up, once the script's `run` function has returned.
pub(super) struct Installed {
    cookie_jar: Arc<Jar>,
    visited_origins: Arc<Mutex<HashSet<String>>>,
}

impl Installed {
    /// Reads `fyde.session` back out of the Lua VM as JSON, and every cookie
    /// now sitting in the jar for a host `fyde.http` actually visited this
    /// run — ready for `service.rs` to persist via `session::Service::save`/
    /// `cookies::Service::save`. Called once after the script's `run`
    /// function returns, whether it succeeded or failed, so state from a
    /// partial run isn't lost.
    pub(super) fn drain(&self, lua: &Lua) -> Result<(JsonValue, Vec<Cookie>)> {
        let fyde: Table = lua
            .globals()
            .get("fyde")
            .context("fetching the `fyde` global")?;
        let session_table: Table = fyde.get("session").context("fetching `fyde.session`")?;
        let session_data: JsonValue = lua
            .from_value(LuaValue::Table(session_table))
            .context("converting fyde.session to JSON")?;

        let cookies = export_cookies(&self.cookie_jar, &self.visited_origins);

        Ok((session_data, cookies))
    }
}

/// Wires every host capability a scraper script is allowed to use into a
/// single `fyde` global table: `fyde.log`, `fyde.http`, `fyde.html`,
/// `fyde.json`, `fyde.progress`, `fyde.input`, `fyde.save_document`,
/// `fyde.session`. Scripts never reach outside this table — no raw
/// `io`/`os`/socket access from script code — see `demo-rust-fyde`'s own
/// `host/mod.rs`, which this is ported from. Unlike that port, `fyde.log`,
/// `fyde.http`, `fyde.progress` and `fyde.input` each also write an entry
/// to `recorder` — the `reports` sub-domain's per-run debug report,
/// `demo-rust-fyde`'s `Report` brought back (`service.rs` saves it once the
/// script's `run` function returns). `debug_http_dump` is forwarded to
/// `fyde.http` only (see `host::http::table`) — it opts this run into
/// recording full request/response headers/bodies instead of just
/// method/url/status/timing. `wreq_emulation` is also forwarded to
/// `fyde.http` only — it toggles `wreq`'s Chrome TLS/HTTP2 fingerprint
/// emulation. `follow_redirects` is also forwarded to `fyde.http` only — it
/// toggles whether the client automatically follows HTTP redirects.
#[allow(clippy::too_many_arguments)]
pub(super) fn install(
    lua: &Lua,
    scraper_name: &str,
    session_data: JsonValue,
    cookies: Vec<Cookie>,
    documents: Arc<dyn DocumentsService>,
    on_progress: Option<Arc<dyn Fn(ProgressEvent) + Send + Sync>>,
    on_question: Option<Arc<dyn Fn(String) -> String + Send + Sync>>,
    recorder: Arc<Recorder>,
    runtime: Handle,
    debug_http_dump: bool,
    wreq_emulation: bool,
    follow_redirects: bool,
) -> Result<Installed> {
    let cookie_jar = build_jar(&cookies);
    let visited_origins = Arc::new(Mutex::new(HashSet::new()));

    let fyde = lua.create_table().context("creating the `fyde` table")?;

    fyde.set("log", log::table(lua, scraper_name, recorder.clone())?)
        .context("installing fyde.log")?;
    fyde.set(
        "http",
        http::table(
            lua,
            cookie_jar.clone(),
            visited_origins.clone(),
            recorder.clone(),
            runtime.clone(),
            debug_http_dump,
            wreq_emulation,
            follow_redirects,
        )?,
    )
    .context("installing fyde.http")?;
    fyde.set("html", html::table(lua)?)
        .context("installing fyde.html")?;
    fyde.set("json", json::table(lua)?)
        .context("installing fyde.json")?;
    fyde.set(
        "progress",
        progress::table(lua, on_progress, recorder.clone())?,
    )
    .context("installing fyde.progress")?;
    fyde.set("input", input::table(lua, on_question, recorder)?)
        .context("installing fyde.input")?;
    fyde.set(
        "save_document",
        documents::save_document_fn(lua, documents, scraper_name, runtime)?,
    )
    .context("installing fyde.save_document")?;

    let session_value = lua
        .to_value(&session_data)
        .context("converting session data to a Lua value")?;
    let session_table = match session_value {
        LuaValue::Table(table) => table,
        _ => lua.create_table().context("creating empty session table")?,
    };
    fyde.set("session", session_table)
        .context("installing fyde.session")?;

    lua.globals()
        .set("fyde", fyde)
        .context("exposing the `fyde` global")?;

    Ok(Installed {
        cookie_jar,
        visited_origins,
    })
}
