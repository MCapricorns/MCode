use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Barrier};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use wasmtime::Linker;

use super::admission::AdmissionLedger;
use super::limits::{MAX_AGGREGATE_MEMORY_BYTES, MAX_AGGREGATE_TABLE_ELEMENTS, MAX_MEMORY_BYTES};
use super::owner::ActiveSegment;
use super::{
    AdmissionError, HOSTCALL_FUEL, MAX_LIVE_RESOURCES, MAX_OPEN_OPERATIONS, OPERATION_FUEL_BUDGET,
    PluginRuntime, RuntimeError,
};

const WASM_PAGE_BYTES: usize = 64 * 1024;

fn wasm(source: &str) -> Vec<u8> {
    wat::parse_str(source).expect("valid test Wasm")
}

fn initialized_runtime() -> PluginRuntime {
    let runtime = PluginRuntime::new();
    runtime
        .compile_pack(
            crate::ComponentWorld::Provider.reference_bytes(),
            crate::ComponentWorld::Provider,
            crate::ComponentLimits::default(),
        )
        .expect("initialize runtime from scanned Provider Pack");
    runtime
}

fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Poll<F::Output> {
    let mut context = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut context)
}

fn parked_fuel(owner: &super::PluginOwner) -> u64 {
    owner
        .store
        .as_ref()
        .expect("owner has Store")
        .get_fuel()
        .expect("fuel enabled")
}

#[test]
fn invalid_input_never_initializes_the_runtime_engine() {
    let runtime = PluginRuntime::new();
    assert!(!runtime.inner.components.is_initialized());

    let core = wasm("(module)");
    assert_eq!(
        runtime
            .compile_pack(
                core,
                crate::ComponentWorld::Web,
                crate::ComponentLimits::default()
            )
            .err(),
        Some(RuntimeError::Preflight(
            crate::PreflightError::InvalidComponent
        ))
    );
    assert!(!runtime.inner.components.is_initialized());
}

#[test]
fn owner_installs_fixed_store_policy_without_raw_access() {
    let runtime = initialized_runtime();
    let engine = runtime.inner.engine().expect("initialized runtime");
    assert!(engine.get_consume_fuel());
    assert!(engine.get_epoch_interruption());
    assert!(!engine.get_concurrency_support());

    let owner = runtime.new_owner().expect("owner");
    let store = owner.store.as_ref().expect("Store");
    assert_eq!(store.get_fuel().expect("fuel"), 0);
    assert_eq!(store.hostcall_fuel(), HOSTCALL_FUEL);
}

#[test]
fn resource_table_accepts_n_and_rejects_n_plus_one() {
    let runtime = initialized_runtime();
    let mut owner = runtime.new_owner().expect("owner");
    let resources = &mut owner.store.as_mut().expect("Store").data_mut().resources;

    for _ in 0..4_096 {
        resources.push(()).expect("resource at or below N");
    }
    assert!(resources.push(()).is_err());
}

#[tokio::test(flavor = "current_thread")]
async fn two_guest_segments_strictly_decrease_one_total_budget() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm("(module (func (export \"run\") nop))"))
        .expect("compile");
    let mut owner = runtime.new_owner().expect("owner");
    let instance = owner
        .instantiate_test_module(&module)
        .await
        .expect("instantiate");
    let run = instance
        .typed_function::<(), ()>(&mut owner, "run")
        .expect("typed function");
    let mut lease = owner.open_operation().expect("operation");

    owner
        .call_typed(&mut lease, &run, ())
        .await
        .expect("first segment");
    let after_first = lease.remaining();
    assert!(after_first < OPERATION_FUEL_BUDGET);
    assert_eq!(parked_fuel(&owner), 0);

    owner
        .call_typed(&mut lease, &run, ())
        .await
        .expect("second segment");
    assert!(lease.remaining() < after_first);
    assert_eq!(parked_fuel(&owner), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn crossed_owner_and_active_operation_are_rejected() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm("(module (func (export \"run\")))"))
        .expect("compile");
    let mut first_owner = runtime.new_owner().expect("first owner");
    let instance = first_owner
        .instantiate_test_module(&module)
        .await
        .expect("instantiate");
    let run = instance
        .typed_function::<(), ()>(&mut first_owner, "run")
        .expect("typed function");
    let mut first_lease = first_owner.open_operation().expect("first operation");
    let mut second_owner = runtime.new_owner().expect("second owner");

    assert_eq!(
        second_owner.call_typed(&mut first_lease, &run, ()).await,
        Err(RuntimeError::OwnerMismatch)
    );
    assert!(second_owner.is_available());

    let mut second_lease = first_owner.open_operation().expect("second operation");
    let store = first_owner.store.as_mut().expect("Store");
    store
        .set_fuel(first_lease.remaining())
        .expect("install active test fuel");
    store.data_mut().active_segment = Some(ActiveSegment {
        owner: first_owner.identity.clone(),
        operation: first_lease.operation,
        installed: first_lease.remaining(),
    });
    let second_remainder = second_lease.remaining();

    assert_eq!(
        first_owner.call_typed(&mut second_lease, &run, ()).await,
        Err(RuntimeError::SegmentActive)
    );
    assert_eq!(second_lease.remaining(), second_remainder);
    assert!(!first_owner.is_available());
}

#[tokio::test(flavor = "current_thread")]
async fn guest_trap_parks_fuel_and_preserves_only_the_remainder() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm(
            "(module
                (func (export \"trap\") unreachable)
                (func (export \"resume\") nop))",
        ))
        .expect("compile");
    let mut owner = runtime.new_owner().expect("owner");
    let instance = owner
        .instantiate_test_module(&module)
        .await
        .expect("instantiate");
    let trap = instance
        .typed_function::<(), ()>(&mut owner, "trap")
        .expect("trap function");
    let resume = instance
        .typed_function::<(), ()>(&mut owner, "resume")
        .expect("resume function");
    let mut lease = owner.open_operation().expect("operation");

    assert_eq!(
        owner.call_typed(&mut lease, &trap, ()).await,
        Err(RuntimeError::Guest)
    );
    let trapped_remainder = lease.remaining();
    assert!(trapped_remainder < OPERATION_FUEL_BUDGET);
    assert_eq!(parked_fuel(&owner), 0);

    owner
        .call_typed(&mut lease, &resume, ())
        .await
        .expect("resume after trap");
    assert!(lease.remaining() < trapped_remainder);
    assert_eq!(parked_fuel(&owner), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_pending_guest_call_parks_and_resumes_saved_remainder() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm(
            "(module
                (import \"host\" \"wait\" (func $wait))
                (func (export \"wait\") call $wait)
                (func (export \"resume\") nop))",
        ))
        .expect("compile");
    let mut owner = runtime.new_owner().expect("owner");
    let mut linker = Linker::new(runtime.inner.engine().expect("initialized runtime"));
    linker
        .func_wrap_async("host", "wait", |_caller, (): ()| {
            Box::new(std::future::pending::<()>())
        })
        .expect("async Host function");
    let instance = owner
        .instantiate_with_linker(&module, &linker)
        .await
        .expect("instantiate");
    let wait = instance
        .typed_function::<(), ()>(&mut owner, "wait")
        .expect("wait function");
    let resume = instance
        .typed_function::<(), ()>(&mut owner, "resume")
        .expect("resume function");
    let mut lease = owner.open_operation().expect("operation");

    let mut pending_call = Box::pin(owner.call_typed(&mut lease, &wait, ()));
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut pending_call)
            .await
            .is_err()
    );
    drop(pending_call);

    let cancelled_remainder = lease.remaining();
    assert!(cancelled_remainder < OPERATION_FUEL_BUDGET);
    assert_eq!(parked_fuel(&owner), 0);
    owner
        .call_typed(&mut lease, &resume, ())
        .await
        .expect("resume after cancellation");
    assert!(lease.remaining() < cancelled_remainder);
    assert_eq!(parked_fuel(&owner), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn dropped_cpu_bound_guest_call_parks_reduced_fuel() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm(
            "(module
                (func (export \"spin\") (loop $forever br $forever)))",
        ))
        .expect("compile");
    let mut owner = runtime.new_owner().expect("owner");
    let instance = owner
        .instantiate_test_module(&module)
        .await
        .expect("instantiate");
    let spin = instance
        .typed_function::<(), ()>(&mut owner, "spin")
        .expect("spin function");
    let mut lease = owner.open_operation().expect("operation");

    let mut pending_call = Box::pin(owner.call_typed(&mut lease, &spin, ()));
    assert!(poll_once(pending_call.as_mut()).is_pending());
    drop(pending_call);

    assert!(lease.remaining() > 0);
    assert!(lease.remaining() < OPERATION_FUEL_BUDGET);
    assert_eq!(parked_fuel(&owner), 0);
    assert!(owner.is_available());
}

#[tokio::test(flavor = "current_thread")]
async fn infinite_guest_with_unbounded_fuel_traps_on_epoch_deadline() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm(
            "(module
                (func (export \"spin\") (loop $forever br $forever)))",
        ))
        .expect("compile");
    let mut owner = runtime.new_owner().expect("owner");
    let instance = owner
        .instantiate_test_module(&module)
        .await
        .expect("instantiate");
    let spin = instance
        .typed_function::<(), ()>(&mut owner, "spin")
        .expect("spin function");
    let mut lease = owner.open_operation().expect("operation");
    lease.remaining = u64::MAX;

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        owner.call_typed(&mut lease, &spin, ()),
    )
    .await;

    assert_eq!(result, Ok(Err(RuntimeError::Guest)));
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(lease.remaining() > OPERATION_FUEL_BUDGET);
    assert_eq!(parked_fuel(&owner), 0);
    assert!(owner.is_available());
}

#[tokio::test(flavor = "current_thread")]
async fn real_store_denial_reserves_nothing_before_valid_growth() {
    let runtime = initialized_runtime();
    let module = runtime
        .compile_test_module(wasm(
            "(module
                (memory 1 1024)
                (func (export \"denied\") (result i32)
                    i32.const 1024
                    memory.grow)
                (func (export \"valid\") (result i32)
                    i32.const 1
                    memory.grow))",
        ))
        .expect("compile");
    let mut owner = runtime.new_owner().expect("owner");
    let instance = owner
        .instantiate_test_module(&module)
        .await
        .expect("instantiate");
    let denied = instance
        .typed_function::<(), i32>(&mut owner, "denied")
        .expect("denied function");
    let valid = instance
        .typed_function::<(), i32>(&mut owner, "valid")
        .expect("valid function");
    let mut lease = owner.open_operation().expect("operation");

    assert_eq!(
        owner
            .call_typed(&mut lease, &denied, ())
            .await
            .expect("normal denial"),
        -1
    );
    assert_eq!(
        owner
            .store
            .as_ref()
            .expect("Store")
            .data()
            .limiter
            .reserved_memory_bytes,
        WASM_PAGE_BYTES
    );
    assert_eq!(
        owner
            .call_typed(&mut lease, &valid, ())
            .await
            .expect("later valid growth"),
        1
    );
    assert_eq!(
        owner
            .store
            .as_ref()
            .expect("Store")
            .data()
            .limiter
            .reserved_memory_bytes,
        2 * WASM_PAGE_BYTES
    );
}

#[tokio::test(flavor = "current_thread")]
async fn real_store_enforces_aggregate_memory_n_and_n_plus_one() {
    let runtime = initialized_runtime();
    let full = runtime
        .compile_test_module(wasm("(module (memory 1024))"))
        .expect("full memory compile");
    let one = runtime
        .compile_test_module(wasm("(module (memory 1))"))
        .expect("one-page compile");
    let mut owner = runtime.new_owner().expect("owner");

    owner
        .instantiate_test_module(&full)
        .await
        .expect("first memory");
    owner
        .instantiate_test_module(&full)
        .await
        .expect("aggregate memory N");
    assert_eq!(
        owner
            .store
            .as_ref()
            .expect("Store")
            .data()
            .limiter
            .reserved_memory_bytes,
        MAX_AGGREGATE_MEMORY_BYTES
    );
    assert_eq!(
        owner.instantiate_test_module(&one).await.err(),
        Some(RuntimeError::Instantiation)
    );
    assert!(!owner.is_available());
}

#[tokio::test(flavor = "current_thread")]
async fn real_store_enforces_aggregate_table_n_and_n_plus_one() {
    let runtime = initialized_runtime();
    let half = runtime
        .compile_test_module(wasm("(module (table 32768 funcref))"))
        .expect("half table compile");
    let one = runtime
        .compile_test_module(wasm("(module (table 1 funcref))"))
        .expect("one table compile");
    let mut owner = runtime.new_owner().expect("owner");

    owner
        .instantiate_test_module(&half)
        .await
        .expect("first half table");
    owner
        .instantiate_test_module(&half)
        .await
        .expect("aggregate table N");
    assert_eq!(
        owner
            .store
            .as_ref()
            .expect("Store")
            .data()
            .limiter
            .reserved_table_elements,
        MAX_AGGREGATE_TABLE_ELEMENTS
    );
    assert_eq!(
        owner.instantiate_test_module(&one).await.err(),
        Some(RuntimeError::Instantiation)
    );
    assert!(!owner.is_available());
}

#[tokio::test(flavor = "current_thread")]
async fn failed_instantiation_disposes_old_store_and_fresh_owner_retries() {
    let runtime = initialized_runtime();
    let failing = runtime
        .compile_test_module(wasm(
            "(module
                (memory 1)
                (func $start unreachable)
                (start $start))",
        ))
        .expect("failing compile");
    let valid = runtime
        .compile_test_module(wasm("(module (memory 1))"))
        .expect("valid compile");
    let mut failed_owner = runtime.new_owner().expect("failed owner");

    assert_eq!(
        failed_owner.instantiate_test_module(&failing).await.err(),
        Some(RuntimeError::Instantiation)
    );
    assert!(!failed_owner.is_available());
    assert_eq!(
        failed_owner.instantiate_test_module(&valid).await.err(),
        Some(RuntimeError::StoreDisposed)
    );

    let mut fresh_owner = runtime.new_owner().expect("fresh owner");
    fresh_owner
        .instantiate_test_module(&valid)
        .await
        .expect("fresh owner retry");
    assert!(fresh_owner.is_available());
}

#[test]
fn resource_admission_reaches_exact_n_under_contention_then_reacquires_n() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = MAX_LIVE_RESOURCES / THREADS;

    let admission = AdmissionLedger::new();
    let full = Arc::new(Barrier::new(THREADS + 1));
    let release = Arc::new(Barrier::new(THREADS + 1));
    let threads = (0..THREADS)
        .map(|_| {
            let admission = admission.clone();
            let full = Arc::clone(&full);
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                let permits = (0..PER_THREAD)
                    .map(|_| admission.admit_resource().expect("contended resource"))
                    .collect::<Vec<_>>();
                full.wait();
                release.wait();
                drop(permits);
            })
        })
        .collect::<Vec<_>>();

    full.wait();
    assert_eq!(
        admission.admit_resource().err(),
        Some(AdmissionError::ResourceCapacity)
    );
    release.wait();
    for thread in threads {
        thread.join().expect("resource thread");
    }
    let all = (0..MAX_LIVE_RESOURCES)
        .map(|_| admission.admit_resource().expect("reacquired resource"))
        .collect::<Vec<_>>();
    assert_eq!(
        admission.admit_resource().err(),
        Some(AdmissionError::ResourceCapacity)
    );
    drop(all);
}

#[test]
fn operation_admission_reaches_exact_n_under_contention_then_reacquires_n() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = MAX_OPEN_OPERATIONS / THREADS;

    let admission = AdmissionLedger::new();
    let full = Arc::new(Barrier::new(THREADS + 1));
    let release = Arc::new(Barrier::new(THREADS + 1));
    let threads = (0..THREADS)
        .map(|_| {
            let admission = admission.clone();
            let full = Arc::clone(&full);
            let release = Arc::clone(&release);
            std::thread::spawn(move || {
                let permits = (0..PER_THREAD)
                    .map(|_| admission.open_operation().expect("contended operation"))
                    .collect::<Vec<_>>();
                full.wait();
                release.wait();
                drop(permits);
            })
        })
        .collect::<Vec<_>>();

    full.wait();
    assert_eq!(
        admission.open_operation().err(),
        Some(AdmissionError::OperationCapacity)
    );
    release.wait();
    for thread in threads {
        thread.join().expect("operation thread");
    }
    let all = (0..MAX_OPEN_OPERATIONS)
        .map(|_| admission.open_operation().expect("reacquired operation"))
        .collect::<Vec<_>>();
    assert_eq!(
        admission.open_operation().err(),
        Some(AdmissionError::OperationCapacity)
    );
    drop(all);
}

#[tokio::test(flavor = "current_thread")]
async fn exact_per_resource_and_count_limits_remain_installed() {
    let runtime = initialized_runtime();
    let full_memory = runtime
        .compile_test_module(wasm("(module (memory 1024))"))
        .expect("memory N compile");
    let oversized_memory = runtime
        .compile_test_module(wasm("(module (memory 1025))"))
        .expect("memory N+1 compile");
    let mut owner = runtime.new_owner().expect("owner");
    owner
        .instantiate_test_module(&full_memory)
        .await
        .expect("memory exactly N");
    assert_eq!(MAX_MEMORY_BYTES, 64 * 1024 * 1024);

    let mut oversized_owner = runtime.new_owner().expect("oversized owner");
    assert_eq!(
        oversized_owner
            .instantiate_test_module(&oversized_memory)
            .await
            .err(),
        Some(RuntimeError::Instantiation)
    );

    let empty = runtime
        .compile_test_module(wasm("(module)"))
        .expect("empty compile");
    let mut count_owner = runtime.new_owner().expect("count owner");
    for _ in 0..64 {
        count_owner
            .instantiate_test_module(&empty)
            .await
            .expect("instance at or below 64");
    }
    assert_eq!(
        count_owner.instantiate_test_module(&empty).await.err(),
        Some(RuntimeError::Instantiation)
    );
}
