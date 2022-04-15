//! We can use a single cond for communication. A channel can use 2 conds. One
//! for waking and one for waiting. A Sender and a Receiver would have those
//! swapped.
//!
//! On startup a proc will create an executor, which can spawn tasks in
//! sepparate fibers. Those tasks will use the same cond for waking up the
//! executor.
//!
//! The executor starts with a single future that it polls. It loops until the
//! future is ready. Each iteration it checks the future and then waits on the
//! cond with a timeout based on all of the timeouts (sleep, recv_timeout,
//! wait_timeout, etc.).
//!
//! This means that all of the waitable things must have a reference to the
//! executor. How does this work? (TODO)
//!
//! Channel recv future checks if the channel has a value and returns it. If it
//! doesn't, it sets the channel's waker (cond) that will be signalled when the
//! value is ready, waking up the executor.
//!
//! Receiver::recv_timeout:
//! - value is ready => `Poll::Ready(value)`
//! - value isn't ready =>
//!     + set the sender's waker (`&executor.cond`)
//!     + set the executor's timeout
use std::{
    rc::Rc,
    time::Duration,
};
use crate::fiber::Cond;

////////////////////////////////////////////////////////////////////////////////
/// Coord
////////////////////////////////////////////////////////////////////////////////

pub trait Coord {
    fn wait(&self);
    fn wake(&self);
    fn wait_timeout(&self, timeout: Duration) -> WaitTimeout;
}

pub enum WaitTimeout {
    Ok,
    TimedOut,
}

impl Coord for Rc<Cond> {
    fn wait(&self) {
        Cond::wait(self);
    }

    fn wait_timeout(&self, timeout: Duration) -> WaitTimeout {
        match Cond::wait_timeout(self, timeout) {
            true => WaitTimeout::Ok,
            false => WaitTimeout::TimedOut,
        }
    }

    fn wake(&self) {
        Cond::signal(self)
    }
}

////////////////////////////////////////////////////////////////////////////////
/// Timer
////////////////////////////////////////////////////////////////////////////////

pub struct Timer;

////////////////////////////////////////////////////////////////////////////////
/// Channel
////////////////////////////////////////////////////////////////////////////////

pub mod mpmc {
    pub mod fixed {
        use std::{
            cell::{Cell, UnsafeCell},
            future::Future,
            marker::PhantomData,
            mem::MaybeUninit,
            num::NonZeroUsize,
            pin::Pin,
            ptr::{drop_in_place, NonNull},
            rc::Rc,
            task::{Context, Poll, Waker},
        };
        use crate::fiber::Cond;
        use super::super::Coord;

        #[inline]
        pub fn channel<T, const N: usize>() -> (Sender<T, N, Rc<Cond>>, Receiver<T, N, Rc<Cond>>) {
            let chan_box = Box::new(ChannelBox::new(Rc::new(Cond::new())));
            // Box::into_raw returns a non-null pointer
            let raw = unsafe { NonNull::new_unchecked(Box::into_raw(chan_box)) };
            (Sender::new(raw), Receiver::new(raw))
        }

        ////////////////////////////////////////////////////////////////////////
        /// ChannelBox
        ////////////////////////////////////////////////////////////////////////

        pub struct ChannelBox<T, const N: usize, S: Coord> {
            data: UnsafeCell<MaybeUninit<[T; N]>>,
            /// First occupied spot. If `tail == head` the buffer is empty.
            tail: Cell<usize>,
            /// First empty spot. If `tail == head` the buffer is empty.
            head: Cell<usize>,
            rx_count: Cell<Option<NonZeroUsize>>,
            tx_count: Cell<Option<NonZeroUsize>>,
            coord: S,
        }

        impl<T, const N: usize, S: Coord> ChannelBox<T, N, S> {
            #[inline]
            fn new(coord: S) -> Self {
                Self {
                    data: UnsafeCell::new(MaybeUninit::uninit()),
                    tail: Cell::new(0),
                    head: Cell::new(0),
                    rx_count: Cell::new(None),
                    tx_count: Cell::new(None),
                    coord,
                }
            }

            #[inline] fn is_full(&self) -> bool { N - self.len() == 1 }
            /// Return number of occupied spots in the buffer.
            #[inline] fn len(&self) -> usize { self.head().wrapping_sub(self.tail()) % N }

            #[inline] fn is_empty(&self) -> bool { self.tail == self.head }
            #[inline] fn tail(&self) -> usize { self.tail.get() }
            #[inline] fn head(&self) -> usize { self.head.get() }

            /// # Safety
            /// `self.is_full()` must not be `true`, otherwise the data will
            #[inline]
            unsafe fn push_back(&self, v: T) {
                let head = self.head();
                self.head.set(head.wrapping_add(1) % N);
                std::ptr::write(self.ptr_to_ith(head), v);
            }

            unsafe fn ptr_to_ith(&self, i: usize) -> *mut T {
                self.data.get().cast::<T>().add(i)
            }

            #[inline]
            fn try_push_back(&self, v: T) -> Result<(), T> {
                if self.is_full() {
                    return Err(v)
                }

                unsafe { self.push_back(v) };

                Ok(())
            }

            /// # Safety
            /// `self.is_empty()` must not be `true`, otherwise undefined
            /// behavior
            #[inline]
            unsafe fn pop_front(&self) -> T {
                let tail = self.tail();
                self.tail.set(tail.wrapping_add(1) % N);
                std::ptr::read(self.ptr_to_ith(tail))
            }

            #[inline]
            fn try_pop_front(&self) -> Option<T> {
                if self.is_empty() {
                    return None
                }
                Some(unsafe { self.pop_front() })
            }

            #[inline]
            fn try_send(&self, v: T, coord: &impl Coord) -> Result<(), TrySendError<T>> {
                if self.rx().is_none() {
                    // Only a receiver can create another receiver so nobody
                    // will ever be able to receive this message
                    return Err(TrySendError::Disconnected(v))
                }

                let was_empty = self.is_empty();

                if let Err(v) = self.try_push_back(v) {
                    Err(TrySendError::Full(v))
                } else {
                    if was_empty {
                        coord.wake()
                    }

                    Ok(())
                }
            }

            #[inline]
            fn send(&self, v: T, coord: &impl Coord) -> Result<(), T> {
                if self.rx().is_none() {
                    // Only a receiver can create another receiver so nobody
                    // will ever be able to receive this message
                    return Err(v)
                }

                while self.is_full() {
                    coord.wait()
                }

                if self.rx().is_none() {
                    return Err(v)
                }

                let was_empty = self.is_empty();
                unsafe { self.push_back(v) }
                if was_empty {
                    coord.wake()
                }

                Ok(())
            }

            #[inline]
            fn try_recv(&self, coord: &impl Coord) -> Result<T, TryRecvError> {
                if self.tx().is_none() && self.is_empty() {
                    // Only a sender can create another sender so nobody
                    // will ever be able to send us a message
                    return Err(TryRecvError::Disconnected)
                }

                let was_full = self.is_full();

                if let Some(v) = self.try_pop_front() {
                    if was_full {
                        coord.wake()
                    }
                    Ok(v)
                } else {
                    Err(TryRecvError::Empty)
                }
            }

            #[inline]
            fn recv(&self, coord: &impl Coord) -> Option<T> {
                if self.tx().is_none() && self.is_empty() {
                    // Only a sender can create another sender so nobody
                    // will ever be able to send us a message
                    return None
                }

                while self.is_empty() {
                    coord.wait()
                }

                let was_full = self.is_full();
                let v = unsafe { self.pop_front() };
                if was_full {
                    coord.wake()
                }

                Some(v)
            }

            #[inline] fn no_refs(&self) -> bool { (self.rx(), self.tx()) == (None, None) }
            #[inline] fn inc_rx(&self) { Self::inc(&self.rx_count) }
            #[inline] fn inc_tx(&self) { Self::inc(&self.tx_count) }

            #[inline]
            fn inc(count: &Cell<Option<NonZeroUsize>>) {
                let new_count = unsafe {
                    NonZeroUsize::new_unchecked(
                        // ignoring possibility of overflow
                        1 + count.take().map(|c| c.get()).unwrap_or(0)
                    )
                };
                count.set(Some(new_count))
            }

            #[inline] fn dec_tx(&self) { Self::dec(&self.tx_count) }
            #[inline] fn dec_rx(&self) { Self::dec(&self.rx_count) }

            #[inline]
            fn dec(count: &Cell<Option<NonZeroUsize>>) {
                if let Some(c) = count.take() {
                    count.set(NonZeroUsize::new(c.get() - 1))
                } else {
                    panic!("decrement called on a zero reference count")
                }
            }

            #[inline] fn tx(&self) -> Option<NonZeroUsize> { self.tx_count.get() }
            #[inline] fn rx(&self) -> Option<NonZeroUsize> { self.rx_count.get() }
        }

        impl<T, const N: usize, S: Coord> Drop for ChannelBox<T, N, S> {
            fn drop(&mut self) {
                unsafe {
                    assert!(self.no_refs());
                    if self.tail() <= self.head() {
                        for i in self.tail()..self.head() {
                            drop_in_place(self.ptr_to_ith(i))
                        }
                    } else {
                        for i in 0..self.head() {
                            drop_in_place(self.ptr_to_ith(i))
                        }
                        for i in self.tail()..N {
                            drop_in_place(self.ptr_to_ith(i))
                        }
                    }
                }
            }
        }

        ////////////////////////////////////////////////////////////////////////
        /// Errors
        ////////////////////////////////////////////////////////////////////////

        pub enum TrySendError<T> {
            Disconnected(T),
            Full(T),
        }

        pub enum SendTimeoutError<T> {
            Disconnected(T),
            Timeout(T),
        }

        pub enum TryRecvError {
            Disconnected,
            Empty,
        }

        pub enum RecvTimeoutError {
            Disconnected,
            Timeout,
        }

        ////////////////////////////////////////////////////////////////////////
        /// Sender/Receiver
        ////////////////////////////////////////////////////////////////////////

        macro_rules! impl_channel_part {
            ($t:ident, $inc:ident, $dec:ident) => {
                pub struct $t<T, const N: usize, S: Coord> {
                    inner: NonNull<ChannelBox<T, N, S>>,
                    marker: PhantomData<ChannelBox<T, N, S>>,
                }

                impl<T, const N: usize, S: Coord> $t<T, N, S> {
                    #[inline]
                    fn new(inner: NonNull<ChannelBox<T, N, S>>) -> Self {
                        unsafe { inner.as_ref() }.$inc();
                        Self { inner, marker: PhantomData, }
                    }
                }

                impl<T, const N: usize, S: Coord> Clone for $t<T, N, S> {
                    #[inline]
                    fn clone(&self) -> Self {
                        Self::new(self.inner)
                    }
                }

                impl<T, const N: usize, S: Coord> Drop for $t<T, N, S> {
                    fn drop(&mut self) {
                        unsafe {
                            self.inner.as_ref().$dec();
                            if self.inner.as_ref().no_refs() {
                                drop(Box::from_raw(self.inner.as_ptr()))
                            }
                        }
                    }
                }
            }
        }

        impl_channel_part!{Sender, inc_tx, dec_tx}

        impl<T, const N: usize, S: Coord> Sender<T, N, S> {
            fn send(&self, v: T) -> Send<'_, T, N, S> {
                todo!()
            }
        }

        pub struct Send<'a, T, const N: usize, S: Coord> {
            sender: &'a Sender<T, N, S>,
        }

        impl<'a, T, const N: usize, S: Coord> Future for Send<'a, T, N, S> {
            type Output = Result<(), T>;

            fn poll(self: Pin<&mut Self>, ctx: &mut Context) -> Poll<Self::Output> {
                todo!()
            }
        }

        impl_channel_part!{Receiver, inc_rx, dec_rx}
    }
}
