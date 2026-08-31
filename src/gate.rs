//! Strict single-flight serialization: at most ONE upstream NVIDIA request
//! may exist at any instant, granted strictly in FIFO arrival order.
//!
//! This is the complete-response barrier from the gateway contract:
//! `start(N+1) >= complete(N)`. A [`Permit`] is held for the request's whole
//! upstream lifecycle — RPM wait, send, retries, and the full response
//! (buffered body read or SSE stream termination). The permit is the LAST
//! thing dropped in a holder's scope: declare it before any upstream
//! response object so Rust's reverse drop order releases the upstream
//! stream first and the permit after it.
//!
//! The gate is deliberately separate from the dispatcher/pool RPM machinery
//! and from the `max_inflight` flood cap: those bound rate and memory; this
//! bounds upstream concurrency to exactly one. Queued waiters wait — they
//! are never rejected (bounded only by the caller's own patience deadline).
//!
//! Waiters whose client hung up are detected by a closed reply channel and
//! skipped, exactly like the dispatcher's abandoned-waiter handling; the
//! permit lands on the next waiter instead of stalling the queue.

use std::time::Instant;

use metrics::{gauge, histogram};
use tokio::sync::{mpsc, oneshot};

/// The single-flight admission gate. Cloneable handle; the task that owns
/// the permit lives in the runtime that built it.
pub struct Gate {
    queue: mpsc::UnboundedSender<Waiter>,
}

struct Waiter {
    reply: oneshot::Sender<Permit>,
    deadline: Instant,
}

impl Clone for Gate {
    fn clone(&self) -> Self {
        Self {
            queue: self.queue.clone(),
        }
    }
}

impl Gate {
    /// Spawn the gate task. Exactly one permit circulates.
    pub fn new() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(run(rx));
        Self { queue: tx }
    }

    /// Join the FIFO queue. Resolves with the single-flight [`Permit`], or
    /// `None` when `deadline` passes first (the waiter is then skipped at
    /// handout time — an expired waiter never receives a permit). Dropping
    /// the returned future leaves the queue without disturbing anyone
    /// else's position; a slot granted to an abandoned waiter is skipped
    /// and the next waiter inherits it.
    pub async fn acquire(&self, deadline: Instant) -> Option<Permit> {
        let (reply, rx) = oneshot::channel();
        gauge!("nimproxy_gate_depth").increment(1.0);
        let queued = Instant::now();
        let _leave = crate::dispatch::scopeguard(|| {
            gauge!("nimproxy_gate_depth").decrement(1.0);
        });
        if self.queue.send(Waiter { reply, deadline }).is_err() {
            return None; // gate task gone: shutting down
        }
        // Race the wait against the caller's deadline so an over-long holder
        // can never pin a queued request past its patience bound; the gate
        // task also re-checks the deadline at handout and skips stale
        // waiters so they can't consume the permit.
        let granted = match tokio::time::timeout_at(deadline.into(), rx).await {
            Ok(Ok(permit)) => Some(permit),
            Ok(Err(_)) => None,    // gate task dropped our reply: shutdown
            Err(_elapsed) => None, // deadline passed while queued
        };
        if granted.is_some() {
            histogram!("nimproxy_gate_wait_seconds").record(queued.elapsed().as_secs_f64());
        }
        granted
    }
}

/// A held single-flight slot. Dropping it returns the permit to the queue
/// head, releasing the next request to start its upstream lifecycle.
///
/// The holder must guarantee the upstream operation has fully finished (or
/// been verifiably terminated) before dropping this.
pub struct Permit {
    ret: mpsc::UnboundedSender<()>,
}

impl Permit {
    /// Explicit release for holders whose upstream object outlives its
    /// binding order; equivalent to dropping the permit.
    pub fn release(self) {}
}

impl Drop for Permit {
    fn drop(&mut self) {
        // Send errors only when the gate task is gone (shutdown).
        let _ = self.ret.send(());
    }
}

async fn run(mut queue: mpsc::UnboundedReceiver<Waiter>) {
    let (ret_tx, mut ret_rx) = mpsc::unbounded_channel::<()>();
    // The permit starts in our hand; after every hand-out we wait for its
    // return before the next one. Waiters pile up in `queue` (FIFO) while
    // the current holder runs its full upstream lifecycle.
    let mut outstanding = false;
    loop {
        if outstanding {
            // The permit is out: block until its holder finishes.
            if ret_rx.recv().await.is_none() {
                return; // gate handle dropped along with every permit
            }
            gauge!("nimproxy_singleflight_active").set(0.0);
        }
        // Hand the permit to the head waiter, skipping anyone whose deadline
        // passed or whose client hung up while queued.
        loop {
            let Some(waiter) = queue.recv().await else {
                return; // all handles dropped; nothing left to serve
            };
            if waiter.deadline <= Instant::now() || waiter.reply.is_closed() {
                continue; // expired or abandoned: try the next waiter
            }
            outstanding = true;
            gauge!("nimproxy_singleflight_active").set(1.0);
            let _ = waiter.reply.send(Permit {
                ret: ret_tx.clone(),
            });
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn acquire(gate: &Gate) -> Permit {
        gate.acquire(Instant::now() + Duration::from_secs(30))
            .await
            .expect("permit")
    }

    #[tokio::test]
    async fn first_acquire_is_immediate() {
        let gate = Gate::new();
        let _p = acquire(&gate).await;
    }

    #[tokio::test]
    async fn permits_are_exclusive_until_released() {
        let gate = Gate::new();
        let a = acquire(&gate).await;
        // B cannot take the permit while A holds it...
        let deadline = Instant::now() + Duration::from_millis(150);
        assert!(gate.acquire(deadline).await.is_none(), "B must not pass");
        drop(a);
        // ...but can immediately after A releases.
        let _b = acquire(&gate).await;
    }

    #[tokio::test]
    async fn waiters_are_served_in_fifo_order() {
        let gate = Gate::new();
        let holder = acquire(&gate).await;
        // Three waiters join in a known order.
        let first = gate.acquire(Instant::now() + Duration::from_secs(30));
        let second = gate.acquire(Instant::now() + Duration::from_secs(30));
        let third = gate.acquire(Instant::now() + Duration::from_secs(30));
        // Give the tasks a moment to reach the send point.
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(holder);

        // 1st waiter's permit arrives immediately; the 2nd must still wait.
        let a = first.await.expect("first waiter served");
        let deadline = Instant::now() + Duration::from_millis(100);
        assert!(
            gate.acquire(deadline).await.is_none(),
            "no second permit while the first is held"
        );
        drop(a);

        // 2nd waiter is next — never overtaken by the 3rd.
        let b = second.await.expect("second waiter served");
        drop(b);

        // 3rd waiter finally inherits the permit.
        let _c = third.await.expect("third waiter served");
    }

    #[tokio::test]
    async fn expired_deadline_waiter_is_skipped_not_served() {
        let gate = Gate::new();
        let holder = acquire(&gate).await;
        // This waiter's deadline is already in the past.
        let stale_deadline = Instant::now() - Duration::from_secs(1);
        let stale = gate.acquire(stale_deadline);
        // This one still has time.
        let live = gate.acquire(Instant::now() + Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(holder);
        assert!(stale.await.is_none(), "expired waiter must not be served");
        let _live_permit = live.await.expect("live waiter inherits the permit");
    }

    #[tokio::test]
    async fn abandoned_waiter_does_not_block_the_queue() {
        let gate = Gate::new();
        let holder = acquire(&gate).await;
        // Waiter joins and immediately gives up (future dropped).
        {
            let _gave_up = gate.acquire(Instant::now() + Duration::from_secs(30));
            tokio::time::sleep(Duration::from_millis(20)).await;
            // dropped here
        }
        let next = gate.acquire(Instant::now() + Duration::from_secs(30));
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(holder);
        let _permit = next.await.expect("permit skips the abandoned waiter");
    }

    #[tokio::test]
    async fn release_method_is_equivalent_to_drop() {
        let gate = Gate::new();
        let a = acquire(&gate).await;
        a.release();
        let _b = acquire(&gate).await;
    }
}
