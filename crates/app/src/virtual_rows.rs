//! Variable-height rows. Unvisited rows use an estimate, never a text layout.
use eframe::egui;

#[derive(Default)]
pub struct VirtualRows {
    key: Option<(u64, u32, u32)>,
    heights: Vec<f32>,
    tree: Vec<f32>,
}
impl VirtualRows {
    pub fn sync(&mut self, revision: u64, count: usize, width: f32, zoom: f32, estimate: f32) {
        let key = (revision, width.to_bits(), zoom.to_bits());
        if self.key == Some(key) && self.heights.len() == count {
            return;
        }
        let same_geometry = self.key.is_some_and(|(_, w, z)| w == key.1 && z == key.2);
        if !same_geometry {
            self.heights.clear();
        }
        self.heights.resize(count, estimate);
        self.tree = vec![0.0; count + 1];
        for index in 0..count {
            let mut i = index + 1;
            while i <= count {
                self.tree[i] += self.heights[index];
                i += i.isolate_lowest_one();
            }
        }
        self.key = Some(key);
    }
    pub fn offset(&self, count: usize) -> f32 {
        let mut i = count.min(self.heights.len());
        let mut sum = 0.0;
        while i > 0 {
            sum += self.tree[i];
            i &= i - 1;
        }
        sum
    }
    pub fn total(&self) -> f32 {
        self.offset(self.heights.len())
    }
    pub fn height(&self, index: usize) -> f32 {
        self.heights[index]
    }
    pub fn set_height(&mut self, index: usize, height: f32) {
        let delta = height - self.heights[index];
        if delta.abs() < 0.5 {
            return;
        }
        self.heights[index] = height;
        let mut i = index + 1;
        while i < self.tree.len() {
            self.tree[i] += delta;
            i += i.isolate_lowest_one();
        }
    }
    pub fn row_at(&self, y: f32) -> usize {
        let mut index = 0;
        let mut sum = 0.0;
        let mut bit = self.heights.len().next_power_of_two();
        while bit > 0 {
            let next = index + bit;
            if next < self.tree.len() && sum + self.tree[next] <= y {
                sum += self.tree[next];
                index = next;
            }
            bit >>= 1;
        }
        index.min(self.heights.len().saturating_sub(1))
    }
    pub fn visible(&self, viewport: egui::Rect) -> std::ops::Range<usize> {
        self.row_at(viewport.top()).saturating_sub(1)
            ..(self.row_at(viewport.bottom()) + 2).min(self.heights.len())
    }
}

pub fn hash(value: impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// A variable-height section inside an existing scroll area, without nested scrolling.
pub fn show<T>(
    ui: &mut egui::Ui,
    rows: &mut VirtualRows,
    revision: u64,
    items: &[T],
    estimate: f32,
    mut render: impl FnMut(&mut egui::Ui, &T),
) {
    let width = ui.available_width();
    rows.sync(
        revision,
        items.len(),
        width,
        ui.ctx().pixels_per_point(),
        estimate,
    );
    if items.is_empty() {
        return;
    }
    let origin = ui.cursor().min;
    let viewport = ui.clip_rect().translate(-origin.to_vec2());
    if viewport.bottom() >= 0.0 && viewport.top() <= rows.total() {
        for index in rows.visible(viewport) {
            let mut child = ui.new_child(egui::UiBuilder::new().id_salt(index).max_rect(
                egui::Rect::from_min_size(
                    origin + egui::vec2(0.0, rows.offset(index)),
                    egui::vec2(width, f32::INFINITY),
                ),
            ));
            render(&mut child, &items[index]);
            let height = child.min_rect().height() + ui.spacing().item_spacing.y;
            let old = rows.offset(index + 1) - rows.offset(index);
            rows.set_height(index, height);
            if (height - old).abs() >= 0.5 {
                ui.ctx().request_repaint();
            }
        }
    }
    ui.allocate_exact_size(egui::vec2(width, rows.total()), egui::Sense::hover());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn height_updates_and_width_changes_preserve_indexed_offsets() {
        let mut rows = VirtualRows::default();
        rows.sync(1, 5000, 600.0, 1.0, 80.0);
        rows.set_height(3, 180.0);
        assert_eq!(rows.offset(4), 420.0);
        assert_eq!(rows.row_at(400.0), 3);
        assert!(
            rows.visible(egui::Rect::from_min_max(
                egui::pos2(0.0, 10000.0),
                egui::pos2(600.0, 10600.0)
            ))
            .len()
                < 12
        );
        rows.sync(2, 5001, 600.0, 1.0, 80.0);
        assert_eq!(rows.offset(4), 420.0);
        rows.sync(2, 5001, 300.0, 1.5, 80.0);
        assert_eq!(rows.offset(4), 320.0);
    }
}
