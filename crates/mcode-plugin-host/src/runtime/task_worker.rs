//! Serialized FeaturePack owner loop and non-blocking operation cleanup.
use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::{Notify, mpsc, oneshot};
use tokio::time::{Instant, timeout_at};

use super::ResourcePermit;
use super::admission::OperationPermit;

const COMMAND_CAPACITY: usize = super::MAX_OPEN_OPERATIONS + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct TaskActorOperationId(u64);

pub(crate) struct TaskActorClient<A: PackTaskActor> {
    sender: mpsc::Sender<Command<A>>,
    worker: tokio::task::AbortHandle,
    live: Arc<Mutex<HashMap<TaskActorOperationId, TaskCloseSignal>>>,
}

enum Command<A: PackTaskActor> {
    Invoke {
        invocation: InvokeCommand<A::Request>,
        reply: oneshot::Sender<Result<TaskActorOperationId, TaskActorError<A::Error>>>,
    },
    Pull {
        operation: TaskActorOperationId,
        reply: oneshot::Sender<Result<A::Pull, TaskActorError<A::Error>>>,
    },
    Close(TaskActorOperationId),
}

struct InvokeCommand<R> {
    request: R,
    deadline: Instant,
    close: TaskCloseSignal,
}

struct WorkerOperation<O> {
    deadline: Instant,
    operation: O,
    close: TaskCloseSignal,
}

#[derive(Clone)]
pub(crate) struct TaskCloseSignal {
    state: Arc<TaskCloseState>,
}

struct TaskCloseState {
    closed: AtomicBool,
    notification: Notify,
    admission: Mutex<Option<TaskOperationAdmission>>,
}

pub(crate) struct TaskOperationAdmission {
    _operation: OperationPermit,
    _resource: ResourcePermit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TaskActorError<E> {
    UnknownOperation,
    Pack(E),
    Unavailable,
}

pub(crate) trait PackTaskActor: Send + 'static {
    type Request: Send + 'static;
    type Operation: Send + 'static;
    type Pull: Send + 'static;
    type Error: Copy + Eq + Send + 'static;

    fn is_available(&self) -> bool;
    fn is_fatal(error: Self::Error) -> bool;
    fn invoke(
        &mut self,
        request: &Self::Request,
    ) -> impl Future<Output = Result<Self::Operation, Self::Error>> + Send;
    fn pull(
        &mut self,
        operation: &mut Self::Operation,
    ) -> impl Future<Output = Result<Self::Pull, Self::Error>> + Send;
    fn drop_operation(
        &mut self,
        operation: Self::Operation,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn take_admission(operation: &mut Self::Operation) -> Option<TaskOperationAdmission>;
}

impl<A: PackTaskActor> TaskActorClient<A> {
    pub(crate) fn start(actor: A) -> Self {
        let (sender, receiver) = mpsc::channel(COMMAND_CAPACITY);
        let live = Arc::new(Mutex::new(HashMap::new()));
        let worker = tokio::spawn(run_worker(
            actor,
            receiver,
            sender.clone(),
            Arc::clone(&live),
        ))
        .abort_handle();
        Self {
            sender,
            worker,
            live,
        }
    }

    #[expect(dead_code, reason = "T10+ activation probes actor availability")]
    pub(crate) fn is_available(&self) -> bool {
        !self.sender.is_closed()
    }

    #[expect(dead_code, reason = "T10+ activation probes live operations")]
    pub(crate) fn is_operation_open(&self, operation: TaskActorOperationId) -> bool {
        self.live
            .lock()
            .expect("task actor live-operation lock")
            .get(&operation)
            .is_some_and(|close| !close.is_closed())
    }

    pub(crate) async fn invoke(
        &self,
        request: A::Request,
        deadline: Instant,
        close: TaskCloseSignal,
    ) -> Result<TaskActorOperationId, TaskActorError<A::Error>> {
        let (reply, response) = oneshot::channel();
        timeout_at(
            deadline,
            self.sender.send(Command::Invoke {
                invocation: InvokeCommand {
                    request,
                    deadline,
                    close,
                },
                reply,
            }),
        )
        .await
        .map_err(|_| TaskActorError::Unavailable)?
        .map_err(|_| TaskActorError::Unavailable)?;
        timeout_at(deadline, response)
            .await
            .map_err(|_| TaskActorError::Unavailable)?
            .map_err(|_| TaskActorError::Unavailable)?
    }

    pub(crate) async fn pull(
        &self,
        operation: TaskActorOperationId,
        deadline: Instant,
    ) -> Result<A::Pull, TaskActorError<A::Error>> {
        let (reply, response) = oneshot::channel();
        timeout_at(
            deadline,
            self.sender.send(Command::Pull { operation, reply }),
        )
        .await
        .map_err(|_| TaskActorError::Unavailable)?
        .map_err(|_| TaskActorError::Unavailable)?;
        timeout_at(deadline, response)
            .await
            .map_err(|_| TaskActorError::Unavailable)?
            .map_err(|_| TaskActorError::Unavailable)?
    }

    pub(crate) fn close(&self, operation: TaskActorOperationId) {
        if let Some(close) = self
            .live
            .lock()
            .expect("task actor live-operation lock")
            .get(&operation)
            .cloned()
        {
            close.close();
        }
    }
}

impl TaskCloseSignal {
    pub(crate) fn new() -> Self {
        Self {
            state: Arc::new(TaskCloseState {
                closed: AtomicBool::new(false),
                notification: Notify::new(),
                admission: Mutex::new(None),
            }),
        }
    }

    pub(crate) fn close(&self) -> bool {
        if self.state.closed.swap(true, Ordering::AcqRel) {
            return false;
        }
        drop(
            self.state
                .admission
                .lock()
                .expect("task close admission lock")
                .take(),
        );
        self.state.notification.notify_waiters();
        true
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.state.closed.load(Ordering::Acquire)
    }

    pub(crate) async fn closed(&self) {
        if self.is_closed() {
            return;
        }
        let notification = self.state.notification.notified();
        tokio::pin!(notification);
        notification.as_mut().enable();
        if self.is_closed() {
            return;
        }
        notification.await;
    }

    fn bind_admission(&self, admission: TaskOperationAdmission) {
        let mut slot = self
            .state
            .admission
            .lock()
            .expect("task close admission lock");
        if self.is_closed() {
            drop(admission);
        } else {
            debug_assert!(slot.is_none());
            *slot = Some(admission);
        }
    }
}

impl TaskOperationAdmission {
    pub(crate) const fn new(operation: OperationPermit, resource: ResourcePermit) -> Self {
        Self {
            _operation: operation,
            _resource: resource,
        }
    }
}

impl<A: PackTaskActor> Drop for TaskActorClient<A> {
    fn drop(&mut self) {
        let live = self
            .live
            .lock()
            .expect("task actor live-operation lock")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for close in live {
            close.close();
        }
        self.worker.abort();
    }
}

async fn run_worker<A: PackTaskActor>(
    mut actor: A,
    mut receiver: mpsc::Receiver<Command<A>>,
    sender: mpsc::Sender<Command<A>>,
    live: Arc<Mutex<HashMap<TaskActorOperationId, TaskCloseSignal>>>,
) {
    let mut next_operation = 1_u64;
    let mut operations = HashMap::new();
    while let Some(command) = receiver.recv().await {
        let keep_running = match command {
            Command::Invoke { invocation, reply } => {
                let result = invoke_operation(
                    &mut actor,
                    &mut operations,
                    &mut next_operation,
                    &sender,
                    &live,
                    invocation,
                )
                .await;
                match result {
                    Ok(operation) => {
                        if reply.send(Ok(operation)).is_err() {
                            close_operation(&mut actor, &mut operations, &live, operation).await
                        } else {
                            true
                        }
                    }
                    Err(error) => {
                        let keep_running = error != TaskActorError::Unavailable;
                        let _ = reply.send(Err(error));
                        keep_running
                    }
                }
            }
            Command::Pull { operation, reply } => {
                let result = pull_operation(&mut actor, &mut operations, operation).await;
                match result {
                    Ok(pull) => {
                        if reply.send(Ok(pull)).is_err() {
                            close_operation(&mut actor, &mut operations, &live, operation).await
                        } else {
                            true
                        }
                    }
                    Err(error) => {
                        let keep_running = error != TaskActorError::Unavailable;
                        let _ = reply.send(Err(error));
                        keep_running
                    }
                }
            }
            Command::Close(operation) => {
                close_operation(&mut actor, &mut operations, &live, operation).await
            }
        };
        if !keep_running || !actor.is_available() {
            break;
        }
    }
    for operation in operations.into_values() {
        operation.close.close();
    }
    live.lock().expect("task actor live-operation lock").clear();
}

async fn invoke_operation<A: PackTaskActor>(
    actor: &mut A,
    operations: &mut HashMap<TaskActorOperationId, WorkerOperation<A::Operation>>,
    next_operation: &mut u64,
    sender: &mpsc::Sender<Command<A>>,
    live: &Mutex<HashMap<TaskActorOperationId, TaskCloseSignal>>,
    invocation: InvokeCommand<A::Request>,
) -> Result<TaskActorOperationId, TaskActorError<A::Error>> {
    let InvokeCommand {
        request,
        deadline,
        close,
    } = invocation;
    if *next_operation == u64::MAX {
        return Err(TaskActorError::Unavailable);
    }
    let invocation = timeout_at(deadline, actor.invoke(&request));
    tokio::pin!(invocation);
    let mut operation = tokio::select! {
        biased;
        () = close.closed() => return Err(TaskActorError::Unavailable),
        result = &mut invocation => result
            .map_err(|_| TaskActorError::Unavailable)?
            .map_err(map_actor_error::<A>)?,
    };
    let admission = A::take_admission(&mut operation).ok_or(TaskActorError::Unavailable)?;
    close.bind_admission(admission);
    if close.is_closed() {
        return Err(TaskActorError::Unavailable);
    }
    let id = TaskActorOperationId(*next_operation);
    *next_operation += 1;
    operations.insert(
        id,
        WorkerOperation {
            deadline,
            operation,
            close: close.clone(),
        },
    );
    live.lock()
        .expect("task actor live-operation lock")
        .insert(id, close.clone());
    let expiry = sender.clone();
    tokio::spawn(async move {
        tokio::select! {
            () = tokio::time::sleep_until(deadline) => {
                close.close();
                let _ = expiry.send(Command::Close(id)).await;
            }
            () = close.closed() => {
                let _ = expiry.send(Command::Close(id)).await;
            }
            () = expiry.closed() => {}
        }
    });
    Ok(id)
}

async fn pull_operation<A: PackTaskActor>(
    actor: &mut A,
    operations: &mut HashMap<TaskActorOperationId, WorkerOperation<A::Operation>>,
    id: TaskActorOperationId,
) -> Result<A::Pull, TaskActorError<A::Error>> {
    let operation = operations
        .get_mut(&id)
        .ok_or(TaskActorError::UnknownOperation)?;
    let pull = timeout_at(operation.deadline, actor.pull(&mut operation.operation));
    tokio::pin!(pull);
    tokio::select! {
        biased;
        () = operation.close.closed() => Err(TaskActorError::Unavailable),
        result = &mut pull => result
            .map_err(|_| TaskActorError::Unavailable)?
            .map_err(map_actor_error::<A>),
    }
}

async fn close_operation<A: PackTaskActor>(
    actor: &mut A,
    operations: &mut HashMap<TaskActorOperationId, WorkerOperation<A::Operation>>,
    live: &Mutex<HashMap<TaskActorOperationId, TaskCloseSignal>>,
    id: TaskActorOperationId,
) -> bool {
    let Some(operation) = operations.remove(&id) else {
        return true;
    };
    operation.close.close();
    live.lock()
        .expect("task actor live-operation lock")
        .remove(&id);
    actor.drop_operation(operation.operation).await.is_ok() && actor.is_available()
}

fn map_actor_error<A: PackTaskActor>(error: A::Error) -> TaskActorError<A::Error> {
    if A::is_fatal(error) {
        TaskActorError::Unavailable
    } else {
        TaskActorError::Pack(error)
    }
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll, Waker};

    use super::{TaskCloseSignal, TaskOperationAdmission};
    use crate::runtime::admission::AdmissionLedger;
    use crate::runtime::{AdmissionError, MAX_LIVE_RESOURCES, MAX_OPEN_OPERATIONS};

    fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
    }

    #[tokio::test]
    async fn close_before_the_first_wait_is_observed() {
        let close = TaskCloseSignal::new();
        assert!(close.close());

        close.closed().await;
        assert!(!close.close());
    }

    #[tokio::test]
    async fn close_wakes_an_enabled_waiter() {
        let close = TaskCloseSignal::new();
        let mut closed = Box::pin(close.closed());
        assert!(poll_once(closed.as_mut()).is_pending());

        assert!(close.close());
        closed.await;
    }

    #[test]
    fn close_releases_admission_before_asynchronous_guest_cleanup() {
        let admission = AdmissionLedger::new();
        let close = TaskCloseSignal::new();
        close.bind_admission(TaskOperationAdmission::new(
            admission.open_operation().expect("operation admission"),
            admission.admit_resource().expect("resource admission"),
        ));
        let operations = (1..MAX_OPEN_OPERATIONS)
            .map(|_| admission.open_operation().expect("remaining operation"))
            .collect::<Vec<_>>();
        let resources = (1..MAX_LIVE_RESOURCES)
            .map(|_| admission.admit_resource().expect("remaining resource"))
            .collect::<Vec<_>>();
        assert_eq!(
            admission.open_operation().err(),
            Some(AdmissionError::OperationCapacity)
        );
        assert_eq!(
            admission.admit_resource().err(),
            Some(AdmissionError::ResourceCapacity)
        );

        assert!(close.close());
        let replacement_operation = admission
            .open_operation()
            .expect("close synchronously releases operation admission");
        let replacement_resource = admission
            .admit_resource()
            .expect("close synchronously releases resource admission");

        drop((replacement_operation, replacement_resource));
        drop((operations, resources));
    }
}
