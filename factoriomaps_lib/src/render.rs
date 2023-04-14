use std::collections::HashMap;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use crossbeam::channel::{Receiver, Sender};
use crossbeam::thread::Scope;

use serde::{Deserialize, Serialize};

use fast_image_resize as fr;
use image::{DynamicImage, GenericImage, GenericImageView};

use include_dir::{include_dir, Dir};

use indicatif::{ProgressBar, ProgressStyle};

//const TILE_SIZE: u32 = 2048;
const TILE_SIZE: u32 = 1024;
const MAX_ZOOM: i32 = 20;
const NUM_PARTS: u32 = 2;
const PART_SIZE: u32 = TILE_SIZE / NUM_PARTS;

const TILE_EXTENSION: &str = "jpg";

static WEB: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/web");

pub struct VirtualFile {
    pub path: PathBuf,
    pub data: Vec<u8>,
}
impl VirtualFile {
    pub fn new<P: AsRef<Path>>(path: P) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            data: vec![],
        }
    }
}
pub enum MessageToMain {
    Finished,
    Killed,
    File(VirtualFile),
    FinishReadImage { tile: Tile, image: DynamicImage },
    FinishWriteParts { tile: Tile, image: DynamicImage },
    FinishBuildParent { parent: Tile, image: DynamicImage },
}

pub enum MessageToWorker {
    ReadImage {
        tile: Tile,
        data: Vec<u8>,
    },
    TileWriteParts {
        tile: Tile,
        image: DynamicImage,
    },
    TileBuildParent {
        parent: Tile,
        children: Vec<(Tile, DynamicImage)>,
    },
}

#[derive(Debug, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
pub struct Tile {
    surface: String,
    zoom: i32,
    x: i32,
    y: i32,
}

impl Tile {
    /// Returns tile containing this tile
    fn zoom_out(&self) -> Tile {
        Tile {
            surface: self.surface.to_owned(),
            zoom: self.zoom - 1,
            x: self.x.div_floor(2),
            y: self.y.div_floor(2),
        }
    }
    /// Returns tile inside this tile with the smallest coordinates
    fn zoom_in(&self) -> Tile {
        Tile {
            surface: self.surface.to_owned(),
            zoom: self.zoom + 1,
            x: self.x * 2,
            y: self.y * 2,
        }
    }
    /// Returns translated Tile offset
    fn translate(&self, x: i32, y: i32) -> Tile {
        Tile {
            surface: self.surface.to_owned(),
            zoom: self.zoom,
            x: self.x + x,
            y: self.y + y,
        }
    }
    /// Returns all child tiles
    fn children(&self) -> Vec<Tile> {
        let origin = self.zoom_in();
        vec![(0, 0), (1, 0), (0, 1), (1, 1)]
            .into_iter()
            .map(|(x, y)| origin.translate(x, y))
            .collect()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct SurfaceInfo {
    name: String,
    tags: HashMap<String, Vec<Tag>>,
    chunks: Vec<Coordinate<i32>>,
}
#[derive(Debug, Serialize, Deserialize)]
struct Coordinate<T> {
    x: T,
    y: T,
}
#[derive(Debug, Serialize, Deserialize)]
struct Tag {
    position: Coordinate<f64>,
    text: String,
}

enum TileState {
    Loaded(DynamicImage),
    Waiting,
    Processed,
}
impl std::fmt::Debug for TileState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                TileState::Loaded(_) => "Loaded",
                TileState::Waiting => "Waiting",
                TileState::Processed => "Processed",
            }
        )
    }
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
    info: Vec<SurfaceInfo>,
    tiles: HashMap<Tile, TileState>,
    min_zoom: HashMap<String, i32>,
    progress: ProgressBar,
    loaded_tiles: usize,
    total_tiles: usize,
}
impl ThreadContext {
    fn new(info: Vec<SurfaceInfo>) -> ThreadContext {
        let mut tiles = HashMap::new();
        let mut min_zoom = HashMap::new();

        for surface in &info {
            let Some(first) = surface.chunks.first() else {
                continue;
            };

            let mut min_x = first.x;
            let mut max_x = first.x;
            let mut min_y = first.y;
            let mut max_y = first.y;

            for chunk in &surface.chunks {
                min_x = min_x.min(chunk.x);
                max_x = max_x.max(chunk.x);
                min_y = min_y.min(chunk.y);
                max_y = max_y.max(chunk.y)
            }
            let max = (1 - min_x).max(1 - min_y).max(max_x).max(max_y);
            let mz = MAX_ZOOM - max.ilog2() as i32 - 6;
            min_zoom.insert(surface.name.to_owned(), mz);

            for chunk in &surface.chunks {
                let mut tile = Tile {
                    surface: surface.name.to_owned(),
                    x: chunk.x,
                    y: chunk.y,
                    zoom: MAX_ZOOM,
                };

                loop {
                    if tile.zoom <= mz || tiles.contains_key(&tile) {
                        break;
                    }
                    tiles.insert(tile.clone(), TileState::Waiting);
                    tile = tile.zoom_out();
                }
            }
        }

        let progress = ProgressBar::new(tiles.len() as u64);
        progress.set_style(
            ProgressStyle::with_template(
                "{wide_bar} Elapsed: {elapsed_precise}, ETA: {eta_precise}",
            )
            .unwrap(),
        );

        ThreadContext {
            info,
            total_tiles: tiles.len(),
            min_zoom,
            tiles,
            progress,
            loaded_tiles: 0,
        }
    }

    fn tile_ready(&self, tile: &Tile) -> bool {
        tile.children()
            .into_iter()
            .all(|tile| match self.tiles.get(&tile) {
                Some(TileState::Loaded(_)) => true,
                Some(TileState::Waiting) => false,
                Some(TileState::Processed) => {
                    panic!("Shouldn't be checking already processed tiles")
                }
                None => true,
            })
    }

    fn progress(&mut self) {
        self.progress.inc(1);
        self.loaded_tiles += 1;
    }
}
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
    fn get_path(&self, tile: &Tile) -> String {
        let components = self.get_path_components(tile);
        format!(
            "{}/{}/{}/{}.{}",
            tile.surface, components.0, components.1, components.2, TILE_EXTENSION
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
fn tile_write_parts<P: AsRef<Path>>(output: P, tile: &Tile, image: &DynamicImage) {
    for part in get_tile_parts() {
        let sub_img = image
            .view(part.x * PART_SIZE, part.y * PART_SIZE, PART_SIZE, PART_SIZE)
            .to_image();
        let path = output.as_ref().join("tiles").join(part.get_path(tile));
        fs::create_dir_all(path.parent().unwrap()).unwrap();

        let dyn_img = DynamicImage::from(sub_img);

        let mut data = vec![];
        let cur = std::io::Cursor::new(&mut data);
        let encoder = jpeg_encoder::Encoder::new(cur, 80);
        let (width, height) = dyn_img.dimensions();
        let mut bytes = dyn_img.into_bytes();
        for p in bytes.chunks_mut(4) {
            if p[3] <= 0x7f {
                p[0] = 27;
                p[1] = 45;
                p[2] = 51;
                p[3] = 0xff;
            }
        }
        encoder.encode(&bytes, width as u16, height as u16, jpeg_encoder::ColorType::Rgba).unwrap();

        std::fs::write(path, &*data).unwrap();
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
    )
    .unwrap();

    // Create container for data of destination image
    let dst_width = NonZeroU32::new(TILE_SIZE).unwrap();
    let dst_height = NonZeroU32::new(TILE_SIZE).unwrap();
    let mut dst_image = fr::Image::new(dst_width, dst_height, src_image.pixel_type());

    // Get mutable view of destination image data
    let mut dst_view = dst_image.view_mut();

    // Create Resizer instance and resize source image
    // into buffer of destination image
    let mut resizer = fr::Resizer::new(fr::ResizeAlg::Convolution(fr::FilterType::Lanczos3));
    resizer.resize(&src_image.view(), &mut dst_view).unwrap();

    // Divide RGB channels of destination image by alpha
    //alpha_mul_div.divide_alpha_inplace(&mut dst_view).unwrap();
    DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(TILE_SIZE, TILE_SIZE, dst_image.into_vec()).unwrap(),
    )
}

pub fn extract_dir<P: AsRef<Path>>(
    dir: &Dir,
    base_path: P,
    find_replace: &HashMap<String, String>,
) -> std::io::Result<()> {
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

pub fn spawn_threads<P: AsRef<Path>>(
    output: P,
    scope: &Scope,
    recv_work: Receiver<MessageToWorker>,
    send_result: Sender<MessageToMain>,
) {
    for _ in 0..std::thread::available_parallelism().unwrap().into() {
        let recv_work = recv_work.clone();
        let send_result = send_result.clone();
        let output = output.as_ref().to_owned();
        scope.spawn(move |_| {
            while let Ok(work) = recv_work.recv() {
                match work {
                    MessageToWorker::ReadImage { tile, data } => {
                        send_result
                            .send(MessageToMain::FinishReadImage {
                                tile,
                                image: image::load_from_memory(&data).unwrap(),
                            })
                            .unwrap();
                    }
                    MessageToWorker::TileWriteParts { tile, image } => {
                        tile_write_parts(&output, &tile, &image);
                        send_result
                            .send(MessageToMain::FinishWriteParts { tile, image })
                            .unwrap();
                    }
                    MessageToWorker::TileBuildParent { parent, children } => {
                        let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
                        for (tile, img) in children {
                            full_size
                                .copy_from(
                                    &img,
                                    (tile.x - parent.x * 2) as u32 * TILE_SIZE,
                                    (tile.y - parent.y * 2) as u32 * TILE_SIZE,
                                )
                                .unwrap();
                        }

                        let image = image_resize(full_size);

                        send_result
                            .send(MessageToMain::FinishBuildParent { parent, image })
                            .unwrap();
                    }
                }
            }
        });
    }
}

pub fn main_loop<P: AsRef<Path>>(
    output: P,
    recv_result: Receiver<MessageToMain>,
    send_work: Sender<MessageToWorker>,
    send_result: Sender<MessageToMain>,
) {
    let mut thread_context = None;

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
                if file.path.file_name() == Some(std::ffi::OsStr::new("info.json")) {
                    let info_exists = thread_context.is_none();
                    assert!(info_exists, "SurfaceInfo already exists");
                    let info = serde_json::from_slice(&file.data).unwrap();
                    thread_context = Some(ThreadContext::new(info));
                } else if file.path.extension() == Some(std::ffi::OsStr::new("bmp")) {
                    let mut split = file
                        .path
                        .file_stem()
                        .and_then(std::ffi::OsStr::to_str)
                        .unwrap()
                        .split(',');
                    let surface = split.next().unwrap().to_owned();
                    let x = split.next().unwrap().parse::<i32>().unwrap();
                    let y = split.next().unwrap().parse::<i32>().unwrap();

                    send_work
                        .send(MessageToWorker::ReadImage {
                            tile: Tile {
                                surface,
                                x,
                                y,
                                zoom: MAX_ZOOM,
                            },
                            data: file.data,
                        })
                        .unwrap();
                }
            }
            MessageToMain::FinishReadImage { tile, image } => {
                send_work
                    .send(MessageToWorker::TileWriteParts { tile, image })
                    .unwrap();
            }
            MessageToMain::FinishWriteParts { tile, image } => {
                let tc = thread_context.as_mut().unwrap();
                tc.progress();

                tc.tiles.insert(tile.clone(), TileState::Loaded(image));

                let parent = tile.zoom_out();
                if parent.zoom > tc.min_zoom[&tile.surface] && tc.tile_ready(&parent) {
                    let mut children: Vec<(Tile, DynamicImage)> = vec![];
                    for tile in parent.children().into_iter() {
                        if let Some(state) = tc.tiles.get_mut(&tile) {
                            children.push((tile.clone(), state.take()));
                        }
                    }

                    send_work
                        .send(MessageToWorker::TileBuildParent { parent, children })
                        .unwrap();
                }

                if tc.loaded_tiles == tc.total_tiles {
                    #[derive(Serialize)]
                    struct MapInfo {
                        surfaces: HashMap<String, Surface>,
                        extension: &'static str,
                    }

                    #[derive(Serialize)]
                    struct Surface {
                        tiles: Vec<(i32, i32, i32)>,
                        tags: HashMap<String, Vec<Tag>>,
                    }

                    let mut surfaces: HashMap<String, Surface> = std::mem::take(&mut tc.info)
                        .into_iter()
                        .map(|s| {
                            (
                                s.name,
                                Surface {
                                    tiles: Default::default(),
                                    tags: s.tags,
                                },
                            )
                        })
                        .collect();
                    for tile in tc.tiles.keys() {
                        surfaces.get_mut(&tile.surface)
                            .unwrap()
                            .tiles
                            .extend(get_tile_parts().iter().map(|p| p.get_path_components(tile)));
                    }

                    let info = MapInfo {
                        surfaces,
                        extension: TILE_EXTENSION,
                    };

                    let mut find_replace = HashMap::new();
                    find_replace.insert(
                        "$MAP_DATA$".to_owned(),
                        serde_json::to_string(&info).unwrap(),
                    );
                    extract_dir(&WEB, &output, &find_replace).unwrap();

                    send_result.send(MessageToMain::Finished).unwrap();
                }
            }
            MessageToMain::FinishBuildParent { parent, image } => {
                send_work
                    .send(MessageToWorker::TileWriteParts {
                        tile: parent,
                        image,
                    })
                    .unwrap();
            }
        }
    }
}
