use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io;
use std::io::{BufRead, stderr};
use std::iter::{Extend, FromIterator};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, atomic::AtomicBool};

use xdg::BaseDirectories;

use crate::errors::{Result, LeftError, LeftErrorKind};

pub struct Nanny {}

impl Default for Nanny {
    fn default() -> Self {
        Self::new()
    }
}

impl Nanny {
    pub fn new() -> Nanny {
        Nanny {}
    }

    pub fn autostart(&self) -> Children {
        dirs::home_dir()
            .map(|mut path| {
                path.push(".config");
                path.push("autostart");
                path
            })
            .and_then(|path| list_desktop_files(&path).ok())
            .map(|files| {
                files
                    .iter()
                    .filter_map(|file| boot_desktop_file(&file).ok())
                    .collect::<Children>()
            })
            .unwrap_or_default()
    }

    pub fn boot_current_theme(&self) -> Result<Option<Child>> {
        let mut path = BaseDirectories::with_prefix("leftwm")?.create_config_directory("")?;
        path.push("themes");
        path.push("current");
        path.push("up");
        if path.is_file() {
            Command::new(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .map(Some)
                .map_err(|e| e.into())
        } else {
            Ok(None)
        }
    }
}

fn boot_desktop_file(path: &PathBuf) -> io::Result<Child> {
    let entries = parse_desktop_file(path)?;
    // let entries = match parse_desktop_file(path) {
    //     Ok(entries) => entries,
    //     Err(err) => return Err(err)
    // };

    if let Some(hidden) = entries.get("Hidden") {
        if hidden == "true" {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "hidden desktop file")); // hack
        }
    }
    // TODO: if TERMINAL is set to true then find users default terminal-emulator and execute within
    let args = match entries.get("Exec") {
        Some(exec) => sanitize_exec(exec),
        None => return Err(io::Error::new(io::ErrorKind::InvalidInput, "could not find Exec key")), // hack
    };
    // // from: https://askubuntu.com/questions/5172/running-a-desktop-file-in-the-terminal
    //let args = format!("`grep '^Exec' {:?} | tail -1 | sed 's/^Exec=//' | sed 's/%.//' | sed 's/^\"//g' | sed 's/\" *$//g'`", path);
    Command::new("sh").arg("-c").arg(args).spawn()
}

fn sanitize_exec(exec: &String) -> String {
    // TODO: sanitize command -> e.g. remove %U, un-escape stuff,
    //  https://developer.gnome.org/desktop-entry-spec/#exec-variables
    // TODO
    format!("`echo \"{}\" | sed 's/%.//' | sed 's/^\\\"//g' | sed 's/\\\" *$//g'`", exec)
}

// reads desktop file from path and return its entries as key-value pairs in a HashMap.
// if a key exists multiple times the last value is finally used.
fn parse_desktop_file(path: &PathBuf) -> io::Result<HashMap<String, String>> {
    let mut entries = HashMap::new();
    let lines = read_lines(path)?;
    for line in lines {
        if let Ok(line) = line {
            // remove trailing newlines and filter comments and empty lines
            let line = line.trim();
            if line.starts_with("#") || line == "" {
                continue;
            }

            // split line into key-value pairs with the first "=" as a separator
            let mut splitter = line.splitn(2, '=');
            let key = splitter.next().unwrap_or_default();
            let value = splitter.next().unwrap_or_default();

            if key != "" {
                entries.insert(String::from(key), String::from(value));
            }
        }
    }
    Ok(entries)
}

// reads a file and returns iterator over its lines
fn read_lines<P>(filename: P) -> io::Result<io::Lines<io::BufReader<File>>> where P: AsRef<Path> {
    let file = File::open(filename)?;
    Ok(io::BufReader::new(file).lines())
}

// get all the .desktop files in a folder
fn list_desktop_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let mut list = vec![];
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension() {
                    if ext == "desktop" {
                        list.push(path);
                    }
                }
            }
        }
    }
    Ok(list)
}

/// A struct managing children processes.
///
/// The `reap` method could be called at any place the user wants to.
/// `register_child_hook` provides a hook that sets a flag. User may use the
/// flag to do a epoch-based reaping.
#[derive(Debug, Default)]
pub struct Children {
    inner: HashMap<u32, Child>,
}

impl Children {
    pub fn new() -> Children {
        Default::default()
    }
    pub fn len(&self) -> usize {
        self.inner.len()
    }
    pub fn is_empty(&self) -> bool {
        self.inner.len() == 0
    }
    /// Insert a `Child` in the `Children`.
    /// If this `Children` did not have this value present, true is returned.
    /// If this `Children` did have this value present, false is returned.
    pub fn insert(&mut self, child: Child) -> bool {
        // Not possible to have duplication!
        self.inner.insert(child.id(), child).is_none()
    }
    /// Merge another `Children` into this `Children`.
    pub fn merge(&mut self, reaper: Children) {
        self.inner.extend(reaper.inner.into_iter())
    }
    /// Try reaping all the children processes managed by this struct.
    pub fn reap(&mut self) {
        // The `try_wait` needs `child` to be `mut`, but only `HashMap::retain`
        // allows modifying the value. Here `id` is not needed.
        self.inner
            .retain(|_, child| child.try_wait().map_or(true, |ret| ret.is_none()))
    }
}

impl FromIterator<Child> for Children {
    fn from_iter<T: IntoIterator<Item=Child>>(iter: T) -> Self {
        Self {
            inner: iter
                .into_iter()
                .map(|child| (child.id(), child))
                .collect::<HashMap<_, _>>(),
        }
    }
}

impl Extend<Child> for Children {
    fn extend<T: IntoIterator<Item=Child>>(&mut self, iter: T) {
        self.inner
            .extend(iter.into_iter().map(|child| (child.id(), child)))
    }
}

/// Register the `SIGCHLD` signal handler. Once the signal is received,
/// the flag will be set true. User needs to manually clear the flag.
pub fn register_child_hook(flag: Arc<AtomicBool>) {
    let _ = signal_hook::flag::register(signal_hook::SIGCHLD, flag)
        .map_err(|err| log::error!("Cannot register SIGCHLD signal handler: {:?}", err));
}
