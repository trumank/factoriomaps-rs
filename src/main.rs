#![feature(nll)]
#![feature(int_roundings)]

use std::{fs, path::Path};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::env;
use std::ffi::OsStr;
use std::time::{Duration, UNIX_EPOCH};

use rayon::prelude::*;

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

const TTL: Duration = Duration::from_secs(1);           // 1 second
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

struct ThreadContext {
    surface_info: SurfaceInfo,
    tiles: HashMap<Tile, TileState>,
    progress: ProgressBar,
    loaded_tiles: usize,
}
impl ThreadContext {
    fn new(surface_info: SurfaceInfo) -> ThreadContext {
        let mut tiles = HashMap::new();
        //let mut total = 0;
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
                //total += 1;
            }
        }

        let progress = ProgressBar::new(surface_info.chunks.len() as u64);
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
            tiles,
            progress,
            loaded_tiles: 0,
        }
    }

    fn tile_ready(&self, tile: &Tile) -> bool {
        tile.children().into_iter()/*.inspect(|tile| {
            //println!("child state: {:?} {}", &tile, match self.tiles.get(&tile) {
                Some(TileState::Loaded(_)) => "loaded",
                Some(TileState::Waiting) => "waiting",
                Some(TileState::Processed) => "processed",
                None => "none",
            });
        })*/.all(|tile| match self.tiles.get(&tile) {
            Some(TileState::Loaded(_)) => true,
            Some(TileState::Waiting) => false,
            Some(TileState::Processed) => panic!("Shouldn't be checking already processed tiles"),
            None => true,
        })
    }

    fn tile_loaded_src(&mut self, tile: Tile, image: DynamicImage) {
        self.tile_loaded(tile, image);
        self.progress.inc(1);
        self.loaded_tiles += 1;
        if self.loaded_tiles == self.surface_info.chunks.len() {
            println!("finished");
            std::process::exit(0);
        }
    }

    fn tile_loaded(&mut self, tile: Tile, image: DynamicImage) {
        self.tiles.insert(tile.clone(), TileState::Loaded(image));
        if let Some((parent, image)) = ThreadContext::tile_loaded_task(self, tile) {
            self.tile_loaded(parent, image);
        }
    }

    fn tile_loaded_task(ctx: &mut ThreadContext, tile: Tile) -> Option<(Tile, DynamicImage)> {
        //println!("{:?}", tile);

        if let TileState::Loaded(image) = &ctx.tiles[&tile] {
            // write out current tile in parts
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
                    ctx.surface_info.name,
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

            // insert tile into map to reference later
            //ctx.tiles.insert(tile.clone(), TileState::Loaded(image));

            // check if this is the last tile needed to create parent tile
            let parent = tile.zoom_out();
            if parent.zoom > MIN_ZOOM {
                if !ctx.tile_ready(&parent) {
                    //println!("Not all children loaded, still waiting");
                    None
                } else {
                    //println!("All children loaded, building parent tile");

                    let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
                    for tile in parent.children() {
                        //let state = &mut ctx.tiles.get_mut(&tile);
                        let state = &ctx.tiles.get(&tile);
                        if let Some(TileState::Loaded(img)) = state {
                            full_size.copy_from(img, (tile.x - parent.x * 2) as u32 * TILE_SIZE, (tile.y - parent.y * 2) as u32 * TILE_SIZE).unwrap();
                            ctx.tiles.insert(tile, TileState::Processed);
                            //ctx.progress.inc(1);
                            //*state = Some(&mut TileState::Processed);
                        }
                    }

                    //let resized = full_size.resize(TILE_SIZE, TILE_SIZE, image::imageops::FilterType::Nearest);

                    let width = NonZeroU32::new(full_size.width()).unwrap();
                    let height = NonZeroU32::new(full_size.height()).unwrap();
                    let src_image = fr::Image::from_vec_u8(
                        width,
                        height,
                        full_size.into_rgba8().into_raw(),
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
                    let resized = DynamicImage::ImageRgba8(image::RgbaImage::from_raw(
                        TILE_SIZE, TILE_SIZE,
                        dst_image.into_vec()
                    ).unwrap());

                    Some((parent, resized))
                }
            } else {
                None
            }

            /*
            println!("work {:?}", ctx.tiles.iter().map(|(k, v)| {
                match v {
                    TileState::Processed => None,
                    TileState::Waiting => Some((k, "Waiting")),
                    TileState::Loaded(_) => Some((k, "Loaded")),
                }
            }).filter(|msg| msg.is_some()).collect::<Vec<_>>());
            */
        } else {
            None
        }
    }
}

struct HelloFS {
    files: HashMap<u64, File>,
    next_inode: u64,
    thread_context: Option<ThreadContext>,
}

struct File {
    path: String,
    data: Vec<u8>,
}

impl HelloFS {
    fn new() -> HelloFS {
        HelloFS {
            files: HashMap::new(),
            next_inode: STARTING_INODE,
            thread_context: None,
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
        parent: u64,
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
        let file = &self.files[&inode];
        //println!("releasing {:?}", file.path);
        if file.path == "info.json" {
            let info_exists = self.thread_context.is_none();
            assert!(info_exists, "SurfaceInfo already exists");
            let surface_info: SurfaceInfo = serde_json::from_slice(&file.data).unwrap();
            //println!("{:?}", &surface_info);

            let mut thread_context = ThreadContext::new(surface_info);

            self.thread_context = Some(thread_context);
            self.files.remove(&inode);
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

            self.thread_context.as_mut().unwrap().tile_loaded_src(Tile {
                x,
                y,
                zoom: MAX_ZOOM,
            }, img);

            self.files.remove(&inode);
        }
        reply.ok();
    }
}

fn setup_fuse() {
    env_logger::init();
    let mountpoint = env::args_os().nth(1).unwrap();

    let options = vec![
        MountOption::FSName("fuser".to_string()),
        MountOption::AutoUnmount,
    ];
    fuser::mount2(
        HelloFS::new(),
        mountpoint,
        &options,
    ).unwrap();
}

fn main() {
    let cmd = std::process::Command::new("./factorio/bin/x64/factorio")
        .arg("--disable-audio")
        .arg("--disable-migration-window")
        .arg("--load-game")
        .arg("maps/1c98b2430bf2c15c78808092871b671e7baed29c1869be652b7b8af1e6aaff40.zip")
        .spawn()
        .unwrap();
    // TODO very unlikely race condition as we start starting factorio before mounting the output directory
    setup_fuse()
}
