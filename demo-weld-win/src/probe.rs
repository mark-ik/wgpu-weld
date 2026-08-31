//! Center-pixel readback for unattended DX12 validation.

const SIDE: u32 = 64;

pub struct Readback {
    pub first_pixels: Vec<[u8; 4]>,
    pixels: Vec<[u8; 4]>,
    pub origin: (u32, u32),
}

impl Readback {
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
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("weld-probe-readback"),
        size: (bytes_per_row * height) as u64,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("weld-probe"),
    });
    encoder.copy_texture_to_buffer(
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
    let submission = queue.submit([encoder.finish()]);

    let slice = buffer.slice(..);
    slice.map_async(wgpu::MapMode::Read, |_| {});
    device
        .poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: None,
        })
        .map_err(|error| format!("poll while mapping readback failed: {error}"))?;

    let data = slice.get_mapped_range().expect("map range");
    let pixels: Vec<[u8; 4]> = (0..height as usize)
        .flat_map(|row| {
            let start = row * bytes_per_row as usize;
            data[start..start + width as usize * 4]
                .chunks_exact(4)
                .map(|pixel| [pixel[0], pixel[1], pixel[2], pixel[3]])
        })
        .collect();
    let first_pixels = pixels.iter().copied().take(4).collect();
    drop(data);
    buffer.unmap();

    Ok(Readback {
        first_pixels,
        pixels,
        origin,
    })
}
