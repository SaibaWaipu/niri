use std::collections::HashMap;
use std::iter::zip;
use std::mem;

use anyhow::{ensure, Context as _};
use niri_config::{Color, CornerRadius, Gradient, GradientRelativeTo, TabIndicatorPosition};
use pangocairo::cairo::{self, ImageSurface};
use pangocairo::pango::{self, FontDescription};
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::utils::{Logical, Point, Rectangle, Size, Transform};
use tracing::warn;

use super::tile::Tile;
use super::LayoutElement;
use crate::animation::{Animation, Clock};
use crate::niri_render_elements;
use crate::render_helpers::border::BorderRenderElement;
use crate::render_helpers::primary_gpu_texture::PrimaryGpuTextureRenderElement;
use crate::render_helpers::renderer::NiriRenderer;
use crate::render_helpers::texture::{TextureBuffer, TextureRenderElement};
use crate::utils::{
    floor_logical_in_physical_max1, round_logical_in_physical, round_logical_in_physical_max1,
    to_physical_precise_round,
};

const TITLE_FONT: &str = "sans 11px";
/// Padding between the tab edges and the title text, in logical pixels.
const TITLE_PADDING: f64 = 8.;
/// Minimum width a tab needs to render its title, in logical pixels.
const MIN_TITLE_WIDTH: f64 = 48.;
/// Minimum height a tab needs to render its title, in logical pixels.
const MIN_TITLE_HEIGHT: f64 = 14.;

#[derive(Debug)]
pub struct TabIndicator {
    shader_locs: Vec<Point<f64, Logical>>,
    shaders: Vec<BorderRenderElement>,
    open_anim: Option<Animation>,
    config: niri_config::TabIndicator,
    title_tabs: Vec<TitleTab>,
    title_cache: std::cell::RefCell<HashMap<TitleKey, TextureBuffer<GlesTexture>>>,
}

#[derive(Debug)]
struct TitleTab {
    title: String,
    rect: Rectangle<f64, Logical>,
    rgb: (u8, u8, u8),
    scale: f64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TitleKey {
    title: String,
    width: i32,
    rgb: (u8, u8, u8),
    scale: u64,
}

#[derive(Debug, Clone)]
pub struct TabInfo {
    /// Gradient for the tab indicator.
    pub gradient: Gradient,
    /// Tab geometry in the same coordinate system as the area.
    pub geometry: Rectangle<f64, Logical>,
    /// Window title, used to render the title on the tab when titles are enabled.
    pub title: Option<String>,
    /// Whether this tab belongs to the active tile.
    pub is_active: bool,
}

niri_render_elements! {
    TabIndicatorRenderElement => {
        Gradient = BorderRenderElement,
        Title = PrimaryGpuTextureRenderElement,
    }
}

impl TabIndicator {
    pub fn new(config: niri_config::TabIndicator) -> Self {
        Self {
            shader_locs: Vec::new(),
            shaders: Vec::new(),
            open_anim: None,
            config,
            title_tabs: Vec::new(),
            title_cache: std::cell::RefCell::new(HashMap::new()),
        }
    }

    pub fn update_config(&mut self, config: niri_config::TabIndicator) {
        self.config = config;
    }

    pub fn update_shaders(&mut self) {
        for elem in &mut self.shaders {
            elem.damage_all();
        }
    }

    pub fn advance_animations(&mut self) {
        if let Some(anim) = &mut self.open_anim {
            if anim.is_done() {
                self.open_anim = None;
            }
        }
    }

    pub fn are_animations_ongoing(&self) -> bool {
        self.open_anim.is_some()
    }

    pub fn start_open_animation(&mut self, clock: Clock, config: niri_config::Animation) {
        self.open_anim = Some(Animation::new(clock, 0., 1., 0., config));
    }

    fn tab_rects(
        &self,
        area: Rectangle<f64, Logical>,
        count: usize,
        scale: f64,
    ) -> impl Iterator<Item = Rectangle<f64, Logical>> {
        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let round_max1 = |logical: f64| round_logical_in_physical_max1(scale, logical);

        let progress = self.open_anim.as_ref().map_or(1., |a| a.value().max(0.));

        let width = round_max1(self.config.width);
        let gap = self.config.gap;
        let gap = round_max1(gap.abs()).copysign(gap);
        let gaps_between = round_max1(self.config.gaps_between_tabs);

        let position = self.config.position;
        let side = match position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Right => area.size.h,
            TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => area.size.w,
        };
        let total_prop = self.config.length.total_proportion.unwrap_or(0.5);
        let min_length = round(side * total_prop.clamp(0., 2.));

        // Compute px_per_tab before applying the animation to gaps_between in order to avoid it
        // growing and shrinking over the duration of the animation.
        let pixel = 1. / scale;
        let shortest_length = count as f64 * (pixel + gaps_between) - gaps_between;
        let length = f64::max(min_length, shortest_length);
        let px_per_tab = (length + gaps_between) / count as f64 - gaps_between;

        let px_per_tab = px_per_tab * progress;
        let gaps_between = round(self.config.gaps_between_tabs * progress);

        let length = count as f64 * (px_per_tab + gaps_between) - gaps_between;
        let px_per_tab = floor_logical_in_physical_max1(scale, px_per_tab);
        let floored_length = count as f64 * (px_per_tab + gaps_between) - gaps_between;
        let mut ones_left = ((length - floored_length) / pixel).round() as usize;

        let mut shader_loc = Point::from((-gap - width, round((side - length) / 2.)));
        match position {
            TabIndicatorPosition::Left => (),
            TabIndicatorPosition::Right => shader_loc.x = area.size.w + gap,
            TabIndicatorPosition::Top => mem::swap(&mut shader_loc.x, &mut shader_loc.y),
            TabIndicatorPosition::Bottom => {
                shader_loc.x = shader_loc.y;
                shader_loc.y = area.size.h + gap;
            }
        }
        shader_loc += area.loc;

        (0..count).map(move |_| {
            let mut px_per_tab = px_per_tab;
            if ones_left > 0 {
                ones_left -= 1;
                px_per_tab += pixel;
            }

            let loc = shader_loc;

            match position {
                TabIndicatorPosition::Left | TabIndicatorPosition::Right => {
                    shader_loc.y += px_per_tab + gaps_between
                }
                TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => {
                    shader_loc.x += px_per_tab + gaps_between
                }
            }

            let size = match position {
                TabIndicatorPosition::Left | TabIndicatorPosition::Right => {
                    Size::from((width, px_per_tab))
                }
                TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => {
                    Size::from((px_per_tab, width))
                }
            };

            Rectangle::new(loc, size)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_render_elements(
        &mut self,
        enabled: bool,
        // Geometry of the tabs area.
        area: Rectangle<f64, Logical>,
        // View rect relative to the tabs area.
        area_view_rect: Rectangle<f64, Logical>,
        // Tab count, should match the tabs iterator length.
        tab_count: usize,
        tabs: impl Iterator<Item = TabInfo>,
        is_active: bool,
        scale: f64,
    ) {
        if !enabled || self.config.off {
            self.shader_locs.clear();
            self.shaders.clear();
            self.title_tabs.clear();
            return;
        }

        let count = tab_count;
        if self.config.hide_when_single_tab && count == 1 {
            self.shader_locs.clear();
            self.shaders.clear();
            self.title_tabs.clear();
            return;
        }

        self.shaders.resize_with(count, Default::default);
        self.shader_locs.resize_with(count, Default::default);

        let position = self.config.position;
        let radius = self.config.corner_radius as f32;
        let shared_rounded_corners = self.config.gaps_between_tabs == 0.;
        let mut tabs_left = tab_count;

        let tabs: Vec<_> = tabs.collect();
        let rects = self.tab_rects(area, count, scale);
        for ((shader, loc), (tab, rect)) in zip(
            zip(&mut self.shaders, &mut self.shader_locs),
            zip(&tabs, rects),
        ) {
            *loc = rect.loc;

            let mut gradient_area = match tab.gradient.relative_to {
                GradientRelativeTo::Window => tab.geometry,
                GradientRelativeTo::WorkspaceView => area_view_rect,
            };
            gradient_area.loc -= *loc;

            let mut color_from = tab.gradient.from;
            let mut color_to = tab.gradient.to;
            if !is_active {
                color_from *= 0.5;
                color_to *= 0.5;
            }

            let radius = if shared_rounded_corners && tab_count > 1 {
                if tabs_left == tab_count {
                    // First tab.
                    match position {
                        TabIndicatorPosition::Left | TabIndicatorPosition::Right => CornerRadius {
                            top_left: radius,
                            top_right: radius,
                            bottom_right: 0.,
                            bottom_left: 0.,
                        },
                        TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => CornerRadius {
                            top_left: radius,
                            top_right: 0.,
                            bottom_right: 0.,
                            bottom_left: radius,
                        },
                    }
                } else if tabs_left == 1 {
                    // Last tab.
                    match position {
                        TabIndicatorPosition::Left | TabIndicatorPosition::Right => CornerRadius {
                            top_left: 0.,
                            top_right: 0.,
                            bottom_right: radius,
                            bottom_left: radius,
                        },
                        TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => CornerRadius {
                            top_left: 0.,
                            top_right: radius,
                            bottom_right: radius,
                            bottom_left: 0.,
                        },
                    }
                } else {
                    // Tab in the middle.
                    CornerRadius::default()
                }
            } else {
                // Separate tabs, or the only tab.
                CornerRadius::from(radius)
            };
            let radius = radius.fit_to(rect.size.w as f32, rect.size.h as f32);
            tabs_left -= 1;

            shader.update(
                rect.size,
                gradient_area,
                tab.gradient.in_,
                color_from,
                color_to,
                ((tab.gradient.angle as f32) - 90.).to_radians(),
                Rectangle::from_size(rect.size),
                0.,
                radius,
                scale as f32,
                1.,
            );
        }

        self.title_tabs.clear();
        let shows_titles = self.config.show_titles
            && matches!(
                position,
                TabIndicatorPosition::Top | TabIndicatorPosition::Bottom
            );
        if shows_titles {
            let rects = self.tab_rects(area, count, scale);
            for (tab, rect) in zip(&tabs, rects) {
                let Some(title) = &tab.title else {
                    continue;
                };
                if rect.size.w < MIN_TITLE_WIDTH || rect.size.h < MIN_TITLE_HEIGHT {
                    continue;
                }
                self.title_tabs.push(TitleTab {
                    title: title.clone(),
                    rect,
                    rgb: contrast_rgb(tab.gradient.from),
                    scale,
                });
            }
        }
    }

    pub fn hit(
        &self,
        area: Rectangle<f64, Logical>,
        tab_count: usize,
        scale: f64,
        point: Point<f64, Logical>,
    ) -> Option<usize> {
        if self.config.off {
            return None;
        }

        let count = tab_count;
        if self.config.hide_when_single_tab && count == 1 {
            return None;
        }

        self.tab_rects(area, count, scale)
            .enumerate()
            .find_map(|(idx, rect)| rect.contains(point).then_some(idx))
    }

    pub fn render(
        &self,
        renderer: &mut impl NiriRenderer,
        pos: Point<f64, Logical>,
        push: &mut dyn FnMut(TabIndicatorRenderElement),
    ) {
        if BorderRenderElement::has_shader(renderer) {
            for (shader, loc) in zip(&self.shaders, &self.shader_locs) {
                let elem = shader.clone().with_location(pos + *loc);
                push(TabIndicatorRenderElement::from(elem));
            }
        }

        self.render_titles(renderer, pos, push);
    }

    fn render_titles(
        &self,
        renderer: &mut impl NiriRenderer,
        pos: Point<f64, Logical>,
        push: &mut dyn FnMut(TabIndicatorRenderElement),
    ) {
        if self.title_tabs.is_empty() {
            return;
        }

        let mut cache = self.title_cache.borrow_mut();
        let mut used = Vec::with_capacity(self.title_tabs.len());
        for tab in &self.title_tabs {
            let width_px = ((tab.rect.size.w - 2. * TITLE_PADDING) * tab.scale)
                .round()
                .max(0.) as i32;
            if width_px <= 0 {
                continue;
            }

            let key = TitleKey {
                title: tab.title.clone(),
                width: width_px,
                rgb: tab.rgb,
                scale: tab.scale.to_bits(),
            };
            if !cache.contains_key(&key) {
                let generated = generate_title_texture(
                    renderer.as_gles_renderer(),
                    &tab.title,
                    width_px,
                    tab.rgb,
                    tab.scale,
                );
                match generated {
                    Ok(buffer) => {
                        cache.insert(key.clone(), buffer);
                    }
                    Err(err) => {
                        warn!("error generating tab title texture: {err:?}");
                        continue;
                    }
                }
            }
            used.push(key.clone());

            let buffer = cache.get(&key).unwrap().clone();
            let size = buffer.logical_size();
            let x = pos.x + tab.rect.loc.x + (tab.rect.size.w - size.w) / 2.;
            let y = pos.y + tab.rect.loc.y + (tab.rect.size.h - size.h) / 2.;
            let loc = Point::from((x, y))
                .to_physical_precise_round(tab.scale)
                .to_logical(tab.scale);
            let elem = TextureRenderElement::from_texture_buffer(
                buffer,
                loc,
                1.,
                None,
                None,
                Kind::Unspecified,
            );
            push(TabIndicatorRenderElement::from(
                PrimaryGpuTextureRenderElement(elem),
            ));
        }

        cache.retain(|key, _| used.contains(key));
    }

    /// Extra size occupied by the tab indicator.
    pub fn extra_size(&self, tab_count: usize, scale: f64) -> Size<f64, Logical> {
        if self.config.off
            || !self.config.place_within_column
            || (self.config.hide_when_single_tab && tab_count == 1)
        {
            return Size::from((0., 0.));
        }

        let round = |logical: f64| round_logical_in_physical(scale, logical);
        let width = round(self.config.width);
        let gap = round(self.config.gap);

        // No, I am *not* falling into the rabbit hole of "what if the tab indicator is wide enough
        // that it peeks from the other side of the window".
        let size = f64::max(0., width + gap);

        match self.config.position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Right => Size::from((size, 0.)),
            TabIndicatorPosition::Top | TabIndicatorPosition::Bottom => Size::from((0., size)),
        }
    }

    /// Offset of the tabbed content due to space occupied by the tab indicator.
    pub fn content_offset(&self, tab_count: usize, scale: f64) -> Point<f64, Logical> {
        match self.config.position {
            TabIndicatorPosition::Left | TabIndicatorPosition::Top => {
                self.extra_size(tab_count, scale).to_point()
            }
            TabIndicatorPosition::Right | TabIndicatorPosition::Bottom => Point::from((0., 0.)),
        }
    }

    pub fn config(&self) -> niri_config::TabIndicator {
        self.config
    }
}

impl TabInfo {
    pub fn from_tile<W: LayoutElement>(
        tile: &Tile<W>,
        position: Point<f64, Logical>,
        is_active: bool,
        is_urgent: bool,
        config: &niri_config::TabIndicator,
    ) -> Self {
        let rules = tile.window().rules();
        let rule = rules.tab_indicator;

        let gradient_from_rule = || {
            let (color, gradient) = if is_urgent {
                (rule.urgent_color, rule.urgent_gradient)
            } else if is_active {
                (rule.active_color, rule.active_gradient)
            } else {
                (rule.inactive_color, rule.inactive_gradient)
            };
            let color = color.map(Gradient::from);
            gradient.or(color)
        };

        let gradient_from_config = || {
            let (color, gradient) = if is_urgent {
                (config.urgent_color, config.urgent_gradient)
            } else if is_active {
                (config.active_color, config.active_gradient)
            } else {
                (config.inactive_color, config.inactive_gradient)
            };
            let color = color.map(Gradient::from);
            gradient.or(color)
        };

        let gradient_from_border = || {
            // Come up with tab indicator gradient matching the focus ring or the border, whichever
            // one is enabled.
            let focus_ring_config = tile.focus_ring().config();
            let border_config = tile.border().config();
            let config = if focus_ring_config.off {
                border_config
            } else {
                focus_ring_config
            };

            let (color, gradient) = if is_urgent {
                (config.urgent_color, config.urgent_gradient)
            } else if is_active {
                (config.active_color, config.active_gradient)
            } else {
                (config.inactive_color, config.inactive_gradient)
            };
            gradient.unwrap_or_else(|| Gradient::from(color))
        };

        let gradient = gradient_from_rule()
            .or_else(gradient_from_config)
            .unwrap_or_else(gradient_from_border);

        let geometry = Rectangle::new(position, tile.animated_tile_size());

        TabInfo {
            gradient,
            geometry,
            title: tile.window().title(),
            is_active,
        }
    }
}

/// Returns a black or white color that contrasts with the given color.
fn contrast_rgb(color: Color) -> (u8, u8, u8) {
    let luminance = 0.2126 * color.r + 0.7152 * color.g + 0.0722 * color.b;
    if luminance > 0.5 {
        (0, 0, 0)
    } else {
        (255, 255, 255)
    }
}

fn generate_title_texture(
    renderer: &mut GlesRenderer,
    title: &str,
    max_width: i32,
    rgb: (u8, u8, u8),
    scale: f64,
) -> anyhow::Result<TextureBuffer<GlesTexture>> {
    let _span = tracy_client::span!("tab_indicator::generate_title_texture");

    let mut font = FontDescription::from_string(TITLE_FONT);
    font.set_absolute_size(to_physical_precise_round(scale, font.size()));

    let make_layout = |cr: &cairo::Context| {
        let layout = pangocairo::functions::create_layout(cr);
        layout.context().set_round_glyph_positions(false);
        layout.set_single_paragraph_mode(true);
        layout.set_ellipsize(pango::EllipsizeMode::End);
        layout.set_width(max_width * pango::SCALE);
        layout.set_font_description(Some(&font));
        layout.set_text(title);
        layout
    };

    let surface = ImageSurface::create(cairo::Format::ARgb32, 0, 0)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = make_layout(&cr);
    let (width, height) = layout.pixel_size();
    ensure!(width > 0 && height > 0);
    drop(layout);

    // Guard against overly long window titles.
    let width = width.min(16383);
    let height = height.min(16383);

    let surface = ImageSurface::create(cairo::Format::ARgb32, width, height)?;
    let cr = cairo::Context::new(&surface)?;
    let layout = make_layout(&cr);
    cr.set_source_rgb(
        f64::from(rgb.0) / 255.,
        f64::from(rgb.1) / 255.,
        f64::from(rgb.2) / 255.,
    );
    pangocairo::functions::show_layout(&cr, &layout);
    drop(cr);

    let data = surface
        .take_data()
        .context("tab title surface is borrowed elsewhere")?;
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
