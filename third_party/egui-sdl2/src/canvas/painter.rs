//! Canvas backend for egui-sdl2.
//!
//! This module provides [`Painter`], which integrates egui rendering with an
//! SDL2 [`Canvas`] — a window's, or a surface's when the app draws offscreen.

use egui::epaint::{ImageDelta, Primitive};
use egui::{ClippedPrimitive, ImageData, TexturesDelta};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use sdl2::render::{BlendMode, Canvas, RenderTarget, Texture, TextureCreator};
use sdl2::surface::{Surface, SurfaceContext};
use sdl2::sys::{SDL_Color, SDL_FPoint, SDL_Vertex};
use sdl2::video::{Window, WindowContext};
use std::collections::HashMap;
use std::os::raw::c_int;

/// The format egui's own pixels are already in: an upload into it is a copy.
#[cfg(target_endian = "little")]
pub const DEFAULT_FORMAT: PixelFormatEnum = PixelFormatEnum::ABGR8888;
/// The format egui's own pixels are already in: an upload into it is a copy.
#[cfg(target_endian = "big")]
pub const DEFAULT_FORMAT: PixelFormatEnum = PixelFormatEnum::RGBA8888;

pub(crate) const BYTES_PER_PIXEL: usize = 4;

/// The format to paint in for `canvas`: [`DEFAULT_FORMAT`] where the renderer
/// takes it, otherwise the first 32-bit format with alpha it lists. A format the
/// renderer lacks is converted on every upload, whole-frame under
/// [`crate::Renderer::CanvasBlit`]; a shuffle per atlas change is the cheaper end.
pub fn preferred_format<T: RenderTarget>(canvas: &Canvas<T>) -> PixelFormatEnum {
    let formats = canvas.info().texture_formats;
    if formats.contains(&DEFAULT_FORMAT) {
        return DEFAULT_FORMAT;
    }
    formats
        .into_iter()
        .find(|&format| channel_offsets(format).is_some())
        .unwrap_or(DEFAULT_FORMAT)
}

/// Byte offsets of R, G, B and A within a pixel of `format`, the order an upload
/// writes them in. `None` unless the format is 32-bit with alpha.
fn channel_offsets(format: PixelFormatEnum) -> Option<[usize; BYTES_PER_PIXEL]> {
    let masks = format.into_masks().ok()?;
    if masks.bpp as usize != BYTES_PER_PIXEL * 8 || masks.amask == 0 {
        return None;
    }
    // A mask names bits of the pixel word, whose low byte is first only on LE.
    let offset = |mask: u32| {
        let byte = (mask.trailing_zeros() / 8) as usize;
        if cfg!(target_endian = "little") {
            byte
        } else {
            BYTES_PER_PIXEL - 1 - byte
        }
    };
    Some([
        offset(masks.rmask),
        offset(masks.gmask),
        offset(masks.bmask),
        offset(masks.amask),
    ])
}

/// An Canvas painter using [`sdl2`].
///
/// This is responsible for painting egui and managing egui textures. The
/// [`Canvas`] stays owned by the caller and is passed in per paint call, so egui
/// can draw over content the application already rendered.
///
/// This struct must be destroyed with [`Painter::destroy`] before dropping, to ensure
/// objects have been properly deleted and are not leaked.
///
/// NOTE: all egui viewports share the same painter.
pub struct Painter<C = WindowContext> {
    textures: HashMap<egui::TextureId, Texture>,
    texture_creator: TextureCreator<C>,
    /// Reused across meshes and frames so `paint_mesh` repacks egui vertices
    /// into SDL's layout without allocating a fresh `Vec` per mesh.
    vertex_scratch: Vec<SDL_Vertex>,
    /// Clip rect currently applied to the canvas within a `paint_primitives`
    /// run, so meshes sharing a clip skip a redundant `SDL_RenderSetClipRect`.
    /// Reset to `None` at the start of each run because the caller draws to the
    /// same canvas between runs.
    last_clip: Option<Rect>,
    /// The area this run may write, from [`Painter::paint_primitives_within`].
    damage: Option<Rect>,
    /// Triangles waiting to be drawn, so a run of them is still one SDL call.
    index_scratch: Vec<u32>,
    /// Reused for the straight-alpha copy an upload needs; the atlas is uploaded
    /// whole whenever it grows, which is not the frame to be allocating in.
    pixel_scratch: Vec<u8>,
    /// This renderer's texture size limit, for egui to lay its atlas out within.
    max_texture_side: Option<usize>,
    /// The format egui's textures are held in.
    format: PixelFormatEnum,
    /// `format`'s channel order, resolved once for the uploads.
    channels: [usize; BYTES_PER_PIXEL],
}

impl Painter<WindowContext> {
    /// Textures are created from `canvas`'s renderer, so pass the same canvas to
    /// the paint calls. Paints in [`preferred_format`] for that renderer.
    pub fn new(canvas: &Canvas<Window>) -> Self {
        Self::with_format(canvas, preferred_format(canvas))
    }

    /// [`Self::new`] in a format of the caller's choosing.
    pub fn with_format(canvas: &Canvas<Window>, format: PixelFormatEnum) -> Self {
        Self::with_creator(canvas.texture_creator(), max_texture_side(canvas), format)
    }
}

impl<'s> Painter<SurfaceContext<'s>> {
    /// Paint into a surface instead of a window, for drivers that only present
    /// texture copies (see [`crate::Renderer::CanvasBlit`]).
    pub fn for_surface(canvas: &Canvas<Surface<'s>>) -> Self {
        Self::for_surface_with_format(canvas, preferred_format(canvas))
    }

    /// [`Self::for_surface`] in a format of the caller's choosing. Give it the
    /// surface's own and the presenting window's, and the frame travels as bytes.
    pub fn for_surface_with_format(canvas: &Canvas<Surface<'s>>, format: PixelFormatEnum) -> Self {
        Self::with_creator(canvas.texture_creator(), max_texture_side(canvas), format)
    }
}

impl<C> Painter<C> {
    fn with_creator(
        texture_creator: TextureCreator<C>,
        max_texture_side: Option<usize>,
        format: PixelFormatEnum,
    ) -> Self {
        // Painting in a format no egui texture fits would show as a blank UI.
        let (format, channels) = match channel_offsets(format) {
            Some(channels) => (format, channels),
            None => {
                log::warn!(
                    "{format:?} cannot hold an egui texture; painting in {DEFAULT_FORMAT:?}"
                );
                let channels = channel_offsets(DEFAULT_FORMAT)
                    .expect("the default format is 32-bit with alpha by construction");
                (DEFAULT_FORMAT, channels)
            }
        };
        Self {
            textures: HashMap::new(),
            texture_creator,
            vertex_scratch: Vec::new(),
            index_scratch: Vec::new(),
            pixel_scratch: Vec::new(),
            last_clip: None,
            damage: None,
            max_texture_side,
            format,
            channels,
        }
    }

    /// The format egui's textures are held in — what a surface or a presentation
    /// texture built around this painter should also be.
    pub fn format(&self) -> PixelFormatEnum {
        self.format
    }

    /// The largest texture this renderer accepts, `None` if it reports no limit.
    /// Feed [`crate::State::set_max_texture_side`]: egui's atlas defaults to
    /// 2048, past what handheld drivers hold (Miyoo Mini: 1920x1080).
    pub fn max_texture_side(&self) -> Option<usize> {
        self.max_texture_side
    }

    /// This function must be called before [`Painter`] is dropped, as [`Painter`] has some objects
    /// that should be deleted.
    pub fn destroy(&mut self) {
        let textures = std::mem::replace(&mut self.textures, HashMap::with_capacity(0));
        for (_id, tex) in textures {
            unsafe {
                tex.destroy();
            }
        }
    }

    /// You are expected to have cleared the color buffer before calling this.
    ///
    /// The deltas are drained: egui 0.36 asserts on drop that every delta was
    /// handled, so applying consumes them.
    pub fn paint_and_update_textures<T: RenderTarget<Context = C>>(
        &mut self,
        canvas: &mut Canvas<T>,
        pixels_per_point: f32,
        textures_delta: &mut TexturesDelta,
        paint_jobs: Vec<ClippedPrimitive>,
    ) -> Result<(), String> {
        // egui 0.36 batches several deltas per texture; apply them in order.
        for (id, deltas) in textures_delta.set.drain() {
            for delta in deltas {
                self.set_texture(id, &delta);
            }
        }

        self.paint_primitives(canvas, pixels_per_point, paint_jobs);

        for id in textures_delta.free.drain() {
            self.free_texture(&id);
        }

        Ok(())
    }

    /// Main entry-point for painting a frame.
    pub fn paint_primitives<T: RenderTarget<Context = C>>(
        &mut self,
        canvas: &mut Canvas<T>,
        pixels_per_point: f32,
        paint_jobs: Vec<ClippedPrimitive>,
    ) {
        self.paint_primitives_within(canvas, pixels_per_point, paint_jobs, None)
    }

    /// [`Self::paint_primitives`] confined to `damage` (in pixels): a mesh
    /// outside it is dropped before it reaches the renderer, and one crossing its
    /// edge is clipped to it. For a caller repainting the part of a frame that
    /// changed — on a software renderer the pixels are the cost, and most frames
    /// change few of them.
    pub fn paint_primitives_within<T: RenderTarget<Context = C>>(
        &mut self,
        canvas: &mut Canvas<T>,
        pixels_per_point: f32,
        paint_jobs: Vec<ClippedPrimitive>,
        damage: Option<Rect>,
    ) {
        // The caller may have drawn to the canvas (and changed its clip) since
        // the last run, so don't assume any clip is still applied.
        self.last_clip = None;
        self.damage = damage;
        // Untextured geometry and rectangle fills blend by the renderer's mode,
        // which SDL leaves at `None` — so a half-transparent fill would overwrite
        // instead of blending. Textures carry their own mode (`create_texture`).
        let caller_blend = canvas.blend_mode();
        canvas.set_blend_mode(BlendMode::Blend);
        for job in paint_jobs.into_iter() {
            match job.primitive {
                Primitive::Mesh(mesh) => {
                    self.paint_mesh(canvas, pixels_per_point, job.clip_rect, mesh)
                }
                Primitive::Callback(_callback) => {
                    // TODO
                    log::warn!("PaintCallbacks are not supported")
                }
            }
        }
        // Clear the clip once, after all meshes, so content the caller draws
        // after `paint()` isn't clipped to the last mesh's rect. Guard on
        // `last_clip`: a frame that drew no meshes never set a clip, so leave
        // the caller's own clip untouched.
        if self.last_clip.is_some() {
            canvas.set_clip_rect(None);
        }
        canvas.set_blend_mode(caller_blend);
    }

    pub fn set_texture(&mut self, id: egui::TextureId, delta: &ImageDelta) {
        let ImageData::Color(img) = &delta.image;
        // Straight alpha, to match the vertex colours: see `into_sdl_vertex`. The
        // font atlas arrives as premultiplied white coverage, and becomes white
        // with the coverage in alpha, which is what modulating a texture expects.
        self.pixel_scratch.clear();
        self.pixel_scratch
            .reserve(img.pixels.len() * BYTES_PER_PIXEL);
        let [r_at, g_at, b_at, a_at] = self.channels;
        if self.channels == [0, 1, 2, 3] {
            for pixel in img.pixels.iter() {
                self.pixel_scratch
                    .extend_from_slice(&pixel.to_srgba_unmultiplied());
            }
        } else {
            for pixel in img.pixels.iter() {
                let [r, g, b, a] = pixel.to_srgba_unmultiplied();
                let mut texel = [0u8; BYTES_PER_PIXEL];
                texel[r_at] = r;
                texel[g_at] = g;
                texel[b_at] = b;
                texel[a_at] = a;
                self.pixel_scratch.extend_from_slice(&texel);
            }
        }
        let w = img.width() as u32;
        let h = img.height() as u32;
        let pitch = (w as usize) * BYTES_PER_PIXEL;

        if delta.pos.is_none() {
            if let Some(tex) = self.textures.get(&id) {
                let q = tex.query();
                if q.width != w || q.height != h {
                    self.free_texture(&id);
                }
            }
        }

        let format = self.format;
        let tex = self
            .textures
            .entry(id)
            .or_insert_with(|| create_texture(&self.texture_creator, w, h, format));
        let rect = delta.pos.map(|[x, y]| Rect::new(x as i32, y as i32, w, h));
        tex.update(rect, &self.pixel_scratch, pitch).unwrap();
    }

    #[inline]
    pub fn free_texture(&mut self, id: &egui::TextureId) {
        if let Some(tex) = self.textures.remove(id) {
            unsafe {
                tex.destroy();
            }
        }
    }

    #[inline]
    fn paint_mesh<T: RenderTarget<Context = C>>(
        &mut self,
        canvas: &mut Canvas<T>,
        pixels_per_point: f32,
        clip_rect: egui::Rect,
        mesh: egui::Mesh,
    ) {
        // egui may draw untextured shapes (nullptr in SDL_RenderGeometry).
        let (texture_ptr, texture_size) = match self.textures.get(&mesh.texture_id) {
            Some(tex) => {
                let q = tex.query();
                (tex.raw(), Some((q.width as f32, q.height as f32)))
            }
            None => (std::ptr::null_mut(), None),
        };

        let min = clip_rect.min * pixels_per_point;
        let max = clip_rect.max * pixels_per_point;
        let clip_rect = sdl2::rect::Rect::new(
            min.x as i32,
            min.y as i32,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        );
        // Confined to the damaged area, and dropped when it falls outside it.
        let clip_rect = match self.damage {
            Some(damage) => match clip_rect.intersection(damage) {
                Some(clipped) => clipped,
                None => return,
            },
            None => clip_rect,
        };
        // Adjacent meshes (e.g. all glyphs in one panel) usually share a clip;
        // only hit `SDL_RenderSetClipRect` when it actually changes.
        if self.last_clip != Some(clip_rect) {
            canvas.set_clip_rect(clip_rect);
            self.last_clip = Some(clip_rect);
        }

        // Text and rectangles tessellate to axis-aligned quads: blit those, they
        // are exact and cheap. Rounded corners, circles and feathering stay on
        // the triangle path. Flushing before each blit keeps egui's draw order.
        self.index_scratch.clear();
        for corners in mesh.indices.chunks(6) {
            match as_axis_aligned_quad(&mesh.vertices, corners, pixels_per_point) {
                Some(quad) => {
                    self.flush_triangles(canvas, texture_ptr, &mesh, pixels_per_point);
                    quad.blit(canvas, texture_ptr, texture_size);
                }
                None => self.index_scratch.extend_from_slice(corners),
            }
        }
        self.flush_triangles(canvas, texture_ptr, &mesh, pixels_per_point);
    }

    /// Draw whatever indices have accumulated in `index_scratch` as triangles.
    fn flush_triangles<T: RenderTarget<Context = C>>(
        &mut self,
        canvas: &mut Canvas<T>,
        texture_ptr: *mut sdl2_sys::SDL_Texture,
        mesh: &egui::Mesh,
        pixels_per_point: f32,
    ) {
        if self.index_scratch.is_empty() {
            return;
        }
        // A blit may have left a colour mod; vertex colours carry their own tint.
        if !texture_ptr.is_null() {
            unsafe {
                sdl2_sys::SDL_SetTextureColorMod(texture_ptr, 255, 255, 255);
                sdl2_sys::SDL_SetTextureAlphaMod(texture_ptr, 255);
            }
        }

        // Repack egui vertices into SDL's layout in a reused buffer. A zero-copy
        // cast is impossible (SDL_Vertex is {position, color, tex_coord} vs egui
        // {pos, uv, color}, and position is scaled by ppp), but reusing the
        // allocation across meshes/frames avoids a malloc+free per mesh.
        self.vertex_scratch.clear();
        self.vertex_scratch.reserve(mesh.vertices.len());
        self.vertex_scratch.extend(
            mesh.vertices
                .iter()
                .map(|v| into_sdl_vertex(v, pixels_per_point)),
        );
        // A feather vertex is fully transparent and epaint gives it no hue, so
        // SDL's straight-alpha interpolation would fade it to black. With its
        // triangle's own hue the ramp stays on colour — antialiasing, not a
        // dark fringe. Corners are only shared within one path, so a triangle
        // may safely write the hue its path owns.
        for triangle in self.index_scratch.as_chunks::<3>().0 {
            let hue = triangle
                .iter()
                .map(|&i| self.vertex_scratch[i as usize].color)
                .find(|c| c.a != 0);
            let Some(hue) = hue else { continue };
            for &i in triangle {
                let c = &mut self.vertex_scratch[i as usize].color;
                if c.a == 0 {
                    (c.r, c.g, c.b) = (hue.r, hue.g, hue.b);
                }
            }
        }
        let verts_len = self.vertex_scratch.len() as c_int;
        let indcs_len = self.index_scratch.len() as c_int;

        let result = unsafe {
            sdl2_sys::SDL_RenderGeometry(
                canvas.raw(),
                texture_ptr,
                if verts_len == 0 {
                    std::ptr::null()
                } else {
                    self.vertex_scratch.as_ptr()
                },
                verts_len,
                self.index_scratch.as_ptr() as *const c_int,
                indcs_len,
            )
        };
        self.index_scratch.clear();

        if result != 0 {
            log::error!("SDL_RenderGeometry failed: {}", result);
        }
    }
}

/// An axis-aligned, single-colour quad: a glyph, or a plain rectangle.
struct Quad {
    dst: sdl2_sys::SDL_FRect,
    /// Normalized, as egui gives it; scaled to texels at blit time.
    uv: egui::Rect,
    color: egui::Color32,
    textured: bool,
}

impl Quad {
    fn blit<T: RenderTarget>(
        &self,
        canvas: &mut Canvas<T>,
        texture_ptr: *mut sdl2_sys::SDL_Texture,
        texture_size: Option<(f32, f32)>,
    ) {
        let [r, g, b, a] = self.color.to_srgba_unmultiplied();
        let result = match (self.textured, texture_size) {
            (true, Some((tw, th))) => unsafe {
                // The atlas is white with coverage in alpha; modulating it matches
                // what the triangle path does per vertex.
                sdl2_sys::SDL_SetTextureColorMod(texture_ptr, r, g, b);
                sdl2_sys::SDL_SetTextureAlphaMod(texture_ptr, a);
                let src = sdl2_sys::SDL_Rect {
                    x: (self.uv.min.x * tw).round() as i32,
                    y: (self.uv.min.y * th).round() as i32,
                    w: (self.uv.width() * tw).round() as i32,
                    h: (self.uv.height() * th).round() as i32,
                };
                let dst = unscaled_onto_pixels(&self.dst, &src);
                sdl2_sys::SDL_RenderCopyF(canvas.raw(), texture_ptr, &src, &dst)
            },
            _ => unsafe {
                sdl2_sys::SDL_SetRenderDrawColor(canvas.raw(), r, g, b, a);
                sdl2_sys::SDL_RenderFillRectF(canvas.raw(), &self.dst)
            },
        };
        if result != 0 {
            log::error!("blitting a quad failed: {result}");
        }
    }
}

/// A copy of the source's own size, landed on whole pixels. `src` is whole
/// texels, so a fractional `dst` makes SDL resample a 1:1 copy and drop a row
/// or a column of it — a glyph loses the crossbar of an H, the arm of an F, the
/// middle of an S. Anything genuinely scaled keeps the rect it asked for.
fn unscaled_onto_pixels(
    dst: &sdl2_sys::SDL_FRect,
    src: &sdl2_sys::SDL_Rect,
) -> sdl2_sys::SDL_FRect {
    /// How far a copy may sit from the source's size and still be one.
    const UNSCALED: f32 = 0.5;
    let (w, h) = (src.w as f32, src.h as f32);
    if (dst.w - w).abs() > UNSCALED || (dst.h - h).abs() > UNSCALED {
        return *dst;
    }
    sdl2_sys::SDL_FRect {
        x: dst.x.round(),
        y: dst.y.round(),
        w,
        h,
    }
}

/// The two triangles of an unrotated, single-colour quad, if that is what these
/// indices are; otherwise `None`, for [`Painter::flush_triangles`].
fn as_axis_aligned_quad(
    vertices: &[egui::epaint::Vertex],
    corners: &[u32],
    pixels_per_point: f32,
) -> Option<Quad> {
    if corners.len() != 6 {
        return None;
    }
    let mut uniq: Vec<&egui::epaint::Vertex> = Vec::with_capacity(4);
    for &i in corners {
        let v = vertices.get(i as usize)?;
        if !uniq.iter().any(|u| u.pos == v.pos && u.uv == v.uv) {
            uniq.push(v);
        }
    }
    if uniq.len() != 4 {
        return None;
    }
    let color = uniq[0].color;
    if uniq.iter().any(|v| v.color != color) {
        return None;
    }

    let rect = egui::Rect::from_points(&uniq.iter().map(|v| v.pos).collect::<Vec<_>>());
    let uv = egui::Rect::from_points(&uniq.iter().map(|v| v.uv).collect::<Vec<_>>());
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return None;
    }
    // A degenerate uv means the mesh samples egui's single white texel: a fill.
    let textured = uv.width() > 0.0 && uv.height() > 0.0;

    for v in &uniq {
        let at_min_x = v.pos.x == rect.min.x;
        let at_min_y = v.pos.y == rect.min.y;
        if !(at_min_x || v.pos.x == rect.max.x) || !(at_min_y || v.pos.y == rect.max.y) {
            return None; // a vertex off the corners: not a rectangle
        }
        // Reject rotated and mirrored mappings; SDL_RenderCopy cannot express them.
        if textured && (at_min_x != (v.uv.x == uv.min.x) || at_min_y != (v.uv.y == uv.min.y)) {
            return None;
        }
    }

    Some(Quad {
        dst: sdl2_sys::SDL_FRect {
            x: rect.min.x * pixels_per_point,
            y: rect.min.y * pixels_per_point,
            w: rect.width() * pixels_per_point,
            h: rect.height() * pixels_per_point,
        },
        uv,
        color,
        textured,
    })
}

/// SDL leaves the fields at 0 for drivers with no limit, its software one included.
fn max_texture_side<T: RenderTarget>(canvas: &Canvas<T>) -> Option<usize> {
    let info = canvas.info();
    let side = match (info.max_texture_width, info.max_texture_height) {
        (0, 0) => return None,
        (0, h) => h,
        (w, 0) => w,
        (w, h) => w.min(h),
    };
    Some(side as usize)
}

#[inline]
fn create_texture<C>(
    texture_creator: &TextureCreator<C>,
    w: u32,
    h: u32,
    format: PixelFormatEnum,
) -> Texture {
    let mut tex = texture_creator
        .create_texture_streaming(format, w, h)
        .unwrap_or_else(|e| {
            // Reached only if egui asked for more than the renderer's limit,
            // which `Painter::max_texture_side` exists to prevent — so the
            // integration failed to pass it on.
            panic!("failed to create a {w}x{h} sdl2 texture: {e}")
        });
    tex.set_blend_mode(BlendMode::Blend);

    tex
}
/// egui's colours are premultiplied; SDL's `BLEND` is `src*a + dst*(1-a)`, which
/// multiplies by alpha a second time. Undo the premultiplication and the two
/// agree — otherwise everything drawn with partial alpha comes out too dark, and
/// an untextured fill (whose blend mode SDL defaults to `NONE`) loses the
/// destination entirely, so egui's fade-in of a new panel reads as a dark flash.
#[inline]
fn into_sdl_vertex(vertex: &egui::epaint::Vertex, pixels_per_point: f32) -> SDL_Vertex {
    let [r, g, b, a] = vertex.color.to_srgba_unmultiplied();
    SDL_Vertex {
        position: SDL_FPoint {
            x: vertex.pos.x * pixels_per_point,
            y: vertex.pos.y * pixels_per_point,
        },
        color: SDL_Color { r, g, b, a },
        tex_coord: SDL_FPoint {
            x: vertex.uv.x,
            y: vertex.uv.y,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_format_takes_egui_bytes_as_they_come() {
        assert_eq!(channel_offsets(DEFAULT_FORMAT), Some([0, 1, 2, 3]));
    }

    #[test]
    fn a_channel_order_is_the_format_s_own() {
        // BGRA in memory, which is what the Miyoo Mini's driver holds.
        assert_eq!(
            channel_offsets(PixelFormatEnum::ARGB8888),
            Some([2, 1, 0, 3])
        );
        assert_eq!(
            channel_offsets(PixelFormatEnum::BGRA8888),
            Some([1, 2, 3, 0])
        );
    }

    /// A glyph at a fractional zoom: the quad is the source's size but lands
    /// between pixels, and the row SDL would drop is a crossbar.
    #[test]
    fn an_unscaled_copy_lands_on_whole_pixels() {
        let src = sdl2_sys::SDL_Rect {
            x: 0,
            y: 0,
            w: 9,
            h: 14,
        };
        let dst = sdl2_sys::SDL_FRect {
            x: 134.62,
            y: 87.25,
            w: 9.0,
            h: 13.75,
        };
        let snapped = unscaled_onto_pixels(&dst, &src);
        assert_eq!(
            (snapped.x, snapped.y, snapped.w, snapped.h),
            (135.0, 87.0, 9.0, 14.0)
        );
    }

    /// An image asked for at a size of its own keeps it; only a copy that was
    /// already 1:1 is snapped.
    #[test]
    fn a_scaled_copy_keeps_the_rect_it_asked_for() {
        let src = sdl2_sys::SDL_Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 64,
        };
        let dst = sdl2_sys::SDL_FRect {
            x: 10.5,
            y: 20.5,
            w: 128.0,
            h: 128.0,
        };
        let kept = unscaled_onto_pixels(&dst, &src);
        assert_eq!((kept.x, kept.y, kept.w, kept.h), (10.5, 20.5, 128.0, 128.0));
    }

    #[test]
    fn nothing_short_of_32_bit_with_alpha_holds_a_texture() {
        for format in [
            PixelFormatEnum::RGB565,
            PixelFormatEnum::RGB24,
            PixelFormatEnum::RGBX8888,
            PixelFormatEnum::YV12,
        ] {
            assert_eq!(channel_offsets(format), None, "{format:?}");
        }
    }
}
