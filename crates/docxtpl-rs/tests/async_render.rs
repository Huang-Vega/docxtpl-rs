mod test_support;

use std::collections::HashSet;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread;
use std::time::Duration;

use docxtpl_rs::{
    AsyncDispatchError, AsyncRenderDispatcher, BlockingExecutor, BlockingTask, DocxTemplate,
    RenderContext, RenderControl, RenderControlError, RenderOptions,
};
use serde_json::json;

const TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/r2_var_basic.docx"
);

#[derive(Clone, Copy)]
struct ThreadExecutor;

impl BlockingExecutor for ThreadExecutor {
    fn execute(&self, task: BlockingTask) -> Result<(), AsyncDispatchError> {
        thread::Builder::new()
            .name("docxtpl-test-worker".to_string())
            .spawn(task)
            .map(|_| ())
            .map_err(|error| AsyncDispatchError::executor_rejected(error.to_string()))
    }
}

#[derive(Clone, Default)]
struct ManualExecutor {
    queue: Arc<Mutex<VecDeque<BlockingTask>>>,
}

impl ManualExecutor {
    fn run_next(&self) {
        let task = self
            .queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
            .expect("queued task");
        task();
    }
}

impl BlockingExecutor for ManualExecutor {
    fn execute(&self, task: BlockingTask) -> Result<(), AsyncDispatchError> {
        self.queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(task);
        Ok(())
    }
}

struct ThreadWaker(thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

fn block_on<F: Future + Unpin>(mut future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(output) = Pin::new(&mut future).poll(&mut context) {
            return output;
        }
        thread::park_timeout(Duration::from_secs(5));
    }
}

#[test]
fn runtime_neutral_dispatcher_renders_and_saves() -> Result<(), Box<dyn std::error::Error>> {
    let dispatcher = AsyncRenderDispatcher::new(ThreadExecutor, 2)?;
    let render = dispatcher.render(
        Arc::new(DocxTemplate::open(TEMPLATE)?),
        json!({"name": "async"}),
        RenderOptions::compat(),
        RenderControl::new().with_timeout(Duration::from_secs(10)),
    )?;
    let document = block_on(render)?;

    let temporary = tempfile::Builder::new()
        .prefix("async-render-")
        .tempdir_in(test_support::target_dir())?;
    let output = temporary.path().join("output.docx");
    let save = dispatcher.save(
        document,
        output.clone(),
        RenderControl::new().with_timeout(Duration::from_secs(10)),
    )?;
    block_on(save)?;
    assert!(output.metadata()?.len() > 0);

    let mut rich_context = RenderContext::new();
    rich_context.insert("name", "rich async");
    let rich_render = dispatcher.render_ctx(
        Arc::new(DocxTemplate::open(TEMPLATE)?),
        rich_context,
        RenderOptions::compat(),
        RenderControl::new().with_timeout(Duration::from_secs(10)),
    )?;
    assert!(!block_on(rich_render)?.to_bytes()?.is_empty());
    assert_eq!(dispatcher.in_flight(), 0);
    Ok(())
}

#[test]
fn bounded_dispatcher_rejects_overload_and_releases_capacity(
) -> Result<(), Box<dyn std::error::Error>> {
    let executor = ManualExecutor::default();
    let dispatcher = AsyncRenderDispatcher::new(executor.clone(), 1)?;
    let template = Arc::new(DocxTemplate::open(TEMPLATE)?);
    let first = dispatcher.render(
        Arc::clone(&template),
        json!({"name": "first"}),
        RenderOptions::compat(),
        RenderControl::new(),
    )?;
    assert_eq!(dispatcher.in_flight(), 1);

    let second = dispatcher.render(
        template,
        json!({"name": "second"}),
        RenderOptions::compat(),
        RenderControl::new(),
    );
    assert!(matches!(
        second,
        Err(AsyncDispatchError::QueueFull { max_in_flight: 1 })
    ));

    executor.run_next();
    let document = block_on(first)?;
    assert!(!document.to_bytes()?.is_empty());
    assert_eq!(dispatcher.in_flight(), 0);
    Ok(())
}

#[test]
fn cancellation_and_deadline_propagate_while_job_is_queued(
) -> Result<(), Box<dyn std::error::Error>> {
    let executor = ManualExecutor::default();
    let dispatcher = AsyncRenderDispatcher::new(executor.clone(), 2)?;
    let template = Arc::new(DocxTemplate::open(TEMPLATE)?);

    let cancelled = dispatcher.render(
        Arc::clone(&template),
        json!({"name": "cancelled"}),
        RenderOptions::compat(),
        RenderControl::new(),
    )?;
    cancelled.cancel();
    executor.run_next();
    assert!(matches!(
        block_on(cancelled),
        Err(RenderControlError::Cancelled)
    ));

    let expired = dispatcher.render(
        template,
        json!({"name": "expired"}),
        RenderOptions::compat(),
        RenderControl::new().with_timeout(Duration::ZERO),
    )?;
    executor.run_next();
    assert!(matches!(
        block_on(expired),
        Err(RenderControlError::DeadlineExceeded)
    ));
    assert_eq!(dispatcher.in_flight(), 0);
    Ok(())
}

#[test]
fn invalid_concurrency_is_rejected() {
    assert!(matches!(
        AsyncRenderDispatcher::new(ThreadExecutor, 0),
        Err(AsyncDispatchError::InvalidConcurrency)
    ));
}

#[test]
fn concurrent_render_stress_keeps_capacity_and_outputs_independent(
) -> Result<(), Box<dyn std::error::Error>> {
    const CONCURRENCY: usize = 8;
    const WAVES: usize = 4;

    let dispatcher = AsyncRenderDispatcher::new(ThreadExecutor, CONCURRENCY)?;
    let template = Arc::new(DocxTemplate::open(TEMPLATE)?);
    for wave in 0..WAVES {
        let tasks = (0..CONCURRENCY)
            .map(|job| {
                dispatcher.render(
                    Arc::clone(&template),
                    json!({"name": format!("wave-{wave}-job-{job}")}),
                    RenderOptions::compat(),
                    RenderControl::new().with_timeout(Duration::from_secs(10)),
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        assert!(dispatcher.in_flight() <= CONCURRENCY);

        let outputs = tasks
            .into_iter()
            .map(|task| block_on(task).and_then(|document| document.to_bytes().map_err(Into::into)))
            .collect::<Result<Vec<_>, RenderControlError>>()?;
        assert_eq!(outputs.iter().collect::<HashSet<_>>().len(), CONCURRENCY);
        assert_eq!(dispatcher.in_flight(), 0);
    }
    Ok(())
}
