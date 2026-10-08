use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub use jobserver_crate::Acquired;
use jobserver_crate::{Client, FromEnv, FromEnvErrorKind, HelperThread};
use parking_lot::{Condvar, Mutex};

// We stick the jobserver client into a global and initialize it once, because there could be
// multiple compiler instances in this process, and the jobserver is per-process.
static CLIENT: OnceLock<Client> = OnceLock::new();

/// Initializes a jobserver client for the current rustc process.
/// If inheriting jobserver from the environment fails for some reason, an new jobserver owned by
/// the current rustc process will be created. If the inheritance failure reason is non-benign,
/// the passed callback will be used to report the error.
pub fn initialize(limit: usize, report: impl FnOnce(String)) {
    CLIENT.get_or_init(|| {
        // Safety: the checked client construction ensures that the jobserver file descriptors
        // (if any) are open and valid. We also try to initialize the jobserver as early as possible
        // to avoid unrelated file descriptors with matching values becoming open and valid between
        // the process start and the jobserver initialization.
        let FromEnv { client, var } = unsafe { Client::from_env_ext(true) };

        let error = match client {
            Ok(client) => return client,
            Err(error) => error,
        };

        if !matches!(
            error.kind(),
            FromEnvErrorKind::NoEnvVar
                | FromEnvErrorKind::NoJobserver
                | FromEnvErrorKind::NegativeFd
                | FromEnvErrorKind::Unsupported
        ) {
            // Environment specifies jobserver, but it looks incorrect.
            // Can unwrap because `var` is `None` only when the error kind is `NoEnvVar`.
            let (name, value) = var.unwrap();
            let msg = "failed to connect to jobserver from environment variable";
            report(format!("{msg} `{name}={value:?}`: {error}"));
        }

        // Create a new jobserver if there's no inherited one.
        let client = Client::new(limit).expect("failed to create jobserver");
        // Acquire the single token that is always held by the rustc process.
        // This is an equivalent of the single token held by a higher level build tool while
        // running this instance of rustc. This token is never released - if we are here, then
        // rustc owns the jobserver, it is teared down when rustc exits, and there's no one to
        // return the token to.
        client.acquire_raw().ok();
        client
    });
}

/// Returns the jobserver client previously initialized by `initialize_checked`.
///
/// # Assumptions about holding jobserver tokens
///
/// Rustc process must always hold a single token to avoid being permanently starved and blocked.
/// - If the jobserver is inherited from a higher level build tool, the assumption is that the tool
///   will hold the token and not release it until the rustc process exits.
/// - If the jobserver is owned by the current rustc, the token is acquired by `default_client`.
///
/// To avoid releasing the last token, users of the client returned by this function must ensure
/// that they never release more tokens than was previously explicitly acquired.
/// Example of a sequence that can accidentally release the last token:
/// `release_raw` -> `wait` -> `acquire_raw`.
/// To avoid situations like this use the `jobserver::Proxy` wrapper instead,
/// it will ensure that the last token is never released.
pub fn client() -> Client {
    CLIENT.get().expect("uninitialized jobserver client").clone()
}

struct ProxyData {
    /// The number of tokens assigned to actively working threads,
    /// possibly including the single permanently held token.
    /// If this number is 0, the single token is still held by the process,
    /// but is not currently used for active CPU work.
    /// This can happen, for example, if the main thread is waiting for something,
    /// in that case some other thread can start using this token to do work.
    used: u16,
    /// The number of threads currently waiting for a token that has not been granted yet.
    pending: u16,
    /// The number of tokens granted to waiting threads that they have not picked up yet.
    granted: u16,
    /// The number of token requests sent to the helper thread that it has not completed yet.
    requested: u16,
}

/// A wrapper around jobserver client used for two purposes:
/// - Ensuring that the single token that must be permanently held by the rustc process
///   cannot be accidentally released.
/// - "Token buffering", immediately acquiring freshly released tokens if necessary,
///   without going through the real jobserver.
///
/// Extra tokens (beyond the one the process always holds) are requested with low priority: a
/// request is only sent to the jobserver when it currently has spare tokens. Otherwise the thread
/// re-checks periodically (or takes over a token released by another thread of this process).
/// This keeps the extra threads of the parallel frontend from competing with the build tool for
/// tokens it needs to start other jobs (e.g. Cargo starting crates or build scripts compiling C
/// code, which are often on the critical path), while still using all idle cores otherwise.
pub struct Proxy {
    /// The wrapped jobserver client.
    client: Client,
    /// Helper thread associated with the wrapped client.
    helper: OnceLock<HelperThread>,
    /// The proxy's own data.
    data: Mutex<ProxyData>,
    /// Threads which are currently waiting for a token will wait on this condvar.
    wake_pending: Condvar,
}

/// How often a thread waiting for a token checks whether the jobserver has spare tokens.
const SPARE_TOKEN_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// How long a yielding thread waits before trying to re-acquire the token it gave back.
const YIELD_GRACE_PERIOD: Duration = Duration::from_millis(1);

impl Proxy {
    pub fn new() -> Arc<Self> {
        let proxy = Arc::new(Proxy {
            client: client(),
            // Assume that the main thread is actively doing work when it creates the proxy.
            data: Mutex::new(ProxyData { used: 1, pending: 0, granted: 0, requested: 0 }),
            wake_pending: Condvar::new(),
            helper: OnceLock::new(),
        });
        let proxy_ = Arc::clone(&proxy);
        let helper = proxy
            .client
            .clone()
            .into_helper_thread(move |token| {
                // Reminder: this callback runs when the helper acquires a token.
                let mut data = proxy_.data.lock();
                data.requested = data.requested.saturating_sub(1);
                if let Ok(token) = token {
                    if data.pending > 0 {
                        // The token is still needed, give it to one of the waiting threads.
                        token.drop_without_releasing();
                        assert!(data.used > 0);
                        data.used += 1;
                        data.pending -= 1;
                        data.granted += 1;
                        proxy_.wake_pending.notify_one();
                    } else {
                        // The token is no longer needed, release it by dropping.
                        drop(data);
                        drop(token);
                    }
                }
            })
            .expect("failed to spawn helper thread");
        proxy.helper.set(helper).unwrap();
        proxy
    }

    /// Whether the wrapped jobserver currently seems to have a spare token. If this cannot be
    /// determined, assume that it does (which falls back to requesting tokens eagerly).
    fn has_spare_token(&self) -> bool {
        self.client.available().map_or(true, |n| n > 0)
    }

    /// Acquires a token, possibly using some buffered tokens as an optimization.
    /// May block and wait until the token is available.
    pub fn acquire_thread(&self) {
        let mut data = self.data.lock();

        if data.used == 0 {
            // No threads are doing any active work, but we are still holding the last token.
            // Give that token to the current thread.
            assert_eq!(data.pending, 0);
            data.used += 1;
            return;
        }

        data.pending += 1;
        loop {
            if data.granted > 0 {
                // A token was given to us, either by the helper thread or by a thread of this
                // process that released its token.
                data.granted -= 1;
                return;
            }
            if data.requested < data.pending && self.has_spare_token() {
                // Request a token from the helper thread, this is a non-blocking operation.
                self.helper.get().unwrap().request_token();
                data.requested += 1;
            }
            if data.requested >= data.pending {
                // Enough requests are in flight, wait until this or some other request succeeds.
                self.wake_pending.wait(&mut data);
            } else {
                // The jobserver had no spare token, check again later (or get woken up when
                // another thread of this process releases its token).
                self.wake_pending.wait_for(&mut data, SPARE_TOKEN_POLL_INTERVAL);
            }
        }
    }

    /// Called periodically by busy threads holding a token. If this process holds extra tokens
    /// while the jobserver has none to spare, other processes (e.g. Cargo wanting to start a job, or
    /// a build script compiling C code that may be on the critical path) may be waiting for one.
    /// Then give this thread's token back to the jobserver for a moment and re-acquire it with low
    /// priority, so that a waiting process gets the token first. Never yields the last token of
    /// the process.
    pub fn yield_thread(&self) {
        let mut data = self.data.lock();
        if data.used <= 1 || data.pending > 0 || self.has_spare_token() {
            return;
        }
        data.used -= 1;
        drop(data);
        if self.client.release_raw().is_err() {
            self.data.lock().used += 1;
            return;
        }
        // Give a process blocked on the jobserver the chance to pick the token up first.
        std::thread::sleep(YIELD_GRACE_PERIOD);
        self.acquire_thread();
    }

    /// Releases a token, possibly immediately giving it to some other thread as an optimization.
    /// Makes sure that the last token is never actually released to the wrapped jobserver.
    pub fn release_thread(&self) {
        let mut data = self.data.lock();

        if data.pending > 0 {
            // Immediately give the released token to one of the waiting threads.
            data.pending -= 1;
            data.granted += 1;
            self.wake_pending.notify_one();
        } else {
            data.used -= 1;

            // Release the token to the wrapped jobserver, unless it's the last one in the process.
            if data.used > 0 {
                drop(data);
                self.client.release_raw().ok();
            }
        }
    }
}
