//! Allows a future to execute for a maximum amount of time.
//!
//! See [`Timeout`] documentation for more details.
//!
//! [`Timeout`]: struct@Timeout
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;
use std::time::Duration;

use super::context::ContextExt;
use crate::fiber;
use crate::time::Instant;

/// Error returned by [`Timeout`]
#[derive(thiserror::Error, Debug, PartialEq, Eq)]
pub enum Error<E> {
    #[error("deadline expired")]
    Expired,
    #[error("{0}")]
    Failed(#[from] E),
}

pub type Result<T, E> = std::result::Result<T, Error<E>>;

/// Future returned by [`timeout`](timeout).
#[derive(Debug)]
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct Timeout<F> {
    future: F,
    /// This flag allows to make one more poll
    /// to inner future after actual timeout, true by default
    extra_check: bool,
    deadline: Option<Instant>,
}

/// Requires a `Future` to complete before the specified duration has elapsed.
///
/// If the future completes before the duration has elapsed, then the completed
/// value is returned. Otherwise, an error is returned and the future is
/// canceled.
///
/// A `timeout` equal to [`Duration::ZERO`] guarantees that awaiting this future
/// will **not** result in a fiber yield.
///
/// ```no_run
/// use tarantool::fiber::r#async::*;
/// use tarantool::fiber;
/// use std::time::Duration;
///
/// let (tx, rx) = oneshot::channel::<i32>();
///
/// // Wrap the future with a `Timeout` set to expire in 10 milliseconds.
/// if let Err(_) = fiber::block_on(timeout::timeout(Duration::from_millis(10), rx)) {
///     println!("did not receive value within 10 ms");
/// }
/// ```
#[inline(always)]
pub fn timeout<F: Future>(timeout: Duration, f: F) -> Timeout<F> {
    Timeout {
        future: f,
        extra_check: true,
        deadline: fiber::clock().checked_add(timeout),
    }
}

/// Like [`timeout`], but with an explicit deadline.
#[inline(always)]
pub fn deadline<F: Future>(deadline: Instant, f: F) -> Timeout<F> {
    Timeout {
        future: f,
        extra_check: true,
        deadline: Some(deadline),
    }
}

impl<F: Future> Timeout<F> {
    /// Disable extra check after timeout
    pub fn no_extra_check(self) -> Self {
        let mut timeout = self;
        timeout.extra_check = false;
        timeout
    }

    #[inline]
    fn pin_get_future(self: Pin<&mut Self>) -> Pin<&mut F> {
        // This is okay because `future` is pinned when `self` is.
        unsafe { self.map_unchecked_mut(|s| &mut s.future) }
    }
}

impl<F, T, E> Future for Timeout<F>
where
    F: Future<Output = std::result::Result<T, E>>,
{
    type Output = Result<T, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let extra_check = self.extra_check;
        let deadline = self.deadline;
        let is_timeout = if let Some(v) = deadline {
            fiber::clock() >= v
        } else {
            false
        };

        // First, try polling the future in two cases
        // - extra check flag (which is set by default)
        // - still not timed out
        if extra_check || !is_timeout {
            if let Poll::Ready(v) = self.pin_get_future().poll(cx) {
                return Poll::Ready(v.map_err(Error::Failed));
            }
        }
        // If operation is timed out
        if is_timeout {
            return Poll::Ready(Err(Error::Expired));
        };
        // If there is None value in deadline - future will never be finished
        if let Some(v) = deadline {
            // SAFETY: This is safe as long as the `Context` really
            // is the `ContextExt`. It's always true within provided
            // `block_on` async runtime.
            unsafe { ContextExt::set_deadline(cx, v) };
        }

        Poll::Pending
    }
}

/// Futures implementing this trait can be constrained with a timeout (see
/// [`Timeout`]).
///
/// **NOTE**: this trait is implemented for all type implementing
/// [`std::future::Future`], but it must be used **only** with futures from
/// [`crate::fiber::async`] otherwise the behaviour is undefined.
pub trait IntoTimeout: Future + Sized {
    /// Adds timeout to a future. See [`Timeout`].
    #[inline(always)]
    fn timeout(self, timeout: Duration) -> Timeout<Self> {
        self::timeout(timeout, self)
    }

    /// Adds a deadline to the future. See [`Timeout`].
    #[inline(always)]
    fn deadline(self, deadline: Instant) -> Timeout<Self> {
        self::deadline(deadline, self)
    }
}

impl<T> IntoTimeout for T where T: Future + Sized {}

#[cfg(feature = "internal_test")]
mod tests {
    use super::*;
    use crate::fiber;
    use crate::fiber::check_yield;
    use crate::fiber::r#async::{oneshot, RecvError};
    use crate::fiber::YieldResult::{DidntYield, Yielded};
    use crate::test::util::ok;
    use std::time::Duration;

    const _0_SEC: Duration = Duration::ZERO;
    const _1_SEC: Duration = Duration::from_secs(1);

    #[crate::test(tarantool = "crate")]
    fn instant_future() {
        let fut = async { 78 };
        assert_eq!(fiber::block_on(fut), 78);

        let fut = timeout(Duration::ZERO, async { ok(79) });
        assert_eq!(fiber::block_on(fut), Ok(79));
    }

    #[crate::test(tarantool = "crate")]
    fn actual_timeout_promise() {
        let (tx, rx) = oneshot::channel::<i32>();
        let fut = async move { rx.timeout(_0_SEC).await };

        let jh = fiber::start_async(fut);
        assert_eq!(jh.join(), Err(Error::Expired));
        drop(tx);
    }

    #[crate::test(tarantool = "crate")]
    fn drop_tx_before_timeout() {
        let (tx, rx) = oneshot::channel::<i32>();
        let fut = async move { rx.timeout(_1_SEC).await };

        let jh = fiber::start(move || fiber::block_on(fut));
        drop(tx);
        assert_eq!(jh.join(), Err(Error::Failed(RecvError)));
    }

    #[crate::test(tarantool = "crate")]
    fn send_tx_before_timeout() {
        let (tx, rx) = oneshot::channel::<i32>();
        let fut = async move { rx.timeout(_1_SEC).await };

        let jh = fiber::start(move || fiber::block_on(fut));
        tx.send(400).unwrap();
        assert_eq!(jh.join(), Ok(400));
    }

    #[crate::test(tarantool = "crate")]
    fn timeout_duration_max() {
        // must not panic
        fiber::block_on(timeout(Duration::MAX, async { ok(1) })).unwrap();
    }

    #[crate::test(tarantool = "crate")]
    fn await_actually_yields() {
        // ready future, no timeout -> no yield
        assert_eq!(
            check_yield(|| fiber::block_on(async { 101 })),
            DidntYield(101)
        );

        // ready future, 0 timeout -> no yield
        assert_eq!(
            check_yield(|| fiber::block_on(timeout(Duration::ZERO, async { ok(202) }))),
            DidntYield(Ok(202))
        );

        // ready future, positive timeout -> no yield
        assert_eq!(
            check_yield(|| fiber::block_on(timeout(Duration::from_secs(1), async { ok(303) }))),
            DidntYield(Ok(303))
        );

        // pending future, no timeout -> yield
        let (_tx, rx) = oneshot::channel::<i32>();
        let f = check_yield(|| fiber::start(|| fiber::block_on(rx)));
        // the yield happens as soon as fiber::start is called,
        // but if fiber::block_on didn't yield we wouldn't even get here,
        // so this check is totally legit
        assert!(matches!(f, Yielded(_)));
        // we leak some memory here, but avoid a panic.
        // Don't do this in your code
        std::mem::forget(f);

        // pending future, 0 timeout -> no yield
        let (_tx, rx) = oneshot::channel::<i32>();
        assert_eq!(
            check_yield(|| fiber::block_on(timeout(Duration::ZERO, rx))),
            DidntYield(Err(Error::Expired))
        );

        // pending future, deadline is now -> no yield
        let (_tx, rx) = oneshot::channel::<i32>();
        let now = fiber::clock();
        assert_eq!(
            check_yield(|| fiber::block_on(deadline(now, rx))),
            DidntYield(Err(Error::Expired))
        );

        // pending future, deadline is past -> no yield
        let (_tx, rx) = oneshot::channel::<i32>();
        let one_second_ago = now.saturating_sub(Duration::from_secs(1));
        assert_eq!(
            check_yield(|| fiber::block_on(deadline(one_second_ago, rx))),
            DidntYield(Err(Error::Expired))
        );

        // pending future, positive timeout -> yield
        let (_tx, rx) = oneshot::channel::<i32>();
        assert_eq!(
            check_yield(|| fiber::block_on(timeout(Duration::from_millis(10), rx))),
            Yielded(Err(Error::Expired))
        );

        // pending future, deadline in future -> yield
        let (_tx, rx) = oneshot::channel::<i32>();
        let in_10_millis = fiber::clock().saturating_add(Duration::from_millis(10));
        assert_eq!(
            check_yield(|| fiber::block_on(deadline(in_10_millis, rx))),
            Yielded(Err(Error::Expired))
        );
    }

    #[crate::test(tarantool = "crate")]
    fn extra_check_works() {
        struct Mock {
            counter: std::rc::Rc<std::cell::Cell<usize>>,
        }

        impl Future for Mock {
            type Output = Result<(), String>;
            fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
                let value = self.counter.get();
                self.counter.set(value + 1);
                Poll::Pending
            }
        }

        let counter = std::rc::Rc::new(std::cell::Cell::new(0));

        let m = Mock { counter: std::rc::Rc::clone(&counter) };

        match fiber::block_on(timeout(_1_SEC, m).no_extra_check()) {
            Ok(_) => unreachable!("Cannot be ok cause future always pending"),
            Err(e) => {
                assert!(matches!(e, Error::Expired))
            }
        };

        assert_eq!(counter.get(), 1);

        let m = Mock { counter: std::rc::Rc::clone(&counter) };

        match fiber::block_on(timeout(_1_SEC, m)) {
            Ok(_) => unreachable!("Cannot be ok cause future always pending"),
            Err(e) => {
                assert!(matches!(e, Error::Expired))
            }
        };

        assert_eq!(counter.get(), 2);
    }
}
