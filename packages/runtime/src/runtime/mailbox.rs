//! Where the finished async calls of one asyncio event loop wait for it.
//!
//! A Tokio worker that finishes a call posts its delivery to the loop's
//! [`Mailbox`], and wakes the loop only when the mailbox was empty, so that
//! the loop settles every call that finished meanwhile in one turn. On Unix,
//! the loop watches one end of a socket pair with `add_reader`, and the
//! worker writes a byte to the other end without the interpreter lock, so a
//! result reaches the loop with one wake-up and no thread waits for the lock
//! to deliver it. A loop without `add_reader`, such as Windows' proactor, is
//! woken by a blocking thread that takes the lock and calls
//! `call_soon_threadsafe`.
//!
//! A forked child never locks or drops a mailbox of the parent, whose lock
//! a worker of the parent may have held at the fork: it leaks it, and makes
//! its own for any loop that it runs.

use std::mem;
use std::sync::{Arc, Mutex};

use pyo3::intern;
use pyo3::prelude::*;

use crate::runtime::executor::{self, Process};

/// What a finished call does on its loop's thread, with the interpreter lock
/// held: it converts the result and settles the call's asyncio future.
pub(crate) type Delivery = Box<dyn for<'py> FnOnce(Python<'py>) + Send>;

/// The reason to expect when locking a mailbox's queue: only a push and a
/// swap of the vector run under the lock.
const UNPOISONED: &str = "no code panics while holding a mailbox's queue";

/// The finished calls of one event loop, and how to wake it.
pub(crate) struct Mailbox {
    queue: Mutex<Vec<Delivery>>,
    wake: Wake,
    process: Process,
}

/// How a worker wakes the loop of a mailbox.
enum Wake {
    /// A byte on a socket that the loop watches.
    #[cfg(unix)]
    Socket {
        reader: std::os::unix::net::UnixStream,
        writer: std::os::unix::net::UnixStream,
    },
    /// `call_soon_threadsafe`, from a blocking thread that takes the lock.
    Threadsafe { event_loop: Py<PyAny> },
}

/// A loop that has had a call wait, kept alive by its entry so that no other
/// loop takes its address, and its mailbox.
struct Entry {
    event_loop: Py<PyAny>,
    mailbox: Arc<Mailbox>,
}

/// The mailbox of each loop. Only code that holds the interpreter lock locks
/// it, and never while it calls Python, so a fork never copies it locked and
/// no other thread waits for it while holding the interpreter lock.
static MAILBOXES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// The reason to expect when locking [`MAILBOXES`].
const UNPOISONED_MAILBOXES: &str = "no code panics while holding the list of mailboxes";

impl Mailbox {
    /// Return the mailbox of `event_loop`, the running loop of this thread,
    /// and make it on the first call that waits there.
    ///
    /// # Errors
    ///
    /// Returns the error of Python when the loop cannot watch the mailbox.
    pub(crate) fn of(event_loop: &Bound<'_, PyAny>) -> PyResult<Arc<Self>> {
        let py = event_loop.py();
        let owners: Vec<Py<PyAny>> = {
            let mailboxes = MAILBOXES.lock().expect(UNPOISONED_MAILBOXES);
            let found = mailboxes.iter().find(|entry| {
                entry.event_loop.as_ptr() == event_loop.as_ptr()
                    && entry.mailbox.process.is_current()
            });
            if let Some(entry) = found {
                return Ok(Arc::clone(&entry.mailbox));
            }
            mailboxes
                .iter()
                .map(|entry| entry.event_loop.clone_ref(py))
                .collect()
        };
        let mailbox = Self::watched(event_loop)?;
        // A closed loop runs no more calls, so its entry goes.
        let mut closed = Vec::new();
        for owner in owners {
            if owner
                .bind(py)
                .call_method0(intern!(py, "is_closed"))?
                .is_truthy()?
            {
                closed.push(owner.as_ptr());
            }
        }
        let removed: Vec<Entry> = {
            let mut mailboxes = MAILBOXES.lock().expect(UNPOISONED_MAILBOXES);
            let (removed, kept) = mem::take(&mut *mailboxes).into_iter().partition(|entry| {
                !entry.mailbox.process.is_current() || closed.contains(&entry.event_loop.as_ptr())
            });
            *mailboxes = kept;
            mailboxes.push(Entry {
                event_loop: event_loop.clone().unbind(),
                mailbox: Arc::clone(&mailbox),
            });
            removed
        };
        // Dropping a loop may run Python code, so it happens past the lock.
        for entry in removed {
            if !entry.mailbox.process.is_current() {
                mem::forget(entry.mailbox);
            }
        }
        Ok(mailbox)
    }

    /// A new mailbox for `event_loop`, which watches its socket on Unix, or
    /// which `call_soon_threadsafe` wakes when the loop has no readers.
    fn watched(event_loop: &Bound<'_, PyAny>) -> PyResult<Arc<Self>> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::net::UnixStream;

            use pyo3::exceptions::PyNotImplementedError;

            let py = event_loop.py();
            let (reader, writer) = UnixStream::pair()?;
            reader.set_nonblocking(true)?;
            writer.set_nonblocking(true)?;
            let descriptor = reader.as_raw_fd();
            let mailbox = Arc::new(Self::new(Wake::Socket { reader, writer }));
            let drain = Py::new(py, Drain(Arc::clone(&mailbox)))?;
            match event_loop.call_method1(intern!(py, "add_reader"), (descriptor, drain)) {
                Ok(_) => return Ok(mailbox),
                Err(error) if error.is_instance_of::<PyNotImplementedError>(py) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(Arc::new(Self::new(Wake::Threadsafe {
            event_loop: event_loop.clone().unbind(),
        })))
    }

    fn new(wake: Wake) -> Self {
        Self {
            queue: Mutex::new(Vec::new()),
            wake,
            process: Process::current(),
        }
    }

    /// Post a finished call, from any thread, and wake the loop unless a call
    /// posted since the loop last drained the mailbox already did.
    pub(crate) fn post(self: &Arc<Self>, delivery: Delivery) {
        let first = {
            let mut queue = self.queue.lock().expect(UNPOISONED);
            queue.push(delivery);
            queue.len() == 1
        };
        if first {
            self.wake();
        }
    }

    /// Wake the loop, which then drains the mailbox.
    fn wake(self: &Arc<Self>) {
        match &self.wake {
            #[cfg(unix)]
            Wake::Socket { writer, .. } => {
                use std::io::Write;

                // A full socket wakes the loop already, and the socket of a
                // closed loop has no reader, whose calls nobody awaits.
                let _ = (&*writer).write(&[0]);
            }
            Wake::Threadsafe { .. } => {
                let mut mailbox = Some(Arc::clone(self));
                drop(executor::spawn_blocking(move || {
                    let attached = Python::try_attach(|py| {
                        if let Some(mailbox) = mailbox.take() {
                            mailbox.schedule(py);
                        }
                    });
                    // An interpreter that is shutting down resumes no one, and
                    // the mailbox's Python objects cannot drop without it.
                    if attached.is_none() {
                        mem::forget(mailbox);
                    }
                }));
            }
        }
    }

    /// Ask the loop of a [`Wake::Threadsafe`] mailbox to drain it.
    fn schedule(self: Arc<Self>, py: Python<'_>) {
        let event_loop = match &self.wake {
            Wake::Threadsafe { event_loop } => event_loop.clone_ref(py),
            #[cfg(unix)]
            Wake::Socket { .. } => return,
        };
        // Scheduling fails on a closed loop, or when Python is out of memory, and
        // either way the calls stay in the mailbox.
        let _ = Py::new(py, Drain(self)).and_then(|drain| {
            event_loop
                .bind(py)
                .call_method1(intern!(py, "call_soon_threadsafe"), (drain,))
        });
    }

    /// Settle every call in the mailbox, on its loop's thread.
    fn drain(&self, py: Python<'_>) {
        if !self.process.is_current() {
            return;
        }
        // The bytes go first, so that a call posted after the swap below,
        // which finds the queue empty, wakes the loop again.
        #[cfg(unix)]
        if let Wake::Socket { reader, .. } = &self.wake {
            use std::io::Read;

            let mut bytes = [0; 64];
            while matches!((&*reader).read(&mut bytes), Ok(read) if read > 0) {}
        }
        let deliveries = mem::take(&mut *self.queue.lock().expect(UNPOISONED));
        for delivery in deliveries {
            delivery(py);
        }
    }
}

/// The callback that drains a mailbox, which its loop calls when the
/// mailbox's socket has bytes, or when a blocking thread schedules it.
#[pyclass(frozen, module = "bimetal", skip_from_py_object)]
struct Drain(Arc<Mailbox>);

#[pymethods]
impl Drain {
    fn __call__(&self, py: Python<'_>) {
        self.0.drain(py);
    }
}
