#![feature(nll)]

use std::{fs, path::Path};
use std::collections::{HashMap, HashSet};
use std::num::NonZeroU32;

use rayon::prelude::*;

use image::{DynamicImage, GenericImage, GenericImageView};
use webp::*;

use fast_image_resize as fr;


//const TILE_SIZE: u32 = 2048;
const TILE_SIZE: u32 = 1024;
const MAX_ZOOM: i32 = 20;
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
            x: self.x / 2,
            y: self.y / 2,
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

fn main() {
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
