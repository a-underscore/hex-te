const CURSOR_BLOCK: u32 = 0u;
const CURSOR_BAR: u32 = 1u;
const CURSOR_UNDERLINE: u32 = 2u;

struct Screen {
    background: vec4<f32>,
    cursor_color: vec4<f32>,
    resolution: vec2<f32>,
    grid: vec2<u32>,
    cursor: vec2<u32>,
    cursor_visible: u32,
    cursor_style: u32,
}

@group(0) @binding(0) var<uniform> screen: Screen;
@group(0) @binding(1) var screen_tex: texture_2d<f32>;
@group(0) @binding(2) var screen_sampler: sampler;

@vertex
fn vs_screen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}

fn grid_size() -> vec2<u32> {
    return max(screen.grid, vec2<u32>(1u));
}

fn cell_size() -> vec2<f32> {
    return screen.resolution / vec2<f32>(grid_size());
}

fn cell_at(pixel: vec2<f32>) -> vec2<u32> {
    return min(vec2<u32>(pixel / cell_size()), grid_size() - vec2<u32>(1u));
}

fn cursor_mask(local: vec2<f32>, size: vec2<f32>, style: u32) -> f32 {
    switch style {
        case 1u: {
            return select(0.0, 1.0, local.x < max(1.0, size.x * 0.125));
        }
        case 2u: {
            return select(0.0, 1.0, local.y >= size.y - max(1.0, size.y * 0.125));
        }
        default: {
            return 1.0;
        }
    }
}

@fragment
fn fs_screen(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = cell_size();
    let cell = cell_at(position.xy);

    // The texture covers the whole screen, so a pixel position maps straight
    // onto a 0..1 UV.
    let uv = position.xy / screen.resolution;
    var color = textureSample(screen_tex, screen_sampler, uv).rgb + screen.background.rgb;

    if (screen.cursor_visible != 0u && all(cell == screen.cursor)) {
        let local = position.xy - vec2<f32>(cell) * size;
        let mask = cursor_mask(local, size, screen.cursor_style);
        color = mix(color, screen.cursor_color.rgb, mask);
    }

    return vec4<f32>(color, 1.0);
}
