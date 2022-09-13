#![feature(nll)]

use std::convert::TryInto;
use std::{fs, path::Path};
use std::collections::HashMap;
use std::num::NonZeroU32;
use std::thread;
use std::sync::mpsc::channel;

use image::{DynamicImage, GenericImage, GenericImageView};

use fast_image_resize as fr;


const TILE_SIZE: u32 = 2048;
const MAX_ZOOM: i32 = 20;

#[derive(Debug, Copy, Clone, Eq, PartialEq, PartialOrd, Ord, Hash)]
struct Tile {
    zoom: i32,
    x: i32,
    y: i32,
}

impl Tile {
    /// Returns tile containing this tile
    fn zoom_out(self) -> Tile {
        Tile {
            zoom: self.zoom - 1,
            x: self.x / 2,
            y: self.y / 2,
        }
    }
    /// Returns tile inside this tile with the smallest coordinates
    fn zoom_in(self) -> Tile {
        Tile {
            zoom: self.zoom + 1,
            x: self.x * 2,
            y: self.y * 2,
        }
    }
    /// Returns translated Tile offset
    fn translate(self, x: i32, y: i32) -> Tile {
        Tile {
            zoom: self.zoom,
            x: self.x + x,
            y: self.y + y,
        }
    }
}

#[derive(Debug,Clone)]
struct Surface {
    name: String,
}

impl Surface {
    fn new(name: String) -> Surface {
        Surface {
            name,
        }
    }
    fn load(&self, tile: Tile) -> Option<DynamicImage> {
        let img = if tile.zoom == MAX_ZOOM {
            println!("loading tile from disk {:#?}", tile);
            let string = &format!("factorio/script-output/{},{},{}.png", self.name, tile.x, tile.y);
            let path = Path::new(&string);
            image::open(&path).ok()
        } else {
            let zoom = tile.zoom_in();

            let q1 = self.load(zoom);
            let q2 = self.load(zoom.translate(1, 0));
            let q3 = self.load(zoom.translate(0, 1));
            let q4 = self.load(zoom.translate(1, 1));

            if q1.is_some() || q2.is_some() || q3.is_some() || q4.is_some() {
                let mut full_size = DynamicImage::new_rgba8(TILE_SIZE * 2, TILE_SIZE * 2);
                if let Some(t) = q1 { full_size.copy_from(&t, 0, 0).unwrap(); }
                if let Some(t) = q2 { full_size.copy_from(&t, TILE_SIZE, 0).unwrap(); }
                if let Some(t) = q3 { full_size.copy_from(&t, 0, TILE_SIZE).unwrap(); }
                if let Some(t) = q4 { full_size.copy_from(&t, TILE_SIZE, TILE_SIZE).unwrap(); }

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
            let parts = 4;
            let part_size = TILE_SIZE / parts;
            for x in 0..parts {
                for y in 0..parts {
                    let sub_img = img.view(x * part_size, y * part_size, part_size, part_size).to_image();
                    let path_str = &format!(
                        "web/Images/4/{}/day/{}/{}/{}.png",
                        &self.name,
                        tile.zoom,
                        x as i32 + tile.x * parts as i32,
                        y as i32 + tile.y * parts as i32
                    );
                    let path = Path::new(path_str);
                    fs::create_dir_all(&path.parent().unwrap()).unwrap();
                    sub_img.save(&path).unwrap();
                }
            }
        }
        img
    }
}

fn main() {
    let paths = fs::read_dir("factorio/script-output").unwrap();

    let mut surfaces = HashMap::new();

    for entry in paths {
        let path = entry.unwrap().path();
        let name = path.file_stem().and_then(std::ffi::OsStr::to_str).unwrap();
        let mut split = name.split(",");
        let surface = split.next().unwrap().to_owned();
        let x = split.next().unwrap().parse::<i32>().unwrap();
        let y = split.next().unwrap().parse::<i32>().unwrap();
        //println!("Name: {}", path.unwrap().path().display())

        println!("{:#?}", Tile {
            zoom: MAX_ZOOM,
            x,
            y,
        });
        surfaces.entry(surface.to_owned()).or_insert_with(|| Surface::new(surface));

        /*
        .load(Tile {
            zoom: MAX_ZOOM,
            x,
            y,
        });
        */
    }
    for surface in surfaces.values_mut() {
        let s1 = surface.clone();
        let s2 = surface.clone();
        let s3 = surface.clone();
        let s4 = surface.clone();
        let q1 = thread::spawn(move || {
            s1.load(Tile {
                zoom: 15,
                x: -1,
                y: -1,
            });
        });
        let q2 = thread::spawn(move || {
            s2.load(Tile {
                zoom: 15,
                x: 0,
                y: -1,
            });
        });
        let q3 = thread::spawn(move || {
            s3.load(Tile {
                zoom: 15,
                x: -1,
                y: 0,
            });
        });
        let q4 = thread::spawn(move || {
            s4.load(Tile {
                zoom: 15,
                x: 0,
                y: 0,
            });
        });
        q1.join().expect("q1 panicked");
        q2.join().expect("q2 panicked");
        q3.join().expect("q3 panicked");
        q4.join().expect("q4 panicked");
    }
    //println!("{:#?}", surfaces);
}
