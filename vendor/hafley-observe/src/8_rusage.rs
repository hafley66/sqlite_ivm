//! Process cost, read from the kernel: CPU, peak RSS, and disk bytes.
//!
//! The harness measures with this sampler on every build, including the build
//! where every layer is off, so the sampler is compiled always and the `rusage`
//! feature adds only the layer that publishes a sample as a record.

#[cfg(feature = "rusage")]
use tracing::Subscriber;
#[cfg(feature = "rusage")]
use tracing_subscriber::layer::{Context, Layer};
#[cfg(feature = "rusage")]
use tracing_subscriber::registry::LookupSpan;

/// One reading of this process.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub cpu_user_secs: f64,
    pub cpu_system_secs: f64,
    pub peak_rss_bytes: Option<u64>,
    pub disk_read_bytes: Option<u64>,
    pub disk_write_bytes: Option<u64>,
}

/// Peak RSS and CPU of this process from `RUSAGE_SELF`. macOS reports resident
/// size in bytes, Linux in kibibytes.
pub fn cpu_and_peak_rss() -> (f64, f64, Option<u64>) {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    if rc != 0 {
        return (0.0, 0.0, None);
    }
    let user = usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1_000_000.0;
    let system = usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1_000_000.0;
    #[cfg(target_os = "macos")]
    let peak = usage.ru_maxrss;
    #[cfg(not(target_os = "macos"))]
    let peak = usage.ru_maxrss * 1024;
    (user, system, Some(peak as u64))
}

/// Cumulative disk I/O of this process as `(bytes read, bytes written)`.
pub fn disk_io_bytes() -> Option<(u64, u64)> {
    #[cfg(target_os = "macos")]
    let value = {
        let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::proc_pid_rusage(
                std::process::id() as libc::c_int,
                libc::RUSAGE_INFO_V2,
                &mut info as *mut libc::rusage_info_v2 as *mut libc::rusage_info_t,
            )
        };
        if rc != 0 {
            None
        } else {
            Some((info.ri_diskio_bytesread, info.ri_diskio_byteswritten))
        }
    };
    #[cfg(target_os = "linux")]
    let value = {
        let text = std::fs::read_to_string("/proc/self/io").ok()?;
        let field = |name: &str| -> Option<u64> {
            text.lines()
                .find_map(|line| line.strip_prefix(name))
                .and_then(|value| value.trim().parse().ok())
        };
        Some((field("read_bytes:")?, field("write_bytes:")?))
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let value = None;
    value
}

pub fn sample() -> Usage {
    let (cpu_user_secs, cpu_system_secs, peak_rss_bytes) = cpu_and_peak_rss();
    let (disk_read_bytes, disk_write_bytes) = match disk_io_bytes() {
        Some((read, write)) => (Some(read), Some(write)),
        None => (None, None),
    };
    Usage {
        cpu_user_secs,
        cpu_system_secs,
        peak_rss_bytes,
        disk_read_bytes,
        disk_write_bytes,
    }
}

/// Publishes one usage sample as a record each time a span closes.
pub struct RusageLayer;

/// The usage layer, or `None` when the feature is off. The sampler itself is
/// always compiled: the harness measures with it on every build.
#[cfg(feature = "rusage")]
pub fn layer<S>() -> Option<Box<dyn Layer<S> + Send + Sync>>
where
    S: Subscriber + for<'a> LookupSpan<'a> + Send + Sync,
{
    Some(Layer::boxed(RusageLayer))
}

#[cfg(not(feature = "rusage"))]
pub fn layer<S>() -> Option<Box<dyn tracing_subscriber::Layer<S> + Send + Sync>>
where
    S: tracing::Subscriber
        + for<'a> tracing_subscriber::registry::LookupSpan<'a>
        + Send
        + Sync,
{
    None
}

#[cfg(feature = "rusage")]
impl<S> Layer<S> for RusageLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_close(&self, id: tracing::Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else {
            return;
        };
        let usage = sample();
        tracing::debug!(
            target: crate::RUSAGE_TARGET,
            span = span.name(),
            "cpu.user_secs" = usage.cpu_user_secs,
            "cpu.system_secs" = usage.cpu_system_secs,
            "mem.rss_bytes" = usage.peak_rss_bytes.unwrap_or_default(),
            "io.read_bytes" = usage.disk_read_bytes.unwrap_or_default(),
            "io.write_bytes" = usage.disk_write_bytes.unwrap_or_default(),
            "process usage sampled"
        );
    }
}