//! A first in, first out lock for the tasks of one server process (ARCHITECTURE 4.1, 5.4).
//!
//! The server runs its tasks on one thread (tokio `current_thread` with one `LocalSet`), so the
//! lock needs no atomics: it is a queue of waiting tasks in a [`RefCell`]. It owns the value it
//! protects, so the value can only be reached through a [`FifoGuard`].
//!
//! Waiters are served strictly in the order in which they first asked. When a guard is dropped
//! while tasks wait, the lock is handed to the first of them directly and is never free in
//! between, so a task that asks later can never overtake one that waits (D156: "FIFO, so every
//! writer queued earlier commits first"). A waiter that is dropped before it got the lock leaves
//! the queue; one that is dropped after the lock was handed to it passes it on.

use std::cell::{Cell, RefCell, RefMut};
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

/// A FIFO lock that owns a `T`.
pub struct FifoLock<T> {
    value: RefCell<T>,
    state: RefCell<State>,
    /// How often the lock was granted, for tests and metrics.
    grants: Cell<u64>,
}

#[derive(Default)]
struct State {
    /// True while a guard exists or the lock was handed to a waiter that has not taken it yet.
    held: bool,
    /// The waiter the lock was handed to and that has not polled since.
    handed_to: Option<u64>,
    next_ticket: u64,
    waiting: VecDeque<(u64, Option<Waker>)>,
}

impl<T> FifoLock<T> {
    /// A free lock around `value`.
    pub fn new(value: T) -> Self {
        FifoLock {
            value: RefCell::new(value),
            state: RefCell::new(State::default()),
            grants: Cell::new(0),
        }
    }

    /// Waits until this task holds the lock.
    pub fn lock(&self) -> Acquire<'_, T> {
        Acquire {
            lock: self,
            ticket: None,
            done: false,
        }
    }

    /// True while a task holds the lock or it was handed to a waiter.
    pub fn is_held(&self) -> bool {
        self.state.borrow().held
    }

    /// The number of tasks that wait for the lock.
    pub fn waiting(&self) -> usize {
        self.state.borrow().waiting.len()
    }

    /// How often the lock was granted since it was made.
    pub fn grants(&self) -> u64 {
        self.grants.get()
    }

    /// The protected value, once no task can use the lock any more.
    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// Ends the hold of the current holder: the first waiter gets the lock, or it is free.
    fn release(&self) {
        let mut state = self.state.borrow_mut();
        match state.waiting.pop_front() {
            Some((ticket, waker)) => {
                state.handed_to = Some(ticket);
                drop(state);
                if let Some(waker) = waker {
                    waker.wake();
                }
            }
            None => {
                state.held = false;
                state.handed_to = None;
            }
        }
    }

    fn guard(&self) -> FifoGuard<'_, T> {
        self.grants.set(self.grants.get().saturating_add(1));
        FifoGuard {
            lock: self,
            // The state says this task holds the lock, so no other borrow of the value exists:
            // every earlier guard released its borrow before it released the lock.
            value: Some(self.value.borrow_mut()),
        }
    }
}

impl<T> fmt::Debug for FifoLock<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FifoLock")
            .field("held", &self.is_held())
            .field("waiting", &self.waiting())
            .finish_non_exhaustive()
    }
}

/// The future of [`FifoLock::lock`].
pub struct Acquire<'a, T> {
    lock: &'a FifoLock<T>,
    ticket: Option<u64>,
    done: bool,
}

impl<'a, T> Future for Acquire<'a, T> {
    type Output = FifoGuard<'a, T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let lock = this.lock;
        let mut state = lock.state.borrow_mut();
        match this.ticket {
            None => {
                if !state.held && state.waiting.is_empty() {
                    state.held = true;
                    drop(state);
                    this.done = true;
                    return Poll::Ready(lock.guard());
                }
                let ticket = state.next_ticket;
                state.next_ticket = state.next_ticket.wrapping_add(1);
                state.waiting.push_back((ticket, Some(cx.waker().clone())));
                this.ticket = Some(ticket);
                Poll::Pending
            }
            Some(ticket) => {
                if state.handed_to == Some(ticket) {
                    state.handed_to = None;
                    drop(state);
                    this.done = true;
                    return Poll::Ready(lock.guard());
                }
                if let Some(entry) = state.waiting.iter_mut().find(|(t, _)| *t == ticket) {
                    entry.1 = Some(cx.waker().clone());
                }
                Poll::Pending
            }
        }
    }
}

impl<T> Drop for Acquire<'_, T> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let Some(ticket) = self.ticket else {
            return;
        };
        let mut state = self.lock.state.borrow_mut();
        if state.handed_to == Some(ticket) {
            // The lock was handed to this waiter, which gave up before it took it.
            drop(state);
            self.lock.release();
        } else {
            state.waiting.retain(|(t, _)| *t != ticket);
        }
    }
}

/// Access to the value of a [`FifoLock`]; dropping it releases the lock.
pub struct FifoGuard<'a, T> {
    lock: &'a FifoLock<T>,
    value: Option<RefMut<'a, T>>,
}

impl<T> Deref for FifoGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        match &self.value {
            Some(value) => value,
            None => unreachable!("the value is only taken in drop"),
        }
    }
}

impl<T> DerefMut for FifoGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        match &mut self.value {
            Some(value) => value,
            None => unreachable!("the value is only taken in drop"),
        }
    }
}

impl<T> Drop for FifoGuard<'_, T> {
    fn drop(&mut self) {
        // Release the borrow of the value before the next holder can take it.
        drop(self.value.take());
        self.lock.release();
    }
}

impl<T> fmt::Debug for FifoGuard<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FifoGuard").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::pin;
    use std::task::Waker;

    fn poll<F: Future>(fut: Pin<&mut F>) -> Poll<F::Output> {
        fut.poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn a_free_lock_is_taken_at_once_and_freed_by_the_guard() {
        let lock = FifoLock::new(5);
        {
            let mut guard = match poll(pin!(lock.lock())) {
                Poll::Ready(g) => g,
                Poll::Pending => unreachable!("the lock is free"),
            };
            *guard += 1;
            assert!(lock.is_held());
        }
        assert!(!lock.is_held());
        assert_eq!(lock.grants(), 1);
        assert_eq!(lock.into_inner(), 6);
    }

    #[test]
    fn waiters_are_served_in_the_order_they_asked() {
        let lock = FifoLock::new(Vec::new());
        let first = match poll(pin!(lock.lock())) {
            Poll::Ready(g) => g,
            Poll::Pending => unreachable!("the lock is free"),
        };
        let mut second = pin!(lock.lock());
        let mut third = pin!(lock.lock());
        assert!(poll(second.as_mut()).is_pending());
        assert!(poll(third.as_mut()).is_pending());
        assert_eq!(lock.waiting(), 2);
        drop(first);
        // Handed to the second waiter: the lock is never free in between, and the third one
        // cannot take it although it is polled first.
        assert!(lock.is_held());
        assert!(poll(third.as_mut()).is_pending());
        let mut g2 = match poll(second.as_mut()) {
            Poll::Ready(g) => g,
            Poll::Pending => unreachable!("handed to the second waiter"),
        };
        g2.push(2);
        drop(g2);
        let mut g3 = match poll(third.as_mut()) {
            Poll::Ready(g) => g,
            Poll::Pending => unreachable!("handed to the third waiter"),
        };
        g3.push(3);
        drop(g3);
        assert!(!lock.is_held());
        let seen = match poll(pin!(lock.lock())) {
            Poll::Ready(g) => g.clone(),
            Poll::Pending => unreachable!("the lock is free"),
        };
        assert_eq!(seen, [2, 3]);
    }

    #[test]
    fn a_new_task_does_not_overtake_a_waiter() {
        let lock = FifoLock::new(());
        let first = match poll(pin!(lock.lock())) {
            Poll::Ready(g) => g,
            Poll::Pending => unreachable!("the lock is free"),
        };
        let mut waiter = pin!(lock.lock());
        assert!(poll(waiter.as_mut()).is_pending());
        drop(first);
        let mut late = pin!(lock.lock());
        assert!(poll(late.as_mut()).is_pending());
        assert!(poll(waiter.as_mut()).is_ready());
    }

    #[test]
    fn a_waiter_that_gives_up_leaves_the_queue() {
        let lock = FifoLock::new(());
        let first = match poll(pin!(lock.lock())) {
            Poll::Ready(g) => g,
            Poll::Pending => unreachable!("the lock is free"),
        };
        {
            let mut gone = pin!(lock.lock());
            assert!(poll(gone.as_mut()).is_pending());
            assert_eq!(lock.waiting(), 1);
        }
        assert_eq!(lock.waiting(), 0);
        drop(first);
        assert!(!lock.is_held());
    }

    #[test]
    fn a_handed_lock_dropped_unpolled_goes_to_the_next_waiter() {
        let lock = FifoLock::new(());
        let first = match poll(pin!(lock.lock())) {
            Poll::Ready(g) => g,
            Poll::Pending => unreachable!("the lock is free"),
        };
        let mut last = pin!(lock.lock());
        {
            let mut handed = pin!(lock.lock());
            assert!(poll(handed.as_mut()).is_pending());
            // The queue follows the first poll, so `handed` waits before `last`.
            assert!(poll(last.as_mut()).is_pending());
            drop(first);
            // The lock is now handed to `handed`, which is dropped without taking it.
        }
        assert!(lock.is_held());
        assert!(poll(last.as_mut()).is_ready());
        assert!(!lock.is_held());
    }
}
