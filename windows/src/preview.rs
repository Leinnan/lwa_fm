//! Quick Look: a window that shows the selected files one at a time. Images
//! are drawn and text is read here; other kinds are shown by the preview
//! handler the system has for them (the one Explorer's preview pane uses),
//! and by their icon when it has none.
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use dirfleet_core::app::Event;
use dirfleet_winshell::{Insets, PreviewHost, has_preview_handler};
use windows_reactor::{
    Border, Callback, Color, Component, ComponentContext, ComponentTimer, ElementRef, Grid,
    GridLength, HorizontalAlignment, Image, KeyEventInfo, Orientation, RoutedCallback,
    ScrollBarVisibility, ScrollViewer, StackPanel, Stretch, TextBlock, TextTrimming, TextWrapping,
    Thickness, VerticalAlignment, View, ViewContext, VirtualKey, WindowBackdrop, WindowSize,
    WindowVisuals,
};

use crate::core_link::CoreLink;
use crate::files::entry_icon;
use crate::ui_thread;

/// The key the application declares the preview window under.
pub const WINDOW_KEY: &str = "preview";
/// How much of a text file is read to show it.
const TEXT_LIMIT: u64 = 256 * 1024;
/// A new window refuses the focus until it is laid out; asked again this often.
const FOCUS_RETRY: Duration = Duration::from_millis(40);
const FOCUS_TRIES: u8 = 10;
/// The space around the system's preview: the content's own margin, and
/// below it the line that names the file.
const AROUND_HOSTED: Insets = Insets {
    left: 16.0,
    top: 16.0,
    right: 16.0,
    bottom: 44.0,
};

/// The system's preview of the file shown, when its kind has one. Shared
/// with the work that makes it, which runs after the window's own.
#[derive(Default)]
struct Hosted {
    /// The file the window shows now, once a preview was asked for it; a
    /// preview that arrives for another file is dropped.
    path: Option<String>,
    host: Option<Rc<PreviewHost>>,
}

#[derive(Clone, PartialEq)]
pub struct PreviewInput {
    pub core: CoreLink,
    /// The tab the files are selected in.
    pub tab: u32,
    pub paths: Rc<Vec<String>>,
    /// The file to start at.
    pub index: u32,
    /// Told when the window goes away.
    pub on_closed: Callback<()>,
}

pub struct PreviewWindow {
    core: CoreLink,
    tab: u32,
    paths: Rc<Vec<String>>,
    index: usize,
    on_closed: Callback<()>,
    on_key: RoutedCallback<KeyEventInfo>,
    focus: ElementRef<Border>,
    focus_timer: Option<ComponentTimer>,
    /// The native window, once known; 0 before.
    handle: isize,
    hosted: Rc<RefCell<Hosted>>,
    on_size: Callback<WindowSize>,
}

#[derive(Clone, Copy, Debug)]
pub enum PreviewMessage {
    Step(isize),
    Open,
    Close,
    TakeFocus(u8),
    Focused(bool, u8),
    /// The native window's handle, once there is one.
    Handle(isize),
    /// The system's preview came, or went: time to draw again.
    Hosted,
    Resized,
}

/// What a file is shown as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Image,
    Text,
    Other,
}

impl Kind {
    pub fn of(path: &str) -> Self {
        let extension = Path::new(path)
            .extension()
            .map(|extension| extension.to_string_lossy().to_lowercase());
        match extension.as_deref() {
            Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tif" | "tiff") => {
                Self::Image
            }
            Some(
                "txt" | "md" | "log" | "rs" | "toml" | "json" | "yml" | "yaml" | "xml" | "html"
                | "css" | "js" | "ts" | "py" | "c" | "h" | "cpp" | "hpp" | "cs" | "java" | "go"
                | "ini" | "cfg" | "conf" | "csv" | "sh" | "ps1" | "bat" | "cmd" | "lua" | "sql"
                | "swift" | "kt",
            ) => Self::Text,
            _ => Self::Other,
        }
    }
}

/// The beginning of a text file, with a note when there is more.
fn read_text(path: &str) -> String {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return "This file can't be read.".to_owned();
    };
    let longer = file.metadata().is_ok_and(|data| data.len() > TEXT_LIMIT);
    let mut bytes = Vec::new();
    if file.take(TEXT_LIMIT).read_to_end(&mut bytes).is_err() {
        return "This file can't be read.".to_owned();
    }
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if longer {
        text.push_str("\n…");
    }
    text
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(
        || path.to_owned(),
        |name| name.to_string_lossy().into_owned(),
    )
}

impl PreviewWindow {
    /// Has the system's handler show the current file, taking the preview of
    /// the one before away. The handler is another program: asking it lets
    /// this thread's messages run, so it is asked once this has returned.
    fn host(&self, context: &ComponentContext<Self>) {
        if self.handle == 0 {
            return;
        }
        let path = self.paths.get(self.index).cloned().unwrap_or_default();
        {
            let mut hosted = self.hosted.borrow_mut();
            if hosted.path.as_deref() == Some(path.as_str()) {
                return;
            }
            hosted.path = Some(path.clone());
        }
        let wanted = Kind::of(&path) == Kind::Other && has_preview_handler(Path::new(&path));
        let (shared, owner, sender) = (Rc::clone(&self.hosted), self.handle, context.sender());
        ui_thread::defer(move || {
            // Nothing is borrowed while the handler is talked to.
            let before = shared.borrow_mut().host.take();
            let had = before.is_some();
            drop(before);
            let host = wanted
                .then(|| PreviewHost::show(owner, Path::new(&path), AROUND_HOSTED))
                .flatten();
            let came = host.is_some();
            let current = shared.borrow().path.as_deref() == Some(path.as_str());
            if current {
                shared.borrow_mut().host = host.map(Rc::new);
            } else {
                // Another file is shown by now: this preview is not for it.
                drop(host);
            }
            if had || came {
                _ = sender.send(PreviewMessage::Hosted);
            }
        });
    }

    fn take_focus(&mut self, attempt: u8, context: &ComponentContext<Self>) {
        self.focus_timer = Some(
            context.set_local_timeout(FOCUS_RETRY, move || PreviewMessage::TakeFocus(attempt)),
        );
    }
}

impl Drop for PreviewWindow {
    fn drop(&mut self) {
        // A preview on its way is for a window that is gone.
        let host = {
            let mut hosted = self.hosted.borrow_mut();
            hosted.path = None;
            hosted.host.take()
        };
        if host.is_some() {
            ui_thread::defer(move || drop(host));
        }
        self.on_closed.call(());
    }
}

impl Component for PreviewWindow {
    type Input = PreviewInput;
    type Message = PreviewMessage;

    fn create(input: &PreviewInput, context: &ComponentContext<Self>) -> Self {
        if !context.run_window(|handle| PreviewMessage::Handle(handle.as_raw() as isize)) {
            crate::trace::note(|| "the preview window has no handle to give".to_owned());
        }
        let sender = context.sender();
        let mut preview = Self {
            core: input.core.clone(),
            tab: input.tab,
            paths: Rc::clone(&input.paths),
            index: input.index as usize,
            on_closed: input.on_closed.clone(),
            on_key: RoutedCallback::new(move |info: KeyEventInfo| {
                let message = match info.key {
                    VirtualKey::LEFT | VirtualKey::UP => PreviewMessage::Step(-1),
                    VirtualKey::RIGHT | VirtualKey::DOWN => PreviewMessage::Step(1),
                    VirtualKey::ENTER => PreviewMessage::Open,
                    VirtualKey::ESCAPE | VirtualKey::SPACE => PreviewMessage::Close,
                    _ => return false,
                };
                sender.send(message)
            }),
            focus: ElementRef::new(),
            focus_timer: None,
            handle: 0,
            hosted: Rc::default(),
            on_size: context.callback(|_: WindowSize| PreviewMessage::Resized),
        };
        preview.take_focus(0, context);
        preview
    }

    fn input_changed(&mut self, input: &PreviewInput, context: &ComponentContext<Self>) {
        // Another selection was sent to the open window.
        self.tab = input.tab;
        self.paths = Rc::clone(&input.paths);
        self.index = input.index as usize;
        self.host(context);
    }

    fn update(&mut self, message: PreviewMessage, context: &ComponentContext<Self>) {
        match message {
            PreviewMessage::Step(by) => {
                let count = self.paths.len();
                if count > 0 {
                    self.index = self.index.saturating_add_signed(by).min(count - 1);
                }
                self.host(context);
            }
            PreviewMessage::Handle(handle) => {
                self.handle = handle;
                self.host(context);
            }
            // The view reads what is hosted.
            PreviewMessage::Hosted => {}
            PreviewMessage::Resized => {
                let host = self.hosted.borrow().host.clone();
                if let Some(host) = host {
                    ui_thread::defer(move || host.fit());
                }
            }
            PreviewMessage::Open => {
                if let Some(path) = self.paths.get(self.index) {
                    self.core.send(Event::OpenPreviewed {
                        tab: self.tab,
                        path: path.clone(),
                    });
                }
                _ = context.close_window();
            }
            PreviewMessage::Close => _ = context.close_window(),
            PreviewMessage::TakeFocus(attempt) => {
                let sender = context.sender();
                _ = self.focus.request_focus_result(move |result| {
                    _ = sender.send(PreviewMessage::Focused(result == Ok(true), attempt));
                });
            }
            PreviewMessage::Focused(taken, attempt) => {
                self.focus_timer = None;
                if !taken && attempt < FOCUS_TRIES {
                    self.take_focus(attempt + 1, context);
                }
            }
        }
    }

    fn view(&self, _: &PreviewInput, context: &mut ViewContext<Self>) -> View {
        let path = self.paths.get(self.index).map_or("", String::as_str);
        let name = file_name(path);
        context.window_title(name.clone());
        context.window_visuals(
            WindowVisuals::new()
                .backdrop(WindowBackdrop::Mica)
                .client_size(900.0, 640.0),
        );
        context.on_window_size(self.on_size.clone());
        let hosted = self.hosted.borrow().host.is_some();
        let shown: View = match Kind::of(path) {
            // The system's preview lies over this space, in a window of its own.
            Kind::Other if hosted => Border::new().into(),
            Kind::Image => Image::new().source_file(path).map_or_else(
                |_| entry_icon(&name, false, 128.0),
                |image| image.stretch(Stretch::Uniform).into(),
            ),
            Kind::Text => ScrollViewer::new()
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(
                    TextBlock::new()
                        .text(read_text(path))
                        .font_family("Cascadia Mono,Consolas")
                        .font_size(13.0)
                        .text_wrapping(TextWrapping::NoWrap)
                        .is_text_selection_enabled(true),
                )
                .into(),
            Kind::Other => StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(12.0)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .children((
                    entry_icon(&name, Path::new(path).is_dir(), 128.0),
                    TextBlock::new()
                        .text(name.clone())
                        .font_size(16.0)
                        .horizontal_alignment(HorizontalAlignment::Center),
                ))
                .into(),
        };
        let place = if self.paths.len() > 1 {
            format!("{} of {}", self.index + 1, self.paths.len())
        } else {
            String::new()
        };
        Border::new()
            .element_ref(&self.focus)
            .is_tab_stop(true)
            .background(Color::transparent())
            .on_preview_key_down(self.on_key.clone())
            .content(
                Grid::new()
                    .rows([GridLength::STAR, GridLength::Auto])
                    .children((
                        Border::new()
                            .padding(Thickness::uniform(16.0))
                            .content(shown),
                        Grid::new()
                            .grid_row(1)
                            .columns([GridLength::STAR, GridLength::Auto])
                            .margin(Thickness::new(16.0, 0.0, 16.0, 10.0))
                            .children((
                                TextBlock::new()
                                    .text(path)
                                    .font_size(12.0)
                                    .opacity(0.6)
                                    .text_trimming(TextTrimming::CharacterEllipsis),
                                TextBlock::new()
                                    .text(place)
                                    .font_size(12.0)
                                    .opacity(0.6)
                                    .grid_column(1),
                            )),
                    )),
            )
            .into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_shown_by_their_extension() {
        assert_eq!(Kind::of("C:\\a\\photo.JPG"), Kind::Image);
        assert_eq!(Kind::of("notes.md"), Kind::Text);
        assert_eq!(Kind::of("movie.mkv"), Kind::Other);
        assert_eq!(Kind::of("LICENSE"), Kind::Other);
    }

    #[test]
    fn a_path_is_named_by_its_last_part() {
        assert_eq!(file_name("C:\\a\\b.txt"), "b.txt");
        assert_eq!(file_name(""), "");
    }
}
