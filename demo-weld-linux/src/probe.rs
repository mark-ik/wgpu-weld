//! Reads a small corner of the imported texture back to the CPU.
//!
//! This demo's whole reason to exist is proving that the DMABUF import produces
//! a wgpu texture actually backed by CEF's buffer. A window on a screen nobody
//! is watching proves nothing, and "it did not crash" proves nearly as little.
//! Sampling real pixels does: if CEF painted example.com, the corner is not
//! uniformly zero.

/// 64 * 4 bytes = 256 bytes per row, which is exactly
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, so no padded-row handling is needed.
const SIDE: u32 = 64;

pub struct Readback {
    pub non_zero_bytes: usize,
    pub total_bytes: usize,
    pub first_pixels: Vec<[u8; 4]>,
}

impl Readback {
    pub fn looks_painted(&self) -> bool {
        self.non_zero_bytes > 0
    }
}

pub fn sample(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
) -> Result<Readback, String> {
    let width = SIDE.min(texture.width());
    let height = SIDE.min(texture.height());
    if width == 0 || height == 0 {
        return Err("imported texture has a zero dimension".into());
    }
    let bytes_per_row = SIDE * 4;
    let size = (bytes_per_row * height) as u64;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("weld-probe-readback"),
        size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("weld-probe"),
    });
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(bytes_per_row),
                rows_per_image: Some(height),
            },
        },
        wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
    );
    let submission = queue.submit([enc.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|err| format!("poll while mapping readback failed: {err}"))?;

    let data = slice.get_mapped_range().expect("map range");
    let non_zero_bytes = data.iter().filter(|b| **b != 0).count();
    let first_pixels = data
        .chunks_exact(4)
        .take(4)
        .map(|p| [p[0], p[1], p[2], p[3]])
        .collect();
    let total_bytes = data.len();
    drop(data);
    buffer.unmap();

    Ok(Readback {
        non_zero_bytes,
        total_bytes,
        first_pixels,
    })
}
