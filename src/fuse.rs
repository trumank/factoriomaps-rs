use libc::ENOENT;
use std::collections::HashMap;
use std::ffi::OsStr;
use std::time::{Duration, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyCreate, ReplyEmpty, ReplyEntry, ReplyWrite,
    Request,
};

use crate::render::{MessageToMain, VirtualFile};

const TTL: Duration = Duration::from_secs(1);
const BLOCK_SIZE: u64 = 512;
const STARTING_INODE: u64 = 2; // https://stackoverflow.com/questions/24613454/what-are-inode-numbers-1-and-2-used-for

const HELLO_DIR_ATTR: FileAttr = FileAttr {
    ino: 1,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH,
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::Directory,
    perm: 0o777,
    nlink: 2,
    uid: 1000,
    gid: 100,
    rdev: 0,
    blksize: BLOCK_SIZE as u32,
    flags: 0,
};

const HELLO_TXT_ATTR: FileAttr = FileAttr {
    ino: 0,
    size: 0,
    blocks: 1,
    atime: UNIX_EPOCH,
    mtime: UNIX_EPOCH,
    ctime: UNIX_EPOCH,
    crtime: UNIX_EPOCH,
    kind: FileType::RegularFile,
    perm: 0o666,
    nlink: 1,
    uid: 1000,
    gid: 100,
    rdev: 0,
    blksize: BLOCK_SIZE as u32,
    flags: 0,
};

pub struct TilesFS {
    files: HashMap<u64, VirtualFile>,
    next_inode: u64,
    tx: crossbeam::channel::Sender<MessageToMain>,
}

impl TilesFS {
    pub fn new(tx: crossbeam::channel::Sender<MessageToMain>) -> Self {
        Self {
            files: HashMap::new(),
            next_inode: STARTING_INODE,
            tx,
        }
    }
}

impl TilesFS {
    fn get_file(&mut self, inode: u64) -> Option<&mut VirtualFile> {
        self.files.get_mut(&inode)
    }
    fn create_file(&mut self, path: String) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        self.files.insert(inode, VirtualFile::new(path));
        inode
    }
}

impl Filesystem for TilesFS {
    fn lookup(&mut self, _req: &Request, _parent: u64, _name: &OsStr, reply: ReplyEntry) {
        reply.error(ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match ino {
            1 => reply.attr(&TTL, &HELLO_DIR_ATTR),
            _ => reply.error(ENOENT),
        }
    }

    fn write(
        &mut self,
        _req: &Request,
        inode: u64,
        _fh: u64,
        _offset: i64,
        data: &[u8],
        _write_flags: u32,
        #[allow(unused_variables)] flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        use std::io::Write;
        self.get_file(inode).unwrap().data.write_all(data).unwrap();
        reply.written(data.len() as u32);
    }

    fn create(
        &mut self,
        _req: &Request,
        _parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        let mut attr = HELLO_TXT_ATTR;
        attr.ino = self.create_file(name.to_str().unwrap().to_string());
        attr.size = 0;
        reply.created(&Duration::new(0, 0), &attr, 0, 10, 0);
    }

    fn release(
        &mut self,
        _req: &Request<'_>,
        inode: u64,
        _fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        self.tx
            .send(MessageToMain::File(self.files.remove(&inode).unwrap()))
            .unwrap();
        reply.ok();
    }
}
