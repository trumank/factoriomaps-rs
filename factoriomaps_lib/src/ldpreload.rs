use crossbeam::channel::{unbounded, Receiver, Sender};
use std::ffi::{CStr, OsStr};
use std::sync::{Arc, Mutex};

use crate::render::{self, MessageToMain, MessageToWorker, VirtualFile};

lazy_static::lazy_static! {
    static ref OPEN_FILES: Arc<Mutex<std::collections::HashMap<usize, Box<VirtualFile>>>> = Default::default();

    static ref SR_RESULT: (Sender<MessageToMain>, Receiver<MessageToMain>) = unbounded::<MessageToMain>();
    static ref SR_WORK: (Sender<MessageToWorker>, Receiver<MessageToWorker>) = unbounded::<MessageToWorker>();
}

hooky::define_hook! {
    unsafe fn fopen(c_filename: *const libc::c_char, c_mode: *const libc::c_char) -> *mut libc::FILE {
        let filename = unsafe { CStr::from_ptr(c_filename) }.to_str().unwrap();
        let mode = unsafe { CStr::from_ptr(c_mode) }.to_str().unwrap();
        let path = std::path::Path::new(filename);
        if  mode.contains('w') && (path.file_name() == Some(OsStr::new("info.json")) || path.extension() == Some(OsStr::new("bmp"))) {
            let file = Box::new(VirtualFile::new(filename));
            let ptr = (&*file as *const VirtualFile) as *mut libc::FILE;
            OPEN_FILES.lock().unwrap().insert(ptr as usize, file);
            return ptr;
        }
        unsafe { real::fopen(c_filename, c_mode) }
    }

    unsafe fn fwrite(ptr: *const libc::c_void, size: libc::size_t, nobj: libc::size_t, file: *mut libc::FILE) -> libc::size_t {
        if let Some(vfile) = OPEN_FILES.lock().unwrap().get_mut(&(file as usize)) {
            let data = unsafe { std::slice::from_raw_parts(ptr as *const u8, size * nobj) };
            vfile.data.extend_from_slice(data);
            return nobj;
        }
        unsafe { real::fwrite(ptr, size, nobj, file) }
    }
    unsafe fn fflush(file: *mut libc::FILE) -> libc::c_int {
        if OPEN_FILES.lock().unwrap().contains_key(&(file as usize)) {
            return 0;
        }
        unsafe { real::fflush(file) }
    }
    unsafe fn fclose(file: *mut libc::FILE) -> libc::c_int {
        if let Some(vfile) = OPEN_FILES.lock().unwrap().remove(&(file as usize)) {
            if vfile.path.file_name() == Some(OsStr::new("info.json")) {
                main();
            }
            SR_RESULT.0
                .send(MessageToMain::File(*vfile))
                .unwrap();
            return 0;
        }
        unsafe { real::fclose(file) }
    }
}

#[repr(C)]
struct CxxString {
    data: *const u8,
    length: usize,
}
impl CxxString {
    fn to_str(&self) -> &str {
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(self.data, self.length) }).unwrap()
    }
}

#[no_mangle]
extern "C" fn save_image(path: *const CxxString,
                             width: u32,
                             height: u32,
                             bytes_per_pixel: u32,
                             data: *const u8,
                             _pitch: i32,
                             _quality: u8) {
    let path = std::path::Path::new(unsafe { (*path).to_str() });

    let data = unsafe { std::slice::from_raw_parts(data, (width * height * bytes_per_pixel) as usize) }.to_vec();

    let mut split = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap()
        .split(',');
    let surface = split.next().unwrap().to_owned();
    let x = split.next().unwrap().parse::<i32>().unwrap();
    let y = split.next().unwrap().parse::<i32>().unwrap();

    let tile = crate::render::Tile::new_max_zoom(surface, x, y);
    let image = image::DynamicImage::ImageRgba8(
        image::RgbaImage::from_raw(width, height, data).unwrap(),
    );
    SR_WORK.0
        .send(MessageToWorker::TileWriteParts { tile, image })
        .unwrap();
}

fn main() {
    unsafe {
        use udbg::prelude::UDbgEngine;
        let mut engine = udbg::os::DefaultEngine::default();
        let target = engine.open_self().unwrap();
        let factorio = target.enum_module().unwrap().find(|m| m.data().name.starts_with("factorio")).unwrap();

        let sym = factorio.get_symbol("_ZN12MemoryBitmap9saveImageERKN10Filesystem4PathEjjjPKhih").unwrap();
        let mut address = sym.offset as usize;
        if &*factorio.data().name != "factorio" {
            address += factorio.data().base; // in package build symbol is relative to the module
        }
        retour::RawDetour::new(address as *const (), save_image as *const ()).unwrap().enable().unwrap();
    }

    let output = std::env::var("FBRS_OUTPUT").unwrap();

    let (result_rx, work_tx, result_tx) =
        (SR_RESULT.1.clone(), SR_WORK.0.clone(), SR_RESULT.0.clone());
    std::thread::spawn(move || {
        let res = crossbeam::scope(|scope| {
            render::spawn_threads(&output, scope, SR_WORK.1.clone(), SR_RESULT.0.clone());
            render::main_loop(output, result_rx, work_tx, result_tx);
            unsafe {
                libc::kill(std::process::id() as i32, libc::SIGTERM);
            }
        });
        res.unwrap();
    });
}
