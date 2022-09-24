#![feature(nll)]
#![feature(int_roundings)]

use std::i8::MAX;
use std::{fs, path::Path};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use rayon::prelude::*;

use image::{DynamicImage, GenericImage, GenericImageView};
use webp::*;

use serde::{Deserialize, Serialize};
use serde_json::Result;

use fast_image_resize as fr;


//const TILE_SIZE: u32 = 2048;
const TILE_SIZE: u32 = 1024;
const MAX_ZOOM: i32 = 20;
const MIN_ZOOM: i32 = 12;
const INPUT_PATH: &str = "factorio/script-output";
const OUTPUT_PATH: &str = "factorio/script-output";

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

fn create_tile(surface: &Surface, tile: &Tile, progress: &indicatif::ProgressBar) -> Option<DynamicImage> {
    let img = if tile.zoom == MAX_ZOOM {
        //println!("reading {:?}", tile);
        //let string = &format!("factorio/script-output/{},{},{}.png", surface, tile.x, tile.y);
        let path = Path::new(INPUT_PATH).join(&format!("{},{},{}.png", surface.name, tile.x, tile.y));
        let img = image::open(&path).ok();
        if img.is_some() {
            progress.inc(1)
        }
        img
    } else {
        let zoom = tile.zoom_in();

        let q: Vec<_> = vec![
            (0, 0),
            (1, 0),
            (0, 1),
            (1, 1),
        ].into_par_iter()
            .map(|(x, y)| ((x, y), create_tile(surface, &zoom.translate(x, y), progress)))
            .filter_map(|data| match data {
                (p, Some(img)) => Some((p, img)),
                _ => None,
            })
            .collect();

        if !q.is_empty() {
            let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
            q.iter().for_each(|((x, y), img)| full_size.copy_from(img, *x as u32 * TILE_SIZE, *y as u32 * TILE_SIZE).unwrap());

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
            Some(resized)
        } else {
            None
        }
    };

    if let Some(ref img) = img {
        let parts = 2;
        let part_size = TILE_SIZE / parts;
        for x in 0..parts {
            for y in 0..parts {
                let sub_img = img.view(x * part_size, y * part_size, part_size, part_size).to_image();
                let path_str = &format!(
                    "web/Images/4/{}/day/{}/{}/{}.png",
                    &surface.name,
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
            }
        }
    }
    img
}

const fn num_bits<T>() -> usize { std::mem::size_of::<T>() * 8 }
fn log_2(x: i32) -> u32 {
    assert!(x > 0);
    num_bits::<i32>() as u32 - x.leading_zeros() - 1
}

fn create_tiles(surface: &Surface) {
    let progress = indicatif::ProgressBar::new(surface.tiles.len() as u64);
    progress.set_style(
        indicatif::ProgressStyle::with_template("{wide_bar} Elapsed: {elapsed_precise}, ETA: {my_eta}").unwrap()
            .with_key("my_eta", |s: &indicatif::ProgressState, w: &mut dyn std::fmt::Write| {
                 match (s.pos(), s.len()) {
                    (pos,Some(len)) => write!(w, "{:#}", humantime::format_duration(std::time::Duration::from_secs(s.elapsed().as_secs() * (len-pos)/pos))),
                    _ => write!(w, "-"),
                }.unwrap()
            })
    );

    let max = std::cmp::max(
        std::cmp::max(
            -surface.min_x,
            -surface.min_y,
        ),
        std::cmp::max(
            1 + surface.max_x,
            1 + surface.max_y,
        )
    );
    let zoom = MAX_ZOOM - log_2(max) as i32 - 1; // TODO off by one?
    println!("{}", zoom);
    vec![
        Tile { zoom, x: -1, y: -1 },
        Tile { zoom, x:  0, y: -1 },
        Tile { zoom, x: -1, y:  0 },
        Tile { zoom, x:  0, y:  0 },
    ].par_iter().for_each(|t| {
        create_tile(&surface, &t, &progress);
    });
}

fn create_map(surface: Surface) {
    let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * (1 + surface.width() as u32), TILE_SIZE * (1 + surface.height() as u32));

    for tile in surface.tiles {
        println!("{:?}", tile);
        let path = Path::new(INPUT_PATH).join(&format!("{},{},{}.png", surface.name, tile.x, tile.y));
        let img = image::open(&path).unwrap();
        full_size.copy_from(&img, (tile.x - surface.min_x) as u32 * TILE_SIZE, (tile.y - surface.min_y) as u32 * TILE_SIZE).unwrap();
    }
    full_size.save(Path::new("full.png")).unwrap();
}

#[derive(Debug)]
struct Surface {
    name: String,
    tiles: Vec<Tile>,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
}
impl Surface {
    fn width(&self) -> i32 {
        self.max_x - self.min_x
    }
    fn height(&self) -> i32 {
        self.max_y - self.min_y
    }
}

fn render() {
    let paths = fs::read_dir(INPUT_PATH).unwrap();

    let mut surfaces = HashMap::new();

    for entry in paths {
        let path = entry.unwrap().path();
        let name = path.file_stem().and_then(std::ffi::OsStr::to_str).unwrap();
        let mut split = name.split(",");
        let surface_name = split.next().unwrap().to_owned();
        let x = split.next().unwrap().parse::<i32>().unwrap();
        let y = split.next().unwrap().parse::<i32>().unwrap();

        println!("{:#?}", Tile {
            zoom: MAX_ZOOM,
            x,
            y,
        });
        let mut surface = surfaces.entry(surface_name.to_owned()).or_insert_with(|| Surface {
            name: surface_name,
            tiles: vec![],
            min_x: x,
            max_x: x,
            min_y: y,
            max_y: y,
        });
        surface.max_x = std::cmp::max(x, surface.max_x);
        surface.min_x = std::cmp::min(x, surface.min_x);
        surface.max_y = std::cmp::max(y, surface.max_y);
        surface.min_y = std::cmp::min(y, surface.min_y);

        surface.tiles.push(Tile {
            zoom: MAX_ZOOM,
            x,
            y,
        });
    }
    for (name, surface) in surfaces {
        println!("{surface:#?}");
        create_tiles(&surface);
        //create_map(surface);
    }
    //println!("{:#?}", surfaces);
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
    surface: String,
    tiles: HashMap<Tile, TileState>,
}
impl ThreadContext {
    fn new(surface: String) -> ThreadContext {
        ThreadContext {
            surface,
            tiles: HashMap::new(),
        }
    }

    fn tile_ready(&self, tile: &Tile) -> bool {
        tile.children().into_iter()/*.inspect(|tile| {
            println!("child state: {:?} {}", &tile, match self.tiles.get(&tile) {
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

    fn tile_loaded(&mut self, tile: Tile, image: DynamicImage) {
        self.tiles.insert(tile.clone(), TileState::Loaded(image));
        if let Some((parent, image)) = ThreadContext::tile_loaded_task(self, tile) {
            self.tile_loaded(parent, image);
        }
    }

    fn tile_loaded_task(ctx: &mut ThreadContext, tile: Tile) -> Option<(Tile, DynamicImage)> {
        println!("{:?}", tile);

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
                    ctx.surface,
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
                    println!("Not all children loaded, still waiting");
                    None
                } else {
                    println!("All children loaded, building parent tile");

                    let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
                    for tile in parent.children() {
                        //let state = &mut ctx.tiles.get_mut(&tile);
                        let state = &ctx.tiles.get(&tile);
                        if let Some(TileState::Loaded(img)) = state {
                            full_size.copy_from(img, (tile.x - parent.x * 2) as u32 * TILE_SIZE, (tile.y - parent.y * 2) as u32 * TILE_SIZE).unwrap();
                            ctx.tiles.insert(tile, TileState::Processed);
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

use std::env;
use std::ffi::OsStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use libc::ENOENT;
use fuser::{consts::FOPEN_DIRECT_IO, MountOption, FileType, FileAttr, Filesystem, Request, ReplyData, ReplyEntry, ReplyAttr, ReplyDirectory, ReplyWrite, ReplyOpen, ReplyCreate, ReplyEmpty, TimeOrNow};

const TTL: Duration = Duration::from_secs(1);           // 1 second
const BLOCK_SIZE: u64 = 512;

const HELLO_DIR_ATTR: FileAttr = FileAttr {
    ino: 1,
    size: 0,
    blocks: 0,
    atime: UNIX_EPOCH,                                  // 1970-01-01 00:00:00
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

const HELLO_TXT_CONTENT: &str = "Hello World!\n";

const HELLO_TXT_ATTR: FileAttr = FileAttr {
    ino: 0,
    size: 0,
    blocks: 1,
    atime: UNIX_EPOCH,                                  // 1970-01-01 00:00:00
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

struct HelloFS {
    files: HashMap<u64, File>,
    next_inode: u64,
    surface_info: Option<SurfaceInfo>,
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
            surface_info: None,
            thread_context: None,
        }
    }
}

const STARTING_INODE: u64 = 2; // https://stackoverflow.com/questions/24613454/what-are-inode-numbers-1-and-2-used-for

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
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        if parent == 1 && name.to_str() == Some("hello.txt") {
            reply.entry(&TTL, &HELLO_TXT_ATTR, 0);
        } else {
            reply.error(ENOENT);
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        match ino {
            1 => reply.attr(&TTL, &HELLO_DIR_ATTR),
            2 => reply.attr(&TTL, &HELLO_TXT_ATTR),
            _ => reply.error(ENOENT),
        }
    }

    fn read(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, _size: u32, _flags: i32, _lock_owner: Option<u64>, reply: ReplyData) {
        if ino == 2 {
            reply.data(&HELLO_TXT_CONTENT.as_bytes()[offset as usize..]);
        } else {
            reply.error(ENOENT);
        }
    }

    fn readdir(&mut self, _req: &Request, ino: u64, _fh: u64, offset: i64, mut reply: ReplyDirectory) {
        if ino != 1 {
            reply.error(ENOENT);
            return;
        }

        let entries = vec![
            (1, FileType::Directory, "."),
            (1, FileType::Directory, ".."),
            (2, FileType::RegularFile, "hello.txt"),
        ];

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            // i + 1 means the index of the next entry
            reply.add(entry.0, (i + 1) as i64, entry.1, entry.2);
        }
        reply.ok();
    }
    fn write(
        &mut self,
        _req: &Request,
        inode: u64,
        fh: u64,
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

    fn open(&mut self, req: &Request, inode: u64, flags: i32, reply: ReplyOpen) {
        println!("open() called for {:?} flags={:?}", inode, flags);

        reply.opened(10, fuser::consts::FOPEN_DIRECT_IO | fuser::consts::FOPEN_KEEP_CACHE);
        //reply.error(libc::EACCES);
    }

    fn setattr(
        &mut self,
        req: &Request,
        inode: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        println!("setattr() called");
        if let Some(size) = size {
            println!("truncate() called with {:?} {:?}", inode, size);
        }
        reply.attr(&Duration::new(0, 0), &HELLO_TXT_ATTR);
    }

    fn create(
        &mut self,
        req: &Request,
        parent: u64,
        name: &OsStr,
        mut mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        println!("create() called with {:?} {:?}", parent, name);
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
        println!("release() called with {:?}", inode);
        let file = &self.files[&inode];
        println!("releasing {:?}", file.path);
        if file.path == "info.json" {
            let info_exists = self.surface_info.is_none();
            assert!(info_exists, "SurfaceInfo already exists");
            let surface_info: SurfaceInfo = serde_json::from_slice(&file.data).unwrap();
            println!("{:?}", &surface_info);

            let mut thread_context = ThreadContext::new(surface_info.name.to_owned());
            for coord in &surface_info.chunks {
                let mut tile = Tile {
                    x: coord.x,
                    y: coord.y,
                    zoom: MAX_ZOOM,
                };
                loop {
                    if tile.zoom <= MIN_ZOOM || thread_context.tiles.contains_key(&tile) {
                        break;
                    }
                    thread_context.tiles.insert(tile.clone() , TileState::Waiting);
                    tile = tile.zoom_out();
                }
            }

            self.surface_info = Some(surface_info);
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

            self.thread_context.as_mut().unwrap().tile_loaded(Tile {
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
    setup_fuse()
    // render();
}
