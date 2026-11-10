//! The Rust value behind one generated resource object.

use std::future::Future;
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

use crate::ir::DefinitionId;

/// The Rust value behind one generated resource object.
///
/// Every call shares the value through an [`Arc`], so an async call keeps it
/// alive after the host closes the resource. The read-write lock lets `&self`
/// methods run together, while a `&mut self` method, including an async one
/// that is waiting, runs alone. An async call holds the lock until its result
/// has converted.
///
/// Cloning shares the slot, so that a clone sees a close.
#[doc(hidden)]
pub struct Resource<T> {
    value: Arc<Mutex<Option<Arc<RwLock<T>>>>>,
}

impl<T> Clone for Resource<T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
        }
    }
}

/// A Rust type that `#[bimetal::export]` turned into a resource.
#[doc(hidden)]
pub trait ResourceType {
    /// Return the identity of the resource's declaration.
    fn identity() -> DefinitionId;
}

/// The reason to expect when locking a slot. Only `Option::clone` and
/// `Option::take` run under the lock, and neither can panic, so the lock is
/// never poisoned.
const UNPOISONED: &str = "no code panics while holding a resource slot";

impl<T> Resource<T> {
    /// Wrap a newly constructed value.
    #[must_use]
    pub fn new(value: T) -> Self {
        Self {
            value: Arc::new(Mutex::new(Some(Arc::new(RwLock::new(value))))),
        }
    }

    /// Share the value with one call of `callable`, the method as the host's
    /// callers write it, such as `Counter.add`, which the error names.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::Closed`] after [`Resource::close`].
    ///
    /// # Panics
    ///
    /// Never: the slot's lock guards only code that cannot panic.
    pub fn get(&self, callable: &'static str) -> Result<Arc<RwLock<T>>, ResourceError> {
        self.value
            .lock()
            .expect(UNPOISONED)
            .clone()
            .ok_or(ResourceError::Closed(callable))
    }

    /// Share the value with one async call of `callable`, as [`Resource::get`]
    /// does, and take its read lock now if it is free, so the call holds the
    /// value from when the host makes it. Otherwise the future waits for it.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::Closed`] after [`Resource::close`].
    pub fn read_owned(
        &self,
        callable: &'static str,
    ) -> Result<impl Future<Output = OwnedRwLockReadGuard<T>> + use<T>, ResourceError> {
        let value = self.get(callable)?;
        let now = Arc::clone(&value).try_read_owned();
        Ok(async move {
            match now {
                Ok(guard) => guard,
                Err(_) => value.read_owned().await,
            }
        })
    }

    /// Hold the value alone for one async call of `callable`, taking its
    /// write lock as [`Resource::read_owned`] takes the read lock.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::Closed`] after [`Resource::close`].
    pub fn write_owned(
        &self,
        callable: &'static str,
    ) -> Result<impl Future<Output = OwnedRwLockWriteGuard<T>> + use<T>, ResourceError> {
        let value = self.get(callable)?;
        let now = Arc::clone(&value).try_write_owned();
        Ok(async move {
            match now {
                Ok(guard) => guard,
                Err(_) => value.write_owned().await,
            }
        })
    }

    /// Close the resource and move its value out, for one call of `callable`
    /// that takes it. A call that is still running keeps the resource open.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::Closed`] after [`Resource::close`], and
    /// [`ResourceError::Busy`] while another call holds the value.
    ///
    /// # Panics
    ///
    /// Never: the slot's lock guards only code that cannot panic.
    pub fn take(&self, callable: &'static str) -> Result<T, ResourceError> {
        let mut slot = self.value.lock().expect(UNPOISONED);
        let value = slot.take().ok_or(ResourceError::Closed(callable))?;
        match Arc::try_unwrap(value) {
            Ok(value) => Ok(value.into_inner()),
            Err(value) => {
                *slot = Some(value);
                Err(ResourceError::Busy(callable))
            }
        }
    }

    /// Release this handle. Calls that are still running keep the value, which
    /// drops when the last of them finishes.
    ///
    /// # Panics
    ///
    /// Never: the slot's lock guards only code that cannot panic.
    pub fn close(&self) {
        self.value.lock().expect(UNPOISONED).take();
    }

    /// Return whether the resource was closed.
    ///
    /// # Panics
    ///
    /// Never: the slot's lock guards only code that cannot panic.
    #[must_use]
    pub fn closed(&self) -> bool {
        self.value.lock().expect(UNPOISONED).is_none()
    }
}

/// The resources that one call has reached so far. A call that reaches one
/// twice could wait for itself: on a second borrow when either needs the value
/// alone, and on a second read when a writer queues between the two, because a
/// writer goes first.
#[doc(hidden)]
#[derive(Default)]
pub struct ReachedResources(Vec<usize>);

impl ReachedResources {
    /// Note that the call named `callable` reached `resource`.
    ///
    /// # Errors
    ///
    /// Returns [`ResourceError::Aliased`] when it reached it already.
    pub fn add<T>(
        &mut self,
        resource: &Resource<T>,
        callable: &'static str,
    ) -> Result<(), ResourceError> {
        let address = Arc::as_ptr(&resource.value).addr();
        if self.0.contains(&address) {
            return Err(ResourceError::Aliased(callable));
        }
        self.0.push(address);
        Ok(())
    }
}

/// Why a call of a resource method could not reach the Rust value. Each
/// variant names the method as the host's callers write it, such as
/// `Counter.add`.
#[doc(hidden)]
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ResourceError {
    /// The host closed the resource.
    #[error("{0}(): the resource is closed")]
    Closed(&'static str),
    /// A synchronous call found the value locked by another call, and it cannot
    /// wait: JavaScript has one thread, and inside a host function the other
    /// call may be the one that called it. A call that takes the value reports
    /// this when any other call holds it.
    #[error("{0}(): the resource is busy with another call")]
    Busy(&'static str),
    /// A call received one resource twice, and could wait for itself.
    #[error("{0}(): one resource cannot be passed twice to the same call")]
    Aliased(&'static str),
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Waker};

    use super::*;

    #[test]
    fn async_calls_take_the_lock_when_they_are_made() {
        let resource = Resource::new(0_u8);
        let value = resource.get("Counter.peek").expect("an open resource");

        let writing = resource
            .write_owned("Counter.add")
            .expect("an open resource");
        assert!(
            value.try_read().is_err(),
            "the call holds the value before its future runs"
        );
        let mut reading = pin!(
            resource
                .read_owned("Counter.peek")
                .expect("an open resource")
        );
        let mut context = Context::from_waker(Waker::noop());
        assert!(reading.as_mut().poll(&mut context).is_pending());

        drop(writing);
        assert!(reading.as_mut().poll(&mut context).is_ready());
        let shared = resource
            .read_owned("Counter.peek")
            .expect("an open resource");
        assert!(value.try_write().is_err(), "readers share the value");
        drop(shared);

        resource.close();
        assert!(matches!(
            resource.write_owned("Counter.add"),
            Err(ResourceError::Closed("Counter.add"))
        ));
    }

    #[test]
    fn taking_a_value_closes_the_resource_unless_a_call_holds_it() {
        let resource = Resource::new(7_u8);
        let held = resource.get("Counter.peek").expect("an open resource");

        assert_eq!(
            resource.take("Counter.finish"),
            Err(ResourceError::Busy("Counter.finish"))
        );
        assert!(!resource.closed(), "a refused take leaves it open");

        drop(held);
        assert_eq!(resource.take("Counter.finish"), Ok(7));
        assert!(resource.closed());
        assert_eq!(
            resource.take("Counter.finish"),
            Err(ResourceError::Closed("Counter.finish"))
        );
    }

    #[test]
    fn a_clone_is_the_same_resource() {
        let resource = Resource::new(0_u8);
        let other = Resource::new(0_u8);
        let clone = resource.clone();

        let mut reached = ReachedResources::default();
        assert_eq!(reached.add(&resource, "Counter.add"), Ok(()));
        assert_eq!(reached.add(&other, "Counter.add"), Ok(()));
        assert_eq!(
            reached.add(&clone, "Counter.add"),
            Err(ResourceError::Aliased("Counter.add"))
        );
        clone.close();
        assert!(resource.closed());
    }

    #[test]
    fn errors_name_the_method_that_was_called() {
        let resource = Resource::new(0_u8);
        resource.close();

        let closed = resource
            .get("Counter.add")
            .expect_err("a closed resource has no value");
        assert_eq!(closed.to_string(), "Counter.add(): the resource is closed");
        assert_eq!(
            ResourceError::Busy("Counter.add").to_string(),
            "Counter.add(): the resource is busy with another call"
        );
    }
}
