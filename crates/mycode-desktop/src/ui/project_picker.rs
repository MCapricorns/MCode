//! In-app folder browser. Project selection stays inside GPUI and still binds
//! a real directory path through the existing session flow.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName;
use gpui_kit::component::Icon;
use gpui_kit::component::{ActiveTheme as _, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, InteractiveElement, IntoElement, ParentElement, StatefulInteractiveElement, Styled,
    div, px,
};

use super::skin;
use crate::workspace::Workspace;

const MAX_ENTRIES: usize = 400;

/// One browsable directory row.
#[derive(Clone, Debug)]
pub(crate) struct PickerEntry {
    pub name: String,
    pub path: PathBuf,
}

/// Ephemeral folder-browser state. Listing happens when the user navigates,
/// not inside the reducer.
#[derive(Clone, Debug)]
pub(crate) struct ProjectPicker {
    pub current: Option<PathBuf>,
    pub entries: Vec<PickerEntry>,
    pub status: Option<String>,
}

impl ProjectPicker {
    pub(crate) fn open() -> Self {
        browse(Some(starting_directory()))
    }
}

pub(crate) fn render(
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> gpui_kit::AnyElement {
    let theme = cx.theme();
    let picker = workspace
        .project_picker
        .clone()
        .unwrap_or_else(ProjectPicker::open);
    let at_roots = picker.current.is_none();
    let path_label = picker
        .current
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "This computer".to_owned());
    let entries = picker.entries;
    let status = picker.status;
    div()
        .id("project-picker-layer")
        .absolute()
        .inset_0()
        .child(
            div()
                .id("project-picker-scrim")
                .absolute()
                .size_full()
                .bg(skin::scrim(theme))
                .on_click(cx.listener(|workspace, _, _, cx| {
                    workspace.on_picker_cancel(cx);
                })),
        )
        .child(
            div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .id("project-picker")
                        .w(px(480.))
                        .h(px(460.))
                        .flex()
                        .flex_col()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(skin::glass_border(theme))
                        .bg(skin::popover(theme))
                        .text_color(theme.foreground)
                        .shadow_lg()
                        .overflow_hidden()
                        .child(picker_header(theme, &path_label))
                        .child(picker_nav(cx, at_roots))
                        .child(picker_list(cx, &entries, status.as_deref()))
                        .child(picker_footer(cx, at_roots)),
                ),
        )
        .into_any_element()
}

fn picker_header(theme: &gpui_kit::component::theme::Theme, path_label: &str) -> impl IntoElement {
    div()
        .px_4()
        .pt_3()
        .pb_2()
        .flex()
        .flex_col()
        .gap_1()
        .border_b_1()
        .border_color(theme.border)
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .child("Choose a project folder"),
        )
        .child(
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(path_label.to_owned()),
        )
}

fn picker_nav(cx: &mut Context<Workspace>, at_roots: bool) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_3()
        .py_2()
        .flex()
        .flex_row()
        .gap_2()
        .child(
            nav_button("picker-up", "Up", !at_roots, theme).on_click(cx.listener(
                |workspace, _, _, cx| {
                    workspace.on_picker_up(cx);
                },
            )),
        )
        .child(
            nav_button("picker-home", "Home", true, theme).on_click(cx.listener(
                |workspace, _, _, cx| {
                    workspace.on_picker_home(cx);
                },
            )),
        )
        .child(
            nav_button("picker-roots", "This computer", !at_roots, theme).on_click(cx.listener(
                |workspace, _, _, cx| {
                    workspace.on_picker_roots(cx);
                },
            )),
        )
}

fn picker_list(
    cx: &mut Context<Workspace>,
    entries: &[PickerEntry],
    status: Option<&str>,
) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .id("project-picker-list")
        .flex_1()
        .min_h_0()
        .overflow_y_scroll()
        .px_2()
        .py_1()
        .flex()
        .flex_col()
        .gap_0p5()
        .children(entries.iter().enumerate().map(|(index, entry)| {
            let path = entry.path.clone();
            let name = entry.name.clone();
            div()
                .id(format!("picker-row-{index}"))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .py(px(6.))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(|style| style.bg(theme.secondary_hover))
                .on_click(cx.listener(move |workspace, _, _, cx| {
                    workspace.on_picker_enter(path.clone(), cx);
                }))
                .child(
                    Icon::new(IconName::FolderOpen)
                        .with_size(px(15.))
                        .text_color(theme.yellow),
                )
                .child(name)
        }))
        .when(entries.is_empty(), |list| {
            list.child(
                div()
                    .px_2()
                    .py_3()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(
                        status
                            .unwrap_or("This folder has no subfolders.")
                            .to_owned(),
                    ),
            )
        })
        .when(status.is_some() && !entries.is_empty(), |list| {
            list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status.unwrap_or_default().to_owned()),
            )
        })
}

fn picker_footer(cx: &mut Context<Workspace>, at_roots: bool) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .px_3()
        .py_3()
        .flex()
        .flex_row()
        .justify_end()
        .gap_2()
        .border_t_1()
        .border_color(theme.border)
        .child(
            footer_button("picker-cancel", "Cancel", false, theme).on_click(cx.listener(
                |workspace, _, _, cx| {
                    workspace.on_picker_cancel(cx);
                },
            )),
        )
        .child(
            footer_button("picker-use", "Use this folder", true, theme)
                .when(!at_roots, |button| {
                    button.on_click(cx.listener(|workspace, _, _, cx| {
                        workspace.on_picker_confirm(cx);
                    }))
                })
                .when(at_roots, |button| button.opacity(0.45)),
        )
}

fn nav_button(
    id: &'static str,
    label: &'static str,
    enabled: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .px_2()
        .py(px(4.))
        .rounded(px(6.))
        .text_xs()
        .border_1()
        .border_color(theme.border)
        .bg(theme.secondary)
        .text_color(if enabled {
            theme.foreground
        } else {
            theme.muted_foreground
        })
        .cursor_pointer()
        .hover(|style| style.bg(theme.secondary_hover))
        .child(label)
}

fn footer_button(
    id: &'static str,
    label: &'static str,
    emphasized: bool,
    theme: &gpui_kit::component::theme::Theme,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let fill = if emphasized {
        theme.primary
    } else {
        theme.secondary
    };
    let ink = if emphasized {
        theme.primary_foreground
    } else {
        theme.foreground
    };
    div()
        .id(id)
        .px_3()
        .py(px(6.))
        .rounded(px(8.))
        .text_sm()
        .border_1()
        .border_color(if emphasized {
            theme.primary
        } else {
            theme.border
        })
        .bg(fill)
        .text_color(ink)
        .cursor_pointer()
        .child(label)
}

/// Parent directory, or `None` at a filesystem root.
pub(crate) fn parent_folder(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() || parent == path {
        return None;
    }
    Some(parent.to_path_buf())
}

pub(crate) fn browse(path: Option<PathBuf>) -> ProjectPicker {
    let Some(path) = path else {
        return ProjectPicker {
            current: None,
            entries: filesystem_roots()
                .into_iter()
                .map(|root| PickerEntry {
                    name: root.display().to_string(),
                    path: root,
                })
                .collect(),
            status: None,
        };
    };
    match list_directories(&path) {
        Ok((entries, truncated)) => ProjectPicker {
            current: Some(path),
            entries,
            status: truncated.then(|| format!("Showing the first {MAX_ENTRIES} folders.")),
        },
        Err(status) => ProjectPicker {
            current: Some(path),
            entries: Vec::new(),
            status: Some(status),
        },
    }
}

fn starting_directory() -> PathBuf {
    for key in ["USERPROFILE", "HOME"] {
        if let Some(value) = std::env::var_os(key) {
            let path = PathBuf::from(value);
            if path.is_dir() {
                return path;
            }
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn list_directories(path: &Path) -> Result<(Vec<PickerEntry>, bool), String> {
    let read = std::fs::read_dir(path).map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for item in read {
        let Ok(item) = item else {
            continue;
        };
        let Ok(file_type) = item.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = item.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        entries.push(PickerEntry {
            name,
            path: item.path(),
        });
        if entries.len() >= MAX_ENTRIES {
            break;
        }
    }
    let truncated = entries.len() >= MAX_ENTRIES;
    entries.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    });
    Ok((entries, truncated))
}

fn filesystem_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        // SAFETY: GetLogicalDrives takes no pointers and has no preconditions.
        let mask = unsafe { windows_sys::Win32::Storage::FileSystem::GetLogicalDrives() };
        (0..26)
            .filter_map(|index| {
                if mask & (1 << index) == 0 {
                    return None;
                }
                let letter = (b'A' + index as u8) as char;
                Some(PathBuf::from(format!("{letter}:\\")))
            })
            .collect()
    }
    #[cfg(not(windows))]
    {
        vec![PathBuf::from("/")]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_stops_at_a_drive_or_filesystem_root() {
        assert!(parent_folder(Path::new("/")).is_none() || cfg!(windows));
        let nested = Path::new("/projects/app");
        if let Some(parent) = parent_folder(nested) {
            assert_eq!(parent, Path::new("/projects"));
        }
        #[cfg(windows)]
        {
            assert!(parent_folder(Path::new(r"C:\")).is_none());
            assert_eq!(
                parent_folder(Path::new(r"C:\projects")).as_deref(),
                Some(Path::new(r"C:\"))
            );
        }
    }
}
