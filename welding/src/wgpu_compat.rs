//! Call-shape compatibility for obtaining the selected wgpu row's Metal
//! device. Native texture wrapping itself lives in Graft.

/// Retain the selected HAL's Metal device as an objc2-metal object.
#[cfg(target_os = "macos")]
pub(crate) unsafe fn metal_device(
    device: &wgpu::Device,
) -> Option<objc2::rc::Retained<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDevice>>> {
    let hal_device = unsafe { device.as_hal::<wgpu::wgc::api::Metal>() }?;
    #[cfg(all(
        feature = "wgpu-28",
        not(feature = "wgpu-29"),
        not(feature = "wgpu-30")
    ))]
    {
        use foreign_types_shared::ForeignType;
        unsafe {
            objc2::rc::Retained::retain(
                hal_device
                    .raw_device()
                    .as_ptr()
                    .cast::<objc2::runtime::ProtocolObject<dyn objc2_metal::MTLDevice>>(),
            )
        }
    }
    #[cfg(any(feature = "wgpu-29", feature = "wgpu-30"))]
    {
        Some(hal_device.raw_device().clone())
    }
}
