#![feature(int_roundings)]

mod render;

use std::collections::HashMap;
use std::ffi::OsStr;
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use clap::Parser;

use crossbeam_channel::unbounded;

use serde::{Deserialize, Serialize};

use include_dir::{include_dir, Dir};

use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate, ReplyEmpty, ReplyEntry,
    ReplyWrite, Request,
};
use libc::ENOENT;

use fs2::FileExt;

use render::{MessageToMain, MessageToWorker, VirtualFile};

static MOD: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/mod");

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

#[derive(Debug, Serialize, Deserialize)]
struct FactorioMods {
    mods: Vec<FactorioModEntry>,
}
#[derive(Debug, Serialize, Deserialize)]
struct FactorioModEntry {
    name: String,
    enabled: bool,
}

struct HelloFS {
    files: HashMap<u64, VirtualFile>,
    next_inode: u64,
    tx: crossbeam::channel::Sender<MessageToMain>,
}

impl HelloFS {
    fn new(tx: crossbeam::channel::Sender<MessageToMain>) -> HelloFS {
        HelloFS {
            files: HashMap::new(),
            next_inode: STARTING_INODE,
            tx,
        }
    }
}

impl HelloFS {
    fn get_file(&mut self, inode: u64) -> Option<&mut VirtualFile> {
        self.files.get_mut(&inode)
    }
    fn create_file(&mut self, path: String) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        self.files.insert(inode, VirtualFile { path, data: vec![] });
        inode
    }
}

impl Filesystem for HelloFS {
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
        //println!("write() called with {:?} offset={:?} size={:?}", inode, offset, data.len());

        use std::io::Write;
        self.get_file(inode).unwrap().data.write_all(data).unwrap();

        reply.written(data.len() as u32);
        //reply.error(libc::EBADF);
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
        //println!("create() called with {:?} {:?}", parent, name);
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
        //println!("release() called with {:?}", inode);
        self.tx
            .send(MessageToMain::File(self.files.remove(&inode).unwrap()))
            .unwrap();
        //println!("releasing {:?}", file.path);
        reply.ok();
    }
}

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        match self.kill() {
            Err(ref e) if e.kind() == std::io::ErrorKind::InvalidInput => {} // already exited, do nothing
            Err(e) => println!("Could not kill child process: {e}"),
            Ok(_) => println!("Successfully killed child process"),
        }
    }
}
impl std::ops::Deref for ChildGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl std::ops::DerefMut for ChildGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(clap::Parser)]
struct Args {
    #[command(subcommand)]
    action: Action,
}

#[derive(clap::Subcommand)]
enum Action {
    RenderFuse(ActionRenderFuse),
    RenderLdPreload(ActionRenderLdPreload),
}

#[derive(clap::Parser)]
struct ActionRenderFuse {
    /// Factorio directory root
    factorio: PathBuf,
    /// Render output path
    output: PathBuf,
    /// Path to map to render
    map: String,
    /// By default Xvfb will be used to run factorio in the background. Set this flag to make
    /// the window visible
    #[clap(long, short)]
    debug: bool,
}
#[derive(clap::Parser)]
struct ActionRenderLdPreload {
    /// Factorio directory root
    factorio: PathBuf,
    /// Render output path
    output: PathBuf,
    /// Path to map to render
    map: String,
    /// By default Xvfb will be used to run factorio in the background. Set this flag to make
    /// the window visible
    #[clap(long, short)]
    debug: bool,
}

fn main() {
    let args = Args::parse().action;
    match args {
        Action::RenderFuse(action) => {
            render_fuse(action);
        }
        Action::RenderLdPreload(action) => {
            render_ldpreload(action);
        }
    }
}

struct SetupGuard {
    mod_path: PathBuf,
    modlist_path: PathBuf,
    modlist_str: String,
}
impl SetupGuard {
    fn new<P: AsRef<Path>>(factorio: P, output: P, map: &str) -> Self {
        // check factorio lockfile
        if let Ok(lockfile) = File::open(factorio.as_ref().join(".lock")) {
            lockfile
                .try_lock_exclusive()
                .expect("Could not open lockfile, is factorio already running?");
            lockfile.unlock().unwrap();
        }

        let mut sync_mods = ChildGuard(
            std::process::Command::new(factorio.as_ref().join("bin/x64/factorio"))
                .arg("--sync-mods")
                .arg(map)
                .spawn()
                .unwrap(),
        );
        sync_mods.wait().unwrap();

        // insert self into factorio mod list and save original to restore later
        let modname = "factoriomaps-rs";
        let modlist_path = factorio.as_ref().join("mods/mod-list.json");
        let modlist_str = fs::read_to_string(&modlist_path).unwrap();
        let mut modlist: FactorioMods = serde_json::from_str(&modlist_str).unwrap();
        let mut found = false;
        for entry in &mut modlist.mods {
            if entry.name == modname {
                entry.enabled = true;
                found = true;
                break;
            }
        }
        if !found {
            modlist.mods.push(FactorioModEntry {
                name: modname.to_owned(),
                enabled: true,
            });
        }
        fs::write(&modlist_path, serde_json::to_vec_pretty(&modlist).unwrap()).unwrap();
        let mod_path = factorio.as_ref().join("mods").join(modname);
        fs::remove_dir_all(&mod_path).ok();
        fs::create_dir(&mod_path).unwrap();
        MOD.extract(&mod_path).unwrap();

        std::fs::create_dir_all(output).unwrap();

        Self {
            modlist_path,
            modlist_str,
            mod_path,
        }
    }
}
impl Drop for SetupGuard {
    fn drop(&mut self) {
        fs::write(&self.modlist_path, self.modlist_str.as_bytes()).unwrap();
        fs::remove_dir_all(&self.mod_path).unwrap();
    }
}

fn render_ldpreload(action: ActionRenderLdPreload) {
    let res = crossbeam::scope(|scope| {
        let ActionRenderLdPreload {
            factorio,
            output,
            map,
            debug,
        } = action;
        let _setup_guard = SetupGuard::new(&factorio, &output, &map);

        let mut factorio_cmd = std::process::Command::new(factorio.join("bin/x64/factorio"));

        let _xvfb = if !debug {
            factorio_cmd.env("DISPLAY", ":8");

            Some(ChildGuard(
                std::process::Command::new("Xvfb")
                    .arg(":8") // TODO don't assume :8 isn't being used
                    .arg("-screen")
                    .arg(",0")
                    .arg("1024x768x16")
                    .spawn()
                    .unwrap(),
            ))
        } else {
            None
        };

        let _factorio = ChildGuard(
            factorio_cmd
                .env("LD_PRELOAD", "./target/release/libfactoriomaps_rs.so")
                .arg("--disable-audio")
                .arg("--disable-migration-window")
                // --benchmark-graphics unpauses the game, but swollows errors
                // --load-game is to figure out why something broke
                .arg(if debug {
                    "--load-game"
                } else {
                    "--benchmark-graphics"
                })
                .arg(map)
                //.stdout(std::process::Stdio::null()) // TODO scan output for errors?
                .spawn()
                .unwrap(),
        );

        let (send_result, recv_result) = unbounded::<MessageToMain>();

        let ctrlc_tx = send_result.clone();
        ctrlc::set_handler(move || {
            ctrlc_tx.send(MessageToMain::Killed).unwrap();
        })
        .unwrap();

        while let Ok(status) = recv_result.recv() {
            match status {
                MessageToMain::Killed => {
                    println!("killed");
                    break;
                }
                _ => {}
            }
        }
    });
}

fn render_fuse(action: ActionRenderFuse) {
    let res = crossbeam::scope(|scope| {
        let ActionRenderFuse {
            factorio,
            output,
            map,
            debug,
        } = action;
        let _setup_guard = SetupGuard::new(&factorio, &output, &map);

        let mut factorio_cmd = std::process::Command::new(factorio.join("bin/x64/factorio"));

        let _xvfb = if !debug {
            factorio_cmd.env("DISPLAY", ":8");

            Some(ChildGuard(
                std::process::Command::new("Xvfb")
                    .arg(":8") // TODO don't assume :8 isn't being used
                    .arg("-screen")
                    .arg(",0")
                    .arg("1024x768x16")
                    .spawn()
                    .unwrap(),
            ))
        } else {
            None
        };

        let _factorio = ChildGuard(
            factorio_cmd
                .arg("--disable-audio")
                .arg("--disable-migration-window")
                // --benchmark-graphics unpauses the game, but swollows errors
                // --load-game is to figure out why something broke
                .arg(if debug {
                    "--load-game"
                } else {
                    "--benchmark-graphics"
                })
                .arg(map)
                //.stdout(std::process::Stdio::null()) // TODO scan output for errors?
                .spawn()
                .unwrap(),
        );

        let (send_result, recv_result) = unbounded::<MessageToMain>();
        let (send_work, recv_work) = unbounded::<MessageToWorker>();

        let mountpoint = factorio.join("script-output");
        let options = vec![
            MountOption::FSName("fuser".to_string()),
            //MountOption::AutoUnmount,
        ];

        let fuse_tx = send_result.clone();
        let session = fuser::spawn_mount2(HelloFS::new(fuse_tx), mountpoint, &options).unwrap();

        let ctrlc_tx = send_result.clone();
        ctrlc::set_handler(move || {
            ctrlc_tx.send(MessageToMain::Killed).unwrap();
        })
        .unwrap();

        render::spawn_threads(&output, scope, recv_work, send_result.clone());

        render::main_loop(output, recv_result, send_work, send_result);

        session.join();
    });
    if let Err(err) = res {
        println!("{err:?}");
    }
}
