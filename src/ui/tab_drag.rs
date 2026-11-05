use std::cell::RefCell;

use anyhow::{ensure, Context as _};
use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::FontDescription;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::output::Output;
use smithay::utils::{Point, Transform};

use crate::layout::TabDrag;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::{output_size, to_physical_precise_round};
use crate::window::Mapped;

const FONT: &str = "sans 12px";
const PADDING_X: i32 = 14;
const PADDING_Y: i32 = 7;
const MAX_WIDTH: i32 = 480;
const CORNER_RADIUS: f64 = 8.;
const BACKDROP: [f64; 4] = [0.09, 0.09, 0.09, 0.92];
const FOREGROUND: [f64; 3] = [1., 1., 1.];
/// Offset of the chip from the pointer.
const OFFSET: (f64, f64) = (18., 22.);

pub struct TabDragUi {
    cache: RefCell<Option<RenderedChip>>,
}

struct RenderedChip {
    title: String,
    scale: f64,
    buffer: TextureBuffer<GlesTexture>,
}

impl Default for TabDragUi {
    fn default() -> Self {
        Self::new()
    }
}

impl TabDragUi {
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(None),
        }
    }

    /// Renders the dragged tab chip, if a tab drag is ongoing on the given output.
    pub fn render(
        &self,
        renderer: &mut impl NiriRenderer,
        output: &Output,
        drag: &TabDrag<Mapped>,
        title: Option<&str>,
    ) -> Option<PrimaryGpuTextureRenderElement> {
        let pos = drag.pos_within_output?;
        let title = title.unwrap_or("(untitled)");

        let scale = output.current_scale().fractional_scale();
        let output_size = output_size(output);

        let mut cache = self.cache.borrow_mut();
        let stale = match &*cache {
            Some(chip) => chip.title != title || chip.scale != scale,
            None => true,
        };
        if stale {
            let buffer = generate_chip(renderer.as_gles_renderer(), title, scale)
                .inspect_err(|err| tracing::warn!("error generating tab drag chip: {err:?}"))
                .ok()?;
            *cache = Some(RenderedChip {
                title: title.to_owned(),
                scale,
                buffer,
            });
        }

        let chip = cache.as_ref().unwrap();
        let size = chip.buffer.logical_size();

        let mut loc = Point::from((pos.x + OFFSET.0, pos.y + OFFSET.1));
        if loc.x + size.w > output_size.to_f64().w {
            loc.x = (pos.x - OFFSET.0 - size.w).max(0.);
        }
        if loc.y + size.h > output_size.to_f64().h {
            loc.y = (pos.y - OFFSET.1 - size.h).max(0.);
        }
        let loc = loc.to_physical_precise_round(scale).to_logical(scale);

        Some(PrimaryGpuTextureRenderElement(
            TextureRenderElement::from_texture_buffer(
                chip.buffer.clone(),
                loc,
                1.,
                None,
                None,
                Kind::Unspecified,
            ),
        ))
    }
}

fn generate_chip(
    renderer: &mut GlesRenderer,
    title: &str,
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let _span = tracy_client::span!("tab_drag::generate_chip");

    let mut font = FontDescription::from_string(FONT);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    let (text_width, text_height) = {
        let measure = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
        let cr = cairo::Context::new(&measure)?;
        let layout = pangocairo::functions::create_layout(&cr);
        layout.context().set_round_glyph_positions(false);
        layout.set_single_paragraph_mode(true);
        layout.set_ellipsize(pangocairo::pango::EllipsizeMode::End);
        layout.set_width((MAX_WIDTH - 2 * PADDING_X) * pangocairo::pango::SCALE);
        layout.set_font_description(Some(&font));
        layout.set_text(title);
        layout.pixel_size()
    };
    ensure!(text_width > 0 && text_height > 0);

    let width =
        (text_width + 2 * PADDING_X).min(to_physical_precise_round::<i32>(scale, MAX_WIDTH));
    let height = text_height + 2 * PADDING_Y;

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;

    // Rounded rect backdrop.
    let radius = CORNER_RADIUS * scale;
    let (w, h) = (f64::from(width), f64::from(height));
    cr.new_sub_path();
    cr.arc(w - radius, radius, radius, -std::f64::consts::FRAC_PI_2, 0.);
    cr.arc(
        w - radius,
        h - radius,
        radius,
        0.,
        std::f64::consts::FRAC_PI_2,
    );
    cr.arc(
        radius,
        h - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    cr.arc(
        radius,
        radius,
        radius,
        std::f64::consts::PI,
        3. * std::f64::consts::FRAC_PI_2,
    );
    cr.close_path();
    cr.set_source_rgba(BACKDROP[0], BACKDROP[1], BACKDROP[2], BACKDROP[3]);
    cr.fill()?;

    // Text.
    let layout = pangocairo::functions::create_layout(&cr);
    layout.context().set_round_glyph_positions(false);
    layout.set_single_paragraph_mode(true);
    layout.set_ellipsize(pangocairo::pango::EllipsizeMode::End);
    layout.set_width((width - 2 * PADDING_X) * pangocairo::pango::SCALE);
    layout.set_font_description(Some(&font));
    layout.set_text(title);
    cr.set_source_rgb(FOREGROUND[0], FOREGROUND[1], FOREGROUND[2]);
    cr.move_to(
        f64::from(PADDING_X) + f64::from((width - 2 * PADDING_X - text_width).max(0)) / 2.,
        f64::from(PADDING_Y),
    );
    pangocairo::functions::show_layout(&cr, &layout);
    drop(cr);

    let data = surface
        .take_data()
        .context("tab drag chip surface is borrowed")?;
    let buffer = TextureBuffer::from_memory(
        renderer,
        &data,
        Fourcc::Argb8888,
        (width, height),
        false,
        scale,
        Transform::Normal,
        Vec::new(),
    )?;

    Ok(buffer)
}
