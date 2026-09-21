//! Tracy: sampled callstacks, and the global allocator that reports every
//! allocation to the same client.

use tracing::Subscriber;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

/// Tracy tracks a span that is entered on one thread and left on another as
/// two spans. Under async, or any work that hops threads mid-span, the
/// timeline it renders is wrong. The table says so beside every Tracy row.
pub const ASYNC_CAVEAT: &str = "tracy drops a span entered and exited on different threads, so its timeline is wrong under async";

/// The sampling depth the client is allowed, in callstack frames.
pub const CALLSTACK_DEPTH: u32 = 32;

/// The Tracy layer, or `None` when the feature is off.
#[cfg(feature = "tracy")]
pub fn layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    use tracing_tracy::TracyLayer;
    Some(TracyLayer::new(Sampling::default()).boxed())
}

/// The client's defaults with callstack collection turned on, which is the
/// whole reason to price the layer.
#[cfg(feature = "tracy")]
#[derive(Default)]
struct Sampling {
    fields: tracing_subscriber::fmt::format::DefaultFields,
}

#[cfg(feature = "tracy")]
impl tracing_tracy::Config for Sampling {
    type Formatter = tracing_subscriber::fmt::format::DefaultFields;

    fn formatter(&self) -> &Self::Formatter {
        &self.fields
    }

    fn stack_depth(&self, _metadata: &tracing::Metadata<'_>) -> u16 {
        CALLSTACK_DEPTH as u16
    }

    fn format_fields_in_zone_name(&self) -> bool {
        false
    }
}

#[cfg(not(feature = "tracy"))]
pub fn layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    None
}

/// The tracked global allocator, from the same client the span layer uses. A
/// binary installs it; a library cannot.
#[cfg(feature = "tracy-alloc")]
pub type Allocator = tracy_client::ProfiledAllocator<std::alloc::System>;

/// The callstack depth the allocator collects. Zero reports each allocation
/// and free with its size, and walks no stack per call.
#[cfg(feature = "tracy-alloc")]
pub const ALLOC_CALLSTACK_DEPTH: u16 = 0;

/// The allocator a binary declares.
#[cfg(feature = "tracy-alloc")]
#[macro_export]
macro_rules! tracy_allocator {
    ($name:ident) => {
        #[global_allocator]
        static $name: $crate::tracy::Allocator = $crate::tracy::Allocator::new(
            std::alloc::System,
            $crate::tracy::ALLOC_CALLSTACK_DEPTH,
        );
    };
}