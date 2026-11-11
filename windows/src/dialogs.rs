//! The questions a window asks in a dialog over its content: one at a time,
//! the most pressing first.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dirfleet_core::app::{
    BatchFields, BatchMode, BatchRenameView, CompressView, ConflictChoice, ConflictView,
    ConnectFields, ConnectView, Event, GoToView, PaletteTarget, Secret, TabViewModel, ViewModel,
};
use dirfleet_core::location::Scheme;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, ComboBox, ContentDialog, ContentDialogButton,
    ContentDialogResult, Grid, GridLength, HorizontalAlignment, Orientation, PasswordBox,
    ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextBox, TextWrapping,
    VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::picker;
use crate::plugins;
use crate::saved::{self, Draft};
use crate::sheets;
use crate::theme;

/// Something the window asks before it tells the core to do it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asking {
    /// Move the selection to the Recycle Bin.
    Trash,
    /// Delete the selection for good.
    Delete,
    EmptyTrash,
    /// Whether to move or copy this many dropped items; the window, which
    /// holds them, acts on the answer.
    Drop(u32),
    /// Not a question: the list of keyboard shortcuts.
    Shortcuts,
    /// Not a question: the settings.
    Settings,
    /// Whether to run a plugin's action that asks first; the window, which
    /// holds the action, acts on the answer.
    Plugin,
    /// The name to save the tab's search under; the window holds the draft.
    SaveSearch,
    /// The editor of a saved search; the window holds the draft.
    EditSearch,
}

/// What decides which of the app's dialogs a window shows.
#[derive(Clone, Copy, Debug)]
pub struct Here<'a> {
    /// The window asks the questions that belong to the whole app.
    pub leads: bool,
    /// The palette was asked for in this window.
    pub palette: bool,
    /// The window's tabs: a dialog about a tab shows in its window.
    pub tabs: &'a [u32],
}

impl Here<'_> {
    fn has(&self, tab: u32) -> bool {
        self.tabs.contains(&tab)
    }
}

/// What a dialog needs of the window that shows it.
#[derive(Clone, Copy)]
pub struct Hooks<'a> {
    pub on_closed: &'a Callback<ContentDialogResult>,
    /// Told what was chosen in the palette.
    pub on_palette: &'a Callback<PaletteTarget>,
    /// The native window, for the system dialogs a field may open.
    pub owner: isize,
    /// The question of the plugin action waiting for an answer, or "".
    pub confirm: &'a str,
    /// The saved search being named or edited.
    pub draft: &'a Draft,
}

/// The archive's name and format, what it will be called, and why not.
fn compress_form(core: &CoreLink, tab: u32, compress: &CompressView) -> View {
    let named = {
        let (core, format) = (core.clone(), compress.format);
        Callback::new(move |text: Rc<str>| {
            core.send(Event::SetCompress {
                tab,
                name: text.to_string(),
                format,
            });
        })
    };
    let chosen = compress
        .formats
        .iter()
        .position(|choice| choice.format == compress.format)
        .unwrap_or(0);
    let formats = {
        let (core, name) = (core.clone(), compress.name.clone());
        let formats: Vec<_> = compress
            .formats
            .iter()
            .map(|choice| choice.format)
            .collect();
        ComboBox::new()
            .automation_name("Format")
            .items_source(compress.formats.iter().map(|choice| choice.label.clone()))
            .selected_index(chosen)
            .on_selection_changed(move |index: Option<usize>| {
                let picked = index.filter(|index| *index != chosen);
                if let Some(format) = picked.and_then(|index| formats.get(index)) {
                    core.send(Event::SetCompress {
                        tab,
                        name: name.clone(),
                        format: *format,
                    });
                }
            })
    };
    let mut lines: Vec<View> = vec![
        TextBox::new()
            .automation_name("Archive name")
            .text(compress.name.clone())
            .placeholder_text("Name")
            .on_text_changed(named)
            .into(),
        formats.into(),
        TextBlock::new()
            .text(compress.file_name.clone())
            .opacity(0.6)
            .text_wrapping(TextWrapping::Wrap)
            .into(),
    ];
    if !compress.error.is_empty() {
        lines.push(
            TextBlock::new()
                .text(compress.error.clone())
                .foreground(Color::rgb(0xE0, 0x50, 0x50))
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}

/// The connect dialog's fields: which kind of server, where, as whom.
#[expect(clippy::too_many_lines, reason = "a line or two for each field")]
fn connect_form(core: &CoreLink, connect: &ConnectView) -> View {
    let fields = &connect.fields;
    // Each field tells the core all of them, with its own changed.
    let text = |label: &str, value: &str, apply: fn(&mut ConnectFields, String)| -> View {
        let (core, fields) = (core.clone(), fields.clone());
        TextBox::new()
            .automation_name(label)
            .header(label)
            .text(value)
            .is_enabled(!connect.busy)
            .on_text_changed(move |text: Rc<str>| {
                let mut fields = fields.clone();
                apply(&mut fields, text.to_string());
                core.send(Event::SetConnect { fields });
            })
            .into()
    };
    let chosen = connect
        .schemes
        .iter()
        .position(|choice| choice.scheme == fields.scheme)
        .unwrap_or(0);
    let kinds = {
        let (core, fields) = (core.clone(), fields.clone());
        let schemes: Vec<_> = connect.schemes.iter().map(|choice| choice.scheme).collect();
        ComboBox::new()
            .automation_name("Kind of server")
            .items_source(connect.schemes.iter().map(|choice| choice.label.clone()))
            .selected_index(chosen)
            .on_selection_changed(move |index: Option<usize>| {
                let picked = index.filter(|index| *index != chosen);
                if let Some(scheme) = picked.and_then(|index| schemes.get(index)) {
                    let mut fields = fields.clone();
                    fields.scheme = *scheme;
                    core.send(Event::SetConnect { fields });
                }
            })
    };
    let s3 = fields.scheme == Scheme::S3;
    let mut lines: Vec<View> = vec![
        kinds.into(),
        text("Name", &fields.name, |fields, text| fields.name = text),
        text(
            if s3 {
                "Endpoint (empty for Amazon's)"
            } else {
                "Server"
            },
            &fields.host,
            |fields, text| fields.host = text,
        ),
        text(
            "Port (empty for the usual one)",
            &fields.port,
            |fields, text| {
                fields.port = text;
            },
        ),
    ];
    if s3 {
        lines.push(text("Bucket", &fields.bucket, |fields, text| {
            fields.bucket = text;
        }));
        lines.push(text("Region", &fields.region, |fields, text| {
            fields.region = text;
        }));
    }
    lines.push(text(
        if s3 { "Access key ID" } else { "User" },
        &fields.user,
        |fields, text| fields.user = text,
    ));
    lines.push(
        PasswordBox::new()
            .header(if s3 { "Secret access key" } else { "Password" })
            .placeholder_text("Empty for the one kept in the keychain")
            .is_enabled(!connect.busy)
            .on_password_changed(|text: Rc<str>| {
                TYPED_PASSWORD.with(|typed| *typed.borrow_mut() = text.to_string());
            })
            .into(),
    );
    lines.push(text("Folder to start in", &fields.root, |fields, text| {
        fields.root = text;
    }));
    let switch = |label: &str, on: bool, changed: Callback<bool>| -> View {
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((
                TextBlock::new()
                    .text(label)
                    .vertical_alignment(VerticalAlignment::Center),
                Border::new()
                    .grid_column(1)
                    .content(theme::switch(on).automation_name(label).on_toggled(changed)),
            ))
            .into()
    };
    // SFTP is always encrypted: there is nothing to choose.
    if fields.scheme != Scheme::Sftp {
        let (core, now) = (core.clone(), fields.clone());
        lines.push(switch(
            "Encrypted connection (TLS)",
            fields.secure,
            Callback::new(move |on: bool| {
                let mut fields = now.clone();
                fields.secure = on;
                core.send(Event::SetConnect { fields });
            }),
        ));
    }
    lines.push(switch(
        "Keep the password in the keychain",
        REMEMBER_PASSWORD.with(Cell::get),
        Callback::new(|on: bool| REMEMBER_PASSWORD.with(|remember| remember.set(on))),
    ));
    if connect.busy {
        lines.push(
            TextBlock::new()
                .text("Connecting\u{2026}")
                .opacity(0.6)
                .into(),
        );
    }
    if !connect.error.is_empty() {
        lines.push(
            TextBlock::new()
                .text(connect.error.clone())
                .foreground(Color::rgb(0xE0, 0x50, 0x50))
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    ScrollViewer::new()
        .max_height(460.0)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(8.0)
                .children(lines),
        )
        .into()
}

thread_local! {
    /// Whether the connect dialog's password goes to the keychain: the
    /// window's own choice, kept between dialogs.
    static REMEMBER_PASSWORD: Cell<bool> = const { Cell::new(true) };
}

thread_local! {
    /// The password being typed into the archive password dialog, until the
    /// dialog is answered. Only the window's thread sees it.
    static TYPED_PASSWORD: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The field an archive's password is typed into, and a word when the one
/// given before did not fit.
fn archive_password(wrong: bool) -> View {
    let field = PasswordBox::new()
        .placeholder_text("Password")
        .on_password_changed(|text: Rc<str>| {
            TYPED_PASSWORD.with(|typed| *typed.borrow_mut() = text.to_string());
        });
    let mut lines: Vec<View> = Vec::new();
    if wrong {
        lines.push(
            TextBlock::new()
                .text("That password did not fit. Try again.")
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    lines.push(field.into());
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}

/// Which dialog a window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dialog {
    #[default]
    None,
    /// Items being copied or moved are already there.
    Conflict,
    /// A drive still in use is about to be ejected.
    Eject,
    /// An archive asks for its password.
    ArchivePassword,
    /// A server showed a key nobody accepted yet.
    HostKey,
    /// Connecting to a server.
    Connect,
    Ask(Asking),
    /// The command palette.
    Palette,
    /// Choosing a video's thumbnail.
    Picker,
    /// A plugin's form, before it runs.
    PluginForm,
    /// What a plugin found.
    PluginResults,
    /// A plugin's action that asks before it runs; the window holds which.
    PluginConfirm,
    /// Renaming several items at once, in the focused tab.
    Batch,
    /// Naming the archive to make of the selection, in the focused tab.
    Compress,
    /// Go to Folder, in the focused tab.
    GoTo,
}

impl Dialog {
    /// The dialog to show for the state the core is in. `leads` is whether
    /// this window shows what belongs to the whole app, `palette` whether the
    /// palette was asked for here, `picker` whether the video whose thumbnail
    /// is being chosen is in one of this window's tabs; `asking` is the
    /// window's own question, if it has one.
    pub fn of(
        shared: &ViewModel,
        tab: &TabViewModel,
        here: &Here<'_>,
        asking: Option<Asking>,
    ) -> Self {
        let from_core = if here.leads && shared.conflict.is_some() {
            Self::Conflict
        } else if here.leads && shared.eject_confirm.is_some() {
            Self::Eject
        } else if here.leads && shared.archive_password.is_some() {
            Self::ArchivePassword
        } else if here.leads && shared.host_key.is_some() {
            Self::HostKey
        } else if here.leads && shared.connect.is_some() {
            Self::Connect
        } else if here.palette && shared.palette.is_some() {
            Self::Palette
        } else if shared
            .thumbnail_picker
            .as_ref()
            .is_some_and(|picker| here.has(picker.tab))
        {
            Self::Picker
        } else if shared
            .plugin_form
            .as_ref()
            .is_some_and(|form| here.has(form.tab))
        {
            Self::PluginForm
        } else if shared
            .plugin_results
            .as_ref()
            .is_some_and(|results| here.has(results.tab))
        {
            Self::PluginResults
        } else if tab.batch_rename.is_some() {
            Self::Batch
        } else if tab.compress.is_some() {
            Self::Compress
        } else if tab.goto.is_some() {
            Self::GoTo
        } else {
            Self::None
        };
        from_core.with(asking)
    }

    /// This dialog, or the window's own question when that is more pressing:
    /// what the whole app asks comes first, a tab's dialog last.
    pub const fn with(self, asking: Option<Asking>) -> Self {
        match (self, asking) {
            (Self::None | Self::GoTo | Self::Batch | Self::Compress, Some(Asking::Plugin)) => {
                Self::PluginConfirm
            }
            (Self::None | Self::GoTo | Self::Batch | Self::Compress, Some(asking)) => {
                Self::Ask(asking)
            }
            (dialog, _) => dialog,
        }
    }

    /// How the connect dialog was answered: connect with what was typed,
    /// forget the connection being edited, or leave it.
    pub fn connect_closed(shared: &ViewModel, result: ContentDialogResult) -> Event {
        let typed = TYPED_PASSWORD.with(RefCell::take);
        match (result, shared.connect.as_ref()) {
            (ContentDialogResult::Primary, _) => Event::SubmitConnect {
                secret: Secret::new(typed),
                remember: REMEMBER_PASSWORD.with(Cell::get),
            },
            (ContentDialogResult::Secondary, Some(connect)) if connect.editing => {
                Event::RemoveConnection {
                    id: connect.id.clone(),
                }
            }
            _ => Event::CancelConnect,
        }
    }

    /// Tells the core how the dialog was answered. `in_trash` is whether the
    /// tab shows the Recycle Bin, where deleting means something else.
    pub fn closed(self, core: &CoreLink, tab: u32, in_trash: bool, result: ContentDialogResult) {
        let confirmed = result == ContentDialogResult::Primary;
        let event = match self {
            // The window acts on a drop's answer: it holds what was dropped.
            Self::None
            | Self::Ask(
                Asking::Drop(_)
                | Asking::Shortcuts
                | Asking::Settings
                | Asking::Plugin
                | Asking::SaveSearch
                | Asking::EditSearch,
            ) => {
                return;
            }
            Self::Palette => Event::ClosePalette,
            Self::Picker => match result {
                ContentDialogResult::Primary => Event::UsePickerFrame,
                ContentDialogResult::Secondary => Event::ResetPickerFrame,
                _ => Event::ClosePicker,
            },
            Self::PluginForm if confirmed => Event::SubmitPluginForm,
            Self::PluginForm => Event::CancelPluginForm,
            // Closing the results leaves a running job to finish on its own.
            Self::PluginResults => Event::HidePluginResults,
            // The window acts on these: it holds the action, and knows
            // which connection the dialog was about.
            Self::PluginConfirm | Self::Connect => return,
            Self::Conflict => Event::ResolveConflict {
                choice: match result {
                    ContentDialogResult::Primary => ConflictChoice::Replace,
                    ContentDialogResult::Secondary => ConflictChoice::Skip,
                    // Closed without an answer: nothing is touched.
                    _ => ConflictChoice::Cancel,
                },
            },
            Self::Eject if confirmed => Event::ConfirmEject,
            Self::Eject => Event::CancelEject,
            Self::HostKey => Event::AcceptHostKey { accept: confirmed },
            // What was typed is taken either way: it is kept nowhere here.
            Self::ArchivePassword => {
                let typed = TYPED_PASSWORD.with(RefCell::take);
                if confirmed {
                    Event::SubmitArchivePassword {
                        password: Secret::new(typed),
                    }
                } else {
                    Event::CancelArchivePassword
                }
            }
            Self::Ask(_) if !confirmed => return,
            Self::Ask(Asking::Trash) => Event::TrashSelection { tab },
            // The Recycle Bin's items are purged; a folder's are deleted for good.
            Self::Ask(Asking::Delete) if in_trash => Event::PurgeSelection { tab },
            Self::Ask(Asking::Delete) => Event::DeleteSelection { tab },
            Self::Ask(Asking::EmptyTrash) => Event::EmptyTrash,
            Self::Batch if confirmed => Event::ApplyBatchRename { tab },
            Self::Batch => Event::CancelBatchRename { tab },
            Self::Compress if confirmed => Event::ApplyCompress { tab },
            Self::Compress => Event::CancelCompress { tab },
            Self::GoTo if confirmed => Event::GoToSubmit { tab, path: None },
            Self::GoTo => Event::CloseGoTo { tab },
        };
        core.send(event);
    }

    /// The dialog itself. Always one, shut when there is nothing to ask, so
    /// the window's content keeps the same attachment.
    #[expect(clippy::too_many_lines, reason = "one arm per dialog")]
    pub fn build(
        self,
        core: &CoreLink,
        shared: &ViewModel,
        tab: &TabViewModel,
        hooks: &Hooks<'_>,
    ) -> ContentDialog {
        let Hooks {
            on_closed,
            on_palette,
            owner,
            confirm,
            draft,
        } = *hooks;
        let on_closed = on_closed.clone();
        let dialog = ContentDialog::new()
            .is_open(self != Self::None)
            .on_closed(on_closed);
        let text = |text: &str| -> View {
            TextBlock::new()
                .text(text)
                .text_wrapping(TextWrapping::Wrap)
                .into()
        };
        let bin = shared.trash.label.as_str();
        let items = if tab.selected_count == 1 {
            "this item".to_owned()
        } else {
            format!("these {} items", tab.selected_count)
        };
        let (dialog, content): (ContentDialog, View) = match self {
            Self::None => (dialog, Border::new().into()),
            Self::Conflict => {
                let conflict = shared.conflict.as_ref();
                (
                    dialog
                        .title(conflict.map_or_else(String::new, conflict_title))
                        .primary_button_text("Replace")
                        .secondary_button_text("Skip")
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Close),
                    conflict.map_or_else(
                        || Border::new().into(),
                        |conflict| keep_both(core, conflict),
                    ),
                )
            }
            Self::Eject => {
                let ask = shared.eject_confirm.as_ref();
                (
                    dialog
                        .title(ask.map_or("", |ask| ask.title.as_str()))
                        .primary_button_text(ask.map_or("Eject", |ask| ask.button.as_str()))
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Close),
                    text(ask.map_or("", |ask| ask.message.as_str())),
                )
            }
            Self::HostKey => {
                let ask = shared.host_key.as_ref();
                (
                    dialog
                        .title("Is this the right server?")
                        .primary_button_text("Trust and Connect")
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Close),
                    text(&ask.map_or_else(String::new, |ask| {
                        format!(
                            "{} shows a key this app has not seen for it:\n\n{}\n\nConnect \
                             only if that is the key you expect.",
                            ask.host, ask.fingerprint
                        )
                    })),
                )
            }
            Self::Connect => {
                let connect = shared.connect.as_ref();
                let dialog = dialog
                    .title(connect.map_or("", |connect| connect.title.as_str()))
                    .primary_button_text("Connect")
                    .is_primary_button_enabled(connect.is_some_and(|connect| !connect.busy))
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary);
                // A saved connection can be forgotten from here.
                let dialog = if connect.is_some_and(|connect| connect.editing) {
                    dialog.secondary_button_text("Remove")
                } else {
                    dialog
                };
                (
                    dialog,
                    connect.map_or_else(
                        || Border::new().into(),
                        |connect| connect_form(core, connect),
                    ),
                )
            }
            Self::ArchivePassword => {
                let ask = shared.archive_password.as_ref();
                (
                    dialog
                        .title(ask.map_or_else(String::new, |ask| {
                            format!("\u{201c}{}\u{201d} needs a password", ask.archive)
                        }))
                        .primary_button_text("OK")
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Primary),
                    archive_password(ask.is_some_and(|ask| ask.wrong)),
                )
            }
            Self::Ask(Asking::Trash) => (
                dialog
                    .title(format!("Move {items} to the {bin}?"))
                    .primary_button_text(format!("Move to {bin}"))
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary),
                text(&format!("You can take them back out of the {bin}.")),
            ),
            Self::Ask(Asking::Delete) => (
                dialog
                    .title(format!("Delete {items} immediately?"))
                    .primary_button_text("Delete")
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Close),
                text("This cannot be undone."),
            ),
            Self::Ask(Asking::EmptyTrash) => (
                dialog
                    .title(format!("Empty the {bin}?"))
                    .primary_button_text(format!("Empty {bin}"))
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Close),
                text("Every item in it is deleted for good. This cannot be undone."),
            ),
            Self::Ask(Asking::Drop(count)) => (
                dialog
                    .title(if count == 1 {
                        "Move or copy this item?".to_owned()
                    } else {
                        format!("Move or copy these {count} items?")
                    })
                    .primary_button_text("Move")
                    .secondary_button_text("Copy")
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary),
                Border::new().into(),
            ),
            Self::Ask(Asking::Shortcuts) => (
                dialog
                    .title("Keyboard Shortcuts")
                    .close_button_text("Close"),
                sheets::shortcuts(shared),
            ),
            Self::Ask(Asking::Settings) => (
                dialog.title("Settings").close_button_text("Done"),
                sheets::settings(core, shared),
            ),
            Self::Ask(Asking::SaveSearch) => (
                dialog
                    .title("Save Search")
                    .primary_button_text("Save")
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary),
                saved::naming(draft),
            ),
            Self::Ask(Asking::EditSearch) => (
                dialog
                    .title("Edit Saved Search")
                    .primary_button_text("Save")
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary),
                saved::editor(draft),
            ),
            Self::Palette => (
                dialog.title("Commands").close_button_text("Cancel"),
                shared.palette.as_ref().map_or_else(
                    || Border::new().into(),
                    |palette| sheets::palette(core, palette, on_palette),
                ),
            ),
            Self::Picker => {
                let choosing = shared.thumbnail_picker.as_ref();
                (
                    dialog
                        .title(choosing.map_or_else(String::new, |choosing| {
                            format!("Thumbnail for “{}”", choosing.name)
                        }))
                        .primary_button_text("Use Frame")
                        .is_primary_button_enabled(
                            choosing.is_some_and(|choosing| choosing.can_apply),
                        )
                        .secondary_button_text("Reset to Default")
                        .is_secondary_button_enabled(
                            choosing.is_some_and(|choosing| choosing.can_reset),
                        )
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Primary),
                    choosing.map_or_else(
                        || Border::new().into(),
                        |choosing| picker::picker(core, choosing),
                    ),
                )
            }
            Self::PluginForm => {
                let form = shared.plugin_form.as_ref();
                let ready = form.is_some_and(|form| !form.loading && !form.busy && !form.blocked);
                let dialog = dialog
                    .title(form.map_or("", |form| form.title.as_str()))
                    .close_button_text(if form.is_some_and(|form| form.blocked) {
                        "Close"
                    } else {
                        "Cancel"
                    });
                (
                    match form.filter(|form| !form.blocked) {
                        Some(form) => dialog
                            .primary_button_text(form.submit.clone())
                            .is_primary_button_enabled(ready)
                            .default_button(ContentDialogButton::Primary),
                        None => dialog,
                    },
                    form.map_or_else(
                        || Border::new().into(),
                        |form| plugins::form(core, form, owner),
                    ),
                )
            }
            Self::PluginResults => {
                let results = shared.plugin_results.as_ref();
                (
                    dialog
                        .title(results.map_or("", |results| results.title.as_str()))
                        .close_button_text("Done"),
                    results.map_or_else(
                        || Border::new().into(),
                        |results| plugins::results(core, results, owner),
                    ),
                )
            }
            Self::PluginConfirm | Self::Ask(Asking::Plugin) => (
                dialog
                    .title(confirm)
                    .primary_button_text("Continue")
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Close),
                Border::new().into(),
            ),
            Self::Compress => {
                let compress = tab.compress.as_ref();
                (
                    dialog
                        .title(compress.map_or_else(String::new, |compress| {
                            if compress.count == 1 {
                                "Compress 1 Item".to_owned()
                            } else {
                                format!("Compress {} Items", compress.count)
                            }
                        }))
                        .primary_button_text("Compress")
                        .is_primary_button_enabled(
                            compress.is_some_and(|compress| compress.error.is_empty()),
                        )
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Primary),
                    compress.map_or_else(
                        || Border::new().into(),
                        |compress| compress_form(core, tab.id, compress),
                    ),
                )
            }
            Self::Batch => {
                let batch = tab.batch_rename.as_ref();
                let ready = batch.is_some_and(|batch| batch.changes > 0 && batch.problems == 0);
                (
                    dialog
                        .title(format!("Rename {} Items", tab.selected_count))
                        .primary_button_text("Rename")
                        .is_primary_button_enabled(ready)
                        .close_button_text("Cancel")
                        .default_button(ContentDialogButton::Primary),
                    batch.map_or_else(
                        || Border::new().into(),
                        |batch| batch_rename(core, tab.id, batch),
                    ),
                )
            }
            Self::GoTo => (
                dialog
                    .title("Go to Folder")
                    .primary_button_text("Go")
                    .close_button_text("Cancel")
                    .default_button(ContentDialogButton::Primary),
                tab.goto
                    .as_ref()
                    .map_or_else(|| Border::new().into(), |goto| go_to(core, tab.id, goto)),
            ),
        };
        // What is asked changes; the border it sits in does not.
        dialog.content(Border::new().min_width(420.0).content(content))
    }
}

fn conflict_title(conflict: &ConflictView) -> String {
    if conflict.count == 1 {
        format!(
            "“{}” already exists in “{}”",
            conflict.name, conflict.folder
        )
    } else {
        format!(
            "{} items already exist in “{}”",
            conflict.count, conflict.folder
        )
    }
}

/// What the conflict dialog says, and its fourth answer: a dialog has three buttons.
fn keep_both(core: &CoreLink, conflict: &ConflictView) -> View {
    let core = core.clone();
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(12.0)
        .children((
            TextBlock::new()
                .text(if conflict.copy {
                    "Replace what is there with what you are copying, skip those items, or keep both?"
                } else {
                    "Replace what is there with what you are moving, skip those items, or keep both?"
                })
                .text_wrapping(TextWrapping::Wrap),
            Button::new()
                .content("Keep Both")
                .horizontal_alignment(HorizontalAlignment::Left)
                .on_click(move || {
                    core.send(Event::ResolveConflict {
                        choice: ConflictChoice::KeepBoth,
                    });
                }),
        ))
        .into()
}

/// Renaming several items: how the new names are made, and what they come to.
#[expect(clippy::too_many_lines, reason = "the dialog's lines, top to bottom")]
fn batch_rename(core: &CoreLink, tab: u32, batch: &BatchRenameView) -> View {
    let fields = &batch.fields;
    // Every edit sends all the fields, changed in one place.
    let edit = |change: fn(&mut BatchFields, String)| {
        let core = core.clone();
        let fields = fields.clone();
        Callback::new(move |text: Rc<str>| {
            let mut fields = fields.clone();
            change(&mut fields, text.to_string());
            core.send(Event::SetBatchRename { tab, fields });
        })
    };
    let mode = |label: &str, mode: BatchMode| {
        let core = core.clone();
        let fields = BatchFields {
            mode,
            ..fields.clone()
        };
        Button::new()
            .content(label)
            .style(if batch.fields.mode == mode {
                ButtonStyle::Accent
            } else {
                ButtonStyle::Default
            })
            .on_click(move || {
                core.send(Event::SetBatchRename {
                    tab,
                    fields: fields.clone(),
                });
            })
    };
    let mut lines: Vec<View> = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children((
                mode("Replace text", BatchMode::Replace),
                mode("Name by pattern", BatchMode::Pattern),
            ))
            .into(),
    ];
    match fields.mode {
        BatchMode::Replace => {
            lines.push(
                TextBox::new()
                    .text(fields.find.clone())
                    .placeholder_text("Find")
                    .on_text_changed(edit(|fields, text| fields.find = text))
                    .into(),
            );
            lines.push(
                TextBox::new()
                    .text(fields.replace.clone())
                    .placeholder_text("Replace with")
                    .on_text_changed(edit(|fields, text| fields.replace = text))
                    .into(),
            );
        }
        BatchMode::Pattern => {
            lines.push(
                TextBox::new()
                    .automation_name("Pattern")
                    .text(fields.pattern.clone())
                    .placeholder_text("{name} {n}{ext}")
                    .on_text_changed(edit(|fields, text| fields.pattern = text))
                    .into(),
            );
            lines.push(
                TextBox::new()
                    .text(fields.start.to_string())
                    .placeholder_text("First number")
                    .on_text_changed(edit(|fields, text| {
                        if let Ok(start) = text.trim().parse() {
                            fields.start = start;
                        }
                    }))
                    .into(),
            );
        }
    }
    lines.push(
        TextBlock::new()
            .text(if batch.problems > 0 {
                format!("{} names can't be used", batch.problems)
            } else {
                format!("{} of {} names change", batch.changes, batch.items.len())
            })
            .font_size(12.0)
            .opacity(0.7)
            .into(),
    );
    // A long selection is shown by its beginning; all of it is renamed.
    let preview: Vec<View> = batch
        .items
        .iter()
        .take(200)
        .map(|item| {
            let refused = !item.problem.is_empty();
            let new = TextBlock::new()
                .text(if refused {
                    format!("{} ({})", item.new, item.problem)
                } else {
                    item.new.clone()
                })
                .font_size(12.0);
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children((
                    TextBlock::new()
                        .text(item.old.clone())
                        .font_size(12.0)
                        .opacity(0.6),
                    TextBlock::new().text("→").font_size(12.0).opacity(0.6),
                    if refused {
                        new.foreground(Color::rgb(0xE0, 0x50, 0x50))
                    } else {
                        new
                    },
                ))
                .into()
        })
        .collect();
    lines.push(
        ScrollViewer::new()
            .max_height(260.0)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .orientation(Orientation::Vertical)
                    .spacing(2.0)
                    .children(preview),
            )
            .into(),
    );
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}

/// Go to Folder: the path typed so far, and the folders it could mean.
fn go_to(core: &CoreLink, tab: u32, goto: &GoToView) -> View {
    let typed = {
        let core = core.clone();
        Callback::new(move |text: Rc<str>| {
            core.send(Event::GoToTyped {
                tab,
                text: text.to_string(),
            });
        })
    };
    let mut lines: Vec<View> = vec![
        TextBox::new()
            .text(goto.text.clone())
            .placeholder_text("A path, or the name of a favorite or recent folder")
            .on_text_changed(typed)
            .into(),
    ];
    if let Some(error) = &goto.error {
        lines.push(
            TextBlock::new()
                .text(error.clone())
                .foreground(Color::rgb(0xE0, 0x50, 0x50))
                .text_wrapping(TextWrapping::Wrap)
                .into(),
        );
    }
    let matches: Vec<View> = goto
        .matches
        .iter()
        .map(|found| {
            let core = core.clone();
            let path = found.path.clone();
            Button::new()
                .style(ButtonStyle::Subtle)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Left)
                .content(
                    StackPanel::new()
                        .orientation(Orientation::Vertical)
                        .children((
                            TextBlock::new().text(found.title.clone()),
                            TextBlock::new()
                                .text(found.path.clone())
                                .font_size(11.0)
                                .opacity(0.6),
                        )),
                )
                .on_click(move || {
                    core.send(Event::GoToSubmit {
                        tab,
                        path: Some(path.clone()),
                    });
                })
                .into()
        })
        .collect();
    lines.push(
        ScrollViewer::new()
            .max_height(280.0)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .content(
                StackPanel::new()
                    .orientation(Orientation::Vertical)
                    .children(matches),
            )
            .into(),
    );
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .children(lines)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window that leads and has no tabs to speak of.
    const HERE: Here<'static> = Here {
        leads: true,
        palette: true,
        tabs: &[],
    };

    #[test]
    fn the_whole_apps_question_comes_before_a_tabs() {
        let mut shared = ViewModel::default();
        let mut tab = TabViewModel::default();
        assert_eq!(Dialog::of(&shared, &tab, &HERE, None), Dialog::None);
        tab.goto = Some(GoToView {
            text: String::new(),
            matches: Vec::new(),
            error: None,
        });
        assert_eq!(Dialog::of(&shared, &tab, &HERE, None), Dialog::GoTo);
        assert_eq!(
            Dialog::of(&shared, &tab, &HERE, Some(Asking::Trash)),
            Dialog::Ask(Asking::Trash)
        );
        shared.eject_confirm = Some(dirfleet_core::app::EjectConfirmView {
            title: "Eject Backup?".to_owned(),
            message: String::new(),
            button: "Eject".to_owned(),
        });
        assert_eq!(
            Dialog::of(&shared, &tab, &HERE, Some(Asking::Trash)),
            Dialog::Eject
        );
        // Only one window asks what belongs to the whole app.
        assert_eq!(
            Dialog::of(
                &shared,
                &tab,
                &Here {
                    leads: false,
                    ..HERE
                },
                None
            ),
            Dialog::GoTo
        );
    }

    #[test]
    fn a_conflict_is_named_by_its_item_or_their_number() {
        let mut conflict = ConflictView {
            count: 1,
            name: "a.txt".to_owned(),
            folder: "Docs".to_owned(),
            copy: true,
        };
        assert_eq!(
            conflict_title(&conflict),
            "“a.txt” already exists in “Docs”"
        );
        conflict.count = 3;
        assert_eq!(conflict_title(&conflict), "3 items already exist in “Docs”");
    }
}
