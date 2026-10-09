# A synthwave theme: a sunset over a neon grid, and the shell's own colours
# graded into the same palette so the text belongs to the picture instead of
# sitting on top of it.
#
# Copy it over the file hext writes on first run — `~/.config/hext/config.py` —
# and save it again while the terminal is open: the file is watched, so the look
# changes without a restart.
#
# A theme is a shader and the two colours the config owns (`background` and
# `cursor_color`), and this one shows why the shader is the interesting half: the
# grid is re-coloured to the theme, which is the only way to theme a terminal
# whose cells carry the shell's colours. That is the `neon()` ramp below — the
# shell's ink is turned into its brightness and walked through three stops, deep
# violet → cyan → hot pink, so `ls`, a diff and a status line all come out in the
# theme. It is a colour grade, so brightness is kept: the brightest colour in the
# shell's palette is the brightest colour on screen.
#
# The rest is the picture: a sky that goes from night to magenta at the horizon,
# a sun sliced by its own bands, a floor grid marching towards the viewer, stars,
# a horizon line, and — over all of it — a vignette, scanlines and a slow flicker.
# `animate = True` is what keeps it moving.
#
# This is the second of the two examples: `docs/config-aurora.py` is the other,
# and it animates from Python as well as from the shader.

shader = """
// The synthwave screen: a sunset over a neon grid, with the grid inked over it
// in the theme's own colours.
//
// `screen.time` is what makes it move — the floor grid scrolling, the sun
// breathing, the stars twinkling — and `animate = True` below is what keeps
// `time` ticking.
//
// The struct matches the uniform `src/render/drawable.rs` writes, field for
// field, and `pad` is only there to keep the size a multiple of sixteen.
struct Screen {
    background: vec4<f32>,
    cursor_color: vec4<f32>,
    resolution: vec2<f32>,
    grid: vec2<u32>,
    cursor: vec2<u32>,
    cursor_visible: u32,
    cursor_style: u32,
    cursor_size: vec2<f32>,
    time: f32,
    pad: u32,
}

@group(0) @binding(0) var<uniform> screen: Screen;
@group(0) @binding(1) var screen_tex: texture_2d<f32>;
@group(0) @binding(2) var screen_sampler: sampler;
@group(0) @binding(3) var background_tex: texture_2d<f32>;
@group(0) @binding(4) var foreground_tex: texture_2d<f32>;

// Where the sky meets the floor, in 0..1 from the top of the window.
const HORIZON: f32 = 0.62;

const CURSOR_BLOCK: u32 = 0u;
const CURSOR_BAR: u32 = 1u;

@vertex
fn vs_screen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));

    return vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}

// A number that jumps about between 0 and 1 for every input: what the stars are
// scattered by.
fn hash(value: vec2<f32>) -> f32 {
    return fract(sin(dot(value, vec2<f32>(127.1, 311.7))) * 43758.5453);
}

// How bright a colour is, for telling a cell's ink from its background.
fn luma(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// The theme's own palette: whatever colour the shell asked for is turned into
// its brightness and read back out of this ramp, so every cell on screen is in
// the theme. Brightness is what is preserved, so brighter still reads as louder.
fn neon(color: vec3<f32>) -> vec3<f32> {
    let level = clamp(luma(color), 0.0, 1.0);

    let deep = vec3<f32>(0.14, 0.04, 0.34);
    let mid = vec3<f32>(0.10, 0.85, 0.95);
    let hot = vec3<f32>(1.00, 0.22, 0.72);

    if (level < 0.5) {
        return mix(deep, mid, level * 2.0);
    }

    return mix(mid, hot, (level - 0.5) * 2.0);
}

// The floor: a grid receding to the horizon, its rows marching towards the
// viewer. The division by depth is what makes the rows evenly spaced in the
// picture rather than on the floor, which is what reads as perspective.
fn floor_grid(uv: vec2<f32>, time: f32) -> f32 {
    if (uv.y <= HORIZON) {
        return 0.0;
    }

    let depth = (uv.y - HORIZON) / (1.0 - HORIZON);
    let scale = max(depth, 0.05);

    let rows = abs(fract(0.7 / scale - time * 0.5) - 0.5);
    let columns = abs(fract((uv.x - 0.5) / scale * 1.4) - 0.5);
    let edge = min(rows, columns);

    // A thin line where the distance to one is small, and brighter near the
    // viewer, so the grid fades out into the horizon rather than ending.
    return (1.0 - smoothstep(0.0, 0.035, edge)) * depth;
}

// The sun on the horizon: a disc, with the half below its middle cut into
// horizontal slices, the way a synthwave sun sets.
fn sun(uv: vec2<f32>, time: f32) -> f32 {
    let centre = vec2<f32>(0.5, 0.50);
    let radius = 0.24 + 0.006 * sin(time * 0.6);
    let offset = vec2<f32>((uv.x - centre.x) * 1.5, uv.y - centre.y);
    let disc = 1.0 - smoothstep(radius - 0.008, radius + 0.008, length(offset));

    let below = step(0.0, uv.y - centre.y);
    let slices = step(0.45, fract((uv.y - centre.y) * 26.0 + time * 0.15));

    return disc * mix(1.0, slices, below);
}

// What the cursor covers in a cell, the way the built-in shader works it out.
fn cursor_mask(local: vec2<f32>, size: vec2<f32>, style: u32) -> f32 {
    let text = min(screen.cursor_size, size);
    let origin = (size - text) * 0.5;
    let inside = all(local >= origin) && all(local < origin + text);

    if (style == CURSOR_BAR) {
        let bar = max(1.0, text.x * 0.125);

        return select(0.0, 1.0, inside && local.x < origin.x + bar);
    }

    return select(0.0, 1.0, inside);
}

@fragment
fn fs_screen(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
    let uv = position.xy / screen.resolution;
    let size = max(screen.cursor_size, vec2<f32>(1.0));
    let sun_centre = vec2<f32>(0.5, 0.50);

    // The sky: night at the top, magenta where it meets the floor.
    var color = mix(
        vec3<f32>(0.04, 0.01, 0.11),
        vec3<f32>(0.30, 0.05, 0.38),
        pow(1.0 - uv.y, 2.2),
    );

    // Stars, twinkling: one candidate per cell of a coarse grid, and only the
    // very few whose hash is near the top are drawn.
    let cells = vec2<f32>(uv.x * 110.0, uv.y * 60.0);
    let star_cell = floor(cells);
    let seed = hash(star_cell);
    let spark = 1.0 - smoothstep(0.04, 0.18, length(abs(fract(cells) - 0.5)));
    color = color + vec3<f32>(0.9, 0.9, 1.0)
        * spark
        * step(0.9965, seed)
        * (0.5 + 0.5 * sin(screen.time * 2.4 + seed * 40.0));

    // The glow the sun throws up into the sky.
    let away = uv - sun_centre;
    color = color + vec3<f32>(0.85, 0.16, 0.50) * (0.18 / (1.0 + 26.0 * dot(away, away)));

    // The sun itself: yellow where it meets the floor, pink at the top.
    let disc = sun(uv, screen.time);
    let sun_color = mix(
        vec3<f32>(1.00, 0.85, 0.25),
        vec3<f32>(1.00, 0.20, 0.75),
        clamp((uv.y - sun_centre.y) / 0.3 + 0.5, 0.0, 1.0),
    );
    color = mix(color, sun_color, disc);

    // The floor grid, and the bright line where it meets the sky.
    color = color + vec3<f32>(0.15, 0.95, 1.00) * floor_grid(uv, screen.time) * 0.9;
    color = color + vec3<f32>(0.90, 0.25, 0.85) * (1.0 - smoothstep(0.0, 0.0025, abs(uv.y - HORIZON)));

    // Everything above is the backdrop: the glass the cursor falls back to, and
    // the thing the grid is inked over.
    let backdrop = color;

    // The grid, graded into the theme and sampled a pixel apart per channel, so
    // the ink fringes like a tube that has not quite converged.
    let pixel = 1.0 / screen.resolution;
    let left = textureSample(screen_tex, screen_sampler, uv - vec2<f32>(pixel.x, 0.0));
    let here = textureSample(screen_tex, screen_sampler, uv);
    let right = textureSample(screen_tex, screen_sampler, uv + vec2<f32>(pixel.x, 0.0));

    let ink = vec3<f32>(neon(left.rgb).r, neon(here.rgb).g, neon(right.rgb).b);
    color = mix(color, ink, here.a);

    // A cheap glow: the two neighbours' ink is added back over this pixel, so
    // bright text lights the space around it. A real bloom blurs a downsampled
    // copy of the frame; at a cell's size this is close enough and far cheaper.
    let above = textureSample(screen_tex, screen_sampler, uv - vec2<f32>(0.0, pixel.y * 2.0));
    let below = textureSample(screen_tex, screen_sampler, uv + vec2<f32>(0.0, pixel.y * 2.0));
    color = color + neon((above.rgb + below.rgb) * 0.5) * (above.a + below.a) * 0.05;

    // The cursor: cyan by default, and a block keeps the character under it
    // readable by falling back to the backdrop wherever the ink brightened the
    // pixel — the same bargain the built-in shader makes.
    let cell = min(
        vec2<u32>(position.xy / size),
        max(screen.grid, vec2<u32>(1u)) - vec2<u32>(1u),
    );

    if (screen.cursor_visible != 0u && all(cell == screen.cursor)) {
        let local = position.xy - vec2<f32>(cell) * size;
        let mask = cursor_mask(local, size, screen.cursor_style);
        var cursor = screen.cursor_color.rgb;

        if (screen.cursor_style == CURSOR_BLOCK) {
            let drawn = clamp((luma(color) - luma(backdrop)) * 4.0, 0.0, 1.0);

            cursor = mix(cursor, backdrop, drawn);
        }

        color = mix(color, cursor, mask);
    }

    // The glass: the corners of a tube fall away, its lines are drawn across the
    // picture, and the supply is never perfectly steady.
    let centred = (uv - vec2<f32>(0.5)) * vec2<f32>(1.0, screen.resolution.y / screen.resolution.x);
    color = color * (1.0 - 0.85 * pow(dot(centred, centred), 1.5));
    color = color * (0.90 + 0.10 * sin(position.y * 3.14159265));
    color = color * (0.985 + 0.015 * sin(screen.time * 24.0));

    // Whatever the config put over everything goes over all of it. There is
    // nothing there here, but this is where a `foreground_image` would land.
    let glass = textureSample(foreground_tex, screen_sampler, uv);
    color = mix(color, glass.rgb, glass.a);

    return vec4<f32>(color, 1.0);
}
"""

# Whether to draw a frame every frame: the floor grid scrolls, the sun breathes
# and the stars twinkle, so this theme wants one. Nothing is drawn while the
# window is in the background.
animate = True

# The two colours the config owns, as (r, g, b) in 0.0..=1.0. The sky is drawn
# by the shader above, so `background` is mostly the colour the window is cleared
# to before it; it is also what a render function would read back out of
# `world.terminal()`. The cursor is the theme's cyan, which the shader draws.
background = (0.04, 0.01, 0.11)
cursor_color = (0.15, 0.95, 1.00)
