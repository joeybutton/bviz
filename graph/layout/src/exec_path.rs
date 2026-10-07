//! The paths below the execution root of the files an aquery's actions read
//! and write.

use std::fmt;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::intern::Id;

/// A path below the execution root, as its directory and its file name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ExecPath<'a> {
    /// Empty for a file at the execution root.
    pub dir: &'a str,
    pub name: &'a str,
}

impl ExecPath<'_> {
    /// Replaces the contents of `buf` with the path's bytes.
    pub fn write_into(self, buf: &mut Vec<u8>) {
        buf.clear();
        if !self.dir.is_empty() {
            buf.extend_from_slice(self.dir.as_bytes());
            buf.push(b'/');
        }
        buf.extend_from_slice(self.name.as_bytes());
    }
}

impl fmt::Display for ExecPath<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.dir {
            "" => f.write_str(self.name),
            dir => write!(f, "{dir}/{}", self.name),
        }
    }
}

/// Paths, each held as a handle to its directory, which the many files of
/// one directory share, and its file name.
#[derive(Clone, Default)]
struct PathStore {
    dirs: Vec<Box<str>>,
    /// By path, its directory in `dirs` and where its name lies in `names`.
    paths: Vec<(u32, u32, u32)>,
    names: String,
}

impl PathStore {
    fn len(&self) -> usize {
        self.paths.len()
    }

    fn get(&self, path: u32) -> ExecPath<'_> {
        let (dir, start, end) = self.paths[path as usize];
        ExecPath {
            dir: &self.dirs[dir as usize],
            name: &self.names[start as usize..end as usize],
        }
    }
}

/// Fills a [`PathStore`], finding each directory it already holds.
#[derive(Default)]
struct PathStoreBuilder {
    store: PathStore,
    dir_index: FxHashMap<Box<str>, u32>,
}

impl PathStoreBuilder {
    fn reserve(&mut self, paths: usize) {
        self.store.paths.reserve(paths);
    }

    /// Adds `path`, returning its handle.
    fn push(&mut self, path: &str) -> u32 {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        let dir = match self.dir_index.get(dir) {
            Some(&dir) => dir,
            None => {
                let at = handle(self.store.dirs.len());
                self.store.dirs.push(dir.into());
                self.dir_index.insert(dir.into(), at);
                at
            }
        };
        let start = handle(self.store.names.len());
        self.store.names.push_str(name);
        let end = handle(self.store.names.len());
        let path = handle(self.store.paths.len());
        self.store.paths.push((dir, start, end));
        path
    }
}

fn handle(index: usize) -> u32 {
    u32::try_from(index).expect("over u32::MAX exec path entries")
}

/// The exec path of each file node an aquery describes.
#[derive(Clone, Default)]
pub struct ExecPaths {
    /// The files carrying a path, in ascending id order.
    ids: Vec<Id>,
    /// By position in `ids`, the file's path in `store`.
    paths: Vec<u32>,
    store: PathStore,
}

impl ExecPaths {
    pub fn get(&self, id: Id) -> Option<ExecPath<'_>> {
        let row = self.ids.binary_search(&id).ok()?;
        Some(self.store.get(self.paths[row]))
    }

    /// Moves every path to the node id `remap` gives its node.
    pub(crate) fn renumber(&mut self, remap: &[Id]) {
        let mut rows: Vec<(Id, u32)> = (self.ids.iter())
            .map(|&id| remap[id.0 as usize])
            .zip(self.paths.iter().copied())
            .collect();
        rows.sort_unstable_by_key(|&(id, _)| id);
        (self.ids, self.paths) = rows.into_iter().unzip();
    }
}

/// Collects [`ExecPaths`], the first path stated for a node being the one it
/// keeps.
#[derive(Default)]
pub(crate) struct ExecPathsBuilder {
    rows: Vec<(Id, u32)>,
    filled: FxHashSet<Id>,
    store: PathStoreBuilder,
}

impl ExecPathsBuilder {
    pub(crate) fn reserve(&mut self, files: usize) {
        self.rows.reserve(files);
        self.filled.reserve(files);
        self.store.reserve(files);
    }

    pub(crate) fn push(&mut self, id: Id, path: &str) {
        if self.filled.insert(id) {
            let path = self.store.push(path);
            self.rows.push((id, path));
        }
    }

    pub(crate) fn seal(mut self) -> ExecPaths {
        self.rows.sort_unstable_by_key(|&(id, _)| id);
        let (ids, paths) = self.rows.into_iter().unzip();
        ExecPaths {
            ids,
            paths,
            store: self.store.store,
        }
    }
}

/// The files each action reads that the graph holds no node for: sources of
/// other repositories, left out unless asked for, and generated files whose
/// producing action lies outside the query. They are held in the nested sets
/// the aquery groups an action's inputs in, each set once however many
/// actions read it, so they take space in proportion to the dump. Only the
/// sets holding such a file, directly or through those they nest, are kept.
#[derive(Clone, Default)]
pub struct LeftOutInputs {
    store: PathStore,
    /// By set, where its entries start in `direct` and `nested`; one entry
    /// past the last set closes both.
    starts: Vec<(u32, u32)>,
    /// The paths each set holds directly.
    direct: Vec<u32>,
    /// The sets each set nests.
    nested: Vec<u32>,
    /// The actions reading such a file, in ascending id order.
    actions: Vec<Id>,
    /// By position in `actions`, where its sets start in `roots`; one entry
    /// past the last action closes them.
    root_starts: Vec<u32>,
    roots: Vec<u32>,
}

impl LeftOutInputs {
    pub fn paths(&self) -> usize {
        self.store.len()
    }

    pub fn path(&self, path: u32) -> ExecPath<'_> {
        self.store.get(path)
    }

    pub fn sets(&self) -> usize {
        self.starts.len().saturating_sub(1)
    }

    /// The paths `set` holds directly.
    pub fn direct(&self, set: u32) -> &[u32] {
        let (from, to) = (self.starts[set as usize], self.starts[set as usize + 1]);
        &self.direct[from.0 as usize..to.0 as usize]
    }

    /// The sets `set` nests.
    pub fn nested(&self, set: u32) -> &[u32] {
        let (from, to) = (self.starts[set as usize], self.starts[set as usize + 1]);
        &self.nested[from.1 as usize..to.1 as usize]
    }

    /// The sets `action` reads.
    pub fn read_by(&self, action: Id) -> &[u32] {
        match self.actions.binary_search(&action) {
            Ok(row) => {
                let (from, to) = (self.root_starts[row], self.root_starts[row + 1]);
                &self.roots[from as usize..to as usize]
            }
            Err(_) => &[],
        }
    }

    /// Moves every action to the node id `remap` gives its node.
    pub(crate) fn renumber(&mut self, remap: &[Id]) {
        self.reorder(|id| remap[id.0 as usize]);
    }

    /// Moves every action to the id `id_of` gives it, and sorts the actions
    /// by it.
    fn reorder(&mut self, id_of: impl Fn(Id) -> Id) {
        let mut rows: Vec<(Id, usize)> = (self.actions.iter().enumerate())
            .map(|(row, &id)| (id_of(id), row))
            .collect();
        rows.sort_unstable_by_key(|&(id, _)| id);
        let mut roots = Vec::with_capacity(self.roots.len());
        let mut root_starts = Vec::with_capacity(self.root_starts.len());
        for &(_, row) in &rows {
            root_starts.push(handle(roots.len()));
            let (from, to) = (self.root_starts[row], self.root_starts[row + 1]);
            roots.extend_from_slice(&self.roots[from as usize..to as usize]);
        }
        root_starts.push(handle(roots.len()));
        self.actions = rows.into_iter().map(|(id, _)| id).collect();
        self.roots = roots;
        self.root_starts = root_starts;
    }
}

/// Builds a [`LeftOutInputs`] from an aquery's nested sets.
pub(crate) struct LeftOutInputsBuilder {
    table: LeftOutInputs,
    paths: PathStoreBuilder,
}

impl LeftOutInputsBuilder {
    /// A builder for `sets` sets, holding `direct` paths and `nested` nested
    /// sets between them, and for `paths` distinct paths.
    pub(crate) fn with_capacity(sets: usize, direct: usize, nested: usize, paths: usize) -> Self {
        let mut table = LeftOutInputs::default();
        table.starts.reserve(sets + 1);
        table.direct.reserve(direct);
        table.nested.reserve(nested);
        let mut store = PathStoreBuilder::default();
        store.reserve(paths);
        LeftOutInputsBuilder {
            table,
            paths: store,
        }
    }

    /// Adds `path`, returning its handle.
    pub(crate) fn push_path(&mut self, path: &str) -> u32 {
        self.paths.push(path)
    }

    /// Adds the next set: the paths it holds directly and the sets it nests,
    /// each by the handle its push returned. Returns the set's handle.
    pub(crate) fn push_set(
        &mut self,
        paths: impl IntoIterator<Item = u32>,
        nested: impl IntoIterator<Item = u32>,
    ) -> u32 {
        let set = handle(self.table.starts.len());
        let starts = (
            handle(self.table.direct.len()),
            handle(self.table.nested.len()),
        );
        self.table.starts.push(starts);
        self.table.direct.extend(paths);
        self.table.nested.extend(nested);
        set
    }

    /// Records that `action` reads `sets`.
    pub(crate) fn push_action(&mut self, action: Id, sets: impl IntoIterator<Item = u32>) {
        let table = &mut self.table;
        if table.root_starts.is_empty() {
            table.root_starts.push(0);
        }
        table.actions.push(action);
        table.roots.extend(sets);
        table.root_starts.push(handle(table.roots.len()));
    }

    pub(crate) fn seal(mut self) -> LeftOutInputs {
        let starts = (
            handle(self.table.direct.len()),
            handle(self.table.nested.len()),
        );
        self.table.starts.push(starts);
        if self.table.root_starts.is_empty() {
            self.table.root_starts.push(0);
        }
        self.table.store = self.paths.store;
        self.table.reorder(|id| id);
        self.table
    }
}
