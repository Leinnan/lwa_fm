//! Servers as folders: SFTP, FTP, `WebDAV` and S3.
//!
//! A connection is saved under an id of its own, and a tab on a server has
//! the location `sftp://<id>/folder`. The effects crate keeps the open
//! sessions and the passwords (in the system's keychain when asked to);
//! here are the saved connections, the dialog that makes and edits them,
//! and what happens when a tab needs a server that is not connected.
use std::collections::BTreeSet;

use crux_core::Command;
use crux_core::capability::Operation;
use crux_core::render::render;
use facet::Facet;
use serde::{Deserialize, Serialize};

use super::archives::Secret;
use super::{DirFleet, Effect, Event, Model, tab_id};
use crate::ids::TabId;
use crate::location::{Location, Scheme};

/// A server the user connects to. Its password is not here.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct Connection {
    /// What its locations name it by; never shown.
    pub id: String,
    /// What the sidebar calls it.
    pub name: String,
    pub scheme: Scheme,
    /// The server's name or address. For S3, the endpoint when it is not
    /// Amazon's own.
    pub host: String,
    /// 0 for the protocol's usual port.
    pub port: u16,
    /// The user; for S3 the access key's id.
    pub user: String,
    /// The folder to start in; empty for the server's own choice.
    pub root: String,
    /// TLS: FTPS, and `https` for `WebDAV` and S3 endpoints.
    pub secure: bool,
    pub bucket: String,
    pub region: String,
    /// SFTP: the fingerprint of the host key the user accepted.
    pub host_key: Option<String>,
    /// SFTP: the private key file to log in with, in place of a password;
    /// the password is then what unlocks the key. Empty for none.
    pub key_file: String,
}

impl Connection {
    /// The top of the server, as a location.
    pub fn top(&self) -> String {
        Location::Remote {
            scheme: self.scheme,
            connection: self.id.clone(),
            path: "/".to_owned(),
        }
        .to_string()
    }

    pub(super) fn shown_name(&self) -> String {
        if self.name.trim().is_empty() {
            if self.host.is_empty() {
                self.bucket.clone()
            } else {
                self.host.clone()
            }
        } else {
            self.name.trim().to_owned()
        }
    }
}

/// Connect to `connection`. Without `secret`, the one kept for it in the
/// keychain is used; with `remember`, the one given is kept there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Connect {
    pub connection: Connection,
    pub secret: Option<Secret>,
    pub remember: bool,
}

/// How a [`Connect`] went. Opaque to facet: it never leaves the core.
#[derive(Clone, Debug, PartialEq, Eq, Facet)]
#[facet(opaque)]
#[repr(C)]
pub enum ConnectOutput {
    Connected,
    /// The server wants a password, or another than the one tried.
    NeedsSecret,
    /// The server's key is not the one accepted for it, or none was yet.
    UnknownHostKey {
        fingerprint: String,
    },
    Failed(String),
}

impl Operation for Connect {
    type Output = ConnectOutput;
}

/// Close the connection `id`, if it is open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disconnect {
    pub id: String,
}

impl Operation for Disconnect {
    type Output = ();
}

/// Take the connection's password out of the keychain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForgetSecret {
    pub id: String,
}

impl Operation for ForgetSecret {
    type Output = ();
}

/// What the connect dialog's fields hold.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
pub struct ConnectFields {
    pub name: String,
    pub scheme: Scheme,
    pub host: String,
    /// As typed; empty for the usual port.
    pub port: String,
    pub user: String,
    pub root: String,
    pub secure: bool,
    pub bucket: String,
    pub region: String,
    /// SFTP: a private key file to log in with; empty for a password.
    #[serde(default)]
    pub key_file: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SchemeChoice {
    pub scheme: Scheme,
    pub label: String,
}

/// The connect dialog, while it is open.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ConnectView {
    /// "Connect to Server", or the name of the one being edited.
    pub title: String,
    /// A saved connection is being edited (it can be removed).
    pub editing: bool,
    /// That connection's id; empty for a new one.
    pub id: String,
    pub fields: ConnectFields,
    pub schemes: Vec<SchemeChoice>,
    /// Why it didn't connect, or what is missing; empty when nothing is.
    pub error: String,
    /// Connecting: the fields wait.
    pub busy: bool,
}

/// A server showed a key nobody accepted yet: is it the right server?
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct HostKeyView {
    pub host: String,
    pub fingerprint: String,
}

struct Form {
    /// The saved connection being edited.
    editing: Option<String>,
    fields: ConnectFields,
    error: String,
    busy: bool,
    /// The tab that goes to the server once connected.
    tab: Option<TabId>,
    /// What was typed last, for the try after the host key is accepted.
    secret: Option<Secret>,
    remember: bool,
}

/// Saved connections and what is going on with them.
#[derive(Default)]
pub(super) struct Remote {
    pub(super) connections: Vec<Connection>,
    /// Ids with an open session, as far as the core was told.
    connected: BTreeSet<String>,
    /// Ids being connected to without a dialog, for tabs that need them.
    connecting: BTreeSet<String>,
    form: Option<Form>,
    /// The key a server showed, and the connection it is asked about.
    host_key: Option<(String, String)>,
    next_id: u32,
}

impl Scheme {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sftp => "SFTP",
            Self::Ftp => "FTP",
            Self::Dav => "WebDAV",
            Self::S3 => "S3",
        }
    }
}

fn fields_of(connection: &Connection) -> ConnectFields {
    ConnectFields {
        name: connection.name.clone(),
        scheme: connection.scheme,
        host: connection.host.clone(),
        port: if connection.port == 0 {
            String::new()
        } else {
            connection.port.to_string()
        },
        user: connection.user.clone(),
        root: connection.root.clone(),
        secure: connection.secure,
        bucket: connection.bucket.clone(),
        region: connection.region.clone(),
        key_file: connection.key_file.clone(),
    }
}

/// What is wrong with the fields; empty when they will do.
fn problem(fields: &ConnectFields) -> &'static str {
    let port = fields.port.trim();
    if fields.scheme == Scheme::S3 {
        if fields.bucket.trim().is_empty() {
            return "S3 needs the bucket's name.";
        }
    } else if fields.host.trim().is_empty() {
        return "The server's name or address is needed.";
    }
    if !port.is_empty() && port.parse::<u16>().is_err() {
        return "The port is a number up to 65535.";
    }
    ""
}

impl Remote {
    pub(super) fn connection(&self, id: &str) -> Option<&Connection> {
        self.connections.iter().find(|known| known.id == id)
    }

    /// What the top of a server is called: the connection's name, where
    /// the location only has its id. `None` for any other place.
    pub(super) fn top_title(&self, path: &str) -> Option<String> {
        match Location::parse(path) {
            Location::Remote {
                connection, path, ..
            } if path == "/" => self.connection(&connection).map(Connection::shown_name),
            _ => None,
        }
    }

    pub(super) fn is_connected(&self, id: &str) -> bool {
        self.connected.contains(id)
    }

    pub(super) fn connect_view(&self) -> Option<ConnectView> {
        let form = self.form.as_ref()?;
        let editing = form.editing.as_deref().and_then(|id| self.connection(id));
        Some(ConnectView {
            title: editing.map_or_else(|| "Connect to Server".to_owned(), Connection::shown_name),
            editing: editing.is_some(),
            id: editing.map_or_else(String::new, |connection| connection.id.clone()),
            fields: form.fields.clone(),
            schemes: Scheme::ALL
                .into_iter()
                .map(|scheme| SchemeChoice {
                    scheme,
                    label: scheme.label().to_owned(),
                })
                .collect(),
            error: form.error.clone(),
            busy: form.busy,
        })
    }

    pub(super) fn host_key_view(&self) -> Option<HostKeyView> {
        let (id, fingerprint) = self.host_key.as_ref()?;
        Some(HostKeyView {
            host: self
                .connection(id)
                .map_or_else(String::new, |connection| connection.host.clone()),
            fingerprint: fingerprint.clone(),
        })
    }

    /// An id no saved connection has.
    fn fresh_id(&mut self) -> String {
        loop {
            self.next_id += 1;
            let id = format!("c{}", self.next_id);
            if self.connection(&id).is_none() {
                return id;
            }
        }
    }
}

impl DirFleet {
    fn connect(
        connection: Connection,
        secret: Option<Secret>,
        remember: bool,
    ) -> Command<Effect, Event> {
        let id = connection.id.clone();
        Command::request_from_shell(Connect {
            connection,
            secret,
            remember,
        })
        .then_send(move |output| Event::ConnectAnswered { id, output })
    }

    /// A tab's server is not connected: the tab says so, and the server is
    /// connected to with the password the keychain has for it.
    pub(super) fn listing_needs_connection(
        model: &mut Model,
        tab: u32,
        connection: &str,
    ) -> Command<Effect, Event> {
        let known = model.remote.connection(connection).cloned();
        model.remote.connected.remove(connection);
        if let Some(state) = tab_id(tab).and_then(|id| model.tabs.get_mut(&id)) {
            state.denied = false;
            state.error = Some(if known.is_some() {
                "Connecting…".to_owned()
            } else {
                "This server is not among the saved ones any more".to_owned()
            });
        }
        let Some(known) = known else {
            return render();
        };
        // One try at a time: its answer reads every tab on the server again.
        if !model.remote.connecting.insert(known.id.clone()) {
            return render();
        }
        Self::connect(known, None, false).and(render())
    }

    /// Reads again every tab that shows a folder of the connection `id`,
    /// but for `except`, which is going there anyway.
    fn refresh_tabs_on(
        model: &mut Model,
        id: &str,
        except: Option<TabId>,
    ) -> Command<Effect, Event> {
        // Collected first: refreshing needs the model.
        #[allow(clippy::needless_collect)]
        let tabs: Vec<u32> = model
            .tabs
            .iter()
            .filter(|(tab, _)| Some(**tab) != except)
            .filter(|(_, tab)| {
                matches!(
                    tab.location(),
                    Some(Location::Remote { connection, .. }) if connection == id
                )
            })
            .map(|(tab, _)| tab.number())
            .collect();
        let refreshes: Vec<_> = tabs
            .into_iter()
            .map(|tab| Self::refresh(model, tab))
            .collect();
        Command::all(refreshes)
    }

    /// Says `reason` on every tab that waits for the connection `id`.
    fn fail_tabs_on(model: &mut Model, id: &str, reason: &str) {
        for tab in model.tabs.values_mut() {
            if matches!(
                tab.location(),
                Some(Location::Remote { connection, .. }) if connection == id
            ) && tab.listing.is_empty()
            {
                tab.error = Some(reason.to_owned());
                tab.loading = false;
            }
        }
    }

    fn connect_answered(
        model: &mut Model,
        id: &str,
        output: ConnectOutput,
    ) -> Command<Effect, Event> {
        model.remote.connecting.remove(id);
        let for_form = model
            .remote
            .form
            .as_ref()
            .is_some_and(|form| form.busy && form.editing.as_deref() == Some(id));
        match output {
            ConnectOutput::Connected => {
                model.remote.connected.insert(id.to_owned());
                let go = model
                    .remote
                    .form
                    .take_if(|_| for_form)
                    .and_then(|form| form.tab);
                let top = model.remote.connection(id).map(Connection::top);
                let shown = match (go, top) {
                    (Some(tab), Some(top)) => Self::navigate(model, tab.number(), top),
                    _ => Command::done(),
                };
                shown
                    .and(Self::refresh_tabs_on(model, id, go))
                    .and(render())
            }
            ConnectOutput::NeedsSecret => {
                // Asked for in the dialog, opened for it if it was not.
                let known = model.remote.connection(id).cloned();
                let form = model.remote.form.get_or_insert_with(|| Form {
                    editing: Some(id.to_owned()),
                    fields: known.as_ref().map(fields_of).unwrap_or_default(),
                    error: String::new(),
                    busy: false,
                    tab: None,
                    secret: None,
                    remember: false,
                });
                if form.editing.as_deref() == Some(id) {
                    form.busy = false;
                    "The server wants the password.".clone_into(&mut form.error);
                }
                Self::fail_tabs_on(model, id, "Not connected: the server wants the password");
                render()
            }
            ConnectOutput::UnknownHostKey { fingerprint } => {
                model.remote.host_key = Some((id.to_owned(), fingerprint));
                render()
            }
            ConnectOutput::Failed(reason) => {
                if for_form && let Some(form) = &mut model.remote.form {
                    form.busy = false;
                    form.error.clone_from(&reason);
                }
                Self::fail_tabs_on(model, id, &reason);
                render()
            }
        }
    }

    /// The events of connecting to servers.
    pub(super) fn remote_event(model: &mut Model, event: Event) -> Command<Effect, Event> {
        match event {
            Event::OpenConnect { tab, edit } => {
                let editing = edit.and_then(|id| model.remote.connection(&id).cloned());
                model.remote.form = Some(Form {
                    fields: editing.as_ref().map(fields_of).unwrap_or_default(),
                    editing: editing.map(|connection| connection.id),
                    error: String::new(),
                    busy: false,
                    tab: tab_id(tab).filter(|id| model.tabs.contains_key(id)),
                    secret: None,
                    remember: false,
                });
                render()
            }
            Event::SetConnect { fields } => {
                if let Some(form) = model.remote.form.as_mut().filter(|form| !form.busy) {
                    form.fields = fields;
                    form.error.clear();
                }
                render()
            }
            Event::CancelConnect => {
                model.remote.form = None;
                model.remote.host_key = None;
                render()
            }
            Event::SubmitConnect { secret, remember } => {
                Self::submit_connect(model, secret, remember)
            }
            Event::AcceptHostKey { accept } => {
                let Some((id, fingerprint)) = model.remote.host_key.take() else {
                    return Command::done();
                };
                if !accept {
                    model.remote.connecting.remove(&id);
                    if let Some(form) = &mut model.remote.form {
                        form.busy = false;
                        "The server's key was not accepted.".clone_into(&mut form.error);
                    }
                    Self::fail_tabs_on(
                        model,
                        &id,
                        "Not connected: the server's key was not accepted",
                    );
                    return render();
                }
                let Some(known) = model
                    .remote
                    .connections
                    .iter_mut()
                    .find(|known| known.id == id)
                else {
                    return render();
                };
                known.host_key = Some(fingerprint);
                let known = known.clone();
                // With what was typed, when the dialog asked; else the keychain's.
                let (secret, remember) = model
                    .remote
                    .form
                    .as_ref()
                    .filter(|form| form.editing.as_deref() == Some(id.as_str()))
                    .map_or((None, false), |form| (form.secret.clone(), form.remember));
                Self::connect(known, secret, remember).and(render())
            }
            Event::RemoveConnection { id } => {
                model.remote.connections.retain(|known| known.id != id);
                model.remote.connected.remove(&id);
                if model
                    .remote
                    .form
                    .as_ref()
                    .is_some_and(|form| form.editing.as_deref() == Some(id.as_str()))
                {
                    model.remote.form = None;
                }
                Self::fail_tabs_on(
                    model,
                    &id,
                    "This server is not among the saved ones any more",
                );
                let closed: Command<Effect, Event> =
                    Command::notify_shell(Disconnect { id: id.clone() }).into();
                let forgotten: Command<Effect, Event> =
                    Command::notify_shell(ForgetSecret { id }).into();
                closed.and(forgotten).and(render())
            }
            Event::DisconnectServer { id } => {
                model.remote.connected.remove(&id);
                let closed: Command<Effect, Event> =
                    Command::notify_shell(Disconnect { id }).into();
                closed.and(render())
            }
            Event::ConnectAnswered { id, output } => Self::connect_answered(model, &id, output),
            _ => Command::done(),
        }
    }

    /// The dialog's fields become a saved connection, and it is tried.
    fn submit_connect(model: &mut Model, secret: Secret, remember: bool) -> Command<Effect, Event> {
        let Some(form) = model.remote.form.as_ref().filter(|form| !form.busy) else {
            return Command::done();
        };
        let wrong = problem(&form.fields);
        if !wrong.is_empty() {
            if let Some(form) = &mut model.remote.form {
                wrong.clone_into(&mut form.error);
            }
            return render();
        }
        let fields = form.fields.clone();
        let id = match form.editing.clone() {
            Some(id) => id,
            None => model.remote.fresh_id(),
        };
        let earlier = model.remote.connection(&id).cloned();
        let connection = Connection {
            id: id.clone(),
            name: fields.name.trim().to_owned(),
            scheme: fields.scheme,
            host: fields.host.trim().to_owned(),
            port: fields.port.trim().parse().unwrap_or(0),
            user: fields.user.trim().to_owned(),
            root: fields.root.trim().to_owned(),
            secure: fields.secure,
            bucket: fields.bucket.trim().to_owned(),
            region: fields.region.trim().to_owned(),
            key_file: if fields.scheme == Scheme::Sftp {
                fields.key_file.trim().to_owned()
            } else {
                String::new()
            },
            // The key accepted for the same server holds; another server's doesn't.
            host_key: earlier
                .as_ref()
                .filter(|earlier| {
                    earlier.host == fields.host.trim() && earlier.scheme == fields.scheme
                })
                .and_then(|earlier| earlier.host_key.clone()),
        };
        match model
            .remote
            .connections
            .iter_mut()
            .find(|known| known.id == id)
        {
            Some(known) => *known = connection.clone(),
            None => model.remote.connections.push(connection.clone()),
        }
        // Nothing typed: the keychain's is tried.
        let secret = (!secret.reveal().is_empty()).then_some(secret);
        if let Some(form) = &mut model.remote.form {
            form.editing = Some(id.clone());
            form.busy = true;
            form.error.clear();
            form.secret.clone_from(&secret);
            form.remember = remember;
        }
        // A changed connection starts over.
        let closed: Command<Effect, Event> = Command::notify_shell(Disconnect { id }).into();
        closed
            .then(Self::connect(connection, secret, remember))
            .and(render())
    }
}
