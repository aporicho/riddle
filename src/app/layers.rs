//! Software layers for modal UI and transient pointer feedback.

use crate::fb::{screen_h, screen_w};
use crate::surface::{OwnedSurface, Surface, WHITE};
use crate::ui::pointer::HitRect;

pub(super) struct ModalLayers {
    paper: OwnedSurface,
    ui: Option<OwnedSurface>,
}

impl ModalLayers {
    pub(super) fn new(front: &Surface) -> Self {
        let mut paper = OwnedSurface::new_like(front, WHITE);
        paper.copy_from_surface(front, full_rect());
        Self { paper, ui: None }
    }

    pub(super) fn is_active(&self) -> bool {
        self.ui.is_some()
    }

    pub(super) fn replace_ui<R>(
        &mut self,
        front: &mut Surface,
        draw: impl FnOnce(&mut Surface) -> R,
    ) -> R {
        if !self.is_active() {
            self.paper.copy_from_surface(front, full_rect());
        }
        self.ui = Some(OwnedSurface::new_like(front, WHITE));
        self.with_ui(draw)
            .expect("replace_ui creates an active UI layer")
    }

    pub(super) fn with_ui<R>(&mut self, draw: impl FnOnce(&mut Surface) -> R) -> Option<R> {
        let ui = self.ui.as_mut()?;
        let mut surface = ui.as_surface();
        Some(draw(&mut surface))
    }

    pub(super) fn compose_all(&mut self, front: &mut Surface) {
        self.compose_rect(front, full_rect());
    }

    pub(super) fn compose_rect(&mut self, front: &mut Surface, rect: HitRect) {
        if let Some(ui) = self.ui.as_mut() {
            ui.copy_to_surface(front, rect);
        } else {
            self.paper.copy_to_surface(front, rect);
        }
    }

    pub(super) fn dismiss(&mut self, front: &mut Surface) {
        self.ui = None;
        self.paper.copy_to_surface(front, full_rect());
    }

    pub(super) fn render_preview(
        &mut self,
        front: &mut Surface,
        rect: HitRect,
        draw: impl FnOnce(&mut Surface),
    ) -> Option<HitRect> {
        let rect = rect.clipped_to(front.w, front.h)?;
        self.compose_rect(front, rect);
        draw(front);
        Some(rect)
    }

    pub(super) fn clear_preview(&mut self, front: &mut Surface, rect: HitRect) -> Option<HitRect> {
        let rect = rect.clipped_to(front.w, front.h)?;
        self.compose_rect(front, rect);
        Some(rect)
    }
}

fn full_rect() -> HitRect {
    HitRect::from_xywh(0, 0, screen_w() as i32, screen_h() as i32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::{PixFmt, BLACK, FADED};

    fn front() -> (Vec<u8>, Surface) {
        let mut bytes = vec![0xff; 64 * 64 * 4];
        let surface = Surface::new(
            bytes.as_mut_ptr(),
            bytes.len(),
            64,
            64,
            64 * 4,
            PixFmt::Rgb32,
        );
        (bytes, surface)
    }

    #[test]
    fn preview_never_mutates_saved_paper_or_ui_layer() {
        crate::fb::init_screen(64, 64);
        let (_bytes, mut front) = front();
        front.fill_rect(5, 5, 10, 10, BLACK);
        let mut layers = ModalLayers::new(&front);
        layers.replace_ui(&mut front, |ui| {
            ui.fill_rect(0, 0, 64, 64, WHITE);
            ui.fill_rect(20, 20, 10, 10, FADED);
        });
        layers.compose_all(&mut front);
        let before_ui = front.copy_rect(0, 0, 64, 64);

        let rect = HitRect::from_xywh(0, 0, 64, 64);
        layers.render_preview(&mut front, rect, |surface| {
            surface.fill_rect(1, 1, 62, 4, BLACK);
        });
        assert_eq!(front.luma(2, 2), 0);

        layers.clear_preview(&mut front, rect);
        assert_eq!(front.copy_rect(0, 0, 64, 64), before_ui);

        layers.dismiss(&mut front);
        assert_eq!(front.luma(6, 6), 0);
        assert_eq!(front.luma(22, 22), 255);
    }

    #[test]
    fn nested_modal_replaces_ui_without_resnapshotting_paper() {
        crate::fb::init_screen(64, 64);
        let (_bytes, mut front) = front();
        front.fill_rect(5, 5, 10, 10, BLACK);
        let mut layers = ModalLayers::new(&front);

        layers.replace_ui(&mut front, |ui| {
            ui.fill_rect(0, 0, 64, 64, WHITE);
            ui.fill_rect(20, 20, 10, 10, FADED);
        });
        layers.compose_all(&mut front);
        assert!(front.luma(22, 22) < 255);

        layers.replace_ui(&mut front, |ui| {
            ui.fill_rect(0, 0, 64, 64, WHITE);
            ui.fill_rect(40, 40, 10, 10, BLACK);
        });
        layers.compose_all(&mut front);
        assert_eq!(front.luma(42, 42), 0);

        layers.dismiss(&mut front);
        assert_eq!(front.luma(6, 6), 0);
        assert_eq!(front.luma(22, 22), 255);
        assert_eq!(front.luma(42, 42), 255);
    }
}
