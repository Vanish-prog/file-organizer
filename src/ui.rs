use file_organizer::{core, duplicates, search, selection::Selection};
use gpui::{prelude::*, *};
use std::{collections::BTreeSet, path::PathBuf};

const BG: u32 = 0x111317;
const PANEL: u32 = 0x1b1e24;
const ORANGE: u32 = 0xff9234;
const MUTED: u32 = 0x9097a5;

#[derive(Default)]
struct ReviewSelection {
    master: Option<PathBuf>,
    selected: BTreeSet<PathBuf>,
}
impl ReviewSelection {
    fn choose_master(&mut self, path: PathBuf) {
        self.selected.remove(&path);
        self.master = Some(path);
    }
    fn merge_paths(&self) -> Vec<PathBuf> {
        self.selected
            .iter()
            .filter(|p| Some(*p) != self.master.as_ref())
            .cloned()
            .collect()
    }
}

fn rank_order(entries: &[PathBuf], hits: &[PathBuf]) -> Vec<usize> {
    let mut seen = BTreeSet::new();
    let mut order: Vec<usize> = hits
        .iter()
        .filter_map(|p| entries.iter().position(|e| e == p))
        .filter(|i| seen.insert(*i))
        .collect();
    order.extend((0..entries.len()).filter(|i| !seen.contains(i)));
    order
}

// A focused GPUI text field: Unicode input, cursor, selection and clipboard.
struct TextField {
    text: String,
    cursor: usize,
    all: bool,
    focus: FocusHandle,
    placeholder: &'static str,
}
impl TextField {
    fn new(placeholder: &'static str, cx: &mut Context<Self>) -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            all: false,
            focus: cx.focus_handle(),
            placeholder,
        }
    }
    fn set(&mut self, text: String, cx: &mut Context<Self>) {
        self.cursor = text.len();
        self.text = text;
        self.all = false;
        cx.notify();
    }
    fn insert(&mut self, text: &str) {
        if self.all {
            self.text.clear();
            self.cursor = 0;
            self.all = false;
        }
        let text = text.replace(['\n', '\r'], " ");
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }
    fn key(&mut self, e: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &e.keystroke;
        if k.modifiers.control || k.modifiers.platform {
            match k.key.as_str() {
                "a" => self.all = true,
                "v" => {
                    if let Some(s) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        self.insert(&s);
                    }
                }
                "c" | "x" if self.all => {
                    cx.write_to_clipboard(ClipboardItem::new_string(self.text.clone()));
                    if k.key == "x" {
                        self.text.clear();
                        self.cursor = 0;
                        self.all = false;
                    }
                }
                _ => return,
            }
        } else {
            match k.key.as_str() {
                "backspace" => {
                    if self.all {
                        self.text.clear();
                        self.cursor = 0;
                        self.all = false;
                    } else if self.cursor > 0 {
                        let prev = self.text[..self.cursor].char_indices().last().unwrap().0;
                        self.text.replace_range(prev..self.cursor, "");
                        self.cursor = prev;
                    }
                }
                "delete" => {
                    if self.all {
                        self.text.clear();
                        self.cursor = 0;
                        self.all = false;
                    } else if self.cursor < self.text.len() {
                        let end = self.cursor
                            + self.text[self.cursor..].chars().next().unwrap().len_utf8();
                        self.text.replace_range(self.cursor..end, "");
                    }
                }
                "left" => {
                    self.all = false;
                    if self.cursor > 0 {
                        self.cursor = self.text[..self.cursor].char_indices().last().unwrap().0;
                    }
                }
                "right" => {
                    self.all = false;
                    if self.cursor < self.text.len() {
                        self.cursor += self.text[self.cursor..].chars().next().unwrap().len_utf8();
                    }
                }
                "home" => {
                    self.cursor = 0;
                    self.all = false;
                }
                "end" => {
                    self.cursor = self.text.len();
                    self.all = false;
                }
                "space" => self.insert(" "),
                _ => {
                    if let Some(s) = &k.key_char {
                        self.insert(s);
                    } else {
                        return;
                    }
                }
            }
        }
        cx.stop_propagation();
        cx.notify();
    }
}
impl Render for TextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let display = if self.text.is_empty() {
            self.placeholder.to_string()
        } else if focused && !self.all {
            format!(
                "{}│{}",
                &self.text[..self.cursor],
                &self.text[self.cursor..]
            )
        } else {
            self.text.clone()
        };
        div()
            .id("text-field")
            .track_focus(&self.focus)
            .w_full()
            .px_3()
            .py_2()
            .rounded_md()
            .bg(rgb(if self.all { 0x5c3924 } else { BG }))
            .border_1()
            .border_color(rgb(if focused { ORANGE } else { 0x343941 }))
            .text_color(rgb(if self.text.is_empty() {
                MUTED
            } else {
                0xe9eaf0
            }))
            .cursor(CursorStyle::IBeam)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|s, _, w, cx| {
                    w.focus(&s.focus);
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(Self::key))
            .child(display)
    }
}

#[derive(Clone)]
enum Dialog {
    IndexPermission,
    DownloadPermission,
    Flatten,
    Merge,
}
struct Organizer {
    root: Option<PathBuf>,
    entries: Vec<core::FileEntry>,
    visible: Vec<usize>,
    selection: Selection,
    groups: Vec<Vec<PathBuf>>,
    review: Option<Vec<PathBuf>>,
    review_selection: ReviewSelection,
    context: Option<usize>,
    folder: Entity<TextField>,
    path: Entity<TextField>,
    reference: Option<PathBuf>,
    category: bool,
    busy: bool,
    status: String,
    dialog: Option<Dialog>,
    allow_processing: bool,
    allow_download: bool,
    generation: u64,
    focus: FocusHandle,
}
impl Organizer {
    fn new(root: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
        let folder = cx.new(|cx| TextField::new("Name a category…", cx));
        let path = cx.new(|cx| TextField::new("Directory path…", cx));
        let mut s = Self {
            root: None,
            entries: vec![],
            visible: vec![],
            selection: Selection::default(),
            groups: vec![],
            review: None,
            review_selection: ReviewSelection::default(),
            context: None,
            folder,
            path,
            reference: None,
            category: false,
            busy: false,
            status: "Choose a directory to start. Your files stay local.".into(),
            dialog: None,
            allow_processing: false,
            allow_download: false,
            generation: 0,
            focus: cx.focus_handle(),
        };
        if let Some(root) = root {
            s.load(root, cx);
        }
        s
    }
    fn load(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        self.load_preserving_category(root, None, cx);
    }
    fn load_preserving_category(
        &mut self,
        root: PathBuf,
        rank: Option<Vec<PathBuf>>,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        self.generation += 1;
        let generation = self.generation;
        self.path
            .update(cx, |f, cx| f.set(root.to_string_lossy().into_owned(), cx));
        if self.root.as_ref() != Some(&root) {
            self.allow_processing = false;
            self.allow_download = false;
        }
        self.root = Some(root.clone());
        if rank.is_none() {
            self.category = false;
            self.reference = None;
        }
        self.review = None;
        self.context = None;
        self.selection.clear();
        self.entries.clear();
        self.visible.clear();
        self.groups.clear();
        self.busy = true;
        self.status = "Reading root-level files and checking duplicate groups…".into();
        let task = cx.background_executor().spawn(async move {
            let entries = core::scan(&root)?;
            let groups = duplicates::groups(&entries)?;
            anyhow::Ok((entries, groups))
        });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                if s.generation != generation {
                    return;
                }
                s.busy = false;
                match result {
                    Ok((entries, groups)) => {
                        s.entries = entries;
                        s.visible = if let Some(rank) = &rank {
                            rank_order(
                                &s.entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
                                rank,
                            )
                        } else {
                            (0..s.entries.len()).collect()
                        };
                        s.groups = groups;
                        s.status = format!(
                            "{} files · {} duplicate groups · root files only",
                            s.entries.len(),
                            s.groups.len()
                        );
                    }
                    Err(e) => s.status = format!("Could not open directory: {e:#}"),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn choose(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let picker = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose photo directory".into()),
        });
        cx.spawn(async move |this, cx| {
            let result = picker.await;
            let _ = this.update(cx, |s, cx| match result {
                Ok(Ok(Some(paths))) => {
                    if let Some(p) = paths.first() {
                        s.load(p.clone(), cx);
                    }
                }
                Ok(Ok(None)) => {}
                other => {
                    s.status = format!(
                        "Directory chooser unavailable ({other:?}). Enter a path and click Open."
                    );
                    cx.notify();
                }
            });
        })
        .detach();
    }
    fn begin_category(
        &mut self,
        index: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.root.is_none() {
            return;
        }
        self.reference = index.map(|i| self.entries[i].path.clone());
        self.category = true;
        self.context = None;
        self.selection.clear();
        self.review = None;
        self.folder.update(cx, |f, cx| f.set(String::new(), cx));
        window.focus(&self.folder.read(cx).focus);
        self.status =
            "Name this category, then rank all remaining root files. Select the photos to include."
                .into();
        cx.notify();
    }
    fn rank(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let query = self.folder.read(cx).text.trim().to_string();
        if query.is_empty() {
            self.status = "Enter a category name first.".into();
            cx.notify();
            return;
        }
        if !self.allow_processing {
            self.dialog = Some(Dialog::IndexPermission);
            cx.notify();
            return;
        }
        if !self.allow_download && !search::model_available() {
            self.dialog = Some(Dialog::DownloadPermission);
            cx.notify();
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let req = search::SearchRequest {
            root,
            query,
            reference: self.reference.clone(),
            limit: usize::MAX,
        };
        let processing = self.allow_processing;
        let download = self.allow_download;
        self.busy = true;
        self.status = "Ranking files locally with findmatch…".into();
        let task = cx
            .background_executor()
            .spawn(async move { search::search(&req, processing, download) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                s.selection.clear();
                match result {
                    Ok(hits) => {
                        s.visible = rank_order(
                            &s.entries.iter().map(|e| e.path.clone()).collect::<Vec<_>>(),
                            &hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
                        );
                        s.status = format!(
                            "{} ranked matches · all {} root files shown · Ctrl / Shift to select",
                            hits.len(),
                            s.entries.len()
                        );
                    }
                    Err(e) => s.status = format!("Ranking failed: {e:#}"),
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn review_duplicates(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let reference = self.entries[index].path.clone();
        let entries = self.entries.clone();
        self.context = None;
        self.busy = true;
        self.status = "Comparing file hashes and photo similarity…".into();
        let task = cx.background_executor().spawn(async move {
            duplicates::find_duplicates(&reference, &entries).map(|d| (reference, d))
        });
        cx.spawn(async move |this,cx| {
            let result=task.await;let _=this.update(cx,|s,cx| {
                s.busy=false;
                match result {Ok((reference,dups))=>{
                    let mut paths=vec![reference.clone()];paths.extend(dups.iter().map(|d|d.path.clone()));
                    s.review_selection=ReviewSelection::default();s.review_selection.choose_master(reference);
                    s.review=Some(paths);s.status=format!("{} candidates. Choose a master and explicitly select copies to trash; similar photos may differ.",dups.len());
                },Err(e)=>s.status=format!("Duplicate comparison failed: {e:#}")};cx.notify();
            });
        }).detach();
        cx.notify();
    }
    fn include(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(root) = self.root.clone() else {
            return;
        };
        let folder = self.folder.read(cx).text.trim().to_string();
        let paths: Vec<_> = self
            .selection
            .selected
            .iter()
            .filter_map(|i| self.visible.get(*i))
            .map(|i| self.entries[*i].path.clone())
            .collect();
        if folder.is_empty() || paths.is_empty() {
            self.status = "Name the category and select at least one file.".into();
            cx.notify();
            return;
        }
        self.mutate(
            "Moving selected files safely…",
            move || core::include(&root, &folder, &paths),
            cx,
        );
    }
    fn mutate(
        &mut self,
        label: &str,
        op: impl FnOnce() -> anyhow::Result<usize> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let category_rank = (self.category && self.review.is_some()).then(|| {
            self.visible
                .iter()
                .map(|i| self.entries[*i].path.clone())
                .collect::<Vec<_>>()
        });
        self.busy = true;
        self.dialog = None;
        self.status = label.into();
        let task = cx.background_executor().spawn(async move { op() });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |s, cx| {
                s.busy = false;
                match result {
                    Ok(n) => {
                        s.status = format!("Completed: {n} files. Choose the next category.");
                        if let Some(root) = s.root.clone() {
                            s.load_preserving_category(root, category_rank, cx);
                        }
                    }
                    Err(e) => {
                        s.status =
                            format!("Operation stopped: {e:#}. Refresh to inspect remaining files.")
                    }
                };
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.dialog.take() else {
            return;
        };
        match dialog {
            Dialog::IndexPermission => {
                self.allow_processing = true;
                self.rank(cx);
            }
            Dialog::DownloadPermission => {
                self.allow_download = true;
                self.rank(cx);
            }
            Dialog::Flatten => {
                if let Some(root) = self.root.clone() {
                    self.mutate(
                        "Flattening nested files safely…",
                        move || core::flatten(&root),
                        cx,
                    );
                }
            }
            Dialog::Merge => {
                if let (Some(root), Some(master)) =
                    (self.root.clone(), self.review_selection.master.clone())
                {
                    let others = self.review_selection.merge_paths();
                    self.mutate(
                        "Moving selected copies to system trash…",
                        move || core::merge_to_trash(&root, &master, &others),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }
    fn tile(&self, index: usize, position: usize, cx: &Context<Self>) -> AnyElement {
        let entry = &self.entries[index];
        let selected = self.selection.selected.contains(&position);
        let duplicate = self.groups.iter().any(|g| g.contains(&entry.path));
        let review = self.review.is_some();
        let master = self.review_selection.master.as_ref() == Some(&entry.path);
        let checked = self.review_selection.selected.contains(&entry.path);
        let path = entry.path.clone();
        let path2 = path.clone();
        let mut tile = div()
            .id(("tile", position))
            .w(px(205.))
            .h(px(229.))
            .flex()
            .flex_col()
            .flex_shrink_0()
            .rounded_lg()
            .overflow_hidden()
            .bg(rgb(PANEL))
            .border_color(rgb(if master || selected || checked {
                ORANGE
            } else if duplicate {
                0xd6a340
            } else {
                0x303640
            }))
            .border(px(if duplicate {
                4.
            } else if selected || master || checked {
                2.
            } else {
                1.
            }))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |s, e: &MouseDownEvent, _, cx| {
                    if s.busy {
                        return;
                    }
                    if review {
                        if s.review_selection.master.as_ref() != Some(&path)
                            && !s.review_selection.selected.remove(&path)
                        {
                            s.review_selection.selected.insert(path.clone());
                        }
                    } else {
                        s.selection.click(
                            position,
                            e.modifiers.control || e.modifiers.platform,
                            e.modifiers.shift,
                            s.visible.len(),
                        );
                    }
                    s.context = None;
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |s, _, _, cx| {
                    if !s.busy {
                        s.context = Some(index);
                        cx.notify();
                    }
                }),
            );
        let preview = if entry.dimensions.is_some() {
            img(entry.path.clone())
                .w_full()
                .h(px(151.))
                .object_fit(ObjectFit::Contain)
                .into_any_element()
        } else {
            div()
                .w_full()
                .h(px(151.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(MUTED))
                .child("FILE")
                .into_any_element()
        };
        tile = tile
            .child(preview)
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .text_sm()
                    .overflow_hidden()
                    .child(entry.name.clone()),
            )
            .child(div().px_2().text_xs().text_color(rgb(MUTED)).child(format!(
                    "{}{}",
                    size_label(entry.size),
                    entry
                        .dimensions
                        .map(|(w, h)| format!(" · {w} × {h}"))
                        .unwrap_or_default()
                )));
        if review {
            tile = tile.child(
                div()
                    .id(("master", position))
                    .px_2()
                    .text_xs()
                    .text_color(rgb(ORANGE))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, _, _, cx| {
                            s.review_selection.choose_master(path2.clone());
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(if master {
                        "● KEEP MASTER"
                    } else if checked {
                        "☑ Trash copy · click here to keep instead"
                    } else {
                        "○ Make master"
                    }),
            );
        } else if duplicate {
            tile = tile.child(
                div()
                    .id(("duplicate", position))
                    .px_2()
                    .text_xs()
                    .text_color(rgb(0xeac76c))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |s, _, _, cx| {
                            cx.stop_propagation();
                            s.review_duplicates(index, cx);
                        }),
                    )
                    .child("Duplicates · review →"),
            );
        }
        tile.into_any_element()
    }
}
fn size_label(bytes: u64) -> String {
    if bytes >= 1_048_576 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.)
    } else if bytes >= 1024 {
        format!("{:.0} KB", bytes as f64 / 1024.)
    } else {
        format!("{bytes} B")
    }
}
fn button(id: &'static str, label: impl Into<SharedString>, accent: bool) -> Stateful<Div> {
    div()
        .id(id)
        .px_3()
        .py_2()
        .rounded_md()
        .cursor_pointer()
        .bg(rgb(if accent { ORANGE } else { 0x2a2e36 }))
        .text_color(rgb(if accent { 0x18130f } else { 0xe5e8ed }))
        .text_sm()
        .hover(|s| s.bg(rgb(if accent { 0xffac5c } else { 0x3a414b })))
        .child(label.into())
}
impl Render for Organizer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.dialog.is_some() && !self.focus.is_focused(window) {
            window.focus(&self.focus);
        }
        let columns = ((f32::from(window.viewport_size().width) - 288.) / 217.)
            .floor()
            .max(1.) as usize;
        let indices: Vec<usize> = if let Some(paths) = &self.review {
            paths
                .iter()
                .filter_map(|p| self.entries.iter().position(|e| &e.path == p))
                .collect()
        } else {
            self.visible.clone()
        };
        let rows = indices.len().div_ceil(columns);
        let count = indices.len();
        let grid = uniform_list(
            "photo-grid",
            rows,
            cx.processor(move |s, range: std::ops::Range<usize>, _, cx| {
                range
                    .map(|row| {
                        let mut line = div().h(px(245.)).flex().gap_3().px_4().pt_2();
                        for (position, &index) in indices
                            .iter()
                            .enumerate()
                            .take(((row + 1) * columns).min(indices.len()))
                            .skip(row * columns)
                        {
                            line = line.child(s.tile(index, position, cx));
                        }
                        line
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full();
        let mut sidebar = div()
            .w(px(266.))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(PANEL))
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .child(div().text_xs().text_color(rgb(MUTED)).child("WORKSPACE"))
            .child(div().text_lg().child(if self.review.is_some() {
                "Duplicate review"
            } else if self.category {
                "Build a category"
            } else {
                "Your photo library"
            }))
            .child(div().text_sm().text_color(rgb(MUTED)).child(
                "Only files in this directory. Organized subfolders stay out of the queue.",
            ));
        if self.review.is_some() {
            sidebar=sidebar.child(div().text_sm().child("1. Choose the master to keep.\n2. Click copies to select them.\n3. Review and confirm trash."))
                .child(div().text_color(rgb(ORANGE)).child(format!("{} copies selected",self.review_selection.selected.len())))
                .child(button("merge","Review merge…",true).on_click(cx.listener(|s,_,_,cx| {if !s.busy&&!s.review_selection.merge_paths().is_empty(){s.dialog=Some(Dialog::Merge);cx.notify();}})))
                .child(button("back","Back to library",false).on_click(cx.listener(|s,_,_,cx|{s.review=None;s.review_selection=ReviewSelection::default();cx.notify();})));
        } else if self.category {
            sidebar =
                sidebar
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(MUTED))
                            .child("FOLDER NAME / SEARCH QUERY"),
                    )
                    .child(self.folder.clone())
                    .child(
                        button("rank", "Rank matching photos", false)
                            .on_click(cx.listener(|s, _, _, cx| s.rank(cx))),
                    )
                    .child(div().text_sm().text_color(rgb(MUTED)).child(
                        if self.reference.is_some() {
                            "Using your chosen photo as a visual reference."
                        } else {
                            "Semantic ranking across every remaining root file."
                        },
                    ))
                    .child(
                        div()
                            .text_color(rgb(ORANGE))
                            .child(format!("{} selected", self.selection.selected.len())),
                    )
                    .child(
                        button("include", "Include selected →", true)
                            .on_click(cx.listener(|s, _, _, cx| s.include(cx))),
                    )
                    .child(
                        button("cancel-category", "Cancel category", false).on_click(cx.listener(
                            |s, _, _, cx| {
                                s.category = false;
                                s.reference = None;
                                s.visible = (0..s.entries.len()).collect();
                                s.selection.clear();
                                cx.notify();
                            },
                        )),
                    );
        } else {
            sidebar=sidebar.child(button("new-category","＋ New category",true).on_click(cx.listener(|s,_,w,cx|s.begin_category(None,w,cx))))
                .child(div().text_sm().text_color(rgb(MUTED)).child("Right-click a photo to find duplicates or create a category based on it. Amber borders flag duplicate groups."))
                .child(button("flatten","Flatten subfolders…",false).on_click(cx.listener(|s,_,_,cx|{if !s.busy&&s.root.is_some(){s.dialog=Some(Dialog::Flatten);cx.notify();}})));
        }
        sidebar=sidebar.child(div().flex_1()).child(div().text_xs().text_color(rgb(MUTED)).child("LOCAL FIRST\nCtrl-click to toggle · Shift-click range\nNo silent downloads. No permanent deletion."));
        let mut app = div()
            .track_focus(&self.focus)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(BG))
            .text_color(rgb(0xe9eaf0))
            .font_family("sans-serif")
            .on_key_down(cx.listener(|s, e: &KeyDownEvent, w, cx| {
                if s.dialog.is_some() && e.keystroke.key != "escape" {
                    return;
                }
                if (e.keystroke.modifiers.control || e.keystroke.modifiers.platform)
                    && e.keystroke.key == "o"
                {
                    s.choose(cx);
                    return;
                }
                match e.keystroke.key.as_str() {
                    "enter" if s.path.read(cx).focus.is_focused(w) => {
                        let p = PathBuf::from(s.path.read(cx).text.trim());
                        if !p.as_os_str().is_empty() {
                            s.load(p, cx);
                        }
                    }
                    "enter" if s.folder.read(cx).focus.is_focused(w) => s.rank(cx),
                    "escape" => {
                        s.dialog = None;
                        s.context = None;
                        cx.notify();
                    }
                    _ => {}
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .p_4()
                    .border_b_1()
                    .border_color(rgb(0x303640))
                    .child(
                        div()
                            .text_lg()
                            .text_color(rgb(ORANGE))
                            .child("PHOTO / SORT"),
                    )
                    .child(div().flex_1().child(self.path.clone()))
                    .child(button("open-path", "Open", false).on_click(cx.listener(
                        |s, _, _, cx| {
                            let p = PathBuf::from(s.path.read(cx).text.trim());
                            if !p.as_os_str().is_empty() {
                                s.load(p, cx);
                            }
                        },
                    )))
                    .child(
                        button("choose", "Choose directory", true)
                            .on_click(cx.listener(|s, _, _, cx| s.choose(cx))),
                    )
                    .child(
                        button("refresh", "↻", false).on_click(cx.listener(|s, _, _, cx| {
                            if let Some(root) = s.root.clone() {
                                s.load(root, cx);
                            }
                        })),
                    ),
            )
            .child(
                div().flex().flex_1().min_h_0().child(sidebar).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .justify_between()
                                .p_4()
                                .child(format!(
                                    "{} · {count} files",
                                    if self.review.is_some() {
                                        "Review candidates"
                                    } else if self.category {
                                        "Ranked category candidates"
                                    } else {
                                        "Unorganized photos"
                                    }
                                ))
                                .child(div().text_sm().text_color(rgb(MUTED)).child(
                                    if self.busy {
                                        "Working…"
                                    } else {
                                        "Photo grid"
                                    },
                                )),
                        )
                        .child(if count == 0 {
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .items_center()
                                .justify_center()
                                .gap_3()
                                .text_color(rgb(MUTED))
                                .child(if self.busy {
                                    "Reading your library…"
                                } else {
                                    "A little order, a lot more room."
                                })
                                .child("Choose a directory with photos to begin.")
                                .into_any_element()
                        } else {
                            grid.into_any_element()
                        }),
                ),
            )
            .child(
                div()
                    .p_3()
                    .border_t_1()
                    .border_color(rgb(0x303640))
                    .text_sm()
                    .text_color(rgb(if self.busy { ORANGE } else { MUTED }))
                    .child(self.status.clone()),
            );
        if let Some(index) = self.context {
            app = app.child(
                div()
                    .id("photo-context-menu")
                    .occlude()
                    .absolute()
                    .top(px(85.))
                    .right(px(24.))
                    .w(px(260.))
                    .bg(rgb(0x282d35))
                    .border_1()
                    .border_color(rgb(0x4a515d))
                    .rounded_lg()
                    .p_3()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(div().text_sm().child(self.entries[index].name.clone()))
                    .child(
                        button("context-duplicates", "Find duplicates", false).on_click(
                            cx.listener(move |s, _, _, cx| s.review_duplicates(index, cx)),
                        ),
                    )
                    .child(
                        button("context-category", "Create folder based on…", false).on_click(
                            cx.listener(move |s, _, w, cx| s.begin_category(Some(index), w, cx)),
                        ),
                    )
                    .child(
                        button("context-close", "Dismiss", false).on_click(cx.listener(
                            |s, _, _, cx| {
                                s.context = None;
                                cx.notify();
                            },
                        )),
                    ),
            );
        }
        if let Some(dialog) = &self.dialog {
            let (title,body,confirm)=match dialog{
                Dialog::IndexPermission=>("Allow local indexing?","findmatch will read and locally index files in this directory for semantic search. This can take time and uses local disk space. No model download is authorized by this permission.".to_string(),"Allow indexing"),
                Dialog::DownloadPermission=>("Download the search model?","The EmbeddingGemma model is not installed. This is a separate network download and requires storage. Your files are not uploaded. Cancel to continue organizing manually.".to_string(),"Allow model download"),
                Dialog::Flatten=>("Flatten this directory?","Move files from nested subfolders into the root, including already organized categories. Name collisions are handled safely. This changes your folder organization; review your directory first.".to_string(),"Confirm flatten"),
                Dialog::Merge=>("Keep master; trash selected copies?",format!("Keep: {}\n\nTrash {} selected copies (first 8 listed):\n{}\n\nSimilar images can differ. Verify the master before continuing. No permanent deletion.",self.review_selection.master.as_ref().map(|p|p.display().to_string()).unwrap_or_default(),self.review_selection.merge_paths().len(),self.review_selection.merge_paths().iter().take(8).map(|p|p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect::<Vec<_>>().join("\n")),"Confirm move to trash"),
            };
            app = app.child(
                div()
                    .id("confirmation-overlay")
                    .occlude()
                    .absolute()
                    .inset_0()
                    .bg(rgba(0x000000b0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(520.))
                            .p_6()
                            .bg(rgb(PANEL))
                            .rounded_lg()
                            .border_1()
                            .border_color(rgb(ORANGE))
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(div().text_xl().child(title))
                            .child(div().text_sm().text_color(rgb(0xbac0cb)).child(body))
                            .child(
                                div()
                                    .flex()
                                    .justify_end()
                                    .gap_3()
                                    .child(button("dialog-cancel", "Cancel", false).on_click(
                                        cx.listener(|s, _, _, cx| {
                                            s.dialog = None;
                                            cx.notify();
                                        }),
                                    ))
                                    .child(
                                        button("dialog-confirm", confirm, true)
                                            .on_click(cx.listener(|s, _, _, cx| s.confirm(cx))),
                                    ),
                            ),
                    ),
            );
        }
        app
    }
}
pub fn launch(root: Option<PathBuf>) {
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(1280.), px(850.)), cx);
        match cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("File Organizer".into()),
                    ..Default::default()
                }),
                app_id: Some("file-organizer".into()),
                window_min_size: Some(size(px(720.), px(520.))),
                ..Default::default()
            },
            move |window, cx| {
                window.set_window_title("File Organizer");
                cx.new(|cx| {
                    let app = Organizer::new(root, cx);
                    window.focus(&app.focus);
                    app
                })
            },
        ) {
            Ok(_) => cx.activate(true),
            Err(e) => {
                eprintln!("Could not open native window: {e:#}");
                cx.quit();
            }
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
}
#[cfg(test)]
mod tests {
    use super::{Organizer, ReviewSelection, rank_order};
    use gpui::{Modifiers, TestAppContext, point, px, size};

    #[gpui::test]
    fn image_review_master_control_is_visible_and_clickable(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("photo.png");
        image::RgbImage::new(32, 32).save(&path).unwrap();
        let entries = file_organizer::core::scan(fixture.path()).unwrap();
        let (view, cx) = cx.add_window_view(|_, cx| {
            let mut app = Organizer::new(None, cx);
            app.entries = entries;
            app.visible = vec![0];
            app.review = Some(vec![path.clone()]);
            app
        });
        cx.simulate_resize(size(px(1400.), px(1000.)));
        cx.run_until_parked();
        // The control belongs below the 151px preview, inside the 229px tile.
        cx.simulate_click(point(px(385.), px(353.)), Modifiers::none());
        cx.update(|_, cx| assert_eq!(view.read(cx).review_selection.master, Some(path)));
    }

    #[gpui::test]
    fn duplicate_mutation_preserves_category_and_remaining_rank_order(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        let a = fixture.path().join("a.txt");
        let b = fixture.path().join("b.txt");
        let c = fixture.path().join("c.txt");
        for p in [&a, &b, &c] {
            std::fs::write(p, "fixture").unwrap();
        }
        let (view, cx) =
            cx.add_window_view(|_, cx| Organizer::new(Some(fixture.path().into()), cx));
        cx.run_until_parked();
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                s.category = true;
                s.reference = Some(a.clone());
                s.visible = vec![2, 0, 1];
                s.folder.update(cx, |f, cx| f.set("Category".into(), cx));
                s.review = Some(vec![a.clone(), b.clone()]);
                s.mutate(
                    "fixture duplicate merge",
                    move || {
                        std::fs::remove_file(b)?;
                        Ok(1)
                    },
                    cx,
                );
            })
        });
        cx.run_until_parked();
        cx.update(|_, cx| {
            let s = view.read(cx);
            assert!(s.category, "duplicate merge must not end categorization");
            assert_eq!(s.folder.read(cx).text, "Category");
            assert_eq!(s.reference, Some(a.clone()));
            assert!(s.review.is_none());
            assert_eq!(
                s.visible
                    .iter()
                    .map(|i| s.entries[*i].path.clone())
                    .collect::<Vec<_>>(),
                vec![c, a]
            );
        });
    }
    use std::path::PathBuf;
    #[gpui::test]
    fn category_input_consent_and_ctrl_shift_selection(cx: &mut TestAppContext) {
        let fixture = tempfile::tempdir().unwrap();
        for name in ["a.txt", "b.txt", "c.txt", "d.txt"] {
            std::fs::write(fixture.path().join(name), "qa").unwrap();
        }
        let (view, cx) =
            cx.add_window_view(|_, cx| Organizer::new(Some(fixture.path().into()), cx));
        cx.simulate_resize(size(px(1400.), px(1000.)));
        cx.run_until_parked();
        cx.update(|w, cx| view.update(cx, |s, cx| s.begin_category(None, w, cx)));
        cx.run_until_parked();
        for ch in ["q", "a"] {
            let mut keystroke = gpui::Keystroke::parse(ch).unwrap();
            keystroke.key_char = Some(ch.into());
            cx.simulate_event(gpui::KeyDownEvent {
                keystroke,
                is_held: false,
            });
        }
        cx.update(|_, cx| assert_eq!(view.read(cx).folder.read(cx).text, "qa"));
        cx.update(|_, cx| view.update(cx, |s, cx| s.rank(cx)));
        cx.update(|_, cx| {
            assert!(matches!(
                view.read(cx).dialog,
                Some(super::Dialog::IndexPermission)
            ))
        });
        cx.simulate_keystrokes("escape");
        cx.update(|_, cx| assert!(!view.read(cx).allow_processing));
        cx.simulate_click(point(px(385.), px(220.)), Modifiers::none());
        cx.simulate_click(
            point(px(600.), px(220.)),
            Modifiers {
                control: true,
                ..Modifiers::none()
            },
        );
        cx.simulate_click(
            point(px(1035.), px(220.)),
            Modifiers {
                shift: true,
                ..Modifiers::none()
            },
        );
        cx.update(|_, cx| assert_eq!(view.read(cx).selection.selected.len(), 3));
        cx.update(|_, cx| view.update(cx, |s, cx| s.include(cx)));
        cx.run_until_parked();
        assert_eq!(
            std::fs::read_dir(fixture.path().join("qa"))
                .unwrap()
                .count(),
            3
        );
        cx.update(|_, cx| {
            assert!(!view.read(cx).category);
            assert_eq!(view.read(cx).entries.len(), 1);
        });
    }

    #[test]
    fn rank_order_includes_each_remaining_file_once() {
        let entries = vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")];
        assert_eq!(
            rank_order(
                &entries,
                &[
                    PathBuf::from("b"),
                    PathBuf::from("b"),
                    PathBuf::from("missing")
                ]
            ),
            vec![1, 0, 2]
        );
    }
    #[test]
    fn merge_excludes_master_even_if_selection_is_stale() {
        let review = ReviewSelection {
            master: Some(PathBuf::from("a")),
            selected: [PathBuf::from("a"), PathBuf::from("b")].into(),
        };
        assert_eq!(review.merge_paths(), vec![PathBuf::from("b")]);
    }
    #[test]
    fn choosing_master_never_trashes_it() {
        let mut review = ReviewSelection::default();
        review
            .selected
            .extend([PathBuf::from("a"), PathBuf::from("b")]);
        review.choose_master(PathBuf::from("a"));
        assert_eq!(review.master, Some(PathBuf::from("a")));
        assert_eq!(review.merge_paths(), vec![PathBuf::from("b")]);
    }
}
