//! Monotone aggregate and per-resource Wasmtime Store limits.
use wasmtime::{ResourceLimiter, ResourceLimiterAsync, StoreLimits, StoreLimitsBuilder};

// These fixed T8 policy values bound every Store; changing one changes guest
// admission behavior and requires corresponding aggregate-limit review.
pub(super) const MAX_MEMORY_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_TABLE_ELEMENTS: usize = 65_536;
pub(super) const MAX_MEMORIES: usize = 2;
pub(super) const MAX_TABLES: usize = 4;
pub(super) const MAX_CORE_INSTANCES: usize = 64;
pub(super) const MAX_AGGREGATE_MEMORY_BYTES: usize = 128 * 1024 * 1024;
pub(super) const MAX_AGGREGATE_TABLE_ELEMENTS: usize = 65_536;

pub(super) struct StoreResourceLimiter {
    per_resource: StoreLimits,
    pub(super) reserved_memory_bytes: usize,
    pub(super) reserved_table_elements: usize,
    poisoned: bool,
}

impl StoreResourceLimiter {
    pub(super) fn new() -> Self {
        Self {
            per_resource: StoreLimitsBuilder::new()
                .memory_size(MAX_MEMORY_BYTES)
                .table_elements(MAX_TABLE_ELEMENTS)
                .memories(MAX_MEMORIES)
                .tables(MAX_TABLES)
                .instances(MAX_CORE_INSTANCES)
                .build(),
            reserved_memory_bytes: 0,
            reserved_table_elements: 0,
            poisoned: false,
        }
    }

    pub(super) const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    fn reject_if_poisoned(&self) -> wasmtime::Result<()> {
        if self.poisoned {
            return Err(wasmtime::Error::msg("Store resource ledger is poisoned"));
        }
        Ok(())
    }

    fn poison_after_growth_failure(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        // Wasmtime can report a type/allocation failure without first calling
        // the allow callback. It can also fail later in instantiation after
        // successful initial allocations. No callback-local delta can prove a
        // rollback, so the reservation remains monotone and the Store is dead.
        self.poisoned = true;
        Err(error)
    }
}

#[async_trait::async_trait]
impl ResourceLimiterAsync for StoreResourceLimiter {
    async fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.reject_if_poisoned()?;
        let Some(delta) = desired.checked_sub(current) else {
            return Ok(false);
        };
        if !self
            .per_resource
            .memory_growing(current, desired, maximum)?
        {
            return Ok(false);
        }
        let Some(reserved) = self.reserved_memory_bytes.checked_add(delta) else {
            return Ok(false);
        };
        if reserved > MAX_AGGREGATE_MEMORY_BYTES {
            return Ok(false);
        }

        self.reserved_memory_bytes = reserved;
        Ok(true)
    }

    fn memory_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.poison_after_growth_failure(error)
    }

    async fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        self.reject_if_poisoned()?;
        let Some(delta) = desired.checked_sub(current) else {
            return Ok(false);
        };
        if !self.per_resource.table_growing(current, desired, maximum)? {
            return Ok(false);
        }
        let Some(reserved) = self.reserved_table_elements.checked_add(delta) else {
            return Ok(false);
        };
        if reserved > MAX_AGGREGATE_TABLE_ELEMENTS {
            return Ok(false);
        }

        self.reserved_table_elements = reserved;
        Ok(true)
    }

    fn table_grow_failed(&mut self, error: wasmtime::Error) -> wasmtime::Result<()> {
        self.poison_after_growth_failure(error)
    }

    fn instances(&self) -> usize {
        self.per_resource.instances()
    }

    fn tables(&self) -> usize {
        self.per_resource.tables()
    }

    fn memories(&self) -> usize {
        self.per_resource.memories()
    }
}

#[cfg(test)]
mod tests {
    use wasmtime::{Error, ResourceLimiterAsync};

    use super::{MAX_AGGREGATE_MEMORY_BYTES, MAX_AGGREGATE_TABLE_ELEMENTS, StoreResourceLimiter};

    const WASM_PAGE_BYTES: usize = 64 * 1024;

    #[tokio::test(flavor = "current_thread")]
    async fn normal_denial_reserves_nothing_before_valid_memory_growth() {
        let mut limiter = StoreResourceLimiter::new();
        assert!(
            !limiter
                .memory_growing(0, MAX_AGGREGATE_MEMORY_BYTES + WASM_PAGE_BYTES, None)
                .await
                .expect("normal denial")
        );
        assert_eq!(limiter.reserved_memory_bytes, 0);
        assert!(
            limiter
                .memory_growing(0, WASM_PAGE_BYTES, None)
                .await
                .expect("later valid growth")
        );
        assert_eq!(limiter.reserved_memory_bytes, WASM_PAGE_BYTES);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn normal_denial_reserves_nothing_before_valid_table_growth() {
        let mut limiter = StoreResourceLimiter::new();
        assert!(
            !limiter
                .table_growing(0, MAX_AGGREGATE_TABLE_ELEMENTS + 1, None)
                .await
                .expect("normal denial")
        );
        assert_eq!(limiter.reserved_table_elements, 0);
        assert!(
            limiter
                .table_growing(0, 1, None)
                .await
                .expect("later valid growth")
        );
        assert_eq!(limiter.reserved_table_elements, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aggregate_reservations_are_monotone_at_n_plus_one() {
        let mut limiter = StoreResourceLimiter::new();
        assert!(
            limiter
                .memory_growing(0, MAX_AGGREGATE_MEMORY_BYTES / 2, None)
                .await
                .expect("first memory")
        );
        assert!(
            limiter
                .memory_growing(0, MAX_AGGREGATE_MEMORY_BYTES / 2, None)
                .await
                .expect("memory N")
        );
        assert!(
            !limiter
                .memory_growing(0, WASM_PAGE_BYTES, None)
                .await
                .expect("memory N+1")
        );
        assert_eq!(limiter.reserved_memory_bytes, MAX_AGGREGATE_MEMORY_BYTES);

        assert!(
            limiter
                .table_growing(0, MAX_AGGREGATE_TABLE_ELEMENTS, None)
                .await
                .expect("table N")
        );
        assert!(!limiter.table_growing(0, 1, None).await.expect("table N+1"));
        assert_eq!(
            limiter.reserved_table_elements,
            MAX_AGGREGATE_TABLE_ELEMENTS
        );
    }

    #[test]
    fn unmatched_growth_failures_poison_without_rollback_or_panic() {
        let mut limiter = StoreResourceLimiter::new();
        assert!(
            limiter
                .memory_grow_failed(Error::msg("unmatched memory failure"))
                .is_err()
        );
        assert!(limiter.is_poisoned());
        assert_eq!(limiter.reserved_memory_bytes, 0);

        let mut limiter = StoreResourceLimiter::new();
        assert!(
            limiter
                .table_grow_failed(Error::msg("unmatched table failure"))
                .is_err()
        );
        assert!(limiter.is_poisoned());
        assert_eq!(limiter.reserved_table_elements, 0);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn post_allow_failure_keeps_reservation_and_poisoned_limiter_rejects() {
        let mut limiter = StoreResourceLimiter::new();
        assert!(
            limiter
                .memory_growing(0, WASM_PAGE_BYTES, None)
                .await
                .expect("allow growth")
        );
        assert!(
            limiter
                .memory_grow_failed(Error::msg("allocation failure"))
                .is_err()
        );
        assert_eq!(limiter.reserved_memory_bytes, WASM_PAGE_BYTES);
        assert!(
            limiter
                .memory_growing(WASM_PAGE_BYTES, 2 * WASM_PAGE_BYTES, None)
                .await
                .is_err()
        );
    }

    #[test]
    fn limiter_reports_exact_resource_counts() {
        let limiter = StoreResourceLimiter::new();
        assert_eq!(limiter.instances(), 64);
        assert_eq!(limiter.memories(), 2);
        assert_eq!(limiter.tables(), 4);
    }
}
