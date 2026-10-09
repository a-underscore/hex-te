#[test]
fn screen_shader_parses_and_validates() {
    let source = include_str!("../src/render/shaders/screen.wgsl");
    let module = naga::front::wgsl::parse_str(source).expect("the screen shader must parse");

    for entry in ["vs_screen", "fs_screen"] {
        assert!(module.entry_points.iter().any(|point| point.name == entry));
    }

    naga::valid::Validator::new(
        naga::valid::ValidationFlags::all(),
        naga::valid::Capabilities::empty(),
    )
    .validate(&module)
    .expect("the screen shader must validate");
}
