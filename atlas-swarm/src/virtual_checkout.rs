//! Linux FUSE surface for daemon-owned Atlas checkouts.
//!
//! The `.jj` subtree is synthesized from checkout identity. It has no backing
//! directory and every inode below it is immutable. Repository working-tree
//! entries will share this filesystem through the daemon's overlay provider;
//! they must never be used as a pass-through escape hatch for `.jj`.

use std::{
    ffi::{OsStr, OsString},
    io,
    path::Path,
    time::{Duration, SystemTime},
};

use fuser::{
    Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, MountOption,
    OpenFlags, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, ReplyOpen, Request,
};

const TTL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug)]
struct VirtualNode {
    inode: INodeNo,
    parent: INodeNo,
    name: OsString,
    kind: FileType,
    contents: Vec<u8>,
}

/// A materialized description of the read-only virtual `.jj` metadata tree.
#[derive(Clone, Debug)]
pub struct VirtualJjFilesystem {
    nodes: Vec<VirtualNode>,
}

impl VirtualJjFilesystem {
    pub fn new(locator: Vec<u8>) -> Self {
        let mut nodes = vec![VirtualNode {
            inode: INodeNo::ROOT,
            parent: INodeNo::ROOT,
            name: OsString::new(),
            kind: FileType::Directory,
            contents: Vec::new(),
        }];
        let jj = Self::push(
            &mut nodes,
            INodeNo::ROOT,
            ".jj",
            FileType::Directory,
            Vec::new(),
        );
        Self::push(
            &mut nodes,
            jj,
            "atlas",
            FileType::RegularFile,
            locator.clone(),
        );
        let repo = Self::push(&mut nodes, jj, "repo", FileType::Directory, Vec::new());
        for name in ["store", "op_store", "op_heads", "index", "submodule_store"] {
            let directory = Self::push(&mut nodes, repo, name, FileType::Directory, Vec::new());
            Self::push(
                &mut nodes,
                directory,
                "type",
                FileType::RegularFile,
                b"atlas".to_vec(),
            );
            Self::push(
                &mut nodes,
                directory,
                "atlas.cbor",
                FileType::RegularFile,
                locator.clone(),
            );
        }
        let working_copy = Self::push(
            &mut nodes,
            jj,
            "working_copy",
            FileType::Directory,
            Vec::new(),
        );
        Self::push(
            &mut nodes,
            working_copy,
            "type",
            FileType::RegularFile,
            b"atlas".to_vec(),
        );
        Self::push(
            &mut nodes,
            working_copy,
            "atlas.cbor",
            FileType::RegularFile,
            locator,
        );
        Self { nodes }
    }

    fn push(
        nodes: &mut Vec<VirtualNode>,
        parent: INodeNo,
        name: &str,
        kind: FileType,
        contents: Vec<u8>,
    ) -> INodeNo {
        let inode = INodeNo(nodes.len() as u64 + 1);
        nodes.push(VirtualNode {
            inode,
            parent,
            name: name.into(),
            kind,
            contents,
        });
        inode
    }

    fn node(&self, inode: INodeNo) -> Option<&VirtualNode> {
        self.nodes.get(inode.0.checked_sub(1)? as usize)
    }

    fn attr(node: &VirtualNode) -> FileAttr {
        let time = SystemTime::UNIX_EPOCH;
        FileAttr {
            ino: node.inode,
            size: node.contents.len() as u64,
            blocks: node.contents.len().div_ceil(512) as u64,
            atime: time,
            mtime: time,
            ctime: time,
            crtime: time,
            kind: node.kind,
            perm: if node.kind == FileType::Directory {
                0o555
            } else {
                0o444
            },
            nlink: if node.kind == FileType::Directory {
                2
            } else {
                1
            },
            uid: 0,
            gid: 0,
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    pub fn read_virtual_file(&self, path: &Path) -> Option<&[u8]> {
        let mut inode = INodeNo::ROOT;
        for component in path.components() {
            let name = component.as_os_str();
            inode = self
                .nodes
                .iter()
                .find(|node| node.parent == inode && node.name == name)?
                .inode;
        }
        let node = self.node(inode)?;
        (node.kind == FileType::RegularFile).then_some(node.contents.as_slice())
    }
}

impl Filesystem for VirtualJjFilesystem {
    fn lookup(&self, _request: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        match self
            .nodes
            .iter()
            .find(|node| node.parent == parent && node.name == name)
        {
            Some(node) => reply.entry(&TTL, &Self::attr(node), Generation(0)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn getattr(
        &self,
        _request: &Request,
        inode: INodeNo,
        _file_handle: Option<FileHandle>,
        reply: ReplyAttr,
    ) {
        match self.node(inode) {
            Some(node) => reply.attr(&TTL, &Self::attr(node)),
            None => reply.error(Errno::ENOENT),
        }
    }

    fn open(&self, _request: &Request, inode: INodeNo, flags: OpenFlags, reply: ReplyOpen) {
        let Some(node) = self.node(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };
        if node.kind != FileType::RegularFile {
            reply.error(Errno::EISDIR);
        } else if flags.0 & 3 != 0 {
            reply.error(Errno::EROFS);
        } else {
            reply.opened(FileHandle(0), fuser::FopenFlags::empty());
        }
    }

    fn read(
        &self,
        _request: &Request,
        inode: INodeNo,
        _file_handle: FileHandle,
        offset: u64,
        size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: ReplyData,
    ) {
        let Some(node) = self.node(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };
        if node.kind != FileType::RegularFile {
            reply.error(Errno::EISDIR);
            return;
        }
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(node.contents.len());
        let end = start.saturating_add(size as usize).min(node.contents.len());
        reply.data(&node.contents[start..end]);
    }

    fn readdir(
        &self,
        _request: &Request,
        inode: INodeNo,
        _file_handle: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        let Some(directory) = self.node(inode) else {
            reply.error(Errno::ENOENT);
            return;
        };
        if directory.kind != FileType::Directory {
            reply.error(Errno::ENOTDIR);
            return;
        }
        let parent_kind = self
            .node(directory.parent)
            .map_or(FileType::Directory, |node| node.kind);
        let entries = [
            (inode, FileType::Directory, OsStr::new(".")),
            (directory.parent, parent_kind, OsStr::new("..")),
        ];
        let mut position = 0u64;
        for (child_inode, kind, name) in entries.into_iter().chain(
            self.nodes
                .iter()
                .filter(|node| node.parent == inode && node.inode != inode)
                .map(|node| (node.inode, node.kind, node.name.as_os_str())),
        ) {
            position += 1;
            if position <= offset {
                continue;
            }
            if reply.add(child_inode, position, kind, name) {
                break;
            }
        }
        reply.ok();
    }

    fn write(
        &self,
        _request: &Request,
        _inode: INodeNo,
        _file_handle: FileHandle,
        _offset: u64,
        _data: &[u8],
        _write_flags: fuser::WriteFlags,
        _flags: OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: fuser::ReplyWrite,
    ) {
        reply.error(Errno::EROFS);
    }
}

/// Mounts the synthetic checkout surface. The returned guard owns the mount;
/// dropping it unmounts the filesystem.
#[cfg(target_os = "linux")]
pub fn mount_virtual_checkout(
    mountpoint: &Path,
    filesystem: VirtualJjFilesystem,
) -> io::Result<fuser::BackgroundSession> {
    let mut config = Config::default();
    config.mount_options = vec![
        MountOption::FSName("atlas".into()),
        MountOption::Subtype("atlas-jj".into()),
        MountOption::RW,
        MountOption::NoDev,
        MountOption::NoSuid,
        MountOption::NoAtime,
        MountOption::DefaultPermissions,
    ];
    fuser::spawn_mount2(filesystem, mountpoint, &config)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jj_metadata_is_virtual_and_atlas_typed() {
        let locator = b"checkout locator".to_vec();
        let filesystem = VirtualJjFilesystem::new(locator.clone());
        assert_eq!(
            filesystem.read_virtual_file(Path::new(".jj/atlas")),
            Some(locator.as_slice())
        );
        for path in [
            ".jj/repo/store/type",
            ".jj/repo/op_store/type",
            ".jj/repo/op_heads/type",
            ".jj/repo/index/type",
            ".jj/repo/submodule_store/type",
            ".jj/working_copy/type",
        ] {
            assert_eq!(
                filesystem.read_virtual_file(Path::new(path)),
                Some(b"atlas".as_slice())
            );
        }
    }
}
