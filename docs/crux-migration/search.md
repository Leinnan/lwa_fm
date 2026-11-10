# Search in the core (T8)

How a tab's search moves from the egui crate into `dirfleet_core`, so the egui
app and the Mac app search the same way.

## Where each part lives

| Part | Was | Now |
|---|---|---|
| Query model and syntax (`Search`, `Chip`, `query::parse`) | already `dirfleet_core::search` | unchanged |
| Matching (`CompiledQuery`, `Highlights`, `kind_of`, `pattern_error`) | `src/app/search/filter.rs` | `dirfleet_core::search::filter`. The current time and the open-history times are passed in, so the core stays pure. |
| Deep walk (`Walk`, partial results every 300 ms, symlink loop guard) | `src/app/dir_handling.rs` | `dirfleet_effects::walk`. egui's `read_directory_streaming` keeps its cached flat reads and calls it for deep walks. |
| Filtering and ordering a listing | egui `filter_visible` on the UI thread | the listing thread's `display_order`, given the tab's filter |
| Search state, history, view model | `TabData.search`, `search_state.rs` | `Tab.search` in the core |

## The flow

A tab's **list view** is its folder view (sort, reverse, hidden items) plus its
**filter**, the search while it has a query. Every listing and order the
effects return carries the list view it was made for. When the tab's list view
has changed since, the core orders the listing again. This is the rule that
already covers sorting, so typing never shows results for an older query.

What is read depends only on the **walk key**: the roots (the folder plus
`in:` folders), the depth, and hidden items. The depth counts only while
there is a query.

- If the walk key is unchanged, a query change re-orders the listing the tab
  already has (`OrderListing`). Typing never reads the disk again.
- If the walk key changed, the old read stops and a new one starts. A flat
  key is read with `ListDir` (one folder, no `in:`, depth 1). Anything else
  goes through `SearchWalk`, a stream of partial listings and then one final
  listing. Leaving the walk sends `StopWalk { id }`, which cancels it in the
  effects; this is the crux #622 workaround already used by `Unwatch`.
- While a walk runs, the tab is `searching`. The view model shows a Stop
  button and the result count, and `StopSearch` keeps what was found so far.

## History and commits

- **Typing** filters live and is committed after 700 ms without typing, on
  Enter, or when the field is left. The core asks the effects for one delay
  per tab (`Delay`) and ignores stale ones by revision.
- **Chips and option changes** commit at once.
- **A commit** records a history entry, so Back and Forward bring searches
  back. Undo steps for searches come with the undo stack (T5).

## View model

- `TabViewModel.search: Option<SearchView>` holds:
  - the typed text;
  - chips, each with its token and an error for a broken regex or glob;
  - match case, depth, match any;
  - `searching`.
- `RowView.highlights` holds byte ranges of the name, only for rows near the
  viewport.
- `RowView.folder` and `ViewModel.columns` gain the Folder column while results
  come from more than one folder.

## Left for later

- Saved searches (area 7) and the Favorites scope, which needs the sidebar (T7).
- Live updates deep inside a walked tree. The root is still watched, as in
  egui for several roots, and a change in it walks again.
