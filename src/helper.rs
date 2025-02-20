use std::io;
use std::process::{Child, Command, Stdio};

pub trait PathFixer {
    fn to_fixed_string(&self) -> String;
}

impl PathFixer for std::path::PathBuf {
    fn to_fixed_string(&self) -> String {
        self.display().to_string().replace("\\\\?\\", "")
    }
}

pub trait DetachedSpawn {
    fn spawn_detached(&mut self) -> io::Result<Child>;
}

impl DetachedSpawn for Command {
    fn spawn_detached(&mut self) -> io::Result<Child> {
        let mut command = self;
        #[cfg(windows)]
        {
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            use std::os::windows::process::CommandExt;
            command = command.creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW);
        }

        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }
}

// pub fn open_in_terminal(&)
