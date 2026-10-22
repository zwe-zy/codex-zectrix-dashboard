//! Local edits never write configuration until the explicit Save action.
use super::*;
use crate::inkcards::{InkcardPage, InkcardSettings, PAGE_THEMES};

pub struct CardEditor {
    pub multiple: bool,
    pub single_page: u8,
    pub single_theme: BoardTheme,
    pub themes: Vec<BoardTheme>,
    pub dirty: bool,
}
impl Default for CardEditor {
    fn default() -> Self {
        Self {
            multiple: false,
            single_page: 1,
            single_theme: BoardTheme::default(),
            themes: PAGE_THEMES.to_vec(),
            dirty: false,
        }
    }
}
impl CardEditor {
    pub fn count(&self) -> usize {
        if self.multiple { self.themes.len() } else { 1 }
    }
    pub fn set_count(&mut self, count: usize) {
        if !(1..=5).contains(&count) || count == self.count() {
            return;
        }
        self.multiple = count > 1;
        if self.multiple {
            self.themes.truncate(count);
            while self.themes.len() < count {
                self.add_card();
            }
        }
        self.dirty = true;
    }
    pub fn load_status(&mut self, status: &DesktopStatus) {
        if self.dirty {
            return;
        }
        self.multiple = status.mode == InkcardMode::All;
        self.single_page = status.configured_page;
        self.single_theme = status.theme;
        if self.multiple && !status.pages.is_empty() {
            self.themes = status.pages.iter().map(|p| p.theme).collect();
        }
    }
    pub fn move_card(&mut self, from: usize, to: usize) {
        if from < self.themes.len() && to < self.themes.len() && from != to {
            let theme = self.themes.remove(from);
            self.themes.insert(to, theme);
            self.dirty = true;
        }
    }
    pub fn add_card(&mut self) {
        if self.themes.len() < 5 {
            let theme = PAGE_THEMES
                .into_iter()
                .find(|t| !self.themes.contains(t))
                .unwrap_or_default();
            self.themes.push(theme);
            self.dirty = true;
        }
    }
    pub fn remove_card(&mut self, index: usize) {
        if self.themes.len() > 2 && index < self.themes.len() {
            self.themes.remove(index);
            self.dirty = true;
        }
    }
    pub fn configuration(&self) -> Result<InkcardSettings, crate::SettingsError> {
        let pages = if self.multiple {
            self.themes
                .iter()
                .enumerate()
                .map(|(i, theme)| InkcardPage {
                    page: i as u8 + 1,
                    theme: *theme,
                })
                .collect()
        } else {
            vec![InkcardPage {
                page: self.single_page,
                theme: self.single_theme,
            }]
        };
        InkcardSettings::from_pages(pages)
    }
    pub fn discard(&mut self, status: &DesktopStatus) {
        self.dirty = false;
        self.load_status(status);
    }
}
