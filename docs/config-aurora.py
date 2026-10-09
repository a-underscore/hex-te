# An animated hext configuration: the reference for what a moving terminal can
# be. Copy it over the file hext writes on first run — `~/.config/hext/config.py`
# — and save it again while the terminal is open: the file is watched, so the
# change lands without a restart.
#
# Two things here move, in the two different ways a config can:
#
#   * the shader draws its own backdrop — an aurora over a night sky — from the
#     `screen.time` field, which is what `animate = True` keeps ticking;
#   * a render function reaches the terminal with `world.terminal()` once per
#     frame, and walks the cursor's colour along with the Python clock.
#
# The grid is drawn over the sky, the vignette and the scanlines go over that,
# and a `foreground_image`, if one were named, would be the glass in front.

# The screen shader, as WGSL source. It has to declare the `vs_screen` and
# `fs_screen` entry points and the bindings `src/render/shaders/screen.wgsl`
# uses — group 0, bindings 0 to 4: the uniform, the screen texture (the grid as
# ink, its alpha the coverage), a sampler, the picture behind the grid and the
# picture over everything. A source that does not compile is reported and the
# built-in shader used instead, so a typo here is a note rather than a terminal
# that will not start.
shader = """
// The aurora screen: a night sky with bands of light drifting across it, drawn
// by the shader itself, with the grid inked over the top, a vignette and
// scanlines. `screen.time` is what makes it move; `animate = True` below is what
// keeps `time` ticking.
//
// The struct matches the uniform `src/render/drawable.rs` writes, field for
// field: the two colours, the resolution, the grid and cursor state, and the
// clock. `pad` is only there to keep the size a multiple of sixteen, which is
// what a uniform buffer binding wants.
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

const CURSOR_BAR: u32 = 1u;
const CURSOR_BLOCK: u32 = 0u;

@vertex
fn vs_screen(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
    let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));

    return vec4<f32>(corner * 2.0 - vec2<f32>(1.0), 0.0, 1.0);
}

// A number that jumps about between 0 and 1 for every input, which is what the
// dither below is made of. Only ever fed a pixel position, never the clock, so
// the pattern it makes is fixed to the screen.
fn hash(value: vec2<f32>) -> f32 {
    return fract(sin(dot(value, vec2<f32>(12.9898, 78.233))) * 43758.5453);
}

// How bright a colour is, for telling a cell's ink from its background.
fn luma(color: vec3<f32>) -> f32 {
    return dot(color, vec3<f32>(0.2126, 0.7152, 0.0722));
}

// The sky and the aurora at `uv`, in 0..1.
fn sky(uv: vec2<f32>, time: f32) -> vec3<f32> {
    // Brighter towards the horizon, so the bands have something to stand
    // against.
    let horizon = 1.0 - uv.y;
    var color = mix(
        vec3<f32>(0.010, 0.015, 0.035),
        vec3<f32>(0.030, 0.045, 0.090),
        horizon,
    );

    // Four bands, each a sine of the x position folded by the y one, drifting
    // sideways at its own speed. They add, so where two cross the light builds.
    for (var band = 0; band < 4; band = band + 1) {
        let index = f32(band);
        let drift = time * (0.20 + 0.07 * index);
        let centre = 0.20 + 0.09 * index
            + 0.04 * sin(uv.x * 5.0 + drift)
            + 0.015 * sin(uv.x * 13.0 - drift * 1.7);
        let width = 0.10 - 0.015 * index;
        let offset = (uv.y - centre) / width;
        let light = exp(-offset * offset);

        // Green low down, teal in the middle, violet at the top: the colours an
        // aurora is actually seen in.
        let tint = mix(
            vec3<f32>(0.20, 0.95, 0.55),
            vec3<f32>(0.45, 0.35, 0.95),
            clamp(index * 0.34, 0.0, 1.0),
        );

        color = color + tint * light * (0.30 - 0.05 * index);
    }

    return color;
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

    // The sky and its aurora, which is what the grid is inked onto.
    var color = sky(uv, screen.time);

    // The grid is ink rather than a picture: its alpha is how much of the pixel
    // a cell covers, so a cell that asked for a colour of its own is opaque and
    // a cell a program left unpainted is not.
    let ink = textureSample(screen_tex, screen_sampler, uv);
    color = mix(color, ink.rgb, ink.a);

    // The cursor: its colour is the terminal's, so a render function in the
    // Python below can walk it about.
    let cell = min(
        vec2<u32>(position.xy / size),
        max(screen.grid, vec2<u32>(1u)) - vec2<u32>(1u),
    );

    if (screen.cursor_visible != 0u && all(cell == screen.cursor)) {
        let local = position.xy - vec2<f32>(cell) * size;
        let mask = cursor_mask(local, size, screen.cursor_style);
        var cursor = screen.cursor_color.rgb;

        // A block cursor keeps the character under it readable, the way the
        // built-in shader does: it takes the colour where the cell is background
        // and the sky where the character's ink is.
        if (screen.cursor_style == CURSOR_BLOCK) {
            let drawn = clamp((luma(color) - luma(sky(uv, screen.time))) * 4.0, 0.0, 1.0);

            cursor = mix(cursor, sky(uv, screen.time), drawn);
        }

        color = mix(color, cursor, mask);
    }

    // A vignette: the corners of the screen fall away, the way a tube does.
    let centred = (uv - vec2<f32>(0.5)) * vec2<f32>(1.0, screen.resolution.y / screen.resolution.x);
    color = color * (1.0 - 0.9 * pow(dot(centred, centred), 1.4));

    // Scanlines: every other line of the image is a little darker.
    color = color * (0.92 + 0.08 * sin(position.y * 3.14159265));

    // A one-step dither, so the flat parts of the sky do not band: a two
    // hundred and fifty-fifth of a step, fixed to the pixel rather than to the
    // frame. Noise that is redrawn every frame is what makes an animated image
    // look grainy — the sky crawls — so `screen.time` is deliberately not part
    // of it.
    color = color + vec3<f32>((hash(position.xy) - 0.5) / 255.0);

    // Whatever the config put over everything goes over all of it: the glass in
    // front of the tube. There is nothing there here, but the composite is what
    // a `foreground_image` would land in.
    let glass = textureSample(foreground_tex, screen_sampler, uv);
    color = mix(color, glass.rgb, glass.a);

    return vec4<f32>(color, 1.0);
}
"""

# Whether to draw a frame every frame. `True` is what a shader with a clock in it
# needs, and what the render function below rides on; nothing is drawn while the
# window is in the background, so the animation pauses rather than running
# unseen.
animate = True

# Colours, as (r, g, b) in 0.0..=1.0. The sky is drawn by the shader, so the
# background colour is only what the window is cleared to before it — and what
# the grid's unpainted cells fall back to if the shader stops using it.
background = (0.02, 0.02, 0.05)
cursor_color = (0.20, 0.95, 0.60)

# A render function: a system in the frame's pipeline, handed the world just
# before the frame is drawn. `world.terminal()` is the running terminal, so this
# is where the terminal's own colours can be animated from Python.
import math
import time

started = time.monotonic()


def drift(world):
    terminal = world.terminal()

    # The file is read before the app builds the terminal, so a system is the
    # only place this can be non-None.
    if terminal is None:
        return

    phase = time.monotonic() - started
    pulse = 0.5 + 0.5 * math.sin(phase * 0.8)

    # The same greens as the bands above, so the cursor seems lit by them. The
    # colour lands on the next frame, which is a frame too soon to see.
    terminal.cursor_color = (
        0.15 + 0.20 * pulse,
        0.75 + 0.20 * pulse,
        0.45 + 0.30 * (1.0 - pulse),
    )


world.add_system(drift, pipeline=render_pipeline)
