use std::sync::Arc;

/// The verbosity of the SDK's internal `tracing` logs, set up by
/// [`crate::Client::init`]. Mirrors [`tracing::Level`] plus an `Off` variant
/// to silence logging entirely, since callers (in particular FFI callers
/// like the Kotlin Multiplatform app) have no `RUST_LOG` environment
/// variable to set.
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
/// [`crate::ClientConfig::on_log`] is set, so callers (in particular FFI
/// callers with no terminal to print to) can observe SDK logs directly.
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
                if field.name() == "message" {
                    let _ = write!(self.0, "{value:?}");
                } else {
                    let _ = write!(self.0, " {}={value:?}", field.name());
                }
            }
        }

        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);

        (self.callback)(
            LogLevel::from(event.metadata().level()),
            format!("{}: {}", event.metadata().target(), visitor.0),
        );
    }
}
