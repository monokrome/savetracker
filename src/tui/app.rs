use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::decompress;
use crate::detect::{self, FileFormat};
use crate::diff::{self, FileDiff};
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
}

impl App {
    pub fn new(idle_timeout_secs: u64, max_versions: Option<usize>) -> Self {
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
        }
    }

    pub fn load_versions(&mut self, storage: &dyn Storage) -> Result<(), StorageError> {
        self.versions.clear();

        let tracked = storage.tracked_files()?;
        for file_name in tracked {
            let file_path = PathBuf::from(&file_name);
            let version_list = storage.list(&file_path)?;

            for (i, info) in version_list.iter().enumerate() {
                let (diff_result, format) = if i > 0 {
                    let old = storage.load(&file_path, &version_list[i - 1].id)?;
                    let new = storage.load(&file_path, &info.id)?;
                    let fmt = detect::detect(&new.data);
                    let old_content = decompress_content(&old.data, &fmt);
                    let new_content = decompress_content(&new.data, &fmt);
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
        file_path: &Path,
        storage: &dyn Storage,
    ) -> Result<(), StorageError> {
        let file_name = file_path
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
                let fmt = detect::detect(&new.data);
                let old_content = decompress_content(&old.data, &fmt);
                let new_content = decompress_content(&new.data, &fmt);
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

fn decompress_content(data: &[u8], format: &FileFormat) -> Vec<u8> {
    match format {
        FileFormat::Compressed(ct, _) => {
            decompress::decompress(data, ct.clone()).unwrap_or_else(|_| data.to_vec())
        }
        _ => data.to_vec(),
    }
}
