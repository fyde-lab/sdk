use std::future::Future;
use std::sync::Arc;

use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::{Error, ErrorKind};

/// One run's "stop now" flag, set by [`super::Service::stop`] and checked
/// wherever the run can spend time: the sandbox's instruction hook (Lua code
/// running), every blocking host call ([`Self::block_on`]: `fyde.http`
/// requests and body reads, `fyde.browser` calls, a pending
/// `fyde.input.ask`), and before each `fyde.save_document` upload
/// ([`Self::check`]). Clones share the flag.
///
/// A `watch` channel rather than a bare `AtomicBool`, so an async wait
/// ([`Self::stopped`]) wakes as soon as the flag is set instead of polling,
/// while a sync check still reads it without blocking.
#[derive(Clone)]
pub(super) struct StopSignal(Arc<watch::Sender<bool>>);

impl StopSignal {
    pub(super) fn new() -> Self {
        Self(Arc::new(watch::Sender::new(false)))
    }

    /// Sets the flag; every waiter wakes. Setting it twice is harmless.
    pub(super) fn stop(&self) {
        self.0.send_replace(true);
    }

    pub(super) fn is_stopped(&self) -> bool {
        *self.0.borrow()
    }

    /// Resolves once the flag is set (at once if it already is).
    pub(super) async fn stopped(&self) {
        let mut receiver = self.0.subscribe();
        // Only fails once every sender is gone, and `self` holds one.
        let _ = receiver.wait_for(|stopped| *stopped).await;
    }

    /// Fails with [`stopped_error`] if the flag is set, for a step that
    /// shouldn't be interrupted halfway but shouldn't start once stopped.
    pub(super) fn check(&self) -> mlua::Result<()> {
        if self.is_stopped() {
            Err(stopped_error())
        } else {
            Ok(())
        }
    }

    /// Blocks on `future` like `runtime.block_on`, but gives up on it (dropping
    /// it) and fails with [`stopped_error`] as soon as the flag is set —
    /// including when it's already set, so a stopped run's next host call
    /// never starts.
    pub(super) fn block_on<F: Future>(
        &self,
        runtime: &Handle,
        future: F,
    ) -> mlua::Result<F::Output> {
        runtime.block_on(async {
            tokio::select! {
                biased;
                () = self.stopped() => Err(stopped_error()),
                output = future => Ok(output),
            }
        })
    }
}

/// The error a stopped run's script is failed with: raised inside Lua as the
/// SDK's own [`ErrorKind::ScraperRunStopped`], so `error::lua_code` keeps
/// its code wherever the script lets it through.
pub(super) fn stopped_error() -> mlua::Error {
    mlua::Error::external(stopped())
}

pub(super) fn stopped() -> Error {
    ErrorKind::ScraperRunStopped.into()
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::ErrorCode;

    fn code(err: mlua::Error) -> ErrorCode {
        Error::from(err).code()
    }

    #[test]
    fn check_passes_until_the_signal_is_stopped() {
        let signal = StopSignal::new();
        assert!(signal.check().is_ok());

        signal.clone().stop();

        assert!(signal.is_stopped());
        assert_eq!(
            code(signal.check().unwrap_err()),
            ErrorCode::ScraperRunStopped
        );
    }

    #[test]
    fn block_on_returns_the_futures_output_while_not_stopped() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let signal = StopSignal::new();

        let output = signal.block_on(runtime.handle(), async { 42 }).unwrap();

        assert_eq!(output, 42);
    }

    #[test]
    fn block_on_never_starts_a_future_once_stopped() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let signal = StopSignal::new();
        signal.stop();

        let result = signal.block_on(runtime.handle(), async { panic!("should not run") });

        assert_eq!(code(result.unwrap_err()), ErrorCode::ScraperRunStopped);
    }

    #[test]
    fn block_on_gives_up_on_a_pending_future_when_stopped() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let signal = StopSignal::new();
        let stopper = signal.clone();
        runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            stopper.stop();
        });

        let result = signal.block_on(runtime.handle(), std::future::pending::<()>());

        assert_eq!(code(result.unwrap_err()), ErrorCode::ScraperRunStopped);
    }
}
