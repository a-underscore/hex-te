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
    cursor_size: vec2<f32>,
    // Seconds since the app started, for a shader that animates. The built-in
    // shader below is still, so it does not read it; a config that wants motion
    // does. `pad` only exists to keep the struct 16-byte aligned.
    time: f32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> screen: Screen;
@group(0) @binding(1) var screen_tex: texture_2d<f32>;
@group(0) @binding(2) var screen_sampler: sampler;
// The picture behind the grid, stretched over the whole window. It is a single
// pixel of `Screen::background` when the config asked for no picture, so the
// composite below is the same thing either way.
@group(0) @binding(3) var background_tex: texture_2d<f32>;
// The picture over everything, stretched the same way: an overlay like a shadow
// mask, a grille or a sheet of glare. It is a transparent pixel when the config
// asked for none, so the composite below leaves the image alone.
@group(0) @binding(4) var foreground_tex: texture_2d<f32>;

@vertex
fn vs_screen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    return vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}

fn grid_size() -> vec2<u32> {
    return max(screen.grid, vec2<u32>(1u));
}

fn cell_size() -> vec2<f32> {
    return max(screen.cursor_size, vec2<f32>(1.0));
}

fn cell_at(pixel: vec2<f32>) -> vec2<u32> {
    return min(vec2<u32>(pixel / cell_size()), grid_size() - vec2<u32>(1u));
}

fn cursor_mask(local: vec2<f32>, cell_size: vec2<f32>, style: u32) -> f32 {
    let text_size = min(screen.cursor_size, cell_size);
    let origin = (cell_size - text_size) * 0.5;
    let inside = all(local >= origin) && all(local < origin + text_size);

    switch style {
        case 1u: {
            let bar_width = max(1.0, text_size.x * 0.125);
            return select(0.0, 1.0, inside && local.x < origin.x + bar_width);
        }
        case 2u: {
            let underline_height = max(1.0, text_size.y * 0.125);
            return select(
                0.0,
                1.0,
                inside && local.y >= origin.y + text_size.y - underline_height,
            );
        }
        default: {
            return select(0.0, 1.0, inside);
        }
    }
}

// How bright a colour is, for telling a cell's ink from its background.
fn luma(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

@fragment
fn fs_screen(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let size = cell_size();
    let cell = cell_at(position.xy);

    // The texture covers the whole screen, so a pixel position maps straight
    // onto a 0..1 UV.
    let uv = position.xy / screen.resolution;

    // The grid is ink rather than a picture: its alpha is how much of a pixel
    // the cell covers, so a cell with a background of its own is opaque and
    // everything else lets the backdrop through. With no picture in the config
    // the backdrop is one pixel of the background colour, and this comes out as
    // the plain colour it always was.
    let ink = textureSample(screen_tex, screen_sampler, uv);
    let backdrop = textureSample(background_tex, screen_sampler, uv).rgb;
    var color = mix(backdrop, ink.rgb, ink.a);

    if (screen.cursor_visible != 0u && all(cell == screen.cursor)) {
        let local = position.xy - vec2<f32>(cell) * size;
        let mask = cursor_mask(local, size, screen.cursor_style);
        var cursor = screen.cursor_color.rgb;

        // A block cursor keeps the character under it readable: the block takes
        // the cursor's colour where the cell is background, and whatever is
        // behind it — the picture, or the background colour — where the
        // character's ink is. The bar and the underline are thin marks drawn
        // over the character, so they stay solid.
        if (screen.cursor_style == CURSOR_BLOCK) {
            let drawn = clamp((luma(color) - luma(backdrop)) * 4.0, 0.0, 1.0);
            cursor = mix(cursor, backdrop, drawn);
        }

        color = mix(color, cursor, mask);
    }

    // Whatever the config put over the screen goes over all of it, cursor and
    // selection included: the overlay is the glass in front of the tube.
    let glass = textureSample(foreground_tex, screen_sampler, uv);
    color = mix(color, glass.rgb, glass.a);

    return vec4<f32>(color, 1.0);
}
