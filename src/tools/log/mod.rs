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
    fn off_maps_to_the_off_filter() {
        assert_eq!(
            tracing_subscriber::filter::LevelFilter::from(LogLevel::Off),
            tracing_subscriber::filter::LevelFilter::OFF
        );
    }
}
