use std::cell::Cell;
use std::sync::Arc;

use tracing_log::NormalizeEvent;

/// The verbosity of the SDK's internal `tracing` logs, set up by
/// [`crate::Client::init`]. Mirrors [`tracing::Level`] plus an `Off` variant
/// to silence logging entirely, since some callers (in particular the
/// Kotlin Multiplatform app) have no `RUST_LOG` environment variable to
/// set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for tracing_subscriber::filter::LevelFilter {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Off => Self::OFF,
            LogLevel::Error => Self::ERROR,
            LogLevel::Warn => Self::WARN,
            LogLevel::Info => Self::INFO,
            LogLevel::Debug => Self::DEBUG,
            LogLevel::Trace => Self::TRACE,
        }
    }
}

impl From<&tracing::Level> for LogLevel {
    fn from(level: &tracing::Level) -> Self {
        match *level {
            tracing::Level::ERROR => Self::Error,
            tracing::Level::WARN => Self::Warn,
            tracing::Level::INFO => Self::Info,
            tracing::Level::DEBUG => Self::Debug,
            tracing::Level::TRACE => Self::Trace,
        }
    }
}

/// A `tracing_subscriber` [`Layer`](tracing_subscriber::Layer) that forwards
/// every log line it sees, formatted as a single string, to a caller-supplied
/// callback. Installed alongside the crate's usual `fmt` layer when
/// [`crate::ClientConfig::on_log`] is set, so callers with no terminal to
/// print to (in particular the Kotlin Multiplatform app) can observe SDK
/// logs directly.
///
/// Never re-enters its callback: an event the callback itself causes on
/// its thread, while it runs, is not forwarded to it (the `fmt` layer still
/// prints it). That includes the ERROR [`install_panic_hook`] logs when the
/// callback panics (a UniFFI callback does when its foreign implementation
/// throws): calling it again from the hook would panic inside the hook,
/// which aborts the process.
pub(crate) struct CallbackLayer {
    pub(crate) callback: Arc<dyn Fn(LogLevel, String) + Send + Sync>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CallbackLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct MessageVisitor(String);

        impl tracing::field::Visit for MessageVisitor {
            fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                if field.name().starts_with("log.") {
                    // Metadata of a bridged `log` record, recovered below.
                } else if field.name() == "message" {
                    let _ = write!(self.0, "{value:?}");
                } else {
                    if !self.0.is_empty() {
                        self.0.push(' ');
                    }
                    let _ = write!(self.0, "{}={value:?}", field.name());
                }
            }
        }

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        // Records bridged from the `log` crate all share a generic `log`
        // target and carry their real one in `log.*` fields, so recover it.
        let normalized = event.normalized_metadata();
        let metadata = normalized.as_ref().unwrap_or_else(|| event.metadata());

        let Some(_guard) = CallbackGuard::enter() else {
            return;
        };
        (self.callback)(
            LogLevel::from(metadata.level()),
            format!("{}: {}", metadata.target(), visitor.0),
        );
    }
}

thread_local! {
    /// Whether this thread is running a [`CallbackLayer`]'s callback.
    static IN_CALLBACK: Cell<bool> = const { Cell::new(false) };
}

/// Marks this thread as running a [`CallbackLayer`]'s callback until it
/// drops, unwinding included.
struct CallbackGuard;

impl CallbackGuard {
    /// `None` when this thread already runs one.
    fn enter() -> Option<Self> {
        (!IN_CALLBACK.replace(true)).then_some(Self)
    }
}

impl Drop for CallbackGuard {
    fn drop(&mut self) {
        IN_CALLBACK.set(false);
    }
}

/// The target panics are logged under (see [`install_panic_hook`]).
const PANIC_TARGET: &str = "fyde_sdk::panic";

/// Logs every panic, on any thread, as an ERROR under [`PANIC_TARGET`]
/// before handing it to the hook that was installed before (by default,
/// the one printing it to stderr). Without it a panic only reaches stderr,
/// which Android sends nowhere: a background thread dying there (the
/// scrapers' browser thread, say) left no trace in the app's logs. Carries
/// the thread, the location and the message, plus a backtrace when
/// `RUST_BACKTRACE` asks for one (logcat cuts long entries short).
///
/// Process-wide, like the `tracing` subscriber it logs through: installed
/// once, by [`crate::Client::init`] when it installs that subscriber.
pub(crate) fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map_or_else(|| "<unknown location>".to_string(), ToString::to_string);
        let message = info.payload_as_str().unwrap_or("Box<dyn Any>");
        let backtrace = std::backtrace::Backtrace::capture();
        if backtrace.status() == std::backtrace::BacktraceStatus::Captured {
            tracing::error!(target: PANIC_TARGET, "thread '{thread}' panicked at {location}: {message}\n{backtrace}");
        } else {
            tracing::error!(target: PANIC_TARGET, "thread '{thread}' panicked at {location}: {message}");
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use tracing_subscriber::prelude::*;

    use super::*;

    /// Runs `emit` under a subscriber whose only layer is a [`CallbackLayer`],
    /// returning every `(level, line)` pair it forwarded.
    fn forwarded(emit: impl FnOnce()) -> Vec<(LogLevel, String)> {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let sink = lines.clone();
        let layer = CallbackLayer {
            callback: Arc::new(move |level, line| sink.lock().unwrap().push((level, line))),
        };
        tracing::subscriber::with_default(tracing_subscriber::registry().with(layer), emit);
        Arc::try_unwrap(lines).unwrap().into_inner().unwrap()
    }

    #[test]
    fn forwards_the_target_and_message() {
        let lines = forwarded(|| tracing::info!(target: "fyde_sdk::sync", "caught up"));

        assert_eq!(
            lines,
            vec![(LogLevel::Info, "fyde_sdk::sync: caught up".to_string())]
        );
    }

    #[test]
    fn appends_structured_fields_after_the_message() {
        let lines = forwarded(
            || tracing::warn!(target: "fyde_sdk::sync", attempt = 3, host = "example.com", "retrying"),
        );

        assert_eq!(
            lines,
            vec![(
                LogLevel::Warn,
                "fyde_sdk::sync: retrying attempt=3 host=\"example.com\"".to_string()
            )]
        );
    }

    #[test]
    fn separates_the_fields_of_an_event_without_a_message_by_one_space() {
        let lines =
            forwarded(|| tracing::info!(target: "fyde_sdk::sync", attempt = 3, done = true));

        assert_eq!(
            lines,
            vec![(
                LogLevel::Info,
                "fyde_sdk::sync: attempt=3 done=true".to_string()
            )]
        );
    }

    #[test]
    fn recovers_the_target_of_records_bridged_from_log() {
        let lines = forwarded(|| {
            let record = log::Record::builder()
                .level(log::Level::Warn)
                .target("some_crate::object")
                .file(Some("object.rs"))
                .line(Some(291))
                .args(format_args!("treating as empty stream"))
                .build();
            tracing_log::format_trace(&record).unwrap();
        });

        assert_eq!(
            lines,
            vec![(
                LogLevel::Warn,
                "some_crate::object: treating as empty stream".to_string()
            )]
        );
    }

    #[test]
    fn maps_every_tracing_level() {
        let lines = forwarded(|| {
            tracing::error!("e");
            tracing::warn!("w");
            tracing::info!("i");
            tracing::debug!("d");
            tracing::trace!("t");
        });

        let levels: Vec<LogLevel> = lines.into_iter().map(|(level, _)| level).collect();
        assert_eq!(
            levels,
            vec![
                LogLevel::Error,
                LogLevel::Warn,
                LogLevel::Info,
                LogLevel::Debug,
                LogLevel::Trace,
            ]
        );
    }

    #[test]
    fn logs_a_panic_with_its_thread_location_and_message() {
        install_panic_hook();

        let lines = forwarded(|| {
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            let thread = std::thread::Builder::new()
                .name("fyde-test-panic".to_string())
                .spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || panic!("boom {}", 42));
                })
                .unwrap();
            assert!(thread.join().is_err());
        });

        let (level, line) = lines
            .into_iter()
            .find(|(_, line)| line.starts_with(PANIC_TARGET))
            .expect("the panic must be logged");
        assert_eq!(level, LogLevel::Error);
        assert!(
            line.starts_with(&format!(
                "{PANIC_TARGET}: thread 'fyde-test-panic' panicked at src/tools/log/mod.rs:"
            )),
            "{line}"
        );
        assert!(line.contains(": boom 42"), "{line}");
    }

    /// Set for [`panicking_callback_under_the_global_subscriber`] when
    /// [`a_panicking_callback_unwinds_instead_of_aborting`] runs it.
    const PANICKING_CALLBACK_CHILD: &str = "FYDE_TEST_PANICKING_LOG_CALLBACK";

    /// The global subscriber is the case that matters: a scoped one
    /// (`with_default`, as in the other tests) already drops events raised
    /// while it dispatches, so it needs its own process.
    #[test]
    fn a_panicking_callback_unwinds_instead_of_aborting() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tools::log::tests::panicking_callback_under_the_global_subscriber",
                "--ignored",
                "--nocapture",
            ])
            .env(PANICKING_CALLBACK_CHILD, "1")
            .output()
            .unwrap();

        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{}\n{stderr}", output.status);
        assert!(stderr.contains("callback calls: 1"), "{stderr}");
    }

    #[test]
    #[ignore = "run in its own process by a_panicking_callback_unwinds_instead_of_aborting"]
    fn panicking_callback_under_the_global_subscriber() {
        if std::env::var_os(PANICKING_CALLBACK_CHILD).is_none() {
            return;
        }
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let layer = CallbackLayer {
            callback: Arc::new(move |_, _| {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                panic!("the foreign logger threw");
            }),
        };
        tracing::subscriber::set_global_default(tracing_subscriber::registry().with(layer))
            .unwrap();
        install_panic_hook();

        let thread = std::thread::spawn(|| tracing::info!("hello"));

        assert!(thread.join().is_err());
        eprintln!(
            "callback calls: {}",
            calls.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn off_maps_to_the_off_filter() {
        assert_eq!(
            tracing_subscriber::filter::LevelFilter::from(LogLevel::Off),
            tracing_subscriber::filter::LevelFilter::OFF
        );
    }
}
