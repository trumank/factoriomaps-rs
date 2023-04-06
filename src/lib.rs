#![feature(int_roundings)]

pub mod render;

use std::sync::{Arc, RwLock};

struct HookFile(String, Vec<u8>);
impl HookFile {
    fn new(path: String) -> Self {
        Self(path, vec![])
    }
}

lazy_static::lazy_static! {
    static ref OPEN_FILES: Arc<RwLock<std::collections::HashMap<usize, Box<HookFile>>>> = Default::default();
}

hooky::define_hook! {
    unsafe fn fopen(c_filename: *const libc::c_char, c_mode: *const libc::c_char) -> *mut libc::FILE {
        unsafe {
            let filename = std::ffi::CStr::from_ptr(c_filename).to_str().unwrap();
            let mode = std::ffi::CStr::from_ptr(c_mode).to_str().unwrap();
            if filename.ends_with(".json") && mode.contains('w') {
                println!("fopen ({mode}) {filename}");
            }
            if (filename.ends_with("info.json") || filename.ends_with(".png")) && mode.contains('w') {
                let file = Box::new(HookFile::new(filename.to_owned()));
                let ptr = (&*file as *const HookFile) as *mut libc::FILE;
                OPEN_FILES.write().unwrap().insert(ptr as usize, file);
                println!("fopen ({}) {}: {ptr:?}", mode, filename);
                ptr
            } else {
                real::fopen(c_filename, c_mode)
            }
        }
    }

    unsafe fn fdopen(fd: libc::c_int, mode: *const libc::c_char) -> *mut libc::FILE {
        unsafe {
            let result = real::fdopen(fd, mode);
            println!("fdopen ({}) {}: {:?}", std::ffi::CStr::from_ptr(mode).to_str().unwrap(), fd, result);

            result
        }
    }

    unsafe fn fwrite(ptr: *const libc::c_void, size: libc::size_t, nobj: libc::size_t, stream: *mut libc::FILE) -> libc::size_t {
        unsafe {
            if OPEN_FILES.read().unwrap().contains_key(&(stream as usize)) {
                let hfile = &mut *(stream as *mut HookFile);
                let data = std::slice::from_raw_parts(ptr as *const u8, size * nobj);
                hfile.1.extend_from_slice(data);
                //println!("write ptr={ptr:?} size={size} nobj={nobj}: => {:?}", stream);
                return nobj;
            }
            real::fwrite(ptr, size, nobj, stream)
        }
    }
    unsafe fn fflush(file: *mut libc::FILE) -> libc::c_int {
        unsafe {
            if OPEN_FILES.read().unwrap().contains_key(&(file as usize)) {
                return 0;
            }
            real::fflush(file)
        }
    }
    unsafe fn fclose(file: *mut libc::FILE) -> libc::c_int {
        unsafe {
            if OPEN_FILES.read().unwrap().contains_key(&(file as usize)) {
                let hfile = &mut *(file as *mut HookFile);
                if hfile.0.ends_with(".info") {
                    println!("close {}: {}", hfile.0, std::str::from_utf8(&hfile.1).unwrap());
                    std::fs::write("out.txt", &hfile.1).unwrap();
                } else if hfile.0.ends_with(".png") {
                    println!("close {}", hfile.0);
                }
                //println!("close");
                OPEN_FILES.write().unwrap().remove(&(file as usize));
                //libc::kill(std::process::id() as i32, libc::SIGTERM);
                return 0;
            }
            real::fclose(file)
        }
    }
}
