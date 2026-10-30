//! Opening a GPU: the fastest hardware adapter, else wgpu's software one (WARP on Windows,
//! Mesa's lavapipe or llvmpipe on Linux), for a window's surface or for offscreen rasters.
//!
//! Contents: `pub(crate) struct Opened`, `pub(crate) async fn open`, `pub(crate) fn ladder`

/// An adapter's device and queue, and the surface format a window draws in (offscreen:
/// `Rgba8Unorm`).
pub(crate) struct Opened {
    pub(crate) adapter: wgpu::Adapter,
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
}

/// Opens a device on `instance` able to draw to `surface` (any when `None`): the adapter
/// `WGPU_ADAPTER_NAME` names, else the preferred one (`WGPU_POWER_PREF`, high performance by
/// default); `fallback` asks for the software adapter instead.
///
/// # Errors
///
/// No adapter matches, or it refuses a device with WebGL2's limits.
pub(crate) async fn open(
    instance: &wgpu::Instance,
    surface: Option<&wgpu::Surface<'static>>,
    fallback: bool,
) -> Result<Opened, String> {
    let named = match fallback {
        false => wgpu::util::initialize_adapter_from_env(instance, surface).await.ok(),
        true => None,
    };
    let adapter = match named {
        Some(a) => a,
        None => instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::from_env()
                    .unwrap_or(wgpu::PowerPreference::HighPerformance),
                force_fallback_adapter: fallback,
                compatible_surface: surface,
                apply_limit_buckets: false,
            })
            .await
            .map_err(|e| e.to_string())?,
    };
    let required_limits = wgpu::Limits::downlevel_webgl2_defaults().using_resolution(adapter.limits());
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            required_limits,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(Opened { adapter, device, queue })
}

/// Tries `open` on hardware, then (naming why) on the software adapter.
///
/// # Errors
///
/// Both rungs failed; the message names each one's reason.
pub(crate) fn ladder<T>(mut open: impl FnMut(bool) -> Result<T, String>) -> Result<T, String> {
    let hardware = match open(false) {
        Ok(g) => return Ok(g),
        Err(e) => e,
    };
    eprintln!("gpu: no hardware adapter ({hardware}); trying the software one");
    // Bounded: two rungs.
    open(true).map_err(|software| format!("hardware: {hardware}; software: {software}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_tries_hardware_then_software() {
        let mut tried = Vec::new();
        let first = ladder(|fallback| {
            tried.push(fallback);
            Ok::<_, String>(fallback)
        });
        assert_eq!((first, &tried[..]), (Ok(false), &[false][..]), "hardware first, alone");
        tried.clear();
        let second = ladder(|fallback| {
            tried.push(fallback);
            if fallback { Ok(1) } else { Err("no Vulkan".to_owned()) }
        });
        assert_eq!((second, &tried[..]), (Ok(1), &[false, true][..]), "then software");
        let none = ladder::<()>(|fallback| Err(format!("{fallback}")));
        assert_eq!(none, Err("hardware: false; software: true".to_owned()), "both named");
    }
}
