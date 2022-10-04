#![feature(nll)]
#![feature(int_roundings)]

use std::{fs, path::Path};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::env;
use std::ffi::OsStr;
use std::time::{Duration, UNIX_EPOCH};

use rayon::prelude::*;
use crossbeam_channel::{select, unbounded, bounded};

use serde::{Deserialize, Serialize};

use image::{DynamicImage, GenericImage, GenericImageView};
use fast_image_resize as fr;
use webp::*;

use libc::ENOENT;
use fuser::{MountOption, FileType, FileAttr, Filesystem, Request, ReplyEntry, ReplyAttr, ReplyWrite, ReplyCreate, ReplyEmpty};

use indicatif::{ProgressBar, ProgressStyle};

//const TILE_SIZE: u32 = 2048;
const TILE_SIZE: u32 = 1024;
const MAX_ZOOM: i32 = 20;
const MIN_ZOOM: i32 = 12;

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
    fn children<'a>(&self) -> Vec<Tile> {
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
        indicatif::ProgressStyle::with_template("{wide_bar} Elapsed: {elapsed_precise}, ETA: {my_eta}").unwrap()
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

    fn tile_write_parts(surface_name: &String, tile: &Tile, image: &DynamicImage) {
        let parts = 2;
        let part_size = TILE_SIZE / parts;
        let mut coords = vec![];
        for x in 0..parts {
            for y in 0..parts {
                coords.push((x, y));
            }
        }
        coords.into_par_iter().for_each(|(x, y)| {
            let sub_img = image.view(x * part_size, y * part_size, part_size, part_size).to_image();
            let path_str = &format!(
                "web/Images/4/{}/day/{}/{}/{}.png",
                surface_name,
                tile.zoom,
                x as i32 + tile.x * parts as i32,
                y as i32 + tile.y * parts as i32
            );
            let path = Path::new(path_str);
            fs::create_dir_all(&path.parent().unwrap()).unwrap();
            //sub_img.save(&path).unwrap();

            let dyn_img = DynamicImage::from(sub_img);
            let encoder = Encoder::from_image(&dyn_img).unwrap();
            //let webp = encoder.encode_lossless();
            let webp = encoder.encode(40.0);
            std::fs::write(Path::new(path_str).with_extension("webp"), &*webp).unwrap();
        });
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
}

struct HelloFS {
    files: HashMap<u64, File>,
    next_inode: u64,
    tx: crossbeam::channel::Sender<MessageToMain>,
}

struct File {
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
    fn get_file(&mut self, inode: u64) -> Option<&mut File> {
        self.files.get_mut(&inode)
    }
    fn create_file(&mut self, path: String) -> u64 {
        let inode = self.next_inode;
        self.next_inode += 1;
        self.files.insert(inode, File {
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
        self.get_file(inode).unwrap().data.write(data).unwrap();

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
        let mut attr = HELLO_TXT_ATTR.clone();
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
    File(File),
    FinishWriteParts {
        tile: Tile,
        image: DynamicImage,
    },
    FinishBuildParent {
        parent: Tile,
        image: DynamicImage,
    },
}

enum MessageToWorker {
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
            Err(e) => println!("Could not kill child process: {}", e),
            Ok(_) => println!("Successfully killed child process"),
        }
    }
}

fn setup_fuse() {
    let res = crossbeam::scope(|scope| {
        env_logger::init();
        let mountpoint = env::args_os().nth(1).unwrap();

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
            .arg(":8")
            .arg("-screen")
            .arg(",0")
            .arg("1024x768x16")
            .spawn()
            .unwrap());
        let _factorio = ChildGuard(std::process::Command::new("./factorio/bin/x64/factorio")
            .env("DISPLAY", ":8")
            .arg("--disable-audio")
            .arg("--disable-migration-window")
            .arg("--load-game")
            //.arg("maps/1c98b2430bf2c15c78808092871b671e7baed29c1869be652b7b8af1e6aaff40.zip") // small
            //.arg("maps/e752be9eade5aa80de908f825382e3bd98e0d29a4c7ffa07a7c0071f92ac39ad.zip") // medium
            .arg("maps/91c009e61f44c3c532f7152b0501ea0fc920723148dd1c38c4da129eb9d399f9.zip") // large
            //.stdout(std::process::Stdio::null()) // TODO scan output for errors?
            .spawn()
            .unwrap());





        let mut thread_context = None;

        for i in 0..32 {
            let recv_work = recv_work.clone();
            let send_result = send_result.clone();
            scope.spawn(move |_| {
                while let Ok(work) = recv_work.recv() {
                    //println!("Thread #{i} recieved work");
                    match work {
                        MessageToWorker::TileWriteParts { surface, tile, image } => {
                            ThreadContext::tile_write_parts(&surface, &tile, &image);
                            send_result.send(MessageToMain::FinishWriteParts {
                                tile,
                                image,
                            }).unwrap();
                        }
                        MessageToWorker::TileBuildParent { parent, children } => {
                            let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
                            for (tile, img) in children {
                                full_size.copy_from(&img, (tile.x - parent.x * 2) as u32 * TILE_SIZE, (tile.y - parent.y * 2) as u32 * TILE_SIZE).unwrap();
                            }

                            let image = ThreadContext::image_resize(full_size);

                            send_result.send(MessageToMain::FinishBuildParent {
                                parent,
                                image,
                            }).unwrap();
                        }
                    }
                }
                println!("exit thread #{i}");
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
                        //println!("{:?}", &surface_info);

                        thread_context = Some(ThreadContext::new(surface_info));
                    } else if file.path.ends_with(".png") {
                        let img = image::load_from_memory(&file.data).unwrap();

                        let mut split = Path::new(&file.path)
                            .file_stem()
                            .and_then(std::ffi::OsStr::to_str)
                            .unwrap()
                            .split(",");
                        let surface_name = split.next().unwrap().to_owned();
                        let x = split.next().unwrap().parse::<i32>().unwrap();
                        let y = split.next().unwrap().parse::<i32>().unwrap();

                        let tc = thread_context.as_mut().unwrap();

                        send_work.send(MessageToWorker::TileWriteParts {
                            surface: tc.surface_info.name.to_owned(),
                            tile: Tile {
                                x,
                                y,
                                zoom: MAX_ZOOM,
                            },
                            image: img,
                        }).unwrap();
                    }
                }
                MessageToMain::FinishWriteParts { tile, image } => {
                    let tc = thread_context.as_mut().unwrap();
                    tc.progress();

                    if tc.loaded_tiles == tc.total_tiles {
                        println!("finished");
                        send_result.send(MessageToMain::Finished).unwrap();
                    }

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
    });
    if let Err(err) = res {
        println!("{:?}", err);
    }
}

fn main() {
    setup_fuse()
}
