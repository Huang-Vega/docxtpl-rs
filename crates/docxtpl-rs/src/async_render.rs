use std::future::Future;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use serde_json::Value;

use crate::{
    DocxTemplate, Error, RenderContext, RenderControl, RenderControlError, RenderOptions,
    RenderedDocument,
};

/// A boxed blocking job accepted by [`BlockingExecutor`].
pub type BlockingTask = Box<dyn FnOnce() + Send + 'static>;

/// Runtime adapter used by [`AsyncRenderDispatcher`].
///
/// Implementations normally forward `task` to a runtime's blocking executor.
/// Returning `Err` means the task was rejected and will never run. The executor
/// must eventually run every task for which it returned `Ok(())`.
pub trait BlockingExecutor: Send + Sync + 'static {
    fn execute(&self, task: BlockingTask) -> Result<(), AsyncDispatchError>;
}

impl<F> BlockingExecutor for F
where
    F: Fn(BlockingTask) -> Result<(), AsyncDispatchError> + Send + Sync + 'static,
{
    fn execute(&self, task: BlockingTask) -> Result<(), AsyncDispatchError> {
        self(task)
    }
}

/// Error returned before a blocking render job is accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AsyncDispatchError {
    #[error("async render concurrency must be greater than zero")]
    InvalidConcurrency,
    #[error("async render queue is full ({max_in_flight} jobs in flight)")]
    QueueFull { max_in_flight: usize },
    #[error("blocking executor rejected the job: {message}")]
    ExecutorRejected { message: String },
}

impl AsyncDispatchError {
    /// Construct an executor-specific rejection without exposing a runtime
    /// error type in the core crate's public API.
    #[must_use]
    pub fn executor_rejected(message: impl Into<String>) -> Self {
        Self::ExecutorRejected {
            message: message.into(),
        }
    }
}

struct TaskState<T> {
    result: Option<Result<T, RenderControlError>>,
    waker: Option<Waker>,
}

struct SharedTask<T> {
    state: Mutex<TaskState<T>>,
}

impl<T> SharedTask<T> {
    fn pending() -> Self {
        Self {
            state: Mutex::new(TaskState {
                result: None,
                waker: None,
            }),
        }
    }

    fn complete(&self, result: Result<T, RenderControlError>) {
        let waker = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.result = Some(result);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Awaitable result of a job submitted through [`AsyncRenderDispatcher`].
///
/// Dropping the handle detaches the accepted blocking job. Call [`Self::cancel`]
/// before dropping it when the result is no longer needed.
pub struct RenderTask<T> {
    shared: Arc<SharedTask<T>>,
    control: RenderControl,
}

impl<T> RenderTask<T> {
    /// Request cooperative cancellation of this job.
    pub fn cancel(&self) {
        self.control.cancel();
    }

    /// Return the control shared with the underlying synchronous operation.
    #[must_use]
    pub fn control(&self) -> RenderControl {
        self.control.clone()
    }
}

impl<T> Future for RenderTask<T> {
    type Output = Result<T, RenderControlError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self
            .shared
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(result) = state.result.take() {
            Poll::Ready(result)
        } else {
            let replace = state
                .waker
                .as_ref()
                .is_none_or(|waker| !waker.will_wake(cx.waker()));
            if replace {
                state.waker = Some(cx.waker().clone());
            }
            Poll::Pending
        }
    }
}

struct InFlight {
    current: AtomicUsize,
    max: usize,
}

impl InFlight {
    fn try_acquire(self: &Arc<Self>) -> Result<InFlightPermit, AsyncDispatchError> {
        let mut current = self.current.load(Ordering::Acquire);
        loop {
            if current >= self.max {
                return Err(AsyncDispatchError::QueueFull {
                    max_in_flight: self.max,
                });
            }
            match self.current.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(InFlightPermit {
                        in_flight: Arc::clone(self),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

struct InFlightPermit {
    in_flight: Arc<InFlight>,
}

impl Drop for InFlightPermit {
    fn drop(&mut self) {
        self.in_flight.current.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Runtime-neutral bounded dispatcher for blocking DOCX work.
///
/// The dispatcher never creates threads. Its executor decides where jobs run,
/// while `max_in_flight` bounds accepted running plus queued jobs. This makes
/// overload visible as [`AsyncDispatchError::QueueFull`] instead of growing an
/// unbounded queue.
pub struct AsyncRenderDispatcher<E> {
    executor: Arc<E>,
    in_flight: Arc<InFlight>,
}

impl<E> Clone for AsyncRenderDispatcher<E> {
    fn clone(&self) -> Self {
        Self {
            executor: Arc::clone(&self.executor),
            in_flight: Arc::clone(&self.in_flight),
        }
    }
}

impl<E: BlockingExecutor> AsyncRenderDispatcher<E> {
    pub fn new(executor: E, max_in_flight: usize) -> Result<Self, AsyncDispatchError> {
        if max_in_flight == 0 {
            return Err(AsyncDispatchError::InvalidConcurrency);
        }
        Ok(Self {
            executor: Arc::new(executor),
            in_flight: Arc::new(InFlight {
                current: AtomicUsize::new(0),
                max: max_in_flight,
            }),
        })
    }

    #[must_use]
    pub fn max_in_flight(&self) -> usize {
        self.in_flight.max
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.in_flight.current.load(Ordering::Acquire)
    }

    /// Schedule a plain JSON render on the injected blocking executor.
    pub fn render(
        &self,
        template: Arc<DocxTemplate>,
        context: Value,
        options: RenderOptions,
        control: RenderControl,
    ) -> Result<RenderTask<RenderedDocument>, AsyncDispatchError> {
        let task_control = control.clone();
        self.submit(control, move || {
            template.render_with_control(&context, &options, &task_control)
        })
    }

    /// Schedule a rich-context render on the injected blocking executor.
    pub fn render_ctx(
        &self,
        template: Arc<DocxTemplate>,
        context: RenderContext,
        options: RenderOptions,
        control: RenderControl,
    ) -> Result<RenderTask<RenderedDocument>, AsyncDispatchError> {
        let task_control = control.clone();
        self.submit(control, move || {
            template.render_ctx_with_control(&context, &options, &task_control)
        })
    }

    /// Schedule controlled atomic file output on the injected blocking
    /// executor. ZIP compression remains in the synchronous implementation.
    pub fn save(
        &self,
        document: RenderedDocument,
        path: PathBuf,
        control: RenderControl,
    ) -> Result<RenderTask<()>, AsyncDispatchError> {
        let task_control = control.clone();
        self.submit(control, move || {
            document.save_with_control(path, &task_control)
        })
    }

    fn submit<T, F>(
        &self,
        control: RenderControl,
        operation: F,
    ) -> Result<RenderTask<T>, AsyncDispatchError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, RenderControlError> + Send + 'static,
    {
        let permit = self.in_flight.try_acquire()?;
        let shared = Arc::new(SharedTask::pending());
        let worker_shared = Arc::clone(&shared);
        let job = Box::new(move || {
            let _permit = permit;
            let result = catch_unwind(AssertUnwindSafe(operation)).unwrap_or_else(|_| {
                Err(RenderControlError::Operation(Error::Io(
                    std::io::Error::other("blocking render job panicked"),
                )))
            });
            worker_shared.complete(result);
        });
        self.executor.execute(job)?;
        Ok(RenderTask { shared, control })
    }
}
