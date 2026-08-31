//! Reads a small center sample of the imported texture back to the CPU.
//!
//! This demo's whole reason to exist is proving that the DMABUF import produces
//! a wgpu texture actually backed by CEF's buffer. A window on a screen nobody
//! is watching proves nothing, and "it did not crash" proves nearly as little.
//! Sampling real pixels does: if CEF painted example.com, the sample is not
//! uniformly zero.

/// 64 * 4 bytes = 256 bytes per row, which is exactly
/// `COPY_BYTES_PER_ROW_ALIGNMENT`, so no padded-row handling is needed.
const SIDE: u32 = 64;

pub struct Readback {
    pub non_zero_bytes: usize,
    pub total_bytes: usize,
    pub first_pixels: Vec<[u8; 4]>,
    pixels: Vec<[u8; 4]>,
    pub origin: (u32, u32),
}

impl Readback {
    pub fn looks_painted(&self) -> bool {
        self.non_zero_bytes > 0
    }

    pub fn matching_pixels(&self, expected: [u8; 4], tolerance: u8) -> usize {
        self.pixels
            .iter()
            .filter(|pixel| {
                pixel
                    .iter()
                    .zip(expected)
                    .all(|(actual, wanted)| actual.abs_diff(wanted) <= tolerance)
            })
            .count()
    }

    pub fn total_pixels(&self) -> usize {
        self.pixels.len()
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
    let origin = (
        (texture.width() - width) / 2,
        (texture.height() - height) / 2,
    );
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
            origin: wgpu::Origin3d {
                x: origin.0,
                y: origin.1,
                z: 0,
            },
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
    let pixels: Vec<[u8; 4]> = (0..height as usize)
        .flat_map(|row| {
            let start = row * bytes_per_row as usize;
            data[start..start + width as usize * 4]
                .chunks_exact(4)
                .map(|p| [p[0], p[1], p[2], p[3]])
        })
        .collect();
    let non_zero_bytes = pixels.iter().flatten().filter(|b| **b != 0).count();
    let first_pixels = pixels.iter().copied().take(4).collect();
    let total_bytes = pixels.len() * 4;
    drop(data);
    buffer.unmap();

    Ok(Readback {
        non_zero_bytes,
        total_bytes,
        first_pixels,
        pixels,
        origin,
    })
}

/// Dump the whole imported texture as a binary PPM.
///
/// The center sample answers "did any pixels arrive". It cannot answer "are
/// they in the right places", which is the question whenever the DMA-BUF
/// modifier had to be assumed rather than read from CEF. A whole-texture image
/// answers that at a glance: a wrong tiling reads as visible scrambling.
/// PPM keeps this dependency-free; convert with ImageMagick to view.
pub fn dump_ppm(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    path: &str,
) -> Result<(), String> {
    let width = texture.width();
    let height = texture.height();
    let unpadded = width * 4;
    // copy_texture_to_buffer wants 256-aligned rows; 1366*4 is not, so the
    // buffer carries padding that has to be skipped when writing pixels out.
    let padded =
        unpadded.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;

    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("weld-probe-dump"),
        size: (padded * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("weld-probe-dump"),
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
                bytes_per_row: Some(padded),
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
        .map_err(|err| format!("poll while mapping dump failed: {err}"))?;

    // CEF's imported frames are always BGRA, but a swapchain may be either,
    // and this is also used on swapchain images. PPM wants RGB.
    let swap_bgr = matches!(
        texture.format(),
        wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Bgra8UnormSrgb
    );

    let data = slice.get_mapped_range().expect("map range");
    let mut out = Vec::with_capacity((width * height * 3) as usize + 32);
    out.extend_from_slice(format!("P6\n{width} {height}\n255\n").as_bytes());
    for row in 0..height {
        let start = (row * padded) as usize;
        for px in data[start..start + unpadded as usize].chunks_exact(4) {
            let (r, g, b) = if swap_bgr {
                (px[2], px[1], px[0])
            } else {
                (px[0], px[1], px[2])
            };
            out.push(r);
            out.push(g);
            out.push(b);
        }
    }
    drop(data);
    buffer.unmap();

    std::fs::write(path, &out).map_err(|err| format!("write {path}: {err}"))
}
