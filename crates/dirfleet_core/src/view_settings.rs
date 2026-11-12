use facet::Facet;
use serde::{Deserialize, Serialize};
use std::ops::{Deref, DerefMut};

#[derive(Facet, Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
#[repr(C)]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Created,
    Accessed,
    MostOpened,
    Size,
    Kind,
    Random,
}

impl Sort {
    pub const ALL: [Self; 8] = [
        Self::Name,
        Self::Modified,
        Self::Created,
        Self::Accessed,
        Self::MostOpened,
        Self::Size,
        Self::Kind,
        Self::Random,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Date modified",
            Self::Created => "Date created",
            Self::Accessed => "Date opened",
            Self::MostOpened => "Times opened",
            Self::Size => "Size",
            Self::Kind => "Kind",
            Self::Random => "Random",
        }
    }
}

/// What the entries of a folder are put into groups by, each group under a
/// heading of its own.
#[derive(Facet, Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
#[repr(C)]
pub enum GroupBy {
    #[default]
    None,
    Kind,
    /// How long ago the entry last changed.
    Modified,
    Size,
    /// The first letter of the name.
    Name,
}

impl GroupBy {
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Kind,
        Self::Modified,
        Self::Size,
        Self::Name,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Kind => "Kind",
            Self::Modified => "Date modified",
            Self::Size => "Size",
            Self::Name => "Name",
        }
    }
}

#[derive(Facet, Deserialize, Serialize, Default, PartialEq, Eq, Debug, Clone, Copy)]
#[repr(C)]
pub enum DisplayType {
    #[default]
    List,
    Icons,
}

/// Where a setting came from: the folder's own, the app-wide default, or neither.
#[derive(Deserialize, Serialize, Default, Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum DataSource {
    Settings,
    Local,
    #[default]
    Generated,
}

#[derive(Deserialize, Serialize, Default, Debug, Clone)]
pub struct Data<T> {
    pub data: T,
    pub source: DataSource,
}

impl<T> Data<T> {
    /// Returns true if the data is from the local source.
    pub const fn is_local(&self) -> bool {
        matches!(self.source, DataSource::Local)
    }
    /// Returns true if the data is from the global source.
    pub const fn is_global(&self) -> bool {
        matches!(self.source, DataSource::Settings)
    }
    /// Creates a new data instance with the local source.
    pub const fn from_local(data: T) -> Self {
        Self {
            data,
            source: DataSource::Local,
        }
    }
    /// Creates a new data instance with the global source.
    pub const fn from_settings(data: T) -> Self {
        Self {
            data,
            source: DataSource::Settings,
        }
    }
    /// Creates a new data instance with the generated source.
    pub const fn generated(data: T) -> Self {
        Self {
            data,
            source: DataSource::Generated,
        }
    }
}

impl<T> Deref for Data<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.data
    }
}
impl<T> DerefMut for Data<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.data
    }
}
