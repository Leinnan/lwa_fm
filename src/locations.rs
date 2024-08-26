use std::path::PathBuf;

#[derive(serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(default)]
pub struct Locations {
    pub locations: Vec<Location>,
    pub editable: bool,
}

#[derive(serde::Deserialize, serde::Serialize, Default, Debug)]
#[serde(default)]
pub struct Location {
    pub name: String,
    pub path: PathBuf,
}

impl Locations {
    #[cfg(not(target_os = "macos"))]
    pub fn get_drives() -> Self {
        let mut drives = sysinfo::Disks::new_with_refreshed_list();
        drives.sort_by(|a, b| a.mount_point().cmp(b.mount_point()));
        let locations = drives
            .iter()
            .map(|drive| Location {
                name: format!(
                    "{} ({})",
                    drive.name().to_str().unwrap_or(""),
                    drive.mount_point().display()
                ),
                path: drive.mount_point().to_path_buf(),
            })
            .collect();

        Self {
            locations,
            editable: false,
        }
    }

    pub fn get_user_dirs() -> Self {
        let locations: Vec<Location> =
            directories::UserDirs::new().map_or_else(Vec::new, |user_dirs| {
                let mut list = vec![Location {
                    name: "User".into(),
                    path: user_dirs.home_dir().to_path_buf(),
                }];
                if let Some(docs) = user_dirs.document_dir() {
                    list.push(Location {
                        name: "Documents".into(),
                        path: docs.to_path_buf(),
                    });
                }
                if let Some(dir) = user_dirs.desktop_dir() {
                    list.push(Location {
                        name: "Desktop".into(),
                        path: dir.to_path_buf(),
                    });
                }
                if let Some(dir) = user_dirs.download_dir() {
                    list.push(Location {
                        name: "Downloads".into(),
                        path: dir.to_path_buf(),
                    });
                }
                if let Some(dir) = user_dirs.picture_dir() {
                    list.push(Location {
                        name: "Pictures".into(),
                        path: dir.to_path_buf(),
                    });
                }
                if let Some(dir) = user_dirs.audio_dir() {
                    list.push(Location {
                        name: "Music".into(),
                        path: dir.to_path_buf(),
                    });
                }
                list
            });

        Self {
            locations,
            editable: false,
        }
    }
}
