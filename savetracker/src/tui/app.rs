use std::path::{Path, PathBuf};
use std::time::Instant;

use watch_path::ConnectionState;

use crate::config::Config;
use crate::detect::FileFormat;
use crate::diff::{self, FileDiff};
use crate::format::{self, FormatRegistry};
use crate::storage::{Storage, StorageError, VersionInfo};

pub struct VersionEntry {
    pub file_name: String,
    pub info: VersionInfo,
    pub diff: Option<FileDiff>,
    pub format: Option<FileFormat>,
}

pub enum View {
    Main,
    DetailDiff,
}

pub struct App {
    pub versions: Vec<VersionEntry>,
    pub selected: usize,
    pub diff_scroll: u16,
    pub view: View,
    pub last_input: Instant,
    pub idle_timeout_secs: u64,
    pub max_versions: Option<usize>,
    pub should_quit: bool,
    pub status_message: Option<String>,
    pub connection_state: ConnectionState,
    pub watch_url: String,
}

impl App {
    pub fn new(idle_timeout_secs: u64, max_versions: Option<usize>, watch_url: String) -> Self {
        Self {
            versions: Vec::new(),
            selected: 0,
            diff_scroll: 0,
            view: View::Main,
            last_input: Instant::now(),
            idle_timeout_secs,
            max_versions,
            should_quit: false,
            status_message: None,
            connection_state: ConnectionState::Connected,
            watch_url,
        }
    }

    pub fn load_versions(
        &mut self,
        storage: &dyn Storage,
        registry: &FormatRegistry,
        config: &Config,
    ) -> Result<(), StorageError> {
        self.versions.clear();

        let tracked = storage.tracked_files()?;
        for file_name in tracked {
            let file_path = PathBuf::from(&file_name);
            let version_list = storage.list(&file_path)?;

            for (i, info) in version_list.iter().enumerate() {
                let (diff_result, format) = if i > 0 {
                    let old = storage.load(&file_path, &version_list[i - 1].id)?;
                    let new = storage.load(&file_path, &info.id)?;
                    let (old_content, _) = format::decode_file(registry, config.forced_format.as_deref(), &file_name, &old.data, &config.format_params);
                    let (new_content, fmt) = format::decode_file(registry, config.forced_format.as_deref(), &file_name, &new.data, &config.format_params);
                    let d = diff::diff(&old_content, &new_content, &fmt);
                    (Some(d), Some(fmt))
                } else {
                    (None, None)
                };

                self.versions.push(VersionEntry {
                    file_name: file_name.clone(),
                    info: info.clone(),
                    diff: diff_result,
                    format,
                });
            }
        }

        if let Some(max) = self.max_versions {
            if self.versions.len() > max {
                let start = self.versions.len() - max;
                self.versions.drain(..start);
            }
        }

        if !self.versions.is_empty() {
            self.selected = self.versions.len() - 1;
        }

        Ok(())
    }

    pub fn on_save_change(
        &mut self,
        path: &str,
        storage: &dyn Storage,
        registry: &FormatRegistry,
        config: &Config,
    ) -> Result<(), StorageError> {
        let file_name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        let fp = PathBuf::from(&file_name);
        let version_list = storage.list(&fp)?;

        if let Some(info) = version_list.last() {
            let (diff_result, format) = if version_list.len() >= 2 {
                let prev_info = &version_list[version_list.len() - 2];
                let old = storage.load(&fp, &prev_info.id)?;
                let new = storage.load(&fp, &info.id)?;
                let (old_content, _) = format::decode_file(registry, config.forced_format.as_deref(), path, &old.data, &config.format_params);
                let (new_content, fmt) = format::decode_file(registry, config.forced_format.as_deref(), path, &new.data, &config.format_params);
                let d = diff::diff(&old_content, &new_content, &fmt);
                (Some(d), Some(fmt))
            } else {
                (None, None)
            };

            self.versions.push(VersionEntry {
                file_name,
                info: info.clone(),
                diff: diff_result,
                format,
            });

            if let Some(max) = self.max_versions {
                if self.versions.len() > max {
                    self.versions.remove(0);
                }
            }

            let idle_elapsed = self.last_input.elapsed().as_secs() >= self.idle_timeout_secs;
            if idle_elapsed && matches!(self.view, View::Main) {
                self.selected = self.versions.len() - 1;
                self.diff_scroll = 0;
            }
        }

        Ok(())
    }

    pub fn select_next(&mut self) {
        if !self.versions.is_empty() && self.selected < self.versions.len() - 1 {
            self.selected += 1;
            self.diff_scroll = 0;
            self.touch_input();
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.diff_scroll = 0;
            self.touch_input();
        }
    }

    pub fn scroll_diff_down(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_add(5);
    }

    pub fn scroll_diff_up(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_sub(5);
    }

    pub fn toggle_detail_diff(&mut self) {
        self.view = match self.view {
            View::Main => View::DetailDiff,
            View::DetailDiff => View::Main,
        };
        self.diff_scroll = 0;
    }

    pub fn exit_overlay(&mut self) {
        if matches!(self.view, View::DetailDiff) {
            self.view = View::Main;
            self.diff_scroll = 0;
        }
    }

    pub fn touch_input(&mut self) {
        self.last_input = Instant::now();
    }

    pub fn selected_entry(&self) -> Option<&VersionEntry> {
        self.versions.get(self.selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_app(num_versions: usize) -> App {
        let mut app = App::new(15, None, "file:///test".to_string());
        for i in 0..num_versions {
            app.versions.push(VersionEntry {
                file_name: format!("save_{i}.dat"),
                info: VersionInfo {
                    id: format!("20260101_00000{i}_000"),
                    timestamp: chrono::Utc::now(),
                    description: None,
                },
                diff: None,
                format: None,
            });
        }
        if !app.versions.is_empty() {
            app.selected = app.versions.len() - 1;
        }
        app
    }

    #[test]
    fn select_next_advances() {
        let mut app = make_app(3);
        app.selected = 0;
        app.select_next();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn select_next_stops_at_end() {
        let mut app = make_app(3);
        app.select_next();
        assert_eq!(app.selected, 2);
    }

    #[test]
    fn select_prev_goes_back() {
        let mut app = make_app(3);
        app.select_prev();
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn select_prev_stops_at_zero() {
        let mut app = make_app(3);
        app.selected = 0;
        app.select_prev();
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn scroll_diff_down_increases() {
        let mut app = make_app(1);
        assert_eq!(app.diff_scroll, 0);
        app.scroll_diff_down();
        assert_eq!(app.diff_scroll, 5);
    }

    #[test]
    fn scroll_diff_up_decreases() {
        let mut app = make_app(1);
        app.diff_scroll = 10;
        app.scroll_diff_up();
        assert_eq!(app.diff_scroll, 5);
    }

    #[test]
    fn scroll_diff_up_saturates_at_zero() {
        let mut app = make_app(1);
        app.diff_scroll = 2;
        app.scroll_diff_up();
        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn toggle_detail_diff_switches_view() {
        let mut app = make_app(1);
        assert!(matches!(app.view, View::Main));
        app.toggle_detail_diff();
        assert!(matches!(app.view, View::DetailDiff));
        app.toggle_detail_diff();
        assert!(matches!(app.view, View::Main));
    }

    #[test]
    fn toggle_detail_diff_resets_scroll() {
        let mut app = make_app(1);
        app.diff_scroll = 10;
        app.toggle_detail_diff();
        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn exit_overlay_returns_to_main() {
        let mut app = make_app(1);
        app.view = View::DetailDiff;
        app.diff_scroll = 10;
        app.exit_overlay();
        assert!(matches!(app.view, View::Main));
        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn exit_overlay_noop_on_main() {
        let mut app = make_app(1);
        app.diff_scroll = 5;
        app.exit_overlay();
        assert!(matches!(app.view, View::Main));
        assert_eq!(app.diff_scroll, 5);
    }

    #[test]
    fn touch_input_resets_idle_timer() {
        let mut app = make_app(1);
        std::thread::sleep(std::time::Duration::from_millis(50));
        let before = app.last_input;
        app.touch_input();
        assert!(app.last_input > before);
    }

    #[test]
    fn select_next_resets_scroll() {
        let mut app = make_app(3);
        app.selected = 0;
        app.diff_scroll = 10;
        app.select_next();
        assert_eq!(app.diff_scroll, 0);
    }

    #[test]
    fn selected_entry_returns_correct() {
        let app = make_app(3);
        let entry = app.selected_entry().unwrap();
        assert_eq!(entry.file_name, "save_2.dat");
    }

    #[test]
    fn selected_entry_empty_returns_none() {
        let app = make_app(0);
        assert!(app.selected_entry().is_none());
    }
}

