//! Tags: coloured labels the user gives to files and folders.
//!
//! The tags themselves (a name and a colour each, in the user's order) are
//! part of the app's state. Which item has which tag is kept by the effects
//! beside it, by path; the core holds a copy to draw from. Tags follow an
//! item renamed or moved in the app. A tag's items show in a tab of their
//! own, a results tab like the duplicates', whose rows are the tagged
//! items wherever they are.
use std::collections::HashMap;
use std::fmt::Write as _;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::results::Results;
use super::{DirFleet, Effect, Event, ListDirOutput, ListView, Model, Tab, tab_id};
use crate::ids::TabId;

/// A tag's colour.
#[derive(Facet, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(C)]
pub enum TagColor {
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Purple,
    #[default]
    Gray,
}

impl TagColor {
    pub const ALL: [Self; 7] = [
        Self::Red,
        Self::Orange,
        Self::Yellow,
        Self::Green,
        Self::Blue,
        Self::Purple,
        Self::Gray,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Red => "Red",
            Self::Orange => "Orange",
            Self::Yellow => "Yellow",
            Self::Green => "Green",
            Self::Blue => "Blue",
            Self::Purple => "Purple",
            Self::Gray => "Gray",
        }
    }
}

/// A tag: kept with the app's state.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct TagDef {
    /// Never given to another tag, so items keep the right one.
    pub id: u32,
    pub name: String,
    pub color: TagColor,
}

/// A tag as shells list it.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct TagView {
    pub id: u32,
    pub name: String,
    pub color: TagColor,
    /// How many items have it.
    pub count: u32,
}

/// The tag being made or changed, in its editor.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct TagDraftView {
    /// The tag changed; `None` for a new one.
    pub id: Option<u32>,
    pub name: String,
    pub color: TagColor,
    /// Why it can't be saved as it is; "" when it can.
    pub problem: String,
}

/// What a tab that shows a tag's items says about them.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct TagPlaceView {
    pub id: u32,
    pub name: String,
    pub color: TagColor,
    /// Tagged items that are no longer where they were.
    pub missing: u32,
}

/// What the effects do with the tags items have. Every operation answers
/// with all of them as they are afterwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TagOp {
    /// The tags' names, so `tag:name` can be searched for; sent at start
    /// and whenever a tag is made, renamed or deleted.
    Names(Vec<(u32, String)>),
    /// Give the items at `paths` the tag, or take it from them.
    Set {
        paths: Vec<String>,
        tag: u32,
        on: bool,
    },
    /// Take every tag from the items at `paths`.
    Clear { paths: Vec<String> },
    /// Items renamed or moved, `(from, to)`: their tags and those of
    /// everything below them follow.
    Rename(Vec<(String, String)>),
    /// The tag was deleted: no item has it any more.
    Forget { tag: u32 },
}

/// The tags of every tagged item, by path. Opaque to facet: it never
/// leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Default, Facet)]
#[facet(opaque)]
pub struct Tagged(pub Vec<(String, Vec<u32>)>);

impl Operation for TagOp {
    type Output = Tagged;
}

/// List the items that have `tag`: the ones on this machine that are still
/// there, and the ones on servers as they were tagged (no server is asked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListTagged {
    pub tag: u32,
    pub view: ListView,
}

/// A tag's items as a listing whose entries know their folders, and the
/// tagged paths where nothing is any more.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
pub struct TaggedListing {
    pub output: ListDirOutput,
    pub missing: Vec<String>,
}

impl Operation for ListTagged {
    type Output = TaggedListing;
}

/// What a results tab keeps when it shows a tag's items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TagPlace {
    pub(super) tag: u32,
    /// The tag's name, which the tab is called by.
    pub(super) name: String,
    /// Tagged paths where nothing is any more.
    missing: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Draft {
    id: Option<u32>,
    name: String,
    color: TagColor,
}

/// The tags, and which item has which.
pub(super) struct Tags {
    defs: Vec<TagDef>,
    next_id: u32,
    /// The tags of each tagged item, by path as rows name items.
    tagged: HashMap<String, Vec<u32>>,
    draft: Option<Draft>,
    /// The effects were told the tags' names.
    names_sent: bool,
}

impl Default for Tags {
    /// The seven tags the app starts with, one of each colour.
    fn default() -> Self {
        let defs: Vec<TagDef> = TagColor::ALL
            .into_iter()
            .zip(1..)
            .map(|(color, id)| TagDef {
                id,
                name: color.label().to_owned(),
                color,
            })
            .collect();
        Self {
            next_id: defs.len() as u32 + 1,
            defs,
            tagged: HashMap::new(),
            draft: None,
            names_sent: false,
        }
    }
}

impl Tags {
    /// The tags to keep with the app's state.
    pub(super) fn saved(&self) -> Vec<TagDef> {
        self.defs.clone()
    }

    /// Takes the kept tags back; `None` (a state from before there were
    /// tags) leaves the ones the app starts with.
    pub(super) fn load(&mut self, saved: Option<Vec<TagDef>>) {
        let Some(mut defs) = saved else {
            return;
        };
        // An id is one tag's: a second tag with it goes.
        let mut seen = Vec::new();
        defs.retain(|def| {
            let fresh = !seen.contains(&def.id);
            seen.push(def.id);
            fresh
        });
        self.next_id = defs.iter().map(|def| def.id).max().unwrap_or(0) + 1;
        self.defs = defs;
    }

    fn def(&self, tag: u32) -> Option<&TagDef> {
        self.defs.iter().find(|def| def.id == tag)
    }

    fn names(&self) -> TagOp {
        TagOp::Names(
            self.defs
                .iter()
                .map(|def| (def.id, def.name.clone()))
                .collect(),
        )
    }

    /// The tags of the item at `path`, in the tags' own order.
    pub(super) fn of(&self, path: &str) -> Vec<u32> {
        let Some(tags) = self.tagged.get(path) else {
            return Vec::new();
        };
        self.defs
            .iter()
            .map(|def| def.id)
            .filter(|id| tags.contains(id))
            .collect()
    }

    /// The colours of the tags of the item at `path`, in the tags' order.
    pub(super) fn colors_of(&self, path: &str) -> Vec<TagColor> {
        let Some(tags) = self.tagged.get(path) else {
            return Vec::new();
        };
        self.defs
            .iter()
            .filter(|def| tags.contains(&def.id))
            .map(|def| def.color)
            .collect()
    }

    /// Whether the item at `path` has a tag called `name`, whatever its case.
    pub(super) fn has_named(&self, path: &str, name: &str) -> bool {
        self.tagged.get(path).is_some_and(|tags| {
            self.defs
                .iter()
                .any(|def| tags.contains(&def.id) && def.name.to_lowercase() == name)
        })
    }

    pub(super) fn any_tagged(&self) -> bool {
        !self.tagged.is_empty()
    }

    pub(super) fn views(&self) -> Vec<TagView> {
        let mut counts: HashMap<u32, u32> = HashMap::new();
        for tags in self.tagged.values() {
            for tag in tags {
                *counts.entry(*tag).or_default() += 1;
            }
        }
        self.defs
            .iter()
            .map(|def| TagView {
                id: def.id,
                name: def.name.clone(),
                color: def.color,
                count: counts.get(&def.id).copied().unwrap_or_default(),
            })
            .collect()
    }

    /// Why the draft can't be saved; "" when it can.
    fn problem(&self, draft: &Draft) -> &'static str {
        let name = draft.name.trim();
        if name.is_empty() {
            "A tag needs a name."
        } else if name.contains(char::is_whitespace) {
            // So `tag:name` finds it without quotes.
            "A tag's name is one word."
        } else if self
            .defs
            .iter()
            .any(|def| Some(def.id) != draft.id && def.name.eq_ignore_ascii_case(name))
        {
            "Another tag has this name."
        } else {
            ""
        }
    }

    pub(super) fn draft_view(&self) -> Option<TagDraftView> {
        let draft = self.draft.as_ref()?;
        Some(TagDraftView {
            id: draft.id,
            name: draft.name.clone(),
            color: draft.color,
            problem: self.problem(draft).to_owned(),
        })
    }
}

impl Results {
    /// What a tab shows while it lists the items of `tag`.
    fn tagged(tag: u32, name: String) -> Self {
        let mut results = Self::of(Vec::new(), None);
        results.tag = Some(TagPlace {
            tag,
            name,
            missing: Vec::new(),
        });
        results
    }
}

impl Tab {
    /// The tag whose items the tab shows.
    pub(super) fn shown_tag(&self) -> Option<u32> {
        Some(self.results.as_ref()?.tag.as_ref()?.tag)
    }

    /// The paths the tab's selection (or cursor) names, as tags are kept.
    fn tag_targets(&self) -> Vec<String> {
        self.op_targets()
            .into_iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    }
}

impl Model {
    /// A tab's results, with what is the tags' to say when it shows a
    /// tag's items: which tag, how many items, how many are gone.
    pub(super) fn results_of(&self, tab: &Tab) -> Option<super::ResultsView> {
        let mut results = tab.results_view()?;
        if let Some(place) = self.tag_place(tab) {
            let items = tab.order.len();
            results.title.clone_from(&place.name);
            results.summary = match (tab.loading, items) {
                (true, _) => String::new(),
                (false, 0) => "Nothing has this tag".to_owned(),
                (false, 1) => "1 item".to_owned(),
                (false, items) => format!("{items} items"),
            };
            if !tab.loading {
                let _ = match place.missing {
                    0 => Ok(()),
                    1 => write!(results.summary, ". 1 more is no longer where it was"),
                    missing => write!(
                        results.summary,
                        ". {missing} more are no longer where they were"
                    ),
                };
            }
            results.tag = Some(place);
        }
        Some(results)
    }

    /// What a tab that shows a tag's items says about them.
    fn tag_place(&self, tab: &Tab) -> Option<TagPlaceView> {
        let place = tab.results.as_ref()?.tag.as_ref()?;
        let def = self.tags.def(place.tag);
        Some(TagPlaceView {
            id: place.tag,
            name: def.map_or_else(|| "Deleted tag".to_owned(), |def| def.name.clone()),
            color: def.map(|def| def.color).unwrap_or_default(),
            missing: u32::try_from(place.missing.len()).unwrap_or(u32::MAX),
        })
    }

    /// The colours of the tags of the tab's row at `position`.
    pub(super) fn row_tags(&self, tab: &Tab, position: usize) -> Vec<TagColor> {
        if !self.tags.any_tagged() {
            return Vec::new();
        }
        tab.row_op_path(position)
            .map(|path| self.tags.colors_of(&path.to_string_lossy()))
            .unwrap_or_default()
    }

    /// The tags every selected row of the tab has, for menus to mark.
    pub(super) fn selection_tags(&self, tab: &Tab) -> Vec<u32> {
        if !self.tags.any_tagged() {
            return Vec::new();
        }
        let mut shared: Option<Vec<u32>> = None;
        for path in tab.tag_targets() {
            let tags = self.tags.of(&path);
            shared = Some(match shared {
                Some(shared) => shared
                    .into_iter()
                    .filter(|tag| tags.contains(tag))
                    .collect(),
                None => tags,
            });
            if shared.as_ref().is_some_and(Vec::is_empty) {
                break;
            }
        }
        shared.unwrap_or_default()
    }
}

impl DirFleet {
    fn tag_op(op: TagOp) -> Command<Effect, Event> {
        Command::request_from_shell(op).then_send(|tagged| Event::TagsChanged { tagged })
    }

    /// What follows the saved state's arrival: the tags are known, so
    /// their items are asked for; access is checked; plugins are found.
    pub(super) fn after_state(model: &mut Model) -> Command<Effect, Event> {
        Self::sync_tags(model)
            .and(Self::check_access(model))
            .and(Self::load_plugins())
    }

    /// Tells the effects the tags' names, which answers with every item's
    /// tags: at start, and whenever the tags change.
    pub(super) fn sync_tags(model: &mut Model) -> Command<Effect, Event> {
        model.tags.names_sent = true;
        Self::tag_op(model.tags.names())
    }

    /// Does `op` on the items' tags, the tags' names told first if they
    /// were not yet: searches by a tag's name go by them.
    fn change_tags(model: &mut Model, op: TagOp) -> Command<Effect, Event> {
        if model.tags.names_sent {
            Self::tag_op(op)
        } else {
            Self::sync_tags(model).then(Self::tag_op(op))
        }
    }

    /// Tags follow items renamed or moved in the app.
    pub(super) fn rename_tagged(
        model: &Model,
        pairs: Vec<(String, String)>,
    ) -> Command<Effect, Event> {
        if !model.tags.any_tagged() {
            return Command::done();
        }
        Self::tag_op(TagOp::Rename(pairs))
    }

    /// The events of tags.
    #[allow(clippy::too_many_lines, reason = "one arm per event")]
    pub(super) fn tag_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::ToggleTag { tab, tag } | Event::SetTag { tab, tag, .. } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get(&id)) else {
                    return Command::done();
                };
                let paths = state.tag_targets();
                if paths.is_empty() || model.tags.def(tag).is_none() {
                    return Command::done();
                }
                let on = match event {
                    Event::SetTag { on, .. } => on,
                    // On, unless every item has it already.
                    _ => !paths.iter().all(|path| model.tags.of(path).contains(&tag)),
                };
                Self::change_tags(model, TagOp::Set { paths, tag, on })
            }
            Event::ClearTags { tab } => {
                let Some(state) = tab_id(tab).and_then(|id| model.tabs.get(&id)) else {
                    return Command::done();
                };
                let paths = state.tag_targets();
                if paths.is_empty() {
                    return Command::done();
                }
                Self::change_tags(model, TagOp::Clear { paths })
            }
            Event::EditTag { tag } => {
                let draft = match tag {
                    Some(id) => model.tags.def(id).map(|def| Draft {
                        id: Some(id),
                        name: def.name.clone(),
                        color: def.color,
                    }),
                    None => Some(Draft {
                        id: None,
                        name: String::new(),
                        // The colour fewest tags have.
                        color: TagColor::ALL
                            .into_iter()
                            .min_by_key(|color| {
                                model
                                    .tags
                                    .defs
                                    .iter()
                                    .filter(|def| def.color == *color)
                                    .count()
                            })
                            .unwrap_or_default(),
                    }),
                };
                model.tags.draft = draft;
                render()
            }
            Event::SetTagDraft { name, color } => {
                if let Some(draft) = model.tags.draft.as_mut() {
                    draft.name = name;
                    draft.color = color;
                }
                render()
            }
            Event::ApplyTagDraft => {
                let Some(draft) = model.tags.draft.clone() else {
                    return Command::done();
                };
                if !model.tags.problem(&draft).is_empty() {
                    return Command::done();
                }
                let name = draft.name.trim().to_owned();
                if let Some(id) = draft.id {
                    // The tabs that show its items are called by it.
                    for place in model
                        .tabs
                        .values_mut()
                        .filter_map(|tab| tab.results.as_mut()?.tag.as_mut())
                        .filter(|place| place.tag == id)
                    {
                        place.name.clone_from(&name);
                    }
                    if let Some(def) = model.tags.defs.iter_mut().find(|def| def.id == id) {
                        def.name = name;
                        def.color = draft.color;
                    }
                } else {
                    let id = model.tags.next_id;
                    model.tags.next_id += 1;
                    model.tags.defs.push(TagDef {
                        id,
                        name,
                        color: draft.color,
                    });
                }
                model.tags.draft = None;
                render().and(Self::sync_tags(model))
            }
            Event::CancelTagDraft => {
                model.tags.draft = None;
                render()
            }
            Event::DeleteTag { tag } => {
                let before = model.tags.defs.len();
                model.tags.defs.retain(|def| def.id != tag);
                if model.tags.defs.len() == before {
                    return Command::done();
                }
                render()
                    .and(Self::tag_op(TagOp::Forget { tag }))
                    .and(Self::sync_tags(model))
            }
            Event::MoveTag { tag, to } => {
                let Some(from) = model.tags.defs.iter().position(|def| def.id == tag) else {
                    return Command::done();
                };
                let def = model.tags.defs.remove(from);
                let to = (to as usize).min(model.tags.defs.len());
                model.tags.defs.insert(to, def);
                render()
            }
            Event::OpenTag { tab, tag } => Self::open_tag(model, tab, tag),
            Event::RemoveMissingTagged { tab } => {
                let missing = tab_id(tab)
                    .and_then(|id| model.tabs.get_mut(&id))
                    .and_then(|state| state.results.as_mut())
                    .and_then(|results| results.tag.as_mut())
                    .map(|place| std::mem::take(&mut place.missing))
                    .unwrap_or_default();
                if missing.is_empty() {
                    return Command::done();
                }
                render().and(Self::tag_op(TagOp::Clear { paths: missing }))
            }
            Event::TagsChanged { tagged } => {
                model.tags.tagged = tagged.0.into_iter().collect();
                // The tabs that show a tag's items show them anew. Collected
                // first: refreshing needs the model.
                #[allow(clippy::needless_collect)]
                let showing: Vec<u32> = model
                    .tabs
                    .iter()
                    .filter(|(_, tab)| tab.shown_tag().is_some())
                    .map(|(id, _)| id.number())
                    .collect();
                let reloads: Vec<_> = showing
                    .into_iter()
                    .map(|tab| Self::refresh(model, tab))
                    .collect();
                render().and(Command::all(reloads))
            }
            Event::TaggedListed {
                tab,
                generation,
                listed,
            } => {
                let Some(place) = Self::current(model, tab, generation)
                    .and_then(|state| state.results.as_mut())
                    .and_then(|results| results.tag.as_mut())
                else {
                    return Command::done();
                };
                place.missing = listed.missing;
                Self::listed(model, tab, generation, listed.output)
            }
            _ => Command::done(),
        }
    }

    /// Shows the tag's items in a tab beside `tab`: the one that shows
    /// them already, when the window has one.
    fn open_tag(model: &mut Model, tab: u32, tag: u32) -> Command<Effect, Event> {
        let Some(id) = tab_id(tab) else {
            return Command::done();
        };
        let Some(name) = model.tags.def(tag).map(|def| def.name.clone()) else {
            return Command::done();
        };
        let Some(index) = model.window_of(id) else {
            return Command::done();
        };
        let open = model.windows[index]
            .layout
            .tabs()
            .into_iter()
            .find(|tab| model.tabs.get(tab).and_then(Tab::shown_tag) == Some(tag));
        if let Some(open) = open {
            return Self::workspace_event(model, Event::ActivateTab { tab: open.number() });
        }
        let new = TabId::new(model.tab_ids.next_id());
        model.tabs.insert(new, Tab::default());
        let window = &mut model.windows[index];
        window.layout.insert_after(id, new);
        window.focused = new;
        Self::show_scan(model, new, Results::tagged(tag, name))
    }

    /// Lists the tab's tag's items again.
    pub(super) fn list_tagged(tab: &mut Tab, number: u32) -> Command<Effect, Event> {
        let Some(tag) = tab.shown_tag() else {
            return Command::done();
        };
        let generation = tab.generation;
        tab.loading = true;
        Command::request_from_shell(ListTagged {
            tag,
            view: tab.list_view(),
        })
        .then_send(move |listed| Event::TaggedListed {
            tab: number,
            generation,
            listed,
        })
    }
}
