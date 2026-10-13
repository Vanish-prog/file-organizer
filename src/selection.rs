use std::collections::BTreeSet;

#[derive(Debug, Default, Clone)]
pub struct Selection {
    pub selected: BTreeSet<usize>,
    pub anchor: Option<usize>,
}

impl Selection {
    pub fn click(&mut self, index: usize, ctrl: bool, shift: bool, len: usize) {
        if index >= len {
            return;
        }
        self.selected.retain(|&i| i < len);
        if shift {
            let anchor = self.anchor.filter(|&i| i < len).unwrap_or(index);
            if !ctrl {
                self.selected.clear();
            }
            self.selected.extend(anchor.min(index)..=anchor.max(index));
            self.anchor = Some(anchor);
        } else {
            if ctrl {
                if !self.selected.remove(&index) {
                    self.selected.insert(index);
                }
            } else {
                self.selected.clear();
                self.selected.insert(index);
            }
            self.anchor = Some(index);
        }
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.anchor = None;
    }
}
