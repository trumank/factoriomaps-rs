#![feature(int_roundings)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::fs;
use std::fs::File;
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::ffi::OsStr;
use std::time::{Duration, UNIX_EPOCH};

use clap::Parser;

use crossbeam_channel::unbounded;

use serde::{Deserialize, Serialize};

use image::{DynamicImage, GenericImage, GenericImageView};
use fast_image_resize as fr;
use webp::*;

use include_dir::{include_dir, Dir};

use libc::ENOENT;
use fuser::{MountOption, FileType, FileAttr, Filesystem, Request, ReplyEntry, ReplyAttr, ReplyWrite, ReplyCreate, ReplyEmpty};

use fs2::FileExt;

use indicatif::{ProgressBar, ProgressStyle};

//const TILE_SIZE: u32 = 2048;
const TILE_SIZE: u32 = 1024;
const MAX_ZOOM: i32 = 20;
const MIN_ZOOM: i32 = 12;
static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web");
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

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
struct Tile {
    zoom: i32,
    x: i32,
    y: i32,
}

impl Tile {
    /// Returns tile containing this tile
    fn zoom_out(&self) -> Tile {
        Tile {
            zoom: self.zoom - 1,
            x: self.x.div_floor(2),
            y: self.y.div_floor(2),
        }
    }
    /// Returns tile inside this tile with the smallest coordinates
    fn zoom_in(&self) -> Tile {
        Tile {
            zoom: self.zoom + 1,
            x: self.x * 2,
            y: self.y * 2,
        }
    }
    /// Returns translated Tile offset
    fn translate(&self, x: i32, y: i32) -> Tile {
        Tile {
            zoom: self.zoom,
            x: self.x + x,
            y: self.y + y,
        }
    }
    /// Returns all child tiles
    fn children(&self) -> Vec<Tile> {
        let origin = self.zoom_in();
        vec![
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
        ].into_iter().map(|(x, y)| origin.translate(x, y)).collect()
    }
}


#[derive(Debug, Serialize, Deserialize)]
struct SurfaceInfo {
    name: String,
    chunks: Vec<Coordinate>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Coordinate {
    x: i32,
    y: i32,
}

#[derive(Debug)]
enum TileState {
    Loaded(DynamicImage),
    Waiting,
    Processed,
}

impl TileState {
    fn take(&mut self) -> DynamicImage {
        let old = std::mem::replace(self, TileState::Processed);
        if let TileState::Loaded(img) = old {
            img
        } else {
            panic!("TileState is not Loaded");
        }
    }
}

#[derive(Debug)]
struct ThreadContext {
    surface_info: SurfaceInfo,
    tiles: HashMap<Tile, TileState>,
    progress: ProgressBar,
    loaded_tiles: usize,
    total_tiles: usize,
}
impl ThreadContext {
    fn new(surface_info: SurfaceInfo) -> ThreadContext {
        let mut tiles = HashMap::new();

        for coord in &surface_info.chunks {
            let mut tile = Tile {
                x: coord.x,
                y: coord.y,
                zoom: MAX_ZOOM,
            };

            loop {
                if tile.zoom <= MIN_ZOOM || tiles.contains_key(&tile) {
                    break;
                }
                tiles.insert(tile.clone() , TileState::Waiting);
                tile = tile.zoom_out();
            }
        }

        let progress = ProgressBar::new(tiles.len() as u64);
        progress.set_style(
        ProgressStyle::with_template("{wide_bar} Elapsed: {elapsed_precise}, ETA: {my_eta}").unwrap()
            .with_key("my_eta", |s: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                 match (s.pos(), s.len()) {
                    (pos,Some(len)) => write!(w, "{:#}", humantime::format_duration(std::time::Duration::from_secs(s.elapsed().as_secs() * (len-pos)/pos))),
                    _ => write!(w, "-"),
                }.unwrap()
            })
        );

        ThreadContext {
            surface_info,
            total_tiles: tiles.len(),
            tiles,
            progress,
            loaded_tiles: 0,
        }
    }

    fn tile_ready(&self, tile: &Tile) -> bool {
        tile.children().into_iter().all(|tile| match self.tiles.get(&tile) {
            Some(TileState::Loaded(_)) => true,
            Some(TileState::Waiting) => false,
            Some(TileState::Processed) => panic!("Shouldn't be checking already processed tiles"),
            None => true,
        })
    }

    fn progress(&mut self) {
        self.progress.inc(1);
        self.loaded_tiles += 1;
    }
}
const NUM_PARTS: u32 = 2;
const PART_SIZE: u32 = TILE_SIZE / NUM_PARTS;
struct TilePart {
    x: u32,
    y: u32,
}
impl TilePart {
    fn get_path_components(&self, tile: &Tile) -> (i32, i32, i32) {
        (
            tile.zoom,
            self.x as i32 + tile.x * NUM_PARTS as i32,
            self.y as i32 + tile.y * NUM_PARTS as i32,
        )
    }
    fn get_path(&self, surface_name: &String, tile: &Tile) -> String {
        let components = self.get_path_components(tile);
        format!(
            "{}/{}/{}/{}.webp",
            surface_name,
            components.0,
            components.1,
            components.2,
        )
    }
}
fn get_tile_parts() -> Vec<TilePart> {
    let mut parts = vec![];
    for x in 0..NUM_PARTS {
        for y in 0..NUM_PARTS {
            parts.push(TilePart { x, y });
        }
    }
    parts
}
fn tile_write_parts(output_path: &Arc<PathBuf>, surface_name: &String, tile: &Tile, image: &DynamicImage) {
    for part in get_tile_parts() {
        let sub_img = image.view(part.x * PART_SIZE, part.y * PART_SIZE, PART_SIZE, PART_SIZE).to_image();
        let path = output_path.join("tiles").join(part.get_path(surface_name, tile));
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let dyn_img = DynamicImage::from(sub_img);
        let encoder = Encoder::from_image(&dyn_img).unwrap();
        let webp = encoder.encode(80.0);
        std::fs::write(path, &*webp).unwrap();
    }
}

fn image_resize(src: DynamicImage) -> DynamicImage {
    let width = NonZeroU32::new(src.width()).unwrap();
    let height = NonZeroU32::new(src.height()).unwrap();
    let src_image = fr::Image::from_vec_u8(
        width,
        height,
        src.into_rgba8().into_raw(),
        fr::PixelType::U8x4,
    ).unwrap();

    // Create container for data of destination image
    let dst_width = NonZeroU32::new(TILE_SIZE).unwrap();
    let dst_height = NonZeroU32::new(TILE_SIZE).unwrap();
    let mut dst_image = fr::Image::new(
        dst_width,
        dst_height,
        src_image.pixel_type(),
    );

    // Get mutable view of destination image data
    let mut dst_view = dst_image.view_mut();

    // Create Resizer instance and resize source image
    // into buffer of destination image
    let mut resizer = fr::Resizer::new(
        fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3),
    );
    resizer.resize(&src_image.view(), &mut dst_view).unwrap();

    // Divide RGB channels of destination image by alpha
    //alpha_mul_div.divide_alpha_inplace(&mut dst_view).unwrap();
    DynamicImage::ImageRgba8(image::RgbaImage::from_raw(
        TILE_SIZE, TILE_SIZE,
        dst_image.into_vec()
    ).unwrap())
}

struct HelloFS {
    files: HashMap<u64, FuseFile>,
    next_inode: u64,
    tx: crossbeam::channel::Sender<MessageToMain>,
}

struct FuseFile {
    path: String,
    data: Vec<u8>,
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
    fn get_file(&mut self, inode: u64) -> Option<&mut FuseFile> {
        self.files.get_mut(&inode)
    }
    fn create_file(&mut self, path: String) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        self.files.insert(inode, FuseFile {
            path,
            data: vec![],
        });
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
        offset: i64,
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
        self.tx.send(MessageToMain::File(self.files.remove(&inode).unwrap())).unwrap();
        //println!("releasing {:?}", file.path);
        reply.ok();
    }
}

struct ChildGuard(std::process::Child);

enum MessageToMain {
    Finished,
    Killed,
    File(FuseFile),
    FinishReadImage {
        surface: String,
        tile: Tile,
        image: DynamicImage,
    },
    FinishWriteParts {
        surface: String,
        tile: Tile,
        image: DynamicImage,
    },
    FinishBuildParent {
        parent: Tile,
        image: DynamicImage,
    },
}

enum MessageToWorker {
    ReadImage {
        surface: String,
        tile: Tile,
        data: Vec<u8>,
    },
    TileWriteParts {
        surface: String,
        tile: Tile,
        image: DynamicImage,
    },
    TileBuildParent {
        parent: Tile,
        children: Vec<(Tile, DynamicImage)>,
    },
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        match self.0.kill() {
            Err(e) => println!("Could not kill child process: {e}"),
            Ok(_) => println!("Successfully killed child process"),
        }
    }
}

fn extract_dir<S: AsRef<Path>>(dir: &Dir, base_path: S, find_replace: &HashMap<String, String>) -> std::io::Result<()> {
    let base_path = base_path.as_ref();

    for entry in dir.entries() {
        let path = base_path.join(entry.path());

        match entry {
            include_dir::DirEntry::Dir(d) => {
                fs::create_dir_all(&path)?;
                extract_dir(d, base_path, find_replace)?;
            }
            include_dir::DirEntry::File(f) => {
                if let Some(utf8) = f.contents_utf8() {
                    let mut contents = utf8.to_owned();
                    for (find, replace) in find_replace.iter() {
                        contents = contents.replace(find, replace);
                    }
                    fs::write(path, contents.as_bytes())?;
                } else {
                    fs::write(path, f.contents())?;
                }
            }
        }
    }

    Ok(())
}

#[derive(clap::Parser)]
struct Args {
   #[command(subcommand)]
   action: Action,
}

#[derive(clap::Subcommand)]
enum Action {
   Render {
       /// Factorio directory root
       factorio: String,
       /// Render output path
       output: String,
       /// Path to map to render
       map: String,
   }
}

fn main() {
    env_logger::init();

    let args = Args::parse().action;
    match args {
        Action::Render { factorio, output, map } => {
            render(PathBuf::from(factorio), PathBuf::from(output), map);
        },
    }
}

fn render(factorio: PathBuf, output: PathBuf, map: String) {
    let res = crossbeam::scope(|scope| {
        // check factorio lockfile
        if let Ok(lockfile) = File::open(&factorio.join(".lock")) {
            lockfile.try_lock_exclusive().expect("Could not open lockfile, is factorio already running?");
            lockfile.unlock().unwrap();
        }

        // insert self into factorio mod list and save original to restore later
        let modname = "factoriomaps-rs";
        let modlist_path = factorio.join("mods/mod-list.json");
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
        fs::write(&modlist_path, &serde_json::to_vec_pretty(&modlist).unwrap()).unwrap();
        let mod_path = factorio.join("mods").join(modname);
        fs::remove_dir_all(&mod_path).ok();
        fs::create_dir(&mod_path).unwrap();
        MOD.extract(&mod_path).unwrap();

        let mountpoint = factorio.join("script-output");
        let options = vec![
            MountOption::FSName("fuser".to_string()),
            //MountOption::AutoUnmount,
        ];

        let (send_result, recv_result) = unbounded::<MessageToMain>();
        let (send_work, recv_work) = unbounded::<MessageToWorker>();

        let fuse_tx = send_result.clone();
        let session = fuser::spawn_mount2(
            HelloFS::new(fuse_tx),
            mountpoint,
            &options,
        ).unwrap();

        let ctrlc_tx = send_result.clone();
        ctrlc::set_handler(move || {
            ctrlc_tx.send(MessageToMain::Killed).unwrap();
        }).unwrap();

        // TODO very unlikely race condition as we start starting factorio before mounting the output directory
        let _xvfb = ChildGuard(std::process::Command::new("Xvfb")
            .arg(":8") // TODO don't assume :8 isn't being used
            .arg("-screen")
            .arg(",0")
            .arg("1024x768x16")
            .spawn()
            .unwrap());
        let _factorio = ChildGuard(std::process::Command::new(factorio.join("bin/x64/factorio"))
            .env("DISPLAY", ":8")
            .arg("--disable-audio")
            .arg("--disable-migration-window")
            .arg("--benchmark-graphics") // use instead of --load-game as it unpauses the game for us
            .arg(map)
            //.stdout(std::process::Stdio::null()) // TODO scan output for errors?
            .spawn()
            .unwrap());

        let mut thread_context = None;
        let output_path = Arc::from(output);
        std::fs::create_dir_all(&*output_path).unwrap();

        for _ in 0..std::thread::available_parallelism().unwrap().into() {
            let recv_work = recv_work.clone();
            let send_result = send_result.clone();
            let arc = Arc::clone(&output_path);
            scope.spawn(move |_| {
                while let Ok(work) = recv_work.recv() {
                    match work {
                        MessageToWorker::ReadImage { surface, tile, data } => {
                            send_result.send(MessageToMain::FinishReadImage {
                                surface,
                                tile,
                                image: image::load_from_memory(&data).unwrap(),
                            }).unwrap();
                        }
                        MessageToWorker::TileWriteParts { surface, tile, image } => {
                            tile_write_parts(&arc, &surface, &tile, &image);
                            send_result.send(MessageToMain::FinishWriteParts {
                                surface,
                                tile,
                                image,
                            }).unwrap();
                        }
                        MessageToWorker::TileBuildParent { parent, children } => {
                            let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
                            for (tile, img) in children {
                                full_size.copy_from(&img, (tile.x - parent.x * 2) as u32 * TILE_SIZE, (tile.y - parent.y * 2) as u32 * TILE_SIZE).unwrap();
                            }

                            let image = image_resize(full_size);

                            send_result.send(MessageToMain::FinishBuildParent {
                                parent,
                                image,
                            }).unwrap();
                        }
                    }
                }
                //println!("exit thread #{i}");
            });
        }

        while let Ok(status) = recv_result.recv() {
            match status {
                MessageToMain::Killed => {
                    println!("killed");
                    break;
                }
                MessageToMain::Finished => {
                    println!("finished");
                    break;
                }
                MessageToMain::File(file) => {
                    if file.path == "info.json" {
                        let info_exists = thread_context.is_none();
                        assert!(info_exists, "SurfaceInfo already exists");
                        let surface_info: SurfaceInfo = serde_json::from_slice(&file.data).unwrap();

                        thread_context = Some(ThreadContext::new(surface_info));
                    } else if file.path.ends_with(".png") {
                        let mut split = Path::new(&file.path)
                            .file_stem()
                            .and_then(std::ffi::OsStr::to_str)
                            .unwrap()
                            .split(',');
                        let surface_name = split.next().unwrap().to_owned();
                        let x = split.next().unwrap().parse::<i32>().unwrap();
                        let y = split.next().unwrap().parse::<i32>().unwrap();

                        let tc = thread_context.as_mut().unwrap();

                        send_work.send(MessageToWorker::ReadImage {
                            surface: tc.surface_info.name.to_owned(),
                            tile: Tile {
                                x,
                                y,
                                zoom: MAX_ZOOM,
                            },
                            data: file.data,
                        }).unwrap();
                    }
                }
                MessageToMain::FinishReadImage { surface, tile, image } => {
                    send_work.send(MessageToWorker::TileWriteParts {
                        surface,
                        tile,
                        image,
                    }).unwrap();
                }
                MessageToMain::FinishWriteParts { surface, tile, image } => {
                    let tc = thread_context.as_mut().unwrap();
                    tc.progress();

                    tc.tiles.insert(tile.clone(), TileState::Loaded(image));

                    let parent = tile.zoom_out();
                    if parent.zoom > MIN_ZOOM && tc.tile_ready(&parent) {
                        let mut children: Vec<(Tile, DynamicImage)> = vec![];
                        for tile in parent.children().into_iter() {
                            if let Some(state) = tc.tiles.get_mut(&tile) {
                                children.push((tile.clone(), state.take()));
                            }
                        }

                        send_work.send(MessageToWorker::TileBuildParent {
                            parent,
                            children,
                        }).unwrap();
                    }

                    if tc.loaded_tiles == tc.total_tiles {
                        let mut info = vec![];
                        for tile in tc.tiles.keys() {
                            for part in get_tile_parts() {
                                let comp = part.get_path_components(tile);
                                info.push(comp);
                            }
                        }

                        let mut find_replace = HashMap::new();
                        find_replace.insert("$MAP_DATA$".to_owned(), serde_json::to_string(&info).unwrap());
                        extract_dir(&WEB, &*output_path, &find_replace).unwrap();

                        send_result.send(MessageToMain::Finished).unwrap();
                    }
                }
                MessageToMain::FinishBuildParent { parent, image } => {
                    let tc = thread_context.as_mut().unwrap();
                    send_work.send(MessageToWorker::TileWriteParts {
                        surface: tc.surface_info.name.to_owned(),
                        tile: parent,
                        image,
                    }).unwrap();
                }
            }
        }

        session.join();

        // TODO proper error handling and cleanup
        fs::write(&modlist_path, modlist_str.as_bytes()).unwrap();
        fs::remove_dir_all(&mod_path).unwrap();
    });
    if let Err(err) = res {
        println!("{err:?}");
    }
}
