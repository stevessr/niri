use std::cell::RefCell;

use glam::Mat3;
use smithay::backend::renderer::gles::{
    GlesError, GlesFrame, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
    UniformValue,
};
use smithay::backend::renderer::vulkan::{
    CustomUniformDecl, CustomUniformKind, VulkanPixelProgram, VulkanRenderer,
    texture_bindings_glsl, uniform_block_glsl,
};

use super::renderer::NiriRenderer;
use super::shader_element::ShaderProgram;
use crate::render_helpers::blur::BlurProgram;

/// Custom texture shader program for either renderer backend.
#[derive(Debug, Clone)]
pub enum NiriTexProgram {
    Gles(GlesTexProgram),
    Vulkan(VulkanPixelProgram),
}

pub struct Shaders {
    pub border: Option<ShaderProgram>,
    pub shadow: Option<ShaderProgram>,
    pub clipped_surface: Option<NiriTexProgram>,
    pub postprocess_and_clip: Option<GlesTexProgram>,
    pub output_hdr: Option<GlesTexProgram>,
    pub resize: Option<ShaderProgram>,
    pub gradient_fade: Option<GlesTexProgram>,
    pub blur: Option<BlurProgram>,
    pub custom_resize: RefCell<Option<ShaderProgram>>,
    pub custom_close: RefCell<Option<ShaderProgram>>,
    pub custom_open: RefCell<Option<ShaderProgram>>,
}

#[derive(Debug, Clone, Copy)]
pub enum ProgramType {
    Border,
    Shadow,
    Resize,
    Close,
    Open,
}

impl Shaders {
    fn compile(renderer: &mut GlesRenderer) -> Self {
        let _span = tracy_client::span!("Shaders::compile");

        let border = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("border.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("colorspace", UniformType::_1f),
                UniformName::new("hue_interpolation", UniformType::_1f),
                UniformName::new("color_from", UniformType::_4f),
                UniformName::new("color_to", UniformType::_4f),
                UniformName::new("grad_offset", UniformType::_2f),
                UniformName::new("grad_width", UniformType::_1f),
                UniformName::new("grad_vec", UniformType::_2f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("outer_radius", UniformType::_4f),
                UniformName::new("border_width", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling border shader: {err:?}");
        })
        .ok();

        let shadow = ShaderProgram::compile(
            renderer,
            concat!(
                include_str!("shadow.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| {
            warn!("error compiling shadow shader: {err:?}");
        })
        .ok();

        let clipped_surface = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("clipped_surface.frag"),
                    include_str!("rounding_alpha.frag"),
                    "\nvec4 postprocess(vec4 color) { return color; }",
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                ],
            )
            .map_err(|err| {
                warn!("error compiling clipped surface shader: {err:?}");
            })
            .ok()
            .map(NiriTexProgram::Gles);


        let postprocess_and_clip = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("clipped_surface.frag"),
                    include_str!("rounding_alpha.frag"),
                    include_str!("postprocess.frag"),
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                    UniformName::new("noise", UniformType::_1f),
                    UniformName::new("saturation", UniformType::_1f),
                    UniformName::new("bg_color", UniformType::_4f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling postprocess_and_clip shader: {err:?}");
            })
            .ok();

        let output_hdr = renderer
            .compile_custom_texture_shader(
                concat!(
                    include_str!("clipped_surface.frag"),
                    include_str!("rounding_alpha.frag"),
                    include_str!("output_hdr.frag"),
                ),
                &[
                    UniformName::new("niri_scale", UniformType::_1f),
                    UniformName::new("geo_size", UniformType::_2f),
                    UniformName::new("corner_radius", UniformType::_4f),
                    UniformName::new("input_to_geo", UniformType::Matrix3x3),
                    UniformName::new("sdr_white_nits", UniformType::_1f),
                ],
            )
            .map_err(|err| {
                warn!("error compiling output HDR shader: {err:?}");
            })
            .ok();

        let resize = compile_resize_program(renderer, include_str!("resize.frag"))
            .map_err(|err| {
                warn!("error compiling resize shader: {err:?}");
            })
            .ok();

        let gradient_fade = renderer
            .compile_custom_texture_shader(
                include_str!("gradient_fade.frag"),
                &[UniformName::new("cutoff", UniformType::_2f)],
            )
            .map_err(|err| {
                warn!("error compiling gradient fade shader: {err:?}");
            })
            .ok();

        let blur = BlurProgram::compile(renderer)
            .map_err(|err| {
                warn!("error compiling blur shaders: {err:?}");
            })
            .ok();

        Self {
            border,
            shadow,
            clipped_surface,
            postprocess_and_clip,
            output_hdr,
            resize,
            gradient_fade,
            blur,
            custom_resize: RefCell::new(None),
            custom_close: RefCell::new(None),
            custom_open: RefCell::new(None),
        }
    }

    pub fn get_from_frame<'a>(frame: &'a mut GlesFrame<'_, '_>) -> &'a Self {
        let data = frame.egl_context().user_data();
        data.get()
            .expect("shaders::init() must be called when creating the renderer")
    }

    pub fn get(renderer: &mut impl NiriRenderer) -> Option<&Self> {
        // Probe the backend before taking the long-lived renderer borrow.
        if renderer.as_gles_renderer().is_some() {
            let renderer = renderer.as_gles_renderer().unwrap();
            let data = renderer.egl_context().user_data();
            Some(
                data.get()
                    .expect("shaders::init() must be called when creating the renderer"),
            )
        } else if renderer.as_vulkan_renderer().is_some() {
            let renderer = renderer.as_vulkan_renderer().unwrap();
            renderer.user_data().get()
        } else {
            None
        }
    }

    pub fn replace_custom_resize_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_resize.replace(program)
    }

    pub fn replace_custom_close_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_close.replace(program)
    }

    pub fn replace_custom_open_program(
        &self,
        program: Option<ShaderProgram>,
    ) -> Option<ShaderProgram> {
        self.custom_open.replace(program)
    }

    pub fn program(&self, program: ProgramType) -> Option<ShaderProgram> {
        match program {
            ProgramType::Border => self.border.clone(),
            ProgramType::Shadow => self.shadow.clone(),
            ProgramType::Resize => self
                .custom_resize
                .borrow()
                .clone()
                .or_else(|| self.resize.clone()),
            ProgramType::Close => self.custom_close.borrow().clone(),
            ProgramType::Open => self.custom_open.borrow().clone(),
        }
    }
}

/// Maps a GLES uniform declaration to Smithay's Vulkan custom-program ABI.
fn uniform_name_to_decl(uniform: &UniformName<'_>) -> Option<CustomUniformDecl> {
    let kind = match uniform.type_ {
        UniformType::_1f => CustomUniformKind::Float,
        UniformType::_2f => CustomUniformKind::Vec2,
        UniformType::_3f => CustomUniformKind::Vec3,
        UniformType::_4f => CustomUniformKind::Vec4,
        UniformType::Matrix3x3 => CustomUniformKind::Mat3,
        _ => return None,
    };
    Some(CustomUniformDecl {
        name: uniform.name.clone().into_owned(),
        kind,
    })
}

/// Transforms niri's GLES-dialect fragment shaders into Vulkan GLSL.
///
/// The Vulkan renderer supplies the quad vertex stage, alpha and debug tint through push
/// constants. Shader-specific uniforms live in a generated std140 block.
fn vulkanize_fragment(src: &str, decls: &[CustomUniformDecl], textures: &[&str]) -> String {
    let mut out = String::from(
        "#version 450\n\
         #define DEBUG_FLAGS\n\
         #define texture2D texture\n",
    );
    out.push_str(&texture_bindings_glsl(textures));
    out.push_str(&uniform_block_glsl(decls));
    out.push_str(
        "layout(location = 0) in vec2 niri_v_coords;\n\
         layout(location = 0) out vec4 niri_frag_color;\n\
         #define gl_FragColor niri_frag_color\n\
         layout(push_constant) uniform NiriPush {\n\
             vec4 niri_pc0; vec4 niri_pc1; vec4 niri_pc2; vec4 niri_pc3; vec4 niri_pc4; vec4 niri_pc5;\n\
         };\n\
         #define niri_alpha niri_pc2.z\n\
         #define niri_tint niri_pc2.w\n\
         #define v_coords niri_v_coords\n",
    );

    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("#version")
            || trimmed.starts_with("#extension")
            || trimmed.starts_with("precision ")
            || trimmed.starts_with("varying ")
            || (trimmed.starts_with("uniform ") && trimmed.trim_end().ends_with(';'))
        {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }

    out
}

/// Compiles one of niri's fragment shaders for the native Vulkan renderer.
pub(super) fn compile_vulkan_program(
    renderer: &mut VulkanRenderer,
    src: &str,
    uniforms: &[UniformName<'_>],
    texture_uniforms: &[&str],
) -> anyhow::Result<VulkanPixelProgram> {
    let mut decls: Vec<CustomUniformDecl> =
        uniforms.iter().filter_map(uniform_name_to_decl).collect();

    if !decls.iter().any(|decl| decl.name == "niri_size") {
        decls.push(CustomUniformDecl {
            name: "niri_size".to_owned(),
            kind: CustomUniformKind::Vec2,
        });
    }
    if !decls.iter().any(|decl| decl.name == "niri_scale") {
        decls.push(CustomUniformDecl {
            name: "niri_scale".to_owned(),
            kind: CustomUniformKind::Float,
        });
    }

    let vulkan_src = vulkanize_fragment(src, &decls, texture_uniforms);
    renderer
        .compile_custom_pixel_shader(&vulkan_src, &decls, texture_uniforms)
        .map_err(|err| {
            if std::env::var_os("NIRI_DUMP_SHADERS").is_some() {
                for (i, line) in vulkan_src.lines().enumerate() {
                    eprintln!("{:4} {line}", i + 1);
                }
            }
            anyhow::anyhow!("error compiling Vulkan shader: {err}")
        })
}

impl Shaders {
    fn compile_vulkan(renderer: &mut VulkanRenderer) -> Self {
        let _span = tracy_client::span!("Shaders::compile_vulkan");

        // Start with shader elements that do not sample compositor-owned textures.
        // Resize/open/close remain disabled until their snapshot textures are renderer-generic.
        let border = ShaderProgram::compile_vulkan(
            renderer,
            concat!(
                include_str!("border.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("colorspace", UniformType::_1f),
                UniformName::new("hue_interpolation", UniformType::_1f),
                UniformName::new("color_from", UniformType::_4f),
                UniformName::new("color_to", UniformType::_4f),
                UniformName::new("grad_offset", UniformType::_2f),
                UniformName::new("grad_width", UniformType::_1f),
                UniformName::new("grad_vec", UniformType::_2f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("outer_radius", UniformType::_4f),
                UniformName::new("border_width", UniformType::_1f),
            ],
            &[],
        )
        .map_err(|err| warn!("error compiling Vulkan border shader: {err:?}"))
        .ok();

        let shadow = ShaderProgram::compile_vulkan(
            renderer,
            concat!(
                include_str!("shadow.frag"),
                include_str!("rounding_alpha.frag")
            ),
            &[
                UniformName::new("shadow_color", UniformType::_4f),
                UniformName::new("sigma", UniformType::_1f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("window_input_to_geo", UniformType::Matrix3x3),
                UniformName::new("window_geo_size", UniformType::_2f),
                UniformName::new("window_corner_radius", UniformType::_4f),
            ],
            &[],
        )
        .map_err(|err| warn!("error compiling Vulkan shadow shader: {err:?}"))
        .ok();

        let clipped_surface = {
            let src = concat!(
                include_str!("clipped_surface.frag"),
                include_str!("rounding_alpha.frag"),
                "\nvec4 postprocess(vec4 color) { return color; }",
            );
            let uniforms = [
                UniformName::new("niri_scale", UniformType::_1f),
                UniformName::new("geo_size", UniformType::_2f),
                UniformName::new("corner_radius", UniformType::_4f),
                UniformName::new("input_to_geo", UniformType::Matrix3x3),
                // Supplied by VulkanFrame::draw_texture_custom for texture overrides.
                UniformName::new("alpha", UniformType::_1f),
                UniformName::new("tint", UniformType::_1f),
            ];

            compile_vulkan_program(renderer, src, &uniforms, &["tex"])
                .map_err(|err| warn!("error compiling Vulkan clipped surface shader: {err:?}"))
                .ok()
                .map(NiriTexProgram::Vulkan)
        };

        Self {
            border,
            shadow,
            clipped_surface,
            postprocess_and_clip: None,
            output_hdr: None,
            resize: None,
            gradient_fade: None,
            blur: None,
            custom_resize: RefCell::new(None),
            custom_close: RefCell::new(None),
            custom_open: RefCell::new(None),
        }
    }
}

/// Compiles and stores niri shader programs for a native Vulkan renderer.
pub fn init_vulkan(renderer: &mut VulkanRenderer) {
    let shaders = Shaders::compile_vulkan(renderer);
    if !renderer.user_data().insert_if_missing(|| shaders) {
        error!("Vulkan shaders were already compiled");
    }
}

pub fn init(renderer: &mut GlesRenderer) {
    let shaders = Shaders::compile(renderer);
    let data = renderer.egl_context().user_data();
    if !data.insert_if_missing(|| shaders) {
        error!("shaders were already compiled");
    }
}

fn compile_resize_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("resize_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("resize_epilogue.frag"));
    program.push_str(include_str!("rounding_alpha.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_curr_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_prev_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_to_next_geo", UniformType::Matrix3x3),
            UniformName::new("niri_curr_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex_prev", UniformType::Matrix3x3),
            UniformName::new("niri_geo_to_tex_next", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_corner_radius", UniformType::_4f),
            UniformName::new("niri_clip_to_geometry", UniformType::_1f),
        ],
        &["niri_tex_prev", "niri_tex_next"],
    )
}

pub fn set_custom_resize_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_resize_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom resize shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) =
        Shaders::get(renderer).and_then(|s| s.replace_custom_resize_program(program))
    {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom resize shader: {err:?}");
        }
    }
}

fn compile_close_program(
    renderer: &mut GlesRenderer,
    src: &str,
) -> Result<ShaderProgram, GlesError> {
    let mut program = include_str!("close_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("close_epilogue.frag"));

    ShaderProgram::compile(
        renderer,
        &program,
        &[
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ],
        &["niri_tex"],
    )
}

pub fn set_custom_close_program(renderer: &mut GlesRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_close_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom close shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).and_then(|s| s.replace_custom_close_program(program))
    {
        if let Err(err) = prev.destroy(renderer) {
            warn!("error destroying previous custom close shader: {err:?}");
        }
    }
}

fn compile_open_program(
    renderer: &mut impl NiriRenderer,
    src: &str,
) -> anyhow::Result<ShaderProgram> {
    let mut program = include_str!("open_prelude.frag").to_string();
    program.push_str(src);
    program.push_str(include_str!("open_epilogue.frag"));

    let uniforms = &[
        UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
        UniformName::new("niri_geo_size", UniformType::_2f),
        UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
        UniformName::new("niri_progress", UniformType::_1f),
        UniformName::new("niri_clamped_progress", UniformType::_1f),
        UniformName::new("niri_random_seed", UniformType::_1f),
    ];
    let textures: &[&str] = &["niri_tex"];

    if renderer.as_gles_renderer().is_some() {
        let renderer = renderer.as_gles_renderer().unwrap();
        Ok(ShaderProgram::compile(
            renderer,
            &program,
            uniforms,
            textures,
        )?)
    } else if renderer.as_vulkan_renderer().is_some() {
        let renderer = renderer.as_vulkan_renderer().unwrap();
        ShaderProgram::compile_vulkan(renderer, &program, uniforms, textures)
    } else {
        anyhow::bail!("unsupported renderer")
    }
}

pub fn set_custom_open_program(renderer: &mut impl NiriRenderer, src: Option<&str>) {
    let program = if let Some(src) = src {
        match compile_open_program(renderer, src) {
            Ok(program) => Some(program),
            Err(err) => {
                warn!("error compiling custom open shader: {err:?}");
                return;
            }
        }
    } else {
        None
    };

    if let Some(prev) = Shaders::get(renderer).and_then(|s| s.replace_custom_open_program(program))
    {
        if let Some(gles_renderer) = renderer.as_gles_renderer() {
            if let Err(err) = prev.destroy(gles_renderer) {
                warn!("error destroying previous custom open shader: {err:?}");
            }
        }
    }
}

pub fn mat3_uniform(name: &str, mat: Mat3) -> Uniform<'_> {
    Uniform::new(
        name,
        UniformValue::Matrix3x3 {
            matrices: vec![mat.to_cols_array()],
            transpose: false,
        },
    )
}


#[cfg(test)]
mod tests {
    use smithay::backend::vulkan::{version::Version, Instance, PhysicalDevice};

    use super::*;

    #[test]
    fn native_vulkan_border_and_shadow_shaders_compile() {
        // CI environments are not required to expose a Vulkan device.
        let Ok(instance) = Instance::new(Version::VERSION_1_3, None) else {
            return;
        };
        let Ok(devices) = PhysicalDevice::enumerate(&instance) else {
            return;
        };
        let Some(mut renderer) = devices
            .into_iter()
            .find_map(|device| VulkanRenderer::new(&device).ok())
        else {
            return;
        };

        let shaders = Shaders::compile_vulkan(&mut renderer);
        assert!(
            shaders.border.is_some(),
            "native Vulkan border shader failed to compile"
        );
        assert!(
            shaders.shadow.is_some(),
            "native Vulkan shadow shader failed to compile"
        );
        assert!(
            shaders.clipped_surface.is_some(),
            "native Vulkan clipped-surface shader failed to compile"
        );

        let mut open = include_str!("open_prelude.frag").to_string();
        open.push_str(
            "vec4 open_color(vec3 coords_geo, vec3 size_geo) {\n\
                 vec3 coords = niri_geo_to_tex * coords_geo;\n\
                 vec4 color = texture2D(niri_tex, coords.st);\n\
                 return color * niri_clamped_progress;\n\
             }\n",
        );
        open.push_str(include_str!("open_epilogue.frag"));
        let uniforms = [
            UniformName::new("niri_input_to_geo", UniformType::Matrix3x3),
            UniformName::new("niri_geo_size", UniformType::_2f),
            UniformName::new("niri_geo_to_tex", UniformType::Matrix3x3),
            UniformName::new("niri_progress", UniformType::_1f),
            UniformName::new("niri_clamped_progress", UniformType::_1f),
            UniformName::new("niri_random_seed", UniformType::_1f),
        ];
        ShaderProgram::compile_vulkan(&mut renderer, &open, &uniforms, &["niri_tex"])
            .expect("native Vulkan window-open shader failed to compile");
    }
}
